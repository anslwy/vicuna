//! Bridge plugin supervisor & stdio JSON-RPC host.
//!
//! Each installed plugin whose `vicuna-plugin.json` declares a `bridge` is
//! spawned as a child process communicating over newline-delimited JSON-RPC on
//! stdin/stdout. The host forwards UI actions (`bridge_list/status/save/set/
//! unpair/remove`), pushes session turn events (`started`/`progress`/`ask`/
//! `finished`) and `session_deleted` notifications, and serves host RPCs
//! (`deliver_message`, `send_now`, `stop_turn`, `answer_ask`, `stage_upload`,
//! `session_info`, `list_engines`, `list_models`, `set_model`, `transcribe_ogg`).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use super::telegram::{phone_text, upload_note};
use super::turns::{self, live_turn};
use super::BridgeState;

// ---------- runtime registry ----------

struct RunningPlugin {
    bin_path: PathBuf,
    version: String,
    tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicU64,
    _last_error: Arc<Mutex<String>>,
    /// Handle to the child watcher task; aborting kills the child (`kill_on_drop`).
    task: tokio::task::JoinHandle<()>,
}

impl RunningPlugin {
    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        let line = json!({ "id": id, "method": method, "params": params }).to_string();
        if self.tx.send(line).is_err() {
            if let Ok(mut p) = self.pending.lock() {
                p.remove(&id);
            }
            return Err("bridge plugin process is not running".into());
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("bridge plugin closed connection".into()),
            Err(_) => {
                if let Ok(mut p) = self.pending.lock() {
                    p.remove(&id);
                }
                Err(format!("bridge plugin timed out on {method}"))
            }
        }
    }

    fn notify(&self, method: &str, params: Value) {
        let line = json!({ "method": method, "params": params }).to_string();
        let _ = self.tx.send(line);
    }
}

#[derive(Default)]
struct Registry {
    plugin_gen: u64,
    plugins: HashMap<String, Arc<RunningPlugin>>,
    /// turn_ids currently being mirrored to bridge plugins
    following: HashSet<String>,
}

fn reg() -> &'static Mutex<Registry> {
    static R: OnceLock<Mutex<Registry>> = OnceLock::new();
    R.get_or_init(Default::default)
}

pub fn notify_plugins_changed() {
    if let Ok(mut r) = reg().lock() {
        r.plugin_gen += 1;
    }
}

pub fn drop_for(sid: &str) {
    broadcast_notification("session_deleted", json!({ "sessionId": sid }));
}

fn broadcast_notification(method: &str, params: Value) {
    let list: Vec<Arc<RunningPlugin>> = reg()
        .lock()
        .map(|r| r.plugins.values().cloned().collect())
        .unwrap_or_default();
    for p in list {
        p.notify(method, params.clone());
    }
}

fn config_path_for(plugin_id: &str) -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("VICUNA_BRIDGE_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join(format!("{plugin_id}.json")));
    }
    Some(
        crate::opencode::home_dir()?
            .join("vicuna")
            .join("config")
            .join("plugins")
            .join(format!("{plugin_id}.json")),
    )
}

// ---------- supervisor ----------

pub fn spawn(st: Arc<BridgeState>) {
    // Plugin process supervisor
    let st_sup = st.clone();
    tokio::spawn(async move {
        loop {
            let pgen = reg().lock().map(|r| r.plugin_gen).unwrap_or(0);
            reconcile_plugins(&st_sup);
            for _ in 0..8 {
                tokio::time::sleep(Duration::from_millis(500)).await;
                if reg().lock().map(|r| r.plugin_gen).unwrap_or(0) != pgen {
                    break;
                }
            }
        }
    });

    // Session turn watcher: mirrors any live turn to running bridge plugins
    let st_watch = st;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(800)).await;
            let has_plugins = reg().lock().map(|r| !r.plugins.is_empty()).unwrap_or(false);
            if !has_plugins {
                continue;
            }
            let active_turns: Vec<(String, String)> = st_watch
                .session_turns
                .lock()
                .map(|m| m.iter().map(|(sid, (tid, _, _))| (sid.clone(), tid.clone())).collect())
                .unwrap_or_default();
            for (sid, tid) in active_turns {
                let should_follow = reg()
                    .lock()
                    .map(|mut r| r.following.insert(tid.clone()))
                    .unwrap_or(false);
                if should_follow {
                    tokio::spawn(follow_turn(st_watch.clone(), sid, tid));
                }
            }
        }
    });
}

