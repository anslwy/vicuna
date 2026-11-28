// Sessions reaching another Vicuna server.
//
// `manage_sessions` with `action: "servers"` or `server: "<saved name>"` lands
// here (server side, after vicuna_mcp checked the session's `remote` permission
// plus Read/Message permissions or lead role as required by the action). The
// target is resolved only from this machine's saved servers (`~/vicuna/config/
// servers.json`): a URL in the tool call is never dialled, so a prompt
// injection can't point a session at a stranger's box. Calls go to the
// remote's `cli_session` action, the same one `vicuna session …` uses.
//
// Reports: the remote server can't reach us, so we watch. After a reported
// create/send a task polls the remote session until its turn ends, then
// delivers the usual `Worker session finished` message to the lead locally.
// Watches persist in `~/vicuna/server/remote-watches.json` and resume at
// startup, so a restart of this server doesn't lose them.

use super::{persist, turns, BridgeState};
use crate::servers::{self, ServerEntry};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const CALL_TIMEOUT: Duration = Duration::from_secs(120);
const POLL_EVERY: Duration = Duration::from_secs(5);
/// A remote turn that never starts (queued behind nothing, engine died) is
/// given this long before the watch reports what it has.
const START_GRACE_POLLS: u32 = 24;
/// Give up on a watch after this long (a day of 5 s polls).
const MAX_POLLS: u32 = 17_280;

/// Find a saved server by id, name (case-insensitive) or host. Never `local`.
pub fn resolve(name: &str, saved: &[ServerEntry]) -> Result<ServerEntry, String> {
    let q = name.trim();
    if q.is_empty() {
        return Err("`server` is empty.".into());
    }
    let ql = q.to_lowercase();
    let host = |url: &str| url.split("://").nth(1).unwrap_or(url).trim_end_matches('/').to_lowercase();
    let hit = saved.iter().find(|s| s.id == q)
        .or_else(|| saved.iter().find(|s| s.name.to_lowercase() == ql))
        .or_else(|| saved.iter().find(|s| { let h = host(&s.url); h == ql || h.split(':').next() == Some(ql.as_str()) }));
    hit.cloned().ok_or_else(|| {
        let names: Vec<String> = saved.iter().map(|s| s.name.clone()).collect();
        if names.is_empty() {
            format!("No saved server named \"{q}\". This machine has no saved servers; the user adds them from the server menu (Connect to new server…) or `vicuna server add`.")
        } else {
            format!("No saved server named \"{q}\". Saved servers: {}. Use `action: \"servers\"` to list them.", names.join(", "))
        }
    })
}

/// Text for `action: "servers"`.
pub fn servers_text(saved: &[ServerEntry]) -> String {
    if saved.is_empty() {
        return "No saved servers on this machine. The user adds them from the server menu (Connect to new server…) or `vicuna server add`.".into();
    }
    let rows: Vec<String> = saved.iter().map(|s| format!("- {} · {}", s.name, s.url)).collect();
    format!("Saved servers (pass the name as `server`):\n{}", rows.join("\n"))
}

async fn cli(entry: &ServerEntry, action: &str, args: Value) -> Result<Value, String> {
    let body = json!({"cmd": "cli_session", "args": merge(json!({"action": action}), args)});
    let v = servers::request(&entry.url, &entry.token, "/invoke", Some(&body), CALL_TIMEOUT).await?;
    unwrap_invoke(v).map_err(|e| {
        if e.contains("unknown bridge command") { format!("{} runs an older Vicuna without remote session control; upgrade it (`vicuna upgrade`).", entry.name) } else { format!("{}: {e}", entry.name) }
    })
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

fn unwrap_invoke(v: Value) -> Result<Value, String> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(v.get("data").cloned().unwrap_or(Value::Null))
    } else {
        Err(v.get("error").and_then(Value::as_str).unwrap_or("the server refused the action").to_string())
    }
}

fn last_reply(session: &Value) -> String {
    session["messages"].as_array().into_iter().flatten().rev()
        .find(|m| persist::is_assistant_reply(m))
        .and_then(|m| m["content"].as_str()).unwrap_or("").to_string()
}

