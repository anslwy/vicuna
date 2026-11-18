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

async fn load(st: &BridgeState, id: &str) -> Result<Value, String> {
    let db = st.db.as_ref().ok_or("this bridge stores no sessions (run `vicuna serve`)")?;
    crate::server_store::load_chat(db, id).await?.ok_or_else(|| format!("no session {id}"))
}

/// Message edits/deletes are refused while the session's turn runs (the
/// window gated them on `busy`; the server is now the authority).
fn idle(st: &BridgeState, id: &str) -> Result<(), String> {
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
        "session_update" => {
            let id = arg_str(args, "id")?;
            let _w = st.chat_lock.lock().await;
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
            if let Some(r) = args.get("role") {
                if r.as_str() == Some("lead") {
                    extra["role"] = json!("lead");
                    // making a session the lead also pins it (the sidebar rule)
                    extra["pinned"] = json!(true);
                } else if let Some(o) = extra.as_object_mut() {
                    o.remove("role");
                }
            }
            chat["extra"] = json!(extra.to_string());
            if super::chat::is_running(st, &id) {
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
}