fn reconcile_plugins(st: &Arc<BridgeState>) {
    let installed = crate::plugins::list_installed_bridges();
    let desired: HashMap<String, (String, PathBuf)> = installed
        .into_iter()
        .map(|(m, bin)| (m.id, (m.version, bin)))
        .collect();

    let mut to_stop = Vec::new();
    let mut to_start = Vec::new();
    {
        let Ok(mut r) = reg().lock() else { return };
        for (id, rp) in &r.plugins {
            match desired.get(id) {
                Some((ver, bin)) if *ver == rp.version && *bin == rp.bin_path && !rp.task.is_finished() => {}
                _ => to_stop.push(id.clone()),
            }
        }
        for id in to_stop {
            if let Some(rp) = r.plugins.remove(&id) {
                rp.task.abort();
            }
        }
        for (id, (ver, bin)) in desired {
            if !r.plugins.contains_key(&id) {
                to_start.push((id, ver, bin));
            }
        }
    }

    for (id, ver, bin) in to_start {
        if let Ok(rp) = spawn_plugin_proc(st.clone(), id.clone(), ver, bin) {
            if let Ok(mut r) = reg().lock() {
                r.plugins.insert(id, Arc::new(rp));
            }
        }
    }
}

fn spawn_plugin_proc(
    st: Arc<BridgeState>,
    plugin_id: String,
    version: String,
    bin_path: PathBuf,
) -> Result<RunningPlugin, String> {
    let mut cmd = Command::new(&bin_path);
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .env("VICUNA_BRIDGE_PLUGIN_ID", &plugin_id);
    if let Some(cfg) = config_path_for(&plugin_id) {
        cmd.env("VICUNA_BRIDGE_CONFIG_PATH", cfg);
    }
    #[cfg(windows)]
    {
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn {}: {e}", bin_path.display()))?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let stderr = child.stderr.take().ok_or("no stderr")?;

    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let last_error = Arc::new(Mutex::new(String::new()));

    let tx_reply = tx.clone();
    let pending_read = pending.clone();
    let err_stderr = last_error.clone();
    let pid_log = plugin_id.clone();

    let task = tokio::spawn(async move {
        // Stdin writer
        let writer = tokio::spawn(async move {
            while let Some(mut line) = rx.recv().await {
                line.push('\n');
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });

        // Stderr collector
        let err_task = tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if !l.trim().is_empty() {
                    if let Ok(mut e) = err_stderr.lock() {
                        *e = l.chars().take(400).collect();
                    }
                }
            }
        });

        // Stdout JSON-RPC reader
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            let id = v.get("id").and_then(Value::as_u64);
            let method = v.get("method").and_then(Value::as_str).map(str::to_string);

            if let (Some(req_id), Some(m)) = (id, method) {
                // Incoming RPC request from plugin -> host
                let params = v.get("params").cloned().unwrap_or(Value::Null);
                let st_c = st.clone();
                let tx_c = tx_reply.clone();
                tokio::spawn(async move {
                    let resp = match handle_host_rpc(&st_c, &m, &params).await {
                        Ok(res) => json!({ "id": req_id, "result": res }),
                        Err(err) => json!({ "id": req_id, "error": err }),
                    };
                    let _ = tx_c.send(resp.to_string());
                });
            } else if let Some(resp_id) = id {
                // Response to a host -> plugin request
                let waiter = pending_read.lock().ok().and_then(|mut p| p.remove(&resp_id));
                if let Some(w) = waiter {
                    if let Some(err) = v.get("error").and_then(Value::as_str) {
                        let _ = w.send(Err(err.to_string()));
                    } else {
                        let _ = w.send(Ok(v.get("result").cloned().unwrap_or(Value::Null)));
                    }
                }
            }
        }

        let _ = child.wait().await;
        writer.abort();
        err_task.abort();
        // Fail any outstanding requests
        if let Ok(mut p) = pending_read.lock() {
            for (_, w) in p.drain() {
                let _ = w.send(Err(format!("bridge plugin {pid_log} exited")));
            }
        }
        // Trigger supervisor to respawn after a short delay
        tokio::time::sleep(Duration::from_secs(2)).await;
        notify_plugins_changed();
    });

    Ok(RunningPlugin {
        bin_path,
        version,
        tx,
        pending,
        next_id: AtomicU64::new(1),
        _last_error: last_error,
        task,
    })
}