fn row_text(r: &Value) -> String {
    let s = |k: &str| r[k].as_str().unwrap_or("").to_string();
    let who = match (s("engine"), s("model")) { (e, m) if !m.is_empty() => format!("{e}/{m}"), (e, _) => e };
    format!("- {} · \"{}\" · {} · {} · {}{}", s("id"), s("title"),
        if r["running"] == true { "running" } else { "idle" }, who, s("workspace"),
        if r["archived"] == true { " [archived]" } else { "" })
}

/// Run one `manage_sessions` action on a remote server.
pub async fn control(st: &Arc<BridgeState>, lead_id: &str, action: &str, args: &Value) -> Result<String, String> {
    let saved = servers::load();
    if action == "servers" {
        return Ok(servers_text(&saved));
    }
    let entry = resolve(args.get("server").and_then(Value::as_str).unwrap_or(""), &saved)?;
    let arg = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let report = args.get("report").and_then(Value::as_bool) != Some(false);
    let name = &entry.name;
    match action {
        "list" => {
            let rows = cli(&entry, "list", json!({})).await?;
            let all = args.get("include_archived").and_then(Value::as_bool) == Some(true);
            let lines: Vec<String> = rows.as_array().into_iter().flatten()
                .filter(|r| r["mode"] == "work" && (all || r["archived"] != true))
                .take(60).map(row_text).collect();
            Ok(format!("Work sessions on {name}:\n{}", if lines.is_empty() { "(none)".into() } else { lines.join("\n") }))
        }
        "create" => {
            let ws = arg("workspace");
            if ws.is_empty() {
                return Err(format!("`workspace` is required for a session on {name}: an absolute folder on THAT machine (your own workspace path doesn't exist there)."));
            }
            let msg = arg("message");
            let row = cli(&entry, "create", json!({"workspace": ws, "title": arg("title"), "engine": arg("engine"), "model": arg("model"), "prompt": msg})).await?;
            let id = row["id"].as_str().unwrap_or("").to_string();
            let watching = !msg.is_empty() && report;
            if watching { watch(st, Watch { server: entry.id.clone(), session: id.clone(), lead: lead_id.into() }); }
            Ok(format!("Created \"{}\" on {name} (server={name}, session_id={id}, engine={}, workspace={}).{}",
                row["title"].as_str().unwrap_or(""), row["engine"].as_str().unwrap_or(""), row["workspace"].as_str().unwrap_or(""),
                if watching { " Its turn has started; its reply will reach you when it ends." } else if !msg.is_empty() { " Its turn has started." } else { "" }))
        }
        _ => {
            let id = arg("session_id");
            if id.is_empty() {
                return Err(format!("`session_id` is required (see `list` with server=\"{name}\")."));
            }
            match action {
                "send" => {
                    let msg = arg("message");
                    if msg.is_empty() { return Err("`message` is required.".into()); }
                    let out = cli(&entry, "send", json!({"id": id, "text": msg})).await?;
                    let full = out["id"].as_str().unwrap_or(&id).to_string();
                    if report { watch(st, Watch { server: entry.id.clone(), session: full.clone(), lead: lead_id.into() }); }
                    Ok(format!("{} session {full} on {name}.{}", if out["queued"] == true { "Queued into" } else { "Started a turn in" },
                        if report { " Its reply will reach you when the turn ends." } else { "" }))
                }
                "status" => {
                    let s = cli(&entry, "get", json!({"id": id})).await?;
                    Ok(format!("{} on {name}: {}\nLast reply:\n{}", s["id"].as_str().unwrap_or(&id),
                        if s["running"] == true { "running" } else { "idle" }, persist::tail_text(&last_reply(&s), 3000)))
                }
                "stop" => { let o = cli(&entry, "stop", json!({"id": id})).await?; Ok(format!("Stopped {} on {name}.", o["id"].as_str().unwrap_or(&id))) }
                "archive" | "unarchive" => { let o = cli(&entry, action, json!({"id": id})).await?; Ok(format!("Session {} on {name}: {action} applied.", o["id"].as_str().unwrap_or(&id))) }
                "delete" => Err(format!("Deleting sessions on another server isn't allowed from a lead; the user can delete it from {name}'s window.")),
                "set_workspace" | "permissions" => Err(format!("`{action}` only works on sessions of this server.")),
                _ => Err(format!("Unknown action {action}.")),
            }
        }
    }
}

