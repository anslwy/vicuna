// Session + message actions: the database-changing operations every
// frontend shares (the Svelte app, the `vicuna` CLI, headless tests, future
// mobile apps). Each loads the stored session, applies one change with the
// exact rules the window used to apply locally, writes it as the session's
// next revision, and returns the updated session (with `rev`) so a window
// adopts it as its new baseline.
//
//   session_update  {id, title?, pinned?, archived?, role?: "lead"|null}
//   session_delete  {id}                       stops its live turn first
//   session_fork    {id, at}                   messages 0..=at → new session
//   message_edit    {id, index, content, images?, truncate?}
//   message_delete  {id, index}                user↔reply pairs go together

use std::sync::Arc;

use serde_json::{json, Value};

use super::turns::{live_sessions, live_turn, now_ms, uuid};
use super::BridgeState;

/// The edited message, applied to `msgs` (pure; the window's rules):
/// a user edit replaces the message with a plain user message (images kept
/// only when given); a reply keeps its other fields and its exact text
/// (trailing whitespace matters when continuing). `truncate` drops
/// everything after it (edit-before-regenerate). Returns whether the title
/// should follow the text (a user edit of a "New session").
pub fn apply_edit(msgs: &mut Vec<Value>, index: usize, content: &str, images: Option<Value>, truncate: bool) -> Result<bool, String> {
    let target = msgs.get(index).cloned().ok_or_else(|| format!("no message {index}"))?;
    let images = images.filter(|i| i.as_array().is_some_and(|a| !a.is_empty()));
    let user = target.get("role").and_then(Value::as_str) != Some("assistant");
    let text = if user { content.trim().to_string() } else { content.to_string() };
    if text.trim().is_empty() && images.is_none() {
        return Err("an edit needs text or an image".into());
    }
    let updated = if user {
        let mut m = json!({ "role": "user", "content": text });
        if let Some(i) = images {
            m["images"] = i;
        }
        m
    } else {
        let mut m = target.clone();
        m["content"] = json!(text);
        match images {
            Some(i) => m["images"] = i,
            None => {
                if let Some(o) = m.as_object_mut() {
                    o.remove("images");
                }
            }
        }
        m
    };
    msgs[index] = updated;
    if truncate {
        msgs.truncate(index + 1);
    }
    Ok(user)
}

/// The messages a delete at `index` removes (pure; the window's rule):
/// a user message takes the reply after it, a reply takes the user message
/// before it. Returns (from, count).
pub fn delete_span(msgs: &[Value], index: usize) -> Result<(usize, usize), String> {
    let role = |i: usize| msgs.get(i).and_then(|m| m.get("role")).and_then(Value::as_str);
    match role(index) {
        None => Err(format!("no message {index}")),
        Some("user") if role(index + 1) == Some("assistant") => Ok((index, 2)),
        Some("assistant") if index > 0 && role(index - 1) == Some("user") => Ok((index - 1, 2)),
        Some(_) => Ok((index, 1)),
    }
}

/// "New session" titles follow the first edited user text (32 chars + "..").
pub fn retitle(title: &str, text: &str) -> Option<String> {
    if title != "New session" && title != "New Session" {
        return None;
    }
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    let short: String = t.chars().take(32).collect();
    Some(if t.chars().count() > 32 { format!("{short}..") } else { short })
}

fn extra_of(chat: &Value) -> Value {
    chat.get("extra").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).filter(Value::is_object).unwrap_or(json!({}))
}

fn arg_str(args: &Value, k: &str) -> Result<String, String> {
    args.get(k).and_then(Value::as_str).map(str::to_string).filter(|s| !s.is_empty()).ok_or_else(|| format!("missing arg {k}"))
}

fn arg_index(args: &Value, k: &str) -> Result<usize, String> {
    args.get(k).and_then(Value::as_u64).map(|n| n as usize).ok_or_else(|| format!("missing arg {k}"))
}

/// A loaded session (load_chat: `forkedFrom` parsed into an object) as a
/// save row (`forkedFrom` as JSON text) — writing the loaded shape back fails
/// with "invalid type: map, expected a string" on every forked session.
pub fn as_save_row(chat: &mut Value) {
    if chat.get("forkedFrom").is_some_and(Value::is_object) {
        chat["forkedFrom"] = json!(chat["forkedFrom"].to_string());
    }
}