// ---------- Turn mirroring ----------

fn recorder_of(st: &BridgeState, session: &str) -> Option<Arc<Mutex<turns::RecState>>> {
    let recs: Vec<_> = st.recorders.lock().ok()?.values().cloned().collect();
    recs.into_iter()
        .find(|r| r.lock().map(|g| g.rec.chat_id == session && !g.finishing).unwrap_or(false))
}

fn pending_ask_for(st: &BridgeState, session: &str) -> Option<Value> {
    recorder_of(st, session)?.lock().ok()?.rec.pending_ask.clone()
}

fn live_view(st: &BridgeState, session: &str) -> Option<(String, Option<Value>)> {
    let r = recorder_of(st, session)?;
    let g = r.lock().ok()?;
    let text = g
        .rec
        .messages
        .get(g.rec.cur)
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    Some((text, g.rec.pending_ask.clone()))
}

async fn follow_turn(st: Arc<BridgeState>, session: String, turn_id: String) {
    broadcast_notification(
        "turn_event",
        json!({ "event": "started", "sessionId": session, "turnId": turn_id }),
    );
    let mut shown = String::new();
    let mut last_edit = std::time::Instant::now();
    let mut asked: Option<String> = None;

    loop {
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let live = live_turn(&st, &session).map(|(t, _)| t);
        if live.as_deref() != Some(turn_id.as_str()) {
            break;
        }
        if let Some((text, ask)) = live_view(&st, &session) {
            if let Some(a) = ask {
                let id = a.get("ask_id").and_then(Value::as_str).unwrap_or("").to_string();
                if asked.as_deref() != Some(id.as_str()) {
                    let q = a
                        .get("question")
                        .and_then(Value::as_str)
                        .unwrap_or("This session has a question");
                    let opts: Vec<String> = a
                        .get("options")
                        .and_then(Value::as_array)
                        .map(|arr| arr.iter().filter_map(Value::as_str).map(str::to_string).collect())
                        .unwrap_or_default();
                    broadcast_notification(
                        "turn_event",
                        json!({
                            "event": "ask",
                            "sessionId": session,
                            "turnId": turn_id,
                            "askId": id,
                            "question": q,
                            "options": opts,
                            "freeform": id.starts_with("ask-"),
                        }),
                    );
                    asked = Some(id);
                }
            }
            let view = phone_text(&text);
            if view != shown && last_edit.elapsed() >= Duration::from_secs(3) && !view.is_empty() {
                broadcast_notification(
                    "turn_event",
                    json!({
                        "event": "progress",
                        "sessionId": session,
                        "turnId": turn_id,
                        "text": view,
                    }),
                );
                shown = view;
                last_edit = std::time::Instant::now();
            }
        }
    }

    // Final DB write settles slightly after turn exits
    tokio::time::sleep(Duration::from_millis(2500)).await;
    let reply = match st.db.as_ref() {
        Some(db) => crate::server_store::load_chat(db, &session).await.ok().flatten(),
        None => None,
    }
    .and_then(|c| {
        let messages = c["messages"].as_array()?;
        Some(phone_text(super::persist::last_reply(messages)))
    })
    .unwrap_or_default();

    broadcast_notification(
        "turn_event",
        json!({
            "event": "finished",
            "sessionId": session,
            "turnId": turn_id,
            "reply": if reply.is_empty() { "(no reply text)".to_string() } else { reply },
        }),
    );

    if let Ok(mut r) = reg().lock() {
        r.following.remove(&turn_id);
    }
}

// ---------- Host RPC handlers (called by plugins) ----------