// ---------- reading sessions on another server ----------

/// `read_session` with `server`: the remote session's history, paged here
/// (`offset`/`limit`/`tail`, `max_chars_per_message`, `plain`).
pub async fn read(server: &str, session_id: &str, args: &Value) -> Result<String, String> {
    let entry = resolve(server, &servers::load())?;
    // updated servers answer `omit_images` with lightweight `images_meta` +
    // `uploads`; older ones ignore it and still send base64 (`page` strips it)
    let s = cli(&entry, "get", json!({"id": session_id, "omit_images": true})).await?;
    Ok(page(&entry.name, &s, args).to_string())
}

/// Image metadata of a remote message: `images_meta` (updated servers) or the
/// raw `images` data URLs (older servers), never base64.
fn message_images_meta(m: &Value) -> Vec<Value> {
    if let Some(meta) = m.get("images_meta").and_then(Value::as_array) {
        return meta.iter().filter_map(|x| {
            Some(json!({"index": x.get("index")?.as_u64()?, "mime": x.get("mime")?.as_str()?, "bytes": x.get("bytes")?.as_u64()?}))
        }).collect();
    }
    m.get("images").map(crate::attachments::images_meta).unwrap_or_default()
}

/// `read_attachment` with `server`: ONE inline image or upload file through
/// the remote's `server_read_attachment` action.
pub async fn read_attachment(server: &str, session_id: &str, which: &crate::attachments::Which) -> Result<crate::attachments::Attachment, String> {
    let entry = resolve(server, &servers::load())?;
    fetch_attachment(&entry.name, &entry.url, &entry.token, session_id, which).await
}

pub(crate) fn attachment_args(session_id: &str, which: &crate::attachments::Which) -> Value {
    use crate::attachments::Which;
    match which {
        Which::Inline { message_idx, image_index } => json!({"session_id": session_id, "message_idx": message_idx, "image_index": image_index}),
        Which::Upload { filename } => json!({"session_id": session_id, "filename": filename}),
    }
}

pub(crate) fn attachment_error(name: &str, e: &str) -> String {
    if e.contains("unknown bridge command") || e.contains("unknown action") || e.contains("unknown command") {
        format!("{name} runs an older Vicuna without attachment reading; the remote Vicuna server needs to be upgraded (`vicuna upgrade` on that machine).")
    } else {
        format!("{name}: {e}")
    }
}

async fn fetch_attachment(name: &str, url: &str, token: &str, session_id: &str, which: &crate::attachments::Which) -> Result<crate::attachments::Attachment, String> {
    let body = json!({"cmd": "server_read_attachment", "args": attachment_args(session_id, which)});
    let v = servers::request(url, token, "/invoke", Some(&body), CALL_TIMEOUT).await?;
    let data = unwrap_invoke(v).map_err(|e| attachment_error(name, &e))?;
    crate::attachments::from_wire(&data)
}