/// Write `chat` as the session's next revision (refused while a Work turn
/// is recording it), then return the stored copy.
async fn commit(st: &BridgeState, chat: &mut Value) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("this bridge stores no sessions (run `vicuna serve`)")?;
    let id = chat["id"].as_str().unwrap_or("").to_string();
    chat["baseRev"] = chat["rev"].clone();
    as_save_row(chat);
    let out = crate::server_store::merge_json(db, &json!([chat]), vec![], &live_sessions(st)).await?;
    if !out.conflicts.is_empty() {
        return Err("the session is busy (the server is recording a turn); try again when it settles".into());
    }
    crate::server_store::load_chat(db, &id).await?.ok_or_else(|| "session vanished".into())
}

pub(super) async fn load(st: &BridgeState, id: &str) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("this bridge stores no sessions (run `vicuna serve`)")?;
    crate::server_store::load_chat(db, id).await?.ok_or_else(|| format!("no session {id}"))
}

/// Message edits/deletes are refused while the session's turn runs (the
/// window gated them on `busy`; the server is now the authority).
pub(super) fn idle(st: &BridgeState, id: &str) -> Result<(), String> {
    if live_turn(st, id).is_some() || super::chat::is_running(st, id) {
        return Err("the session is running; wait for it or stop it first".into());
    }
    Ok(())
}

pub(super) fn fork_row(src: &Value, id: &str, at: usize, fork_id: &str, msgs: Vec<Value>) -> Value {
    let title = src["title"].as_str().unwrap_or("Untitled").to_string();
    // forks are plain sessions: no lead role, pin, archive or goal;
    // the harness/model/MCP pick carries over
    let mut extra = extra_of(src);
    if let Some(o) = extra.as_object_mut() {
        o.remove("mcpEnabled");
        o.remove("skillsEnabled");
        o.remove("mcp");
        for k in ["role", "pinned", "archived", "leadId"] {
            o.remove(k);
        }
    }
    let now = now_ms();
    let mut fork = json!({
        "id": fork_id,
        "title": format!("{title} (fork)"),
        "createdAt": now,
        "updatedAt": now,
        "messages": msgs,
        // save rows carry it as JSON text (loads parse it back)
        "forkedFrom": json!({ "id": id, "title": title }).to_string(),
        "forkFromIndex": at,
        "mode": src["mode"].as_str().unwrap_or("chat"),
        "workspace": src["workspace"].clone(),
    });
    if extra.as_object().is_some_and(|o| !o.is_empty()) {
        fork["extra"] = json!(extra.to_string());
    }
    fork
}