fn session_engine_model(chat: &Value) -> (String, String) {
    let x = turns::parse_extra(chat);
    let engine = crate::work_engine::effective_engine(x.engine.as_deref());
    let model = if x.engine.is_some() {
        x.work_model.clone()
    } else {
        engine.saved_model()
    };
    (
        engine.id().to_string(),
        model.filter(|m| !m.is_empty()).unwrap_or_else(|| "default".into()),
    )
}

async fn handle_host_rpc(st: &Arc<BridgeState>, method: &str, params: &Value) -> Result<Value, String> {
    let sid = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .ok_or_else(|| format!("missing {k}"))
    };

    match method {
        "session_info" => {
            let id = sid("sessionId")?;
            let Some(db) = st.db.as_ref() else {
                return Err("no server db".into());
            };
            let Some(ch) = crate::server_store::load_chat(db, &id).await? else {
                return Ok(json!({ "exists": false }));
            };
            let title = ch
                .get("title")
                .and_then(Value::as_str)
                .filter(|t| !t.trim().is_empty())
                .unwrap_or("This session");
            let (engine, model) = session_engine_model(&ch);
            Ok(json!({
                "exists": true,
                "title": title,
                "working": live_turn(st, &id).is_some(),
                "engine": engine,
                "model": model,
                "pendingAsk": pending_ask_for(st, &id).is_some(),
            }))
        }
        "deliver_message" => {
            let id = sid("sessionId")?;
            let text = params.get("text").and_then(Value::as_str).unwrap_or("");
            let images: Vec<Value> = params
                .get("images")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut m = json!({ "role": "user", "content": text });
            if !images.is_empty() {
                m["images"] = Value::Array(images);
            }
            let queued = live_turn(st, &id).is_some();
            turns::deliver(st, &id, m).await?;
            let tid = live_turn(st, &id).map(|(t, _)| t);
            Ok(json!({ "queued": queued, "turnId": tid }))
        }
        "send_now" => {
            let id = sid("sessionId")?;
            let n = turns::send_now(st, &id).await?;
            Ok(json!({ "sent": n }))
        }
        "stop_turn" => {
            let id = sid("sessionId")?;
            match live_turn(st, &id) {
                Some((tid, _)) => {
                    if let Some(t) = st.agent.get_turn(&tid) {
                        crate::work_agent::agent_stop(&t);
                    }
                    Ok(json!({ "stopped": true }))
                }
                None => Ok(json!({ "stopped": false })),
            }
        }
        "answer_ask" => {
            let id = sid("sessionId")?;
            let ask = pending_ask_for(st, &id).ok_or("That question was already answered.")?;
            let ask_id = ask.get("ask_id").and_then(Value::as_str).unwrap_or("").to_string();
            let options: Vec<String> = ask
                .get("options")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
                .unwrap_or_default();
            let multi = ask.get("multi").and_then(Value::as_bool).unwrap_or(false);
            let text = if let Some(idx) = params.get("choice").and_then(Value::as_u64) {
                options.get(idx as usize).cloned().ok_or("unknown choice")?
            } else if let Some(t) = params.get("text").and_then(Value::as_str) {
                t.to_string()
            } else {
                return Err("missing choice or text".into());
            };
            let (tid, turn) = live_turn(st, &id).ok_or("That turn already ended.")?;
            let wire = if multi { json!([text]).to_string() } else { text.clone() };
            turns::record_answer(st, &turn, &tid, &ask_id, wire, Some(&text), false)?;
            Ok(json!({ "shown": text }))
        }
        "stage_upload" => {
            let id = sid("sessionId")?;
            let name = sid("name")?;
            let b64 = sid("base64")?;
            let path = crate::upload_stage_file_logic(id, name, b64).await?;
            let note = upload_note(std::slice::from_ref(&path));
            Ok(json!({ "path": path, "note": note }))
        }
        "list_engines" => {
            let info = serde_json::to_value(crate::work_engine::engines_info().await).unwrap_or(Value::Null);
            Ok(json!({ "engines": info["engines"] }))
        }
        "list_models" => {
            let e = sid("engine")?;
            let engine = crate::work_engine::Engine::parse(&e).ok_or_else(|| format!("Unknown harness \"{e}\"."))?;
            if !engine.is_present() {
                return Err(engine.missing_message());
            }
            let v = serde_json::to_value(crate::work_engine::list_models(engine, &st.acp).await?)
                .unwrap_or(Value::Null);
            let list = v
                .get("models")
                .and_then(Value::as_array)
                .or_else(|| v.as_array())
                .cloned()
                .unwrap_or_default();
            let models: Vec<String> = list
                .iter()
                .filter_map(|m| match m {
                    Value::String(s) => Some(s.clone()),
                    o => o
                        .get("id")
                        .or_else(|| o.get("value"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                })
                .filter(|s| !s.is_empty())
                .collect();
            Ok(json!({ "models": models }))
        }
        "set_model" => {
            let id = sid("sessionId")?;
            let engine = sid("engine")?;
            let model = sid("model")?;
            turns::cli_session(st, "update", &json!({ "id": id, "engine": engine, "model": model })).await?;
            let note = if live_turn(st, &id).is_some() {
                " (from the next turn; one is running now)"
            } else {
                ""
            };
            Ok(json!({ "message": format!("✅ Now runs {engine}/{model}{note}.") }))
        }
        "transcribe_ogg" => {
            if crate::whisper::installed_model().is_none() {
                return Ok(json!({ "available": false, "text": Value::Null }));
            }
            let b64 = sid("base64")?;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&b64)
                .map_err(|e| format!("invalid base64: {e}"))?;
            let text = crate::whisper::transcribe_ogg_partial(bytes, |_| {}).await?;
            Ok(json!({ "available": true, "text": text }))
        }
        _ => Err(format!("unknown host rpc {method}")),
    }
}