fn page(server: &str, s: &Value, args: &Value) -> Value {
    let msgs: Vec<&Value> = s["messages"].as_array().into_iter().flatten().collect();
    let total = msgs.len();
    let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(20).clamp(1, 100) as usize;
    let tail = args.get("tail").and_then(Value::as_bool) == Some(true);
    let offset = match args.get("offset").and_then(Value::as_u64) {
        Some(o) => (o as usize).min(total),
        None if tail => total.saturating_sub(limit),
        None => 0,
    };
    let max = args.get("max_chars_per_message").and_then(Value::as_u64).unwrap_or(8000) as usize;
    let plain = args.get("plain").and_then(Value::as_bool) == Some(true);
    let thinking = args.get("include_thinking").and_then(Value::as_bool) == Some(true);
    let out: Vec<Value> = msgs.iter().enumerate().skip(offset).take(limit).map(|(i, m)| {
        let raw = m["content"].as_str().unwrap_or("");
        let text = if plain { crate::session_insight::plain_text(raw) } else { raw.to_string() };
        let chars = text.chars().count();
        // tail pages keep each message's END (that's where the result is)
        let cut = if max == 0 || chars <= max { text } else if tail {
            format!("…{}", text.chars().skip(chars - max).collect::<String>())
        } else { format!("{}…", text.chars().take(max).collect::<String>()) };
        let mut o = json!({"index": i, "role": m["role"], "content": cut, "chars": chars});
        if thinking { if let Some(t) = m.get("thinking").filter(|t| t.is_string()) { o["thinking"] = t.clone(); } }
        if let Some(model) = m.get("model").filter(|v| v.is_string()) { o["model"] = model.clone(); }
        let imgs = message_images_meta(m);
        if !imgs.is_empty() { o["images"] = json!(imgs); }
        o
    }).collect();
    let next = offset + out.len();
    let uploads: Vec<Value> = s["uploads"].as_array().into_iter().flatten().filter_map(|u| {
        Some(json!({"name": u.get("name")?.as_str()?, "bytes": u.get("bytes")?.as_u64()?}))
    }).collect();
    let mut out = json!({
        "server": server, "session_id": s["id"], "title": s["title"], "mode": s["mode"],
        "workspace": s["workspace"], "workspace_note": "This folder is on the other server's machine, not here.",
        "engine": s["engine"], "model": s["model"], "running": s["running"],
        "total": total, "offset": offset, "returned": out.len(),
        "next_offset": if next < total { json!(next) } else { Value::Null },
        "messages": out,
    });
    if !uploads.is_empty() { out["uploads"] = json!(uploads); }
    out
}

/// `search_sessions` with `server`.
pub async fn search(server: &str, query: &str, limit: i64) -> Result<String, String> {
    let entry = resolve(server, &servers::load())?;
    let body = json!({"cmd": "server_search_chats", "args": {"query": query, "limit": limit.clamp(1, 50)}});
    let v = servers::request(&entry.url, &entry.token, "/invoke", Some(&body), CALL_TIMEOUT).await?;
    let mut data = unwrap_invoke(v).map_err(|e| format!("{}: {e}", entry.name))?;
    if let Some(o) = data.as_object_mut() { o.insert("server".into(), json!(entry.name)); }
    else { data = json!({"server": entry.name, "results": data}); }
    Ok(data.to_string())
}

// ---------- report watches ----------

#[derive(Serialize, Deserialize, Clone, PartialEq, Debug)]
pub struct Watch {
    pub server: String,
    pub session: String,
    pub lead: String,
}

fn watches_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VICUNA_REMOTE_WATCHES") { return Some(PathBuf::from(p)); }
    crate::opencode::home_dir().map(|h| h.join("vicuna").join("server").join("remote-watches.json"))
}

fn active() -> &'static Mutex<Vec<Watch>> {
    static A: OnceLock<Mutex<Vec<Watch>>> = OnceLock::new();
    A.get_or_init(|| Mutex::new(Vec::new()))
}

fn save(list: &[Watch]) {
    let Some(p) = watches_path() else { return };
    if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
    let _ = std::fs::write(p, serde_json::to_vec_pretty(list).unwrap_or_default());
}

/// Start watching (one watch per server+session; a newer lead replaces it).
fn watch(st: &Arc<BridgeState>, w: Watch) {
    {
        let Ok(mut a) = active().lock() else { return };
        if let Some(old) = a.iter_mut().find(|x| x.server == w.server && x.session == w.session) {
            old.lead = w.lead.clone();
            save(&a);
            return; // its task is already running and reads the lead fresh
        }
        a.push(w.clone());
        save(&a);
    }
    tokio::spawn(run_watch(st.clone(), w));
}

fn finish(w: &Watch) {
    if let Ok(mut a) = active().lock() {
        a.retain(|x| !(x.server == w.server && x.session == w.session));
        save(&a);
    }
}

/// Resume saved watches at server start.
pub fn spawn(st: Arc<BridgeState>) {
    let saved: Vec<Watch> = watches_path().and_then(|p| std::fs::read(p).ok())
        .and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if let Ok(mut a) = active().lock() { *a = saved.clone(); }
    for w in saved { tokio::spawn(run_watch(st.clone(), w)); }
}