pub async fn dispatch(st: &Arc<BridgeState>, cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "session_create" => {
            let id = arg_str(args, "id")?;
            if id.len() > 128 || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err("Invalid session id.".into());
            }
            let mode = args["mode"].as_str().unwrap_or("chat");
            if !matches!(mode, "chat" | "work") {
                return Err("Invalid session mode.".into());
            }
            let title = args["title"].as_str().map(str::trim).filter(|s| !s.is_empty()).unwrap_or("New session");
            let workspace = match args["workspace"].as_str().filter(|path| !path.trim().is_empty()) {
                Some(path) if mode == "work" => Some(crate::work_validate_root_logic(path.to_owned()).await?),
                _ => None,
            };
            let db = st.db.as_ref().ok_or("this bridge stores no sessions")?;
            let _write = st.chat_lock.lock().await;
            if crate::server_store::load_chat(db, &id).await?.is_some() {
                return Err("A session with that id already exists.".into());
            }
            let now = now_ms();
            let mut chat = json!({
                "id": id, "title": title, "createdAt": now, "updatedAt": now,
                "mode": mode, "workspace": workspace, "messages": [],
            });
            if let Some(source) = args["forkedFromId"].as_str() {
                let original = load(st, source).await?;
                chat["forkedFrom"] = json!({"id": source, "title": original["title"]});
                if let Some(model) = args["placeholderModel"].as_str() {
                    chat["messages"] = json!([super::persist::assistant_placeholder(model)]);
                }
            }
            as_save_row(&mut chat);
            crate::server_store::create_chat(db, &chat).await?;
            load(st, &id).await
        }
        "session_update" => {
            let id = arg_str(args, "id")?;
            let _w = st.chat_lock.lock().await;
            if let Some(toggle) = args.get("skillToggle") {
                let slug = arg_str(toggle, "slug")?;
                let enabled = toggle["enabled"].as_bool().ok_or("Missing skill enabled state")?;
                crate::skills::read_skill(&slug)?;
                let db = st.db.as_ref().ok_or("this bridge stores no sessions")?;
                crate::server_store::set_session_skill(db, &id, &slug, enabled, &crate::skills::defaults()).await?;
                return load(st, &id).await;
            }
            if let Some(toggle) = args.get("mcpToggle") {
                let server_id = arg_str(toggle, "id")?;
                let enabled = toggle["enabled"].as_bool().ok_or("Missing MCP enabled state")?;
                if crate::mcp::is_fixed(&server_id) {
                    return Err("vicuna-mcp is always enabled".into());
                }
                let db = st.db.as_ref().ok_or("this bridge stores no sessions")?;
                crate::server_store::set_session_mcp(db, &id, &server_id, enabled).await?;
                return load(st, &id).await;
            }
            let mut chat = load(st, &id).await?;
            let mut extra = extra_of(&chat);
            if let Some(t) = args.get("title").and_then(Value::as_str) {
                let t = t.trim();
                if t.is_empty() {
                    return Err("a title can't be empty".into());
                }
                chat["title"] = json!(t);
            }
            for k in ["pinned", "archived"] {
                if let Some(b) = args.get(k).and_then(Value::as_bool) {
                    extra[k] = json!(b);
                }
            }
            if let Some(p) = args.get("permissions").filter(|v| v.is_object()) {
                for k in ["read", "message"] {
                    if let Some(b) = p.get(k).and_then(Value::as_bool) {
                        extra["permissions"][k] = json!(b);
                    }
                }
            }
            let mut promoting_lead = false;
            let mut demoting_self = false;
            if let Some(r) = args.get("role") {
                if r.as_str() == Some("lead") {
                    promoting_lead = true;
                    extra["role"] = json!("lead");
                    // only one session may be the lead; making one the lead also pins it
                    extra["pinned"] = json!(true);
                } else {
                    demoting_self = extra.get("role").and_then(Value::as_str) == Some("lead");
                    if let Some(o) = extra.as_object_mut() {
                        o.remove("role");
                    }
                }
            }
            // the session's own harness + model (what its next turn runs on)
            if let Some(eng) = args.get("engine").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty()) {
                let e = crate::work_engine::Engine::parse(eng).ok_or_else(|| format!("unknown harness {eng}"))?;
                extra["engine"] = json!(e.id());
                extra["workModel"] = match args.get("workModel").and_then(Value::as_str).filter(|m| !m.is_empty()) {
                    Some(m) => json!(m),
                    None => Value::Null,
                };
            }
            if let Some(workspace) = args.get("workspace") {
                idle(st, &id)?;
                if chat["mode"] != "work" {
                    return Err("Only Work sessions have a workspace.".into());
                }
                chat["workspace"] = match workspace.as_str().filter(|path| !path.trim().is_empty()) {
                    Some(path) => json!(crate::work_validate_root_logic(path.to_owned()).await?),
                    None => Value::Null,
                };
            }
            if promoting_lead {
                let db = st.db.as_ref().ok_or("this bridge stores no sessions")?;
                let demoted = crate::server_store::clear_other_leads(db, &id).await?;
                for old_id in demoted {
                    if let Some((_, turn)) = live_turn(st, &old_id) {
                        turn.lead.store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            } else if demoting_self {
                if let Some((_, turn)) = live_turn(st, &id) {
                    turn.lead.store(false, std::sync::atomic::Ordering::Relaxed);
                }
            }
            chat["extra"] = json!(extra.to_string());
            if live_turn(st, &id).is_some() || super::chat::is_running(st, &id) {
                // The live recorder owns messages. Sidebar changes touch only
                // metadata, and the recorder's next checkpoint preserves them.
                let db = st.db.as_ref().ok_or("this bridge stores no sessions")?;
                crate::server_store::write_title_and_extra(db, &id, &chat).await?;
                return load(st, &id).await;
            }
            commit(st, &mut chat).await
        }
        "session_delete" => {
            let id = arg_str(args, "id")?;
            super::turns::cli_session(st, "delete", &json!({ "id": id })).await
        }
        "session_fork" => {
            let id = arg_str(args, "id")?;
            let at = arg_index(args, "at")?;
            let db = st.db.as_ref().ok_or("this bridge stores no sessions (run `vicuna serve`)")?;
            let src = load(st, &id).await?;
            let msgs = src["messages"].as_array().cloned().unwrap_or_default();
            if at >= msgs.len() {
                return Err(format!("no message {at}"));
            }
            let fork_id = uuid();
            let fork = fork_row(&src, &id, at, &fork_id, msgs[..=at].to_vec());
            let _w = st.chat_lock.lock().await;
            crate::server_store::create_chat(db, &fork).await?;
            crate::server_store::load_chat(db, &fork_id).await?.ok_or_else(|| "fork vanished".into())
        }
        "message_edit" => {
            let id = arg_str(args, "id")?;
            let index = arg_index(args, "index")?;
            let content = args.get("content").and_then(Value::as_str).unwrap_or("");
            let _w = st.chat_lock.lock().await;
            idle(st, &id)?;
            let mut chat = load(st, &id).await?;
            let mut msgs = chat["messages"].as_array().cloned().unwrap_or_default();
            let truncate = args.get("truncate").and_then(Value::as_bool).unwrap_or(false);
            let user = apply_edit(&mut msgs, index, content, args.get("images").cloned(), truncate)?;
            if user {
                chat["updatedAt"] = json!(now_ms());
                if let Some(t) = retitle(chat["title"].as_str().unwrap_or(""), content) {
                    chat["title"] = json!(t);
                }
            }
            chat["messages"] = Value::Array(msgs);
            commit(st, &mut chat).await
        }
        "message_delete" => {
            let id = arg_str(args, "id")?;
            let index = arg_index(args, "index")?;
            let _w = st.chat_lock.lock().await;
            idle(st, &id)?;
            let mut chat = load(st, &id).await?;
            let mut msgs = chat["messages"].as_array().cloned().unwrap_or_default();
            let (from, count) = delete_span(&msgs, index)?;
            msgs.drain(from..from + count);
            chat["messages"] = Value::Array(msgs);
            commit(st, &mut chat).await
        }
        other => Err(format!("unknown action {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(role: &str, c: &str) -> Value {
        json!({ "role": role, "content": c })
    }

    #[test]
    fn delete_pairs_like_the_window() {
        let v = vec![m("user", "a"), m("assistant", "A"), m("user", "b"), m("assistant", "B"), m("user", "c")];
        assert_eq!(delete_span(&v, 0).unwrap(), (0, 2), "user takes its reply");
        assert_eq!(delete_span(&v, 3).unwrap(), (2, 2), "reply takes its user");
        assert_eq!(delete_span(&v, 4).unwrap(), (4, 1), "a lone last user goes alone");
        assert!(delete_span(&v, 9).is_err());
        let lone = vec![m("assistant", "x")];
        assert_eq!(delete_span(&lone, 0).unwrap(), (0, 1));
    }

    #[test]
    fn edits_follow_the_window_rules() {
        let mut v = vec![m("user", "a"), json!({"role":"assistant","content":"A","model":"m","genMs":5.0}), m("user", "b")];
        // reply edit keeps model/stats and exact (trailing-space) text
        assert!(!apply_edit(&mut v, 1, "Yo! How's ", None, false).unwrap());
        assert_eq!(v[1]["content"], "Yo! How's ");
        assert_eq!(v[1]["model"], "m");
        assert_eq!(v.len(), 3);
        // user edit trims, becomes a plain user message, keeps later messages
        assert!(apply_edit(&mut v, 0, "  hello  ", None, false).unwrap());
        assert_eq!(v[0], json!({"role":"user","content":"hello"}));
        assert_eq!(v.len(), 3);
        // truncate drops everything after (edit before regenerate)
        apply_edit(&mut v, 0, "x", Some(json!(["data:img"])), true).unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0]["images"], json!(["data:img"]));
        assert!(apply_edit(&mut v, 0, "   ", None, false).is_err(), "empty edit refused");
    }

    #[test]
    fn loaded_fork_becomes_a_save_row() {
        let mut c = json!({"id":"f","forkedFrom":{"id":"a","title":"T"}});
        as_save_row(&mut c);
        assert_eq!(c["forkedFrom"], json!(r#"{"id":"a","title":"T"}"#));
        let mut plain = json!({"id":"p","forkedFrom":null});
        as_save_row(&mut plain);
        assert!(plain["forkedFrom"].is_null());
    }

    #[test]
    fn retitle_only_new_sessions() {
        assert_eq!(retitle("New session", "hello"), Some("hello".into()));
        assert_eq!(retitle("New Session", &"x".repeat(40)), Some(format!("{}..", "x".repeat(32))));
        assert_eq!(retitle("My chat", "hello"), None);
    }

    #[tokio::test]
    async fn session_update_enforces_single_lead() {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        crate::server_store::init_schema(&db).await.unwrap();
        let st = Arc::new(BridgeState { db: Some(db), ..Default::default() });
        dispatch(&st, "session_create", &json!({"id": "a", "title": "First", "mode": "work"})).await.unwrap();
        dispatch(&st, "session_create", &json!({"id": "b", "title": "Second", "mode": "work"})).await.unwrap();

        let a1 = dispatch(&st, "session_update", &json!({"id": "a", "role": "lead"})).await.unwrap();
        assert_eq!(extra_of(&a1)["role"], "lead");
        assert_eq!(extra_of(&a1)["pinned"], true);

        let b1 = dispatch(&st, "session_update", &json!({"id": "b", "role": "lead"})).await.unwrap();
        assert_eq!(extra_of(&b1)["role"], "lead");
        assert_eq!(extra_of(&b1)["pinned"], true);

        let b_perm = dispatch(&st, "session_update", &json!({"id": "b", "permissions": {"read": false, "message": false}})).await.unwrap();
        assert_eq!(extra_of(&b_perm)["permissions"], json!({"read": false, "message": false}));

        let a2 = load(&st, "a").await.unwrap();
        assert!(extra_of(&a2).get("role").is_none(), "previous lead must be demoted: {}", a2["extra"]);
        assert_eq!(extra_of(&a2)["pinned"], true, "previous lead keeps its pin");
        assert!(a2["rev"].as_i64().unwrap() > a1["rev"].as_i64().unwrap(), "demoted lead bumps rev");

        // A failing update must not demote the current lead.
        assert!(dispatch(&st, "session_update", &json!({"id": "a", "title": "   ", "role": "lead"})).await.is_err());
        let b2 = load(&st, "b").await.unwrap();
        assert_eq!(extra_of(&b2)["role"], "lead");
    }

    #[tokio::test]
    async fn session_update_sets_the_sessions_own_harness() {
        let db = sqlx::sqlite::SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        crate::server_store::init_schema(&db).await.unwrap();
        let st = Arc::new(BridgeState { db: Some(db), ..Default::default() });
        dispatch(&st, "session_create", &json!({"id": "a", "title": "A", "mode": "work"})).await.unwrap();
        dispatch(&st, "session_create", &json!({"id": "b", "title": "B", "mode": "work"})).await.unwrap();
        let a = dispatch(&st, "session_update", &json!({"id": "a", "engine": "claude", "workModel": "opus"})).await.unwrap();
        assert_eq!(extra_of(&a)["engine"], "claude");
        assert_eq!(extra_of(&a)["workModel"], "opus");
        let a = dispatch(&st, "session_update", &json!({"id": "a", "engine": "agy", "workModel": null})).await.unwrap();
        assert_eq!(extra_of(&a)["engine"], "agy");
        assert!(extra_of(&a)["workModel"].is_null());
        assert!(extra_of(&load(&st, "b").await.unwrap()).get("engine").is_none(), "other session untouched");
        assert!(dispatch(&st, "session_update", &json!({"id": "a", "engine": "nope"})).await.is_err());
    }
}