// ---------- Server API actions (called by frontend) ----------

fn get_plugin(plugin_id: &str) -> Result<Arc<RunningPlugin>, String> {
    let rp = reg()
        .lock()
        .ok()
        .and_then(|r| r.plugins.get(plugin_id).cloned());
    match rp {
        Some(p) => Ok(p),
        None => Err(format!("bridge plugin \"{plugin_id}\" is not running (check Settings → Plugins)")),
    }
}

pub async fn action(_st: &Arc<BridgeState>, cmd: &str, args: &Value) -> Result<Value, String> {
    let pid = args
        .get("pluginId")
        .or_else(|| args.get("plugin_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());

    match cmd {
        "bridge_list" => {
            if let Some(id) = pid {
                let p = get_plugin(id)?;
                return p.call("bridge_list", json!({}), Duration::from_secs(10)).await;
            }
            let list: Vec<(String, Arc<RunningPlugin>)> = reg()
                .lock()
                .map(|r| r.plugins.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default();
            let mut out = serde_json::Map::new();
            for (id, p) in list {
                match p.call("bridge_list", json!({}), Duration::from_secs(5)).await {
                    Ok(v) => {
                        let bridges = v.get("bridges").cloned().unwrap_or_else(|| json!({}));
                        out.insert(id, bridges);
                    }
                    Err(_) => {
                        out.insert(id, json!({}));
                    }
                }
            }
            Ok(json!({ "plugins": out }))
        }
        "bridge_status" | "bridge_save" | "bridge_set" | "bridge_unpair" | "bridge_remove" => {
            let id = pid.ok_or("missing pluginId")?;
            let p = get_plugin(id)?;
            let timeout = if cmd == "bridge_save" {
                Duration::from_secs(20)
            } else {
                Duration::from_secs(10)
            };
            p.call(cmd, args.clone(), timeout).await
        }
        _ => Err(format!("unknown bridge action {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_path_uses_override_dir() {
        let dir = std::env::temp_dir().join("vicuna-bridge-cfg-test");
        unsafe { std::env::set_var("VICUNA_BRIDGE_CONFIG_DIR", &dir) };
        assert_eq!(config_path_for("demo"), Some(dir.join("demo.json")));
        unsafe { std::env::remove_var("VICUNA_BRIDGE_CONFIG_DIR") };
    }

    #[test]
    fn get_plugin_reports_missing() {
        let err = get_plugin("not-installed-xyz").err().unwrap();
        assert!(err.contains("not running"), "{err}");
    }
}