async fn run_watch(st: Arc<BridgeState>, w: Watch) {
    let mut seen_running = false;
    let mut failures = 0u32;
    for n in 0..MAX_POLLS {
        tokio::time::sleep(POLL_EVERY).await;
        let Some(entry) = servers::load().into_iter().find(|s| s.id == w.server) else {
            finish(&w);
            return; // server forgotten by the user
        };
        let s = match cli(&entry, "get", json!({"id": w.session})).await {
            Ok(s) => { failures = 0; s }
            Err(e) if e.contains("no session") => { finish(&w); return; }
            Err(_) => { failures += 1; if failures > 720 { finish(&w); return; } continue; } // offline: keep trying ~1 h
        };
        if s["running"] == true { seen_running = true; continue; }
        if !seen_running && n < START_GRACE_POLLS && s["messages"].as_array().and_then(|m| m.last()).map(|m| m["role"] == "user").unwrap_or(false) {
            continue; // turn not picked up yet
        }
        let lead = active().lock().ok().and_then(|a| a.iter().find(|x| x.server == w.server && x.session == w.session).map(|x| x.lead.clone())).unwrap_or(w.lead.clone());
        finish(&w);
        let title = format!("{} · {}", s["title"].as_str().unwrap_or(""), entry.name);
        let engine = s["engine"].as_str().unwrap_or("");
        let model = s["model"].as_str();
        let text = persist::worker_done_message(&w.session, &title, engine, model, &last_reply(&s), false)
            + &format!("\n(This worker runs on server \"{}\"; pass server=\"{}\" to manage_sessions for it.)", entry.name, entry.name);
        let msg = json!({"role": "user", "content": text});
        for _ in 0..30 {
            match turns::deliver(&st, &lead, msg.clone()).await {
                Ok(()) => return,
                Err(e) if e.contains("no longer exists") || e.contains("not a Work session") => return,
                Err(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            }
        }
        return;
    }
    finish(&w);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, name: &str, url: &str) -> ServerEntry {
        ServerEntry { id: id.into(), url: url.into(), name: name.into(), token: "t".into(), last_used: 0 }
    }

    #[test]
    fn resolves_saved_servers_only() {
        let saved = vec![e("a1", "VPS", "https://vps.tail1.ts.net:8443"), e("b2", "deck", "http://100.1.2.3:7421")];
        assert_eq!(resolve("vps", &saved).unwrap().id, "a1");
        assert_eq!(resolve("b2", &saved).unwrap().id, "b2");
        assert_eq!(resolve("vps.tail1.ts.net", &saved).unwrap().id, "a1");
        assert_eq!(resolve("100.1.2.3:7421", &saved).unwrap().id, "b2");
        let err = resolve("https://evil.example", &saved).unwrap_err();
        assert!(err.contains("Saved servers: VPS, deck"), "{err}");
        assert!(resolve("x", &[]).unwrap_err().contains("no saved servers"));
        assert!(resolve(" ", &saved).is_err());
    }

    #[test]
    fn invoke_envelope_and_rows() {
        assert_eq!(unwrap_invoke(json!({"ok":true,"data":{"id":"x"}})).unwrap()["id"], "x");
        assert_eq!(unwrap_invoke(json!({"ok":false,"error":"no session matches z"})).unwrap_err(), "no session matches z");
        let r = json!({"id":"s1","title":"T","running":true,"engine":"claude","model":"haiku","workspace":"/w","archived":true});
        assert_eq!(row_text(&r), "- s1 · \"T\" · running · claude/haiku · /w [archived]");
        let s = json!({"messages":[{"role":"user","content":"q"},{"role":"assistant","content":"a1"},{"role":"user","content":"q2"}]});
        assert_eq!(last_reply(&s), "a1");
        let marked = json!({"messages":[{"role":"assistant","content":"real"},{"role":"assistant","content":"sum","summary":true},{"role":"assistant","content":"","contextCleared":true}]});
        assert_eq!(last_reply(&marked), "real", "summaries and clear markers are not replies");
        assert!(servers_text(&[]).contains("No saved servers"));
    }

    #[test]
    fn remote_read_pages_like_read_session() {
        let msgs: Vec<Value> = (0..5).map(|i| json!({"role": if i % 2 == 0 {"user"} else {"assistant"}, "content": format!("m{i}-abcdef")})).collect();
        let s = json!({"id":"s1","title":"T","workspace":"/home/me/repo","messages":msgs});
        let p = page("deck", &s, &json!({"limit": 2}));
        assert_eq!((p["offset"].as_u64(), p["returned"].as_u64(), p["next_offset"].as_u64()), (Some(0), Some(2), Some(2)));
        assert_eq!(p["server"], "deck");
        let t = page("deck", &s, &json!({"limit": 2, "tail": true, "max_chars_per_message": 3}));
        assert_eq!(t["offset"], 3);
        assert_eq!(t["messages"][1]["content"], "…def", "tail keeps the end");
        assert_eq!(t["messages"][1]["chars"], 9);
        assert!(t["next_offset"].is_null());
        assert_eq!(page("deck", &s, &json!({"offset": 99}))["returned"], 0);
    }

    #[test]
    fn remote_page_extracts_image_metadata_new_and_old_servers() {
        let b64 = "iVBORw==";
        // updated server: images_meta + uploads
        let new = json!({"id":"s","messages":[{"role":"user","content":"a","images_meta":[{"index":0,"mime":"image/png","bytes":4}]}],"uploads":[{"name":"r.pdf","bytes":9}]});
        let p = page("deck", &new, &json!({}));
        assert_eq!(p["messages"][0]["images"], json!([{"index":0,"mime":"image/png","bytes":4}]));
        assert_eq!(p["uploads"], json!([{"name":"r.pdf","bytes":9}]));
        // older server ignoring omit_images: raw data URLs, never echoed
        let old = json!({"id":"s","messages":[{"role":"user","content":"a","images":[format!("data:image/png;base64,{b64}")]},{"role":"assistant","content":"b"}]});
        let p = page("deck", &old, &json!({}));
        assert_eq!(p["messages"][0]["images"], json!([{"index":0,"mime":"image/png","bytes":4}]));
        assert!(p["messages"][1].get("images").is_none());
        assert!(p.get("uploads").is_none());
        assert!(!p.to_string().contains(b64));
    }

    #[test]
    fn server_omits_images_and_old_servers_get_a_clear_error() {
        let mut row = json!({"messages":[{"role":"user","content":"a","images":["data:image/jpeg;base64,QUJD"]},{"role":"assistant","content":"b"}]});
        super::turns::omit_images(&mut row, "../bad id");
        assert!(row["messages"][0].get("images").is_none());
        assert_eq!(row["messages"][0]["images_meta"], json!([{"index":0,"mime":"image/jpeg","bytes":3}]));
        assert!(row["messages"][1].get("images_meta").is_none());
        let e = attachment_error("VPS", "unknown bridge command: server_read_attachment");
        assert!(e.contains("needs to be upgraded") && e.contains("VPS"), "{e}");
        assert_eq!(attachment_error("VPS", "Message 3 not found"), "VPS: Message 3 not found");
        use crate::attachments::Which;
        assert_eq!(attachment_args("s", &Which::Inline { message_idx: 2, image_index: 1 }), json!({"session_id":"s","message_idx":2,"image_index":1}));
        assert_eq!(attachment_args("s", &Which::Upload { filename: "f".into() }), json!({"session_id":"s","filename":"f"}));
    }

    #[tokio::test]
    async fn server_read_attachment_round_trips_one_image() {
        use crate::attachments::{from_wire, parse_which, read_local, to_wire};
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE messages (chat_id TEXT, idx INTEGER, images TEXT)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO messages VALUES ('s', 0, '[\"data:image/gif;base64,R0lG\"]')").execute(&pool).await.unwrap();
        let args = attachment_args("s", &parse_which(&json!({"message_idx": 0})).unwrap());
        let wire = to_wire(&read_local(Some(&pool), "s", &parse_which(&args).unwrap()).await.unwrap());
        assert_eq!(wire["mime"], "image/gif");
        let a = from_wire(&wire).unwrap();
        assert_eq!(a.data, b"GIF");
    }
}
