// Vicuna backend server: the one backend every frontend talks to.
// `vicuna serve` (headless CLI), the dev bridge, and the desktop app (embedded,
// see `embedded`) all run this code. Moved from bridge/src/main.rs.
//
// Vicuna headless bridge: loopback HTTP (+ poll-based events) exposing the
// backend commands so a plain browser (live-browser/Chrome) can drive the
// REAL backend without the Tauri shell. Dev/test tool only: binds
// 127.0.0.1, no auth — never expose beyond loopback.
//
//   cargo run --bin bridge [--port 1421]      (src-tauri/)
//   POST /invoke {"cmd": "...", "args": {...}} -> {"ok": true, "data": ...}
//   GET  /poll/<channel-id>                    -> {"events": [ ... ]}
//   GET  /health                                -> {"ok": true}
//
// Argument keys are camelCase (same as Tauri's IPC convention) so frontend
// code is identical between runtimes. Channel args arrive as
// {"__bridge_channel": "<uuid>"} markers; the bridge creates the queue up
// front and hands run_loop a real Channel wired into it.
//
// VICUNA_TEST_RESOURCE_DIR (test-only): when set, browser_* commands
// resolve the bundled `lb` from that dir as if it were the app resource
// root — this is how the bundled (no-PATH) path is verified headlessly
// (see /tmp/vicuna-browser-bundled.sh).


pub mod actions;
mod chat;
pub mod embedded;
pub mod persist;
pub mod turns;
mod session_tools;
mod self_upgrade;
pub mod telegram;
pub mod bridge_plugins;


use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use crate::ipc::{Channel, InvokeResponseBody, IpcResponse};

use crate::acp::AcpState;
use crate::browser::BrowserState;
use crate::mcp::McpManager;
use crate::work_agent::{
    agent_queue_msg, agent_remove_msg, agent_stop, WorkAgentState,
};
use crate::{AgentShellState, BgExecState, BgJobStatus, BgJobSummary, ChatMessage, UndoState};

const EVENT_CAP: usize = 2000;
/// Error prefix of a refused turn start (the window reloads the session).
const SESSION_CHANGED: &str = "SESSION_CHANGED";
const BODY_CAP: u64 = 32 * 1024 * 1024;

#[derive(Clone, Default)]
pub struct EventQueues {
    map: Arc<Mutex<HashMap<String, VecDeque<serde_json::Value>>>>,
    notify: Arc<tokio::sync::Notify>,
}

impl EventQueues {
    pub fn lock(
        &self,
    ) -> std::sync::LockResult<std::sync::MutexGuard<'_, HashMap<String, VecDeque<serde_json::Value>>>> {
        self.map.lock()
    }

    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.notify.notified()
    }

    pub fn notify_waiters(&self) {
        self.notify.notify_waiters();
    }
}

pub struct BridgeState {
    pub bg: Arc<BgExecState>,
    pub shell: Arc<AgentShellState>,
    pub mcp: McpManager,
    pub agent: WorkAgentState,
    // Arc so work_agent turns can hold it across the detached run_loop
    // (shared-ref arms below keep compiling via deref coercion)
    pub browser: Arc<BrowserState>,
    pub undo: Arc<UndoState>,
    pub acp: Arc<AcpState>,
    pub events: EventQueues,
    /// server mode (`serve`): bearer token every request must carry
    pub token: Option<String>,
    /// server mode: this machine's own vicuna.db (chats live on the server)
    pub db: Option<crate::server_store::Pool>,
    /// terminal tabs (remote-server terminals): ptys + term id -> channel id
    pub terms: Arc<crate::TermState>,
    pub term_channels: Arc<Mutex<HashMap<String, String>>>,
    /// channel id -> last time a client polled it (orphan-terminal reaper)
    pub last_poll: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    /// Vicuna session id -> (turn id, channel id) of its live Work turn, so a
    /// window that closed/reloaded mid-turn reattaches (`work_agent_live`)
    /// instead of losing the stream or starting a second engine process on
    /// the same session.
    /// (turn id, channel id, start ms epoch: a reopened window's elapsed
    /// clock must count from the turn's real start, not from the reopen)
    pub session_turns: Arc<Mutex<HashMap<String, (String, String, u64)>>>,
    /// server mode: each recorded Work turn (event channel -> transcript
    /// being written to the db), see server_turns.rs
    pub recorders: turns::Recorders,
    chat_turns: chat::ChatTurns,
    redo_history: session_tools::RedoHistory,
    /// the user's UTC offset (minutes), as last reported by a window; the
    /// server resolves `schedule_wakeup` local times with it
    pub tz_offset_min: std::sync::atomic::AtomicI64,
    /// serializes read-modify-write of the stored Scheduled list
    pub sched_lock: tokio::sync::Mutex<()>,
    /// Serialize turn handoffs, recorder writes and client merges. Take this
    /// before reading a recorder snapshot so a delayed write cannot roll back
    /// a final write or the next turn's user message.
    pub chat_lock: tokio::sync::Mutex<()>,
    /// app resource root for browser_* (desktop embedded server)
    pub resource_dir: Option<std::path::PathBuf>,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            bg: Arc::new(BgExecState::default()),
            shell: Arc::new(AgentShellState::default()),
            mcp: McpManager::default(),
            agent: WorkAgentState::default(),
            browser: Arc::new(BrowserState::default()),
            undo: Arc::new(UndoState::default()),
            acp: Arc::new(AcpState::default()),
            token: None,
            db: None,
            terms: Arc::new(crate::TermState::default()),
            term_channels: Arc::new(Mutex::new(HashMap::new())),
            last_poll: Arc::new(Mutex::new(HashMap::new())),
            session_turns: Arc::new(Mutex::new(HashMap::new())),
            recorders: Arc::new(Mutex::new(HashMap::new())),
            chat_turns: Mutex::new(HashMap::new()),
            redo_history: Default::default(),
            tz_offset_min: std::sync::atomic::AtomicI64::new(0),
            sched_lock: tokio::sync::Mutex::new(()),
            chat_lock: tokio::sync::Mutex::new(()),
            events: EventQueues::default(),
            resource_dir: None,
        }
    }
}

pub(crate) fn push_event(queues: &EventQueues, id: &str, raw: &str) {
    let v: serde_json::Value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()));
    if let Ok(mut map) = queues.lock() {
        // the channel's own queue plus every reader a reattaching client
        // opened on it (`<id>~<reader>`, see `open_reader`)
        let prefix = format!("{id}{READER_SEP}");
        let readers: Vec<String> = map.keys().filter(|k| k.starts_with(&prefix)).cloned().collect();
        enqueue(map.entry(id.to_string()).or_default(), v.clone());
        for r in readers {
            if let Some(q) = map.get_mut(&r) {
                enqueue(q, v.clone());
            }
        }
    }
    queues.notify_waiters();
}

fn enqueue(q: &mut VecDeque<serde_json::Value>, v: serde_json::Value) {
    // merge adjacent token/think chunks: nobody may poll for hours (window
    // closed mid-turn), and the cap below must not drop streamed text
    if let Some(prev) = q.back_mut() {
        if merge_text_event(prev, &v) {
            return;
        }
    }
    q.push_back(v);
    while q.len() > EVENT_CAP {
        q.pop_front();
    }
}

/// Separates a channel id from a reader id in a reader queue's key.
pub(crate) const READER_SEP: char = '~';

/// A second client following a live channel (a window reattaching, `vicuna
/// session attach`) gets its own queue: polling the channel's own queue
/// would split every event between it and the client already following
/// (each saw a shredded reply). The reader starts with `seed` and then gets
/// every event pushed to the channel. Returns the key to poll.
pub(crate) fn open_reader(queues: &EventQueues, id: &str, seed: Vec<serde_json::Value>) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let key = format!("{id}{READER_SEP}r{}", NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    if let Ok(mut map) = queues.lock() {
        map.insert(key.clone(), seed.into());
    }
    key
}

/// Append `next`'s text onto `prev` when both are the same token/think event.
fn merge_text_event(prev: &mut serde_json::Value, next: &serde_json::Value) -> bool {
    let kind = next.get("type").and_then(|t| t.as_str());
    if !matches!(kind, Some("token" | "think")) || prev.get("type").and_then(|t| t.as_str()) != kind {
        return false;
    }
    let (Some(p), Some(n)) = (prev.as_object_mut(), next.as_object()) else { return false };
    if p.len() != 2 || n.len() != 2 {
        return false;
    }
    let add = n.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
    match p.get_mut("text") {
        Some(serde_json::Value::String(s)) => {
            s.push_str(&add);
            true
        }
        _ => false,
    }
}

fn drain_events(queues: &EventQueues, id: &str) -> Vec<serde_json::Value> {
    if let Ok(mut map) = queues.lock() {
        if let Some(q) = map.get_mut(id) {
            let out: Vec<serde_json::Value> = q.drain(..).collect();
            // a reader has nothing left to follow once the turn ended
            let ended = out.iter().any(|e| matches!(e.get("type").and_then(|t| t.as_str()), Some("done" | "error")));
            if ended && id.contains(READER_SEP) {
                map.remove(id);
            }
            return out;
        }
    }
    Vec::new()
}

fn channel_for<T: IpcResponse>(queues: &EventQueues, uuid: &str) -> Channel<T> {
    // Real Tauri Channel wired into the poll queue: run_loop is unchanged,
    // its Token/Think/Tool/Done sends land here as JSON for GET /poll/<id>.
    let queues = queues.clone();
    let id = uuid.to_string();
    Channel::<T>::new(move |body| {
        if let InvokeResponseBody::Json(s) = body {
            push_event(&queues, &id, &s);
        }
        Ok(())
    })
}

fn arg_str(args: &serde_json::Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("missing arg {key}"))
}

fn arg_opt_str(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

fn arg_str_list(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Test-only resource root for browser_* (see header comment).
pub(crate) fn test_resource_dir() -> Option<std::path::PathBuf> {
    if let Some(d) = EMBEDDED_RESOURCE_DIR.get() {
        return Some(d.clone());
    }
    std::env::var("VICUNA_TEST_RESOURCE_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    st: &Arc<BridgeState>,
    cmd: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let s = |k: &str| arg_str(args, k);
    match cmd {
        "chat_turn_start" => chat::start(st, args).await,
        "chat_turn_attach" => chat::attach(st, args).await,
        "chat_turn_live" => Ok(serde_json::json!(chat::live_turns(st))),
        "chat_turn_stop" => Ok(serde_json::json!(chat::stop(st, &s("id")?, args["turnId"].as_str()))),
        "ollama_list_models" => crate::ollama_list_models().await,
        "ollama_list_running" => crate::ollama_list_running().await,
        "ollama_model_show" => Ok(serde_json::json!(crate::ollama_model_show(s("model")?).await?)),
        "ollama_server_version" => Ok(serde_json::json!(crate::ollama_server_version().await?)),
        "ollama_warm_model" => {
            crate::ollama_warm_model(s("model")?, s("keepAlive")?).await?;
            Ok(serde_json::Value::Null)
        }
        "ollama_delete_model" => {
            crate::ollama_delete_model(s("model")?).await?;
            Ok(serde_json::Value::Null)
        }
        "ollama_pull_model" => {
            let id = args["onEvent"]["__bridge_channel"].as_str().ok_or("missing onEvent channel")?.to_string();
            let model = s("model")?;
            let events = st.events.clone();
            tokio::spawn(async move {
                let channel = channel_for(&events, &id);
                let result = crate::ollama_pull_model(model, channel).await;
                let event = match result {
                    Ok(()) => serde_json::json!({"type": "done"}),
                    Err(message) => serde_json::json!({"type": "error", "message": message}),
                };
                push_event(&events, &id, &event.to_string());
            });
            Ok(serde_json::Value::Null)
        }
        // ----- work agent turn -----
        "work_agent_start" => {
            let _write = st.chat_lock.lock().await;
            let uuid = args
                .get("onEvent")
                .and_then(|v| v.get("__bridge_channel"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing channel marker onEvent".to_string())?
                .to_string();
            let engine = crate::work_engine::requested_engine(arg_opt_str(args, "engine").as_deref())?;
            if !engine.is_present() {
                return Err(engine.missing_message());
            }
            let model = arg_opt_str(args, "model");
            let messages: Vec<ChatMessage> =
                serde_json::from_value(args.get("messages").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad messages: {e}"))?;
            let session_id = arg_opt_str(args, "sessionId");
            if let Some(off) = args.get("tzOffsetMin").and_then(|v| v.as_i64()) {
                st.tz_offset_min.store(off, std::sync::atomic::Ordering::Relaxed);
            }
            // server mode: the window hands over the session as it stands at
            // turn start (user message + placeholder); the server writes it now
            // and records the rest of the turn itself
            let mut rec = None;
            if let (Some(db), Some(sid), Some(row)) = (&st.db, &session_id, args.get("persistChat").filter(|v| v.is_object())) {
                if let Some((tid, _)) = turns::live_turn(st, sid) {
                    return Err(format!(
                        "this session already has a Work turn running on the server ({tid}); reopen the session to follow it"
                    ));
                }
                let out = crate::server_store::merge_json(
                    db,
                    &serde_json::Value::Array(vec![row.clone()]),
                    vec![],
                    &turns::live_sessions(st),
                )
                .await?;
                if !out.conflicts.is_empty() {
                    return Err(format!("{SESSION_CHANGED}: this session changed on the server since this window loaded it. Vicuna reloaded it; send again."));
                }
                let mut msgs: Vec<serde_json::Value> = row.get("messages").and_then(|m| m.as_array()).cloned().unwrap_or_default();
                let idx = args.get("persistIndex").and_then(|v| v.as_u64()).map(|n| n as usize).unwrap_or(msgs.len().saturating_sub(1));
                if idx < msgs.len() {
                    if msgs[idx].get("workspace").and_then(|v| v.as_str()).map(str::trim).unwrap_or("").is_empty() {
                        if let Some(ws) = args.get("workspace").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
                            msgs[idx]["workspace"] = serde_json::json!(ws);
                        }
                    }
                    rec = Some(persist::Recorder::new(sid, msgs, idx, model.clone().filter(|m| !m.is_empty()), engine.id()));
                }
            }
            let selection = match (&st.db, &session_id) {
                (Some(db), Some(id)) => crate::server_store::session_mcp(db, id).await?,
                _ => None,
            }.unwrap_or_else(|| {
                crate::mcp::SessionSelection::from_extra(&serde_json::json!({
                    "mcpEnabled": args.get("mcpEnabled"),
                    "mcp": args.get("mcpOnly"),
                }))
            });
            let turn_id = turns::launch(
                st,
                turns::LaunchParams {
                    uuid,
                    engine,
                    model,
                    workspace: s("workspace")?,
                    messages,
                    session_id,
                    fresh_session: args.get("freshSession").and_then(|v| v.as_bool()).unwrap_or(false),
                    lead: args.get("lead").and_then(|v| v.as_bool()).unwrap_or(false),
                    report_to: rec.as_ref().and(arg_opt_str(args, "reportTo")),
                    mcp_selection: selection,
                    skills_enabled: crate::skills::selection(args),
                    rec,
                },
            )
            .await?;
            Ok(serde_json::json!(turn_id))
        }
        // a window reopening a session with a live server turn: the recorded
        // transcript so far + the channel to keep following
        "work_agent_attach" => Ok(turns::attach(st, &s("sessionId")?)),
        "work_engine_info" => Ok(serde_json::to_value(crate::work_engine::engines_info().await)
            .map_err(|e| e.to_string())?),
        "work_engine_select" => Ok(serde_json::to_value(
            crate::work_engine::select_engine(&s("engine")?).await?,
        )
        .map_err(|e| e.to_string())?),
        "work_models" => {
            let engine = crate::work_engine::requested_engine(arg_opt_str(args, "engine").as_deref())?;
            Ok(serde_json::to_value(crate::work_engine::list_models(engine, &st.acp).await?)
                .map_err(|e| e.to_string())?)
        }
        "work_set_approval" => {
            let ask = args.get("ask").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::to_value(crate::work_engine::set_approval(ask).await?).map_err(|e| e.to_string())?)
        }
        "work_set_model" => {
            crate::work_engine::set_model(
                arg_opt_str(args, "engine").as_deref(),
                arg_opt_str(args, "model").as_deref(),
            )?;
            Ok(serde_json::Value::Null)
        }
        "work_default_workspace" => {
            let db = st.db.as_ref().ok_or("This bridge stores no settings (run `vicuna serve`).")?;
            let key = "pref_default_workspace";
            match args.get("workspace") {
                None => Ok(serde_json::json!(crate::server_store::meta_get(db, key).await?)),
                Some(value) => {
                    let path = value.as_str().ok_or("workspace must be a string")?;
                    let workspace = if path.is_empty() {
                        String::new()
                    } else {
                        crate::work_validate_root_logic(path.to_owned()).await?
                    };
                    crate::server_store::meta_put(db, key, &workspace).await?;
                    Ok(serde_json::json!(workspace))
                }
            }
        }
        "work_account_support" => Ok(serde_json::json!({
            "engines": if cfg!(windows) { vec!["claude", "codex"] } else { vec!["claude", "codex", "agy"] },
        })),
        "work_account_add" | "work_account_rename" | "work_account_remove" => {
            let engine = arg_opt_str(args, "engine").unwrap_or_default();
            let label = arg_opt_str(args, "label").unwrap_or_default();
            let id = arg_opt_str(args, "id").unwrap_or_default();
            if engine == "agy" {
                match cmd {
                    "work_account_add" => Ok(serde_json::to_value(crate::agy_accounts::add_logic(&label)?).map_err(|e| e.to_string())?),
                    "work_account_rename" => Ok(serde_json::to_value(crate::agy_accounts::rename_logic(&id, &label)?).map_err(|e| e.to_string())?),
                    _ => crate::agy_accounts::remove_logic(&id),
                }
            } else {
                let harness = crate::cli_accounts::Harness::parse(&engine).ok_or("This harness does not support extra accounts.")?;
                Ok(serde_json::to_value(crate::cli_accounts::mutate(harness, cmd, &id, &label)?).map_err(|e| e.to_string())?)
            }
        }
        // extra Antigravity accounts (agy_accounts.rs)
        "agy_account_list" => crate::agy_accounts::list_logic(),
        "agy_account_add" => Ok(serde_json::to_value(
            crate::agy_accounts::add_logic(arg_opt_str(args, "label").as_deref().unwrap_or(""))?,
        )
        .map_err(|e| e.to_string())?),
        "agy_account_rename" => Ok(serde_json::to_value(crate::agy_accounts::rename_logic(
            arg_opt_str(args, "id").as_deref().unwrap_or(""),
            arg_opt_str(args, "label").as_deref().unwrap_or(""),
        )?)
        .map_err(|e| e.to_string())?),
        "agy_account_remove" => crate::agy_accounts::remove_logic(arg_opt_str(args, "id").as_deref().unwrap_or("")),
        "work_usage" => Ok(serde_json::to_value(
            crate::work_usage::fetch_usage_logic(arg_opt_str(args, "engine")).await?,
        )
        .map_err(|e| e.to_string())?),
        "opencode_models" => Ok(serde_json::to_value(crate::opencode::list_models().await?)
            .map_err(|e| e.to_string())?),
        "opencode_set_model" => {
            crate::opencode::save_model(arg_opt_str(args, "model").as_deref())?;
            Ok(serde_json::Value::Null)
        }
        // A window (re)opening a session asks whether its Work turn is still
        // running here; it then keeps polling the same channel, whose queue
        // held every event since the previous window went away.
        "work_agent_live" => {
            let live = st.session_turns.lock().map(|m| m.clone()).unwrap_or_default();
            let mut out = serde_json::Map::new();
            for (sid, (tid, ch, started_ms)) in live {
                if st.agent.get_turn(&tid).is_some() {
                    out.insert(sid, serde_json::json!({ "turnId": tid, "channel": ch, "startedMs": started_ms }));
                }
            }
            Ok(serde_json::Value::Object(out))
        }
        "work_agent_turn_live" => Ok(serde_json::json!(crate::work_agent::turn_is_live(&st.agent, &s("turnId")?))),
        "work_agent_stop" => {
            let turn = st.agent.get_turn(&s("turnId")?).ok_or("unknown turn")?;
            agent_stop(&turn);
            Ok(serde_json::Value::Null)
        }
        "work_agent_queue" => {
            let turn = st.agent.get_turn(&s("turnId")?).ok_or("unknown turn")?;
            let message: ChatMessage =
                serde_json::from_value(args.get("message").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad message: {e}"))?;
            agent_queue_msg(&turn, s("id")?, message)?;
            Ok(serde_json::Value::Null)
        }
        "work_agent_remove" => {
            let turn = st.agent.get_turn(&s("turnId")?).ok_or("unknown turn")?;
            agent_remove_msg(&turn, &s("id")?)?;
            Ok(serde_json::Value::Null)
        }
        "work_agent_answer" => {
            let turn = st.agent.get_turn(&s("turnId")?).ok_or("unknown turn")?;
            let ask_id = s("askId")?;
            // the Q/A record the window writes into its own copy (agent
            // questions only); the recorder writes the same into the db
            let record = args.get("record");
            turns::record_answer(
                st,
                &turn,
                &s("turnId")?,
                &ask_id,
                s("answer")?,
                record.and_then(|r| r.get("display")).and_then(|v| v.as_str()),
                record.and_then(|r| r.get("dismissed")).and_then(|v| v.as_bool()).unwrap_or(false),
            )?;
            Ok(serde_json::Value::Null)
        }
        // ----- workspace validation / tools -----
        "work_validate_root" => {
            Ok(serde_json::json!(crate::work_validate_root_logic(s("path")?).await?))
        }
        "work_ensure_scratch_workspace" => {
            Ok(serde_json::json!(
                crate::work_ensure_scratch_workspace_logic(s("sessionId")?).await?
            ))
        }
        "work_list_dir" => {
            let path = arg_opt_str(args, "path");
            Ok(serde_json::json!(crate::work_list_dir_logic(s("root")?, path).await?))
        }
        "work_tree" => {
            let max = args.get("maxDepth").and_then(|v| v.as_u64()).map(|n| n as usize);
            Ok(serde_json::json!(crate::work_tree_logic(s("root")?, max).await?))
        }
        "work_read_file" => {
            let max = args.get("maxBytes").and_then(|v| v.as_u64());
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                crate::work_read_file_logic(s("root")?, s("path")?, max, offset, limit).await?
            ))
        }
        "work_read_chunk" => {
            let offset = args.get("offset").and_then(|v| v.as_u64()).unwrap_or(0);
            let length = args.get("length").and_then(|v| v.as_u64()).unwrap_or(4 * 1024 * 1024);
            Ok(crate::work_read_chunk_logic(s("root")?, s("path")?, offset, length).await?)
        }
        "work_read_image" => {
            Ok(serde_json::json!(crate::work_read_image_logic(s("root")?, s("path")?).await?))
        }
        "work_grep" => {
            let include = arg_opt_str(args, "include");
            Ok(serde_json::json!(
                crate::work_grep_logic(s("root")?, s("pattern")?, include).await?
            ))
        }
        "work_fetch_url" => {
            Ok(serde_json::json!(crate::work_fetch_url_logic(s("url")?).await?))
        }
        "work_write_file" => {
            Ok(serde_json::json!(
                crate::work_write_file_logic(s("root")?, s("path")?, s("content")?).await?
            ))
        }
        "work_exec" => {
            let timeout = args.get("timeoutMs").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                crate::work_exec_logic(s("root")?, s("command")?, timeout).await?
            ))
        }
        "work_exec_start" => {
            Ok(serde_json::json!(crate::bg_start(&st.bg, s("root")?, s("command")?).await?))
        }
        "work_exec_status" => {
            let r: BgJobStatus = crate::bg_snapshot(&st.bg, &s("id")?)?;
            Ok(serde_json::json!(r))
        }
        "work_shell_exec" => {
            let timeout = args.get("timeoutMs").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                crate::agent_shell_exec(
                    &st.shell,
                    s("root")?,
                    arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
                    s("command")?,
                    timeout,
                )
                .await?
            ))
        }
        "work_shell_status" => {
            Ok(serde_json::json!(crate::agent_shell_status(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
            )
            .await?))
        }
        "work_shell_input" => {
            Ok(serde_json::json!(crate::agent_shell_input(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
                s("text")?,
            )
            .await?))
        }
        "work_shell_interrupt" => {
            Ok(serde_json::json!(crate::agent_shell_interrupt(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
            )
            .await?))
        }
        "work_exec_list" => {
            let l: Vec<BgJobSummary> = crate::bg_list(&st.bg)?;
            Ok(serde_json::json!(l))
        }
        "work_exec_kill" => {
            let r: BgJobStatus = crate::bg_kill(&st.bg, &s("id")?)?;
            Ok(serde_json::json!(r))
        }
        // ----- git panel -----
        "git_status" => Ok(serde_json::json!(crate::git_panel::git_status_logic(s("root")?).await?)),
        "git_diff" => {
            let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::json!(
                crate::git_panel::git_diff_logic(s("root")?, s("path")?, b("staged"), b("untracked")).await?
            ))
        }
        "git_stage" => {
            crate::git_panel::git_stage_logic(s("root")?, arg_str_list(args, "paths")).await?;
            Ok(serde_json::Value::Null)
        }
        "git_unstage" => {
            crate::git_panel::git_unstage_logic(s("root")?, arg_str_list(args, "paths")).await?;
            Ok(serde_json::Value::Null)
        }
        "git_discard" => {
            crate::git_panel::git_discard_logic(s("root")?, arg_str_list(args, "paths"), arg_str_list(args, "untracked"))
                .await?;
            Ok(serde_json::Value::Null)
        }
        // ----- folder picker (remote servers / browsers) -----
        "fs_list_dirs" => crate::fs_list_dirs_logic(
            arg_opt_str(args, "path"),
            args.get("hidden").and_then(|v| v.as_bool()).unwrap_or(false),
        ),
        // ----- terminal tabs (remote-server terminals) -----
        "term_spawn" => {
            let uuid = args
                .get("onEvent")
                .and_then(|v| v.get("__bridge_channel"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing channel marker onEvent".to_string())?
                .to_string();
            let cols = args.get("cols").and_then(|v| v.as_u64()).map(|n| n as u16);
            let rows = args.get("rows").and_then(|v| v.as_u64()).map(|n| n as u16);
            let ch: Channel<crate::TermEvent> = channel_for(&st.events, &uuid);
            let id = crate::term_spawn_logic(&st.terms, &s("root")?, cols, rows, ch)?;
            if let Ok(mut lp) = st.last_poll.lock() {
                lp.insert(uuid.clone(), std::time::Instant::now());
            }
            if let Ok(mut tc) = st.term_channels.lock() {
                tc.insert(id.clone(), uuid);
            }
            Ok(serde_json::json!(id))
        }
        "term_write" => {
            crate::term_write_logic(&st.terms, &s("id")?, &s("data")?)?;
            Ok(serde_json::Value::Null)
        }
        "term_resize" => {
            let cols = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            crate::term_resize_logic(&st.terms, &s("id")?, cols, rows)?;
            Ok(serde_json::Value::Null)
        }
        "term_kill" => {
            let id = s("id")?;
            if let Ok(mut tc) = st.term_channels.lock() {
                tc.remove(&id);
            }
            crate::term_kill_logic(&st.terms, &id)?;
            Ok(serde_json::Value::Null)
        }
        // ----- server mode (desktop "Connect to server") -----
        "server_info" => Ok(serde_json::json!({
            "app": "vicuna",
            "name": server_name(),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "home": std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_default(),
            "chats": st.db.is_some(),
            // records Work turns itself + per-chat revisions (server_turns.rs)
            "persist": st.db.is_some(),
            "chatTurns": st.db.is_some(),
            "sessionTools": st.db.is_some(),
            "chatsList": st.db.is_some(),
            "canUpgrade": self_upgrade::can_upgrade(),
        })),
        "server_upgrade_check" => self_upgrade::check().await,
        "server_upgrade" => self_upgrade::start(),
        "server_upgrade_status" => self_upgrade::status(),
        "chats_load" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let mut chats = crate::server_store::load_chats(db).await?;
            for chat in chats.as_array_mut().into_iter().flatten() {
                st.redo_history.decorate(chat);
            }
            Ok(chats)
        }
        "chats_list" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let mut chats = crate::server_store::load_chat_list(db).await?;
            for chat in chats.as_array_mut().into_iter().flatten() {
                st.redo_history.decorate(chat);
            }
            Ok(chats)
        }
        "chats_merge" => {
            let _write = st.chat_lock.lock().await;
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let chats = args.get("chats").cloned().unwrap_or(serde_json::Value::Array(vec![]));
            let out = crate::server_store::merge_json(db, &chats, arg_str_list(args, "deleted"), &turns::live_sessions(st)).await?;
            Ok(out.to_json())
        }
        "chats_get" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let mut chat = crate::server_store::load_chat(db, &s("id")?).await?.unwrap_or(serde_json::Value::Null);
            st.redo_history.decorate(&mut chat);
            Ok(chat)
        }
        "session_compact" => session_tools::compact(st, args).await,
        "session_undo" => session_tools::change_turn(st, args, false).await,
        "session_redo" => session_tools::change_turn(st, args, true).await,
        "schedule_list" => turns::schedule_list(st).await,
        "telegram_list" | "telegram_status" | "telegram_save" | "telegram_set" | "telegram_unpair" | "telegram_remove" => {
            telegram::action(st, cmd, args).await
        }
        "bridge_list" | "bridge_status" | "bridge_save" | "bridge_set" | "bridge_unpair" | "bridge_remove" => {
            bridge_plugins::action(st, cmd, args).await
        }
        "whisper_status" | "whisper_install" | "whisper_uninstall" => telegram::action(st, cmd, args).await,
        "plugin_catalog" | "plugin_list" | "plugin_install" | "plugin_upgrade" | "plugin_uninstall" | "plugin_harness_consent" | "plugin_harness_revoke" => {
            let res = crate::plugins::action(cmd, &args).await;
            if res.is_ok() && matches!(cmd, "plugin_install" | "plugin_upgrade" | "plugin_uninstall") {
                bridge_plugins::notify_plugins_changed();
            }
            res
        }
        "plugin_storage_get" | "plugin_storage_keys" | "plugin_storage_set" => crate::plugins::action(cmd, &args).await,
        "plugin_storage_remove" => crate::plugins::action(cmd, &args).await,
        "cli_session" => turns::cli_session(st, &s("action")?, args).await,
        // session + message actions (server/actions.rs)
        "session_create" | "session_update" | "session_delete" | "session_fork" | "message_edit" | "message_delete" => {
            actions::dispatch(st, cmd, args).await
        }
        "schedule_put" => {
            if let Some(off) = args.get("tzOffsetMin").and_then(|v| v.as_i64()) {
                st.tz_offset_min.store(off, std::sync::atomic::Ordering::Relaxed);
            }
            let items = args.get("items").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            turns::schedule_put(st, items).await
        }
        "schedule_remove" => turns::schedule_remove(st, arg_str_list(args, "ids")).await,
        // cross-host Cmd+P: another window searches this server's sessions
        "server_search_chats" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let query = arg_opt_str(args, "query").unwrap_or_default();
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            crate::server_store::search_chats(db, &query, &arg_str_list(args, "ids"), limit).await
        }
        "git_apply" => {
            let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            crate::git_panel::git_apply_logic(
                s("root")?,
                s("patch")?,
                b("cached"),
                b("reverse"),
                arg_str_list(args, "intentToAdd"),
            )
            .await?;
            Ok(serde_json::Value::Null)
        }
        "work_rename" => {
            Ok(serde_json::json!(crate::work_rename_logic(s("root")?, s("path")?, s("newName")?).await?))
        }
        "work_delete" => {
            crate::work_delete_logic(s("root")?, s("path")?).await?;
            Ok(serde_json::Value::Null)
        }
        "work_undo" => {
            let sid = arg_opt_str(args, "sessionId")
                .or_else(|| arg_opt_str(args, "session_id"))
                .or_else(|| arg_opt_str(args, "id"))
                .ok_or_else(|| "missing arg sessionId".to_string())?;
            Ok(serde_json::json!(crate::work_undo_logic(&st.undo, sid).await?))
        }
        "work_redo" => {
            let sid = arg_opt_str(args, "sessionId")
                .or_else(|| arg_opt_str(args, "session_id"))
                .or_else(|| arg_opt_str(args, "id"))
                .ok_or_else(|| "missing arg sessionId".to_string())?;
            Ok(serde_json::json!(crate::work_redo_logic(&st.undo, sid).await?))
        }
        "work_import_file" => {
            Ok(serde_json::json!(
                crate::work_import_file_logic(s("root")?, s("path")?, s("base64")?).await?
            ))
        }
        "upload_stage_file" => {
            Ok(serde_json::json!(
                crate::upload_stage_file_logic(s("sessionId")?, s("fileName")?, s("base64")?).await?
            ))
        }
        "upload_read_file" => {
            Ok(serde_json::json!(
                crate::upload_read_file_logic(s("sessionId")?, s("name")?).await?
            ))
        }
        // ----- MCP -----
        "mcp_list_servers" => Ok(serde_json::json!(crate::mcp::session_servers(test_resource_dir().as_deref())?)),
        "mcp_set_defaults" => {
            let enabled = serde_json::from_value(args["enabled"].clone()).map_err(|e| format!("bad MCP defaults: {e}"))?;
            crate::mcp::set_defaults(&enabled)?;
            Ok(serde_json::json!(crate::mcp::session_servers(test_resource_dir().as_deref())?))
        }
        "mcp_save_server" => {
            let cfg: crate::mcp::McpServerConfig =
                serde_json::from_value(args.get("config").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad config: {e}"))?;
            crate::mcp::save_file_server(&cfg, args["previousId"].as_str())?;
            Ok(serde_json::json!(crate::mcp::session_servers(test_resource_dir().as_deref())?))
        }
        "mcp_delete_server" => {
            crate::mcp::delete_file_server(&s("id")?)?;
            Ok(serde_json::json!(crate::mcp::session_servers(test_resource_dir().as_deref())?))
        }
        // ----- Skills -----
        "skills_list" => Ok(serde_json::json!(crate::skills::list_skills()?)),
        "skills_read" => Ok(serde_json::json!(crate::skills::read_skill(&s("slug")?)?)),
        "skills_save" => {
            let _write = st.chat_lock.lock().await;
            Ok(serde_json::json!(crate::skills::save_skill(&s("slug")?, &s("content")?)?))
        }
        "skills_set_defaults" => {
            let enabled = crate::skills::selection(args).ok_or("Missing skill enabled states")?;
            let _write = st.chat_lock.lock().await;
            if let Some(db) = &st.db {
                let defaults = crate::skills::defaults();
                crate::server_store::update_skill_maps(db, |old| old.is_none().then(|| defaults.clone())).await?;
            }
            crate::skills::set_defaults(&enabled)?;
            Ok(serde_json::Value::Null)
        }
        "skills_remove" => {
            let slug = s("slug")?;
            let _write = st.chat_lock.lock().await;
            crate::skills::remove_skill(&slug)?;
            if let Some(db) = &st.db {
                crate::server_store::remove_session_skill(db, &slug).await?;
            }
            Ok(serde_json::Value::Null)
        }
        "mcp_connect" => {
            let cfg: crate::mcp::McpServerConfig =
                serde_json::from_value(args.get("config").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad config: {e}"))?;
            Ok(serde_json::json!(st.mcp.connect(&cfg).await?))
        }
        "mcp_disconnect" => {
            st.mcp.disconnect(&s("id")?).await?;
            Ok(serde_json::Value::Null)
        }
        "mcp_list_tools" => Ok(serde_json::json!(st.mcp.list_tools(&s("id")?).await?)),
        "mcp_call_tool" => {
            let call_args = args.get("args").cloned().unwrap_or_default();
            Ok(serde_json::json!(
                st.mcp.call_tool(&s("serverId")?, &s("tool")?, call_args).await?
            ))
        }
        "mcp_signout" => {
            let id = s("id")?;
            let name = crate::mcp::list_file_servers()?
                .into_iter()
                .find(|cf| cf.id == id)
                .map(|cf| cf.name)
                .unwrap_or(id.clone());
            crate::mcp::delete_auth(&name);
            let _ = st.mcp.disconnect(&id).await;
            Ok(serde_json::Value::Null)
        }
        "mcp_login_present" => {
            let id = s("id")?;
            let name = crate::mcp::list_file_servers()?
                .into_iter()
                .find(|cf| cf.id == id)
                .map(|cf| cf.name)
                .unwrap_or(id);
            Ok(serde_json::json!(crate::mcp::auth_present(&name)))
        }
        "mcp_signin" => {
            let cfg: crate::mcp::McpServerConfig =
                serde_json::from_value(args.get("config").cloned().unwrap_or_default()).map_err(|e| format!("bad config: {e}"))?;
            if args.get("sidePanelBrowser").and_then(|v| v.as_bool()) == Some(true) {
                crate::mcp::oauth_login(&cfg, |url| async move {
                    let resources = test_resource_dir();
                    crate::browser::open_sign_in_tab(resources.as_deref(), &url).await
                })
                .await?;
            } else {
                let open = URL_OPENER
                    .get()
                    .ok_or_else(|| "browser sign-in needs the Vicuna desktop app".to_string())?;
                crate::mcp::oauth_login(&cfg, |url| std::future::ready(open(&url))).await?;
            }
            Ok(serde_json::json!(st.mcp.connect(&cfg).await?))
        }
        // ----- in-app updates -----
        "app_build_info" => Ok(crate::app_build_info_logic().await?),
        "github_latest_release" => Ok(crate::github_latest_release_logic().await?),
        "github_download_asset" => {
            Ok(serde_json::json!(crate::github_download_asset_logic(s("url")?, s("fileName")?).await?))
        }
        "bridge_open_path" => {
            crate::open_installer(&s("path")?)?;
            Ok(serde_json::Value::Null)
        }
        // ----- registry fetch (no CORS in Rust) -----
        "browser_status" => {
            Ok(serde_json::json!(
                crate::browser::browser_status_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_start" => {
            Ok(serde_json::json!(
                crate::browser::browser_start_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_stop" => Ok(serde_json::json!(crate::browser::browser_stop_logic(&st.browser).await?)),
        "browser_restart" => {
            Ok(serde_json::json!(
                crate::browser::browser_restart_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_ensure_engine" => {
            let background = args
                .get("background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            Ok(serde_json::json!(
                crate::browser::browser_ensure_engine_logic(test_resource_dir(), background).await?
            ))
        }
        "browser_takeover" => {
            Ok(serde_json::json!(
                crate::browser::browser_takeover_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_mcp_command" => {
            Ok(serde_json::json!(crate::browser::browser_mcp_command(test_resource_dir().as_deref())?))
        }
        "ollama_fetch_text" => {
            Ok(serde_json::json!(crate::ollama_fetch_text_logic(s("url")?).await?))
        }
        // the registry itself: every action this server answers
        "api_actions" => Ok(serde_json::json!(API_ACTIONS)),
        _ => Err(format!("unknown bridge command {cmd}")),
    }
}

type BoxBody = http_body_util::combinators::BoxBody<bytes::Bytes, std::convert::Infallible>;

fn full_body(data: impl Into<bytes::Bytes>) -> BoxBody {
    use http_body_util::BodyExt;
    http_body_util::Full::new(data.into()).boxed()
}

fn json_resp(status: u16, body: impl Into<bytes::Bytes>) -> hyper::Response<BoxBody> {
    hyper::Response::builder()
        .status(status)
        .header("Access-Control-Allow-Origin", "*")
        .header("Access-Control-Allow-Methods", "GET, POST, OPTIONS")
        .header("Access-Control-Allow-Headers", "Content-Type, Authorization")
        .header("Content-Type", "application/json")
        .body(full_body(body))
        .unwrap()
}

fn lb_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client")
    })
}

fn strip_lb_uri(uri: &hyper::Uri) -> String {
    let path = uri.path();
    let stripped = path.strip_prefix("/lb").unwrap_or(path);
    let target_path = if stripped.is_empty() { "/" } else { stripped };
    match uri.query() {
        Some(q) => format!("{target_path}?{q}"),
        None => target_path.to_string(),
    }
}

async fn proxy_lb_ws(
    mut req: hyper::Request<hyper::body::Incoming>,
    target_uri: String,
    port: u16,
) -> hyper::Response<BoxBody> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut lb_stream = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        _ => return json_resp(502, r#"{"ok":false,"error":"live-browser unreachable"}"#),
    };

    let mut raw_req = format!("{} {target_uri} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n", req.method());
    for (name, val) in req.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "host" || lower == "authorization" {
            continue;
        }
        if let Ok(v) = val.to_str() {
            raw_req.push_str(&format!("{name}: {v}\r\n"));
        }
    }
    raw_req.push_str("\r\n");

    if lb_stream.write_all(raw_req.as_bytes()).await.is_err() {
        return json_resp(502, r#"{"ok":false,"error":"live-browser write failed"}"#);
    }

    let mut buf = Vec::with_capacity(2048);
    let mut tmp = [0u8; 1024];
    let header_end = loop {
        match tokio::time::timeout(std::time::Duration::from_secs(5), lb_stream.read(&mut tmp)).await {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => {
                return json_resp(502, r#"{"ok":false,"error":"live-browser handshake failed"}"#);
            }
            Ok(Ok(n)) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    break pos;
                }
                if buf.len() > 16 * 1024 {
                    return json_resp(502, r#"{"ok":false,"error":"live-browser header too large"}"#);
                }
            }
        }
    };

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let leftover = buf[header_end + 4..].to_vec();
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);

    let mut builder = hyper::Response::builder().status(status_code);
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            builder = builder.header(k.trim(), v.trim());
        }
    }

    if status_code != 101 {
        return builder.body(full_body(leftover)).unwrap_or_else(|_| json_resp(502, ""));
    }

    tokio::spawn(async move {
        if let Ok(upgraded) = hyper::upgrade::on(&mut req).await {
            let mut client_io = hyper_util::rt::TokioIo::new(upgraded);
            if !leftover.is_empty() && client_io.write_all(&leftover).await.is_err() {
                return;
            }
            let _ = tokio::io::copy_bidirectional(&mut client_io, &mut lb_stream).await;
        }
    });

    builder.body(full_body("")).unwrap()
}

async fn proxy_lb(req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<BoxBody> {
    use http_body_util::BodyExt;
    let port = crate::browser::lb_port();
    let target_uri = strip_lb_uri(req.uri());

    let is_ws = req
        .headers()
        .get(hyper::header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    if is_ws {
        return proxy_lb_ws(req, target_uri, port).await;
    }

    let method = match reqwest::Method::from_bytes(req.method().as_str().as_bytes()) {
        Ok(m) => m,
        Err(_) => return json_resp(405, r#"{"ok":false,"error":"unsupported method"}"#),
    };
    let url = format!("http://127.0.0.1:{port}{target_uri}");
    let mut fwd = lb_client().request(method, &url);
    for (name, val) in req.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(lower.as_str(), "host" | "authorization" | "connection" | "transfer-encoding" | "content-length") {
            continue;
        }
        if let (Ok(rn), Ok(rv)) = (
            reqwest::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            reqwest::header::HeaderValue::from_bytes(val.as_bytes()),
        ) {
            fwd = fwd.header(rn, rv);
        }
    }

    let body_bytes = match req.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return json_resp(400, r#"{"ok":false,"error":"unreadable request body"}"#),
    };
    if !body_bytes.is_empty() {
        fwd = fwd.body(body_bytes);
    }

    let resp = match fwd.send().await {
        Ok(r) => r,
        Err(e) => {
            let msg = serde_json::json!({"ok": false, "error": format!("live-browser unreachable: {e}")}).to_string();
            return json_resp(502, msg);
        }
    };

    let status = resp.status().as_u16();
    let mut builder = hyper::Response::builder().status(status);
    for (name, val) in resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if matches!(lower.as_str(), "transfer-encoding" | "connection" | "content-length") {
            continue;
        }
        if let (Ok(hn), Ok(hv)) = (
            hyper::header::HeaderName::from_bytes(name.as_str().as_bytes()),
            hyper::header::HeaderValue::from_bytes(val.as_bytes()),
        ) {
            builder = builder.header(hn, hv);
        }
    }
    let bytes = resp.bytes().await.unwrap_or_default();
    builder.body(full_body(bytes)).unwrap_or_else(|_| json_resp(502, ""))
}

/// Responses smaller than this go out as-is (gzip would barely pay off).
const GZIP_MIN: usize = 1024;

fn accepts_gzip(headers: &hyper::HeaderMap) -> bool {
    headers
        .get_all(hyper::header::ACCEPT_ENCODING)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .any(|e| {
            let mut parts = e.split(';');
            let name = parts.next().unwrap_or("").trim();
            let q_zero = parts.any(|p| matches!(p.trim().replace(' ', "").as_str(), "q=0" | "q=0.0" | "q=0.00" | "q=0.000"));
            (name.eq_ignore_ascii_case("gzip") || name == "*") && !q_zero
        })
}

fn gzip_bytes(data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    // fast level: the server may be a Steam Deck, and the win is the wire
    let mut enc = flate2::write::GzEncoder::new(Vec::with_capacity(data.len() / 4), flate2::Compression::fast());
    enc.write_all(data).ok()?;
    enc.finish().ok()
}

/// gzip a JSON response for a client that accepts it. Chat history is text
/// heavy and a remote `chats_load` is megabytes: over a slow link the window
/// waited a minute or more for it before drawing (measured 6 MB in 85 s).
/// Only our own JSON responses: the live-browser proxy keeps its encoding and
/// streams untouched.
async fn maybe_gzip(resp: hyper::Response<BoxBody>) -> hyper::Response<BoxBody> {
    use http_body_util::BodyExt;
    let is_json = resp
        .headers()
        .get(hyper::header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes() == b"application/json");
    if !is_json || resp.headers().contains_key(hyper::header::CONTENT_ENCODING) {
        return resp;
    }
    let (mut parts, body) = resp.into_parts();
    let data = match body.collect().await {
        Ok(c) => c.to_bytes(),
        Err(e) => match e {},
    };
    if data.len() < GZIP_MIN {
        return hyper::Response::from_parts(parts, full_body(data));
    }
    match gzip_bytes(&data) {
        Some(z) => {
            parts.headers.insert(hyper::header::CONTENT_ENCODING, hyper::header::HeaderValue::from_static("gzip"));
            parts.headers.insert(hyper::header::VARY, hyper::header::HeaderValue::from_static("Accept-Encoding"));
            parts.headers.remove(hyper::header::CONTENT_LENGTH);
            hyper::Response::from_parts(parts, full_body(z))
        }
        None => hyper::Response::from_parts(parts, full_body(data)),
    }
}

/// A plugin panel file with the locked-down headers (see plugins::PANEL_CSP).
fn panel_resp(path: &str) -> hyper::Response<BoxBody> {
    let (status, ctype, body) = crate::plugins::serve_panel(path);
    hyper::Response::builder()
        .status(status)
        .header("Content-Type", ctype)
        .header("Content-Security-Policy", crate::plugins::PANEL_CSP)
        .header("X-Content-Type-Options", "nosniff")
        .header("Referrer-Policy", "no-referrer")
        .header("Cache-Control", "no-store")
        .header("Cross-Origin-Resource-Policy", "cross-origin")
        .header("Permissions-Policy", "camera=(), microphone=(), geolocation=(), clipboard-read=(), usb=(), serial=(), payment=()")
        .body(full_body(body))
        .unwrap()
}

async fn handle(st: Arc<BridgeState>, req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<BoxBody> {
    let gzip = accepts_gzip(req.headers()) && !req.uri().path().starts_with("/lb");
    let resp = handle_inner(st, req).await;
    if gzip { maybe_gzip(resp).await } else { resp }
}

async fn handle_inner(st: Arc<BridgeState>, req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<BoxBody> {
    use http_body_util::BodyExt;
    use hyper::Method;
    let path = req.uri().path().to_string();
    if *req.method() == Method::OPTIONS {
        return json_resp(200, "");
    }
    if *req.method() == Method::GET && path == "/health" {
        let body = if st.token.is_some() { r#"{"ok":true,"bridge":1,"server":true}"# } else { r#"{"ok":true,"bridge":1}"# };
        return json_resp(200, body);
    }
    // Plugin panel files: no bearer token (an iframe can't send one); the
    // per-plugin capability in the path gates them (plugins::serve_panel).
    if *req.method() == Method::GET && path.starts_with("/plugin-panel/") {
        return panel_resp(&path);
    }
    if let Some(want) = &st.token {
        let given = req
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim();
        let given = given.strip_prefix("Bearer ").unwrap_or("").trim();
        if !token_eq(given, want) {
            return json_resp(401, r#"{"ok":false,"error":"wrong or missing token"}"#);
        }
    }
    if path == "/lb" || path.starts_with("/lb/") {
        return proxy_lb(req).await;
    }
    if *req.method() == Method::GET && path.starts_with("/poll/") {
        let id = path.trim_start_matches("/poll/").to_string();
        // `?wait=<ms>` long-polls (remote clients): hold the request until
        // events arrive or the wait runs out, so terminal echo and turn
        // tokens arrive at once instead of on the next poll tick.
        let wait_ms: u64 = req
            .uri()
            .query()
            .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("wait=")))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
            .min(25_000);
        let touch = |st: &BridgeState| {
            if let Ok(mut lp) = st.last_poll.lock() {
                lp.insert(id.clone(), std::time::Instant::now());
            }
        };
        touch(&st);
        let events = if wait_ms == 0 {
            drain_events(&st.events, &id)
        } else {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
            loop {
                let notified = st.events.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let batch = drain_events(&st.events, &id);
                if !batch.is_empty() {
                    break batch;
                }
                if tokio::time::timeout_at(deadline, notified).await.is_err() {
                    break drain_events(&st.events, &id);
                }
            }
        };
        touch(&st);
        let body = serde_json::json!({"events": events}).to_string();
        return json_resp(200, body);
    }
    if *req.method() == Method::POST && path == "/invoke" {
        let mut body = Vec::new();
        let mut ok = true;
        let mut incoming = req.into_body();
        while let Some(frame) = incoming.frame().await {
            match frame {
                Ok(f) => {
                    if let Some(chunk) = f.data_ref() {
                        if body.len() + chunk.len() > BODY_CAP as usize {
                            ok = false;
                            break;
                        }
                        body.extend_from_slice(chunk);
                    }
                }
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        let out = if !ok {
            serde_json::json!({"ok": false, "error": "unreadable or oversized body"}).to_string()
        } else {
            match serde_json::from_slice::<serde_json::Value>(&body) {
                Err(e) => serde_json::json!({"ok": false, "error": format!("bad json: {e}")}).to_string(),
                Ok(v) => {
                    let cmd = v.get("cmd").and_then(|c| c.as_str()).unwrap_or("");
                    let args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);
                    match dispatch(&st, cmd, &args).await {
                        Ok(data) => serde_json::json!({"ok": true, "data": data}).to_string(),
                        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
                    }
                }
            }
        };
        return json_resp(200, out);
    }
    json_resp(404, r#"{"ok":false,"error":"not found"}"#)
}

fn token_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn server_name() -> String {
    for k in ["VICUNA_SERVER_NAME", "HOSTNAME", "COMPUTERNAME"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return v.trim().to_string();
            }
        }
    }
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "vicuna".into())
}

pub fn server_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join("vicuna").join("server")
}

/// `VICUNA_SERVER_TOKEN`, else `~/vicuna/server/token` (created 0600 with a
/// fresh 128-bit token on first run).
pub fn server_token() -> String {
    if let Ok(t) = std::env::var("VICUNA_SERVER_TOKEN") {
        if !t.trim().is_empty() {
            return t.trim().to_string();
        }
    }
    let p = server_dir().join("token");
    if let Ok(t) = std::fs::read_to_string(&p) {
        if !t.trim().is_empty() {
            return t.trim().to_string();
        }
    }
    let mut b = [0u8; 16];
    getrandom::fill(&mut b).expect("os randomness");
    let t: String = b.iter().map(|x| format!("{x:02x}")).collect();
    let _ = std::fs::create_dir_all(server_dir());
    std::fs::write(&p, &t).expect("write server token");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
    }
    t
}

#[derive(Default, Clone)]
pub struct Config {
    pub bind: String,
    pub port: u16,
    pub token: Option<String>,
    pub db: Option<std::path::PathBuf>,
    /// `--tailscale [--https-port N]`: expose over `tailscale serve` (HTTPS,
    /// tailnet only) after binding, and print the URL to paste in the app
    pub tailscale: bool,
    pub https_port: Option<u16>,
    /// app resource root (the desktop's bundled `lb`); None = headless
    pub resource_dir: Option<std::path::PathBuf>,
}


type UrlOpener = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
static URL_OPENER: std::sync::OnceLock<UrlOpener> = std::sync::OnceLock::new();

/// The desktop app lets its embedded server open the system browser
/// (MCP OAuth sign-in).
pub fn set_url_opener(f: impl Fn(&str) -> Result<(), String> + Send + Sync + 'static) {
    let _ = URL_OPENER.set(Box::new(f));
}

/// The desktop app's resource root (bundled `lb`), set once by an embedded server.
static EMBEDDED_RESOURCE_DIR: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

/// Every action the API answers (`POST /invoke {cmd, args}`). Anything that
/// touches the database is an action here, so every frontend — the Svelte
/// app, the `vicuna` CLI, a future mobile app — and headless tests reach the
/// same behavior. `vicuna api list` prints this; a test keeps it equal to the
/// `dispatch` arms.
pub const API_ACTIONS: &[&str] = &[
    "message_delete",
    "message_edit",
    "session_compact",
    "session_create",
    "session_delete",
    "session_fork",
    "session_redo",
    "session_undo",
    "session_update",
    "app_build_info",
    "bridge_open_path",
    "browser_ensure_engine",
    "browser_mcp_command",
    "browser_restart",
    "browser_start",
    "browser_status",
    "browser_stop",
    "browser_takeover",
    "chat_turn_attach",
    "chat_turn_live",
    "chat_turn_start",
    "chat_turn_stop",
    "chats_get",
    "chats_list",
    "chats_load",
    "chats_merge",
    "cli_session",
    "fs_list_dirs",
    "git_apply",
    "git_diff",
    "git_discard",
    "git_stage",
    "git_status",
    "git_unstage",
    "github_download_asset",
    "github_latest_release",
    "mcp_call_tool",
    "mcp_connect",
    "mcp_delete_server",
    "mcp_disconnect",
    "mcp_list_servers",
    "mcp_set_defaults",
    "mcp_list_tools",
    "mcp_login_present",
    "mcp_save_server",
    "mcp_signin",
    "mcp_signout",
    "ollama_delete_model",
    "ollama_fetch_text",
    "ollama_list_models",
    "ollama_list_running",
    "ollama_model_show",
    "ollama_pull_model",
    "ollama_server_version",
    "ollama_warm_model",
    "opencode_models",
    "opencode_set_model",
    "bridge_list",
    "bridge_remove",
    "bridge_save",
    "bridge_set",
    "bridge_status",
    "bridge_unpair",
    "plugin_catalog",
    "plugin_install",
    "plugin_list",
    "plugin_storage_get",
    "plugin_storage_keys",
    "plugin_storage_remove",
    "plugin_storage_set",
    "plugin_uninstall",
    "plugin_harness_consent",
    "plugin_harness_revoke",
    "plugin_upgrade",
    "schedule_list",
    "schedule_put",
    "schedule_remove",
    "server_info",
    "server_search_chats",
    "server_upgrade",
    "server_upgrade_check",
    "server_upgrade_status",
    "skills_list",
    "skills_read",
    "skills_remove",
    "skills_save",
    "skills_set_defaults",
    "telegram_list",
    "telegram_remove",
    "telegram_save",
    "telegram_set",
    "telegram_status",
    "telegram_unpair",
    "term_kill",
    "term_resize",
    "term_spawn",
    "term_write",
    "upload_read_file",
    "upload_stage_file",
    "work_agent_answer",
    "work_agent_attach",
    "work_agent_live",
    "work_agent_queue",
    "work_agent_remove",
    "work_agent_start",
    "work_agent_stop",
    "work_agent_turn_live",
    "work_default_workspace",
    "work_delete",
    "work_engine_info",
    "work_engine_select",
    "work_exec",
    "work_exec_kill",
    "work_exec_list",
    "work_exec_start",
    "work_exec_status",
    "work_fetch_url",
    "work_grep",
    "work_import_file",
    "work_list_dir",
    "work_models",
    "work_read_chunk",
    "work_read_file",
    "work_read_image",
    "work_redo",
    "work_rename",
    "work_set_approval",
    "work_set_model",
    "work_shell_exec",
    "work_shell_input",
    "work_shell_interrupt",
    "work_shell_status",
    "work_tree",
    "work_undo",
    "work_usage",
    "work_account_support",
    "work_account_add",
    "work_account_rename",
    "work_account_remove",
    "agy_account_list",
    "agy_account_add",
    "agy_account_rename",
    "agy_account_remove",
    "work_validate_root",
    "work_ensure_scratch_workspace",
    "work_write_file",
    "api_actions",
    "whisper_install",
    "whisper_status",
    "whisper_uninstall",
];

/// A bound (not yet serving) server.
pub struct Bound {
    pub state: Arc<BridgeState>,
    pub listener: tokio::net::TcpListener,
    pub addr: std::net::SocketAddr,
}

/// Open the db, wire the server-mode hooks and bind <bind>:<port> (port 0 =
/// any free port). Must run inside a tokio runtime.
pub async fn bind(cfg: &Config) -> Result<Bound, String> {
    let mut state = BridgeState::default();
    if let Some(path) = &cfg.db {
        let pool = crate::server_store::open(path).await?;
        crate::vicuna_mcp::set_db_pool_override(pool.clone());
        state.db = Some(pool);
    }
    state.token = cfg.token.clone();
    state.resource_dir = cfg.resource_dir.clone();
    if let Some(d) = &cfg.resource_dir {
        let _ = EMBEDDED_RESOURCE_DIR.set(d.clone());
    }
    let addr = format!("{}:{}", cfg.bind, cfg.port);
    let listener = tokio::net::TcpListener::bind(&addr).await.map_err(|e| format!("bind {addr}: {e}"))?;
    let addr = listener.local_addr().map_err(|e| e.to_string())?;
    let state = Arc::new(state);
    if state.db.is_some() {
        // server mode: scheduled sends and MCP wake-ups run with no window
        turns::spawn_scheduler(state.clone());
        telegram::spawn(state.clone());
        bridge_plugins::spawn(state.clone());

    }
    spawn_term_reaper(state.clone());
    Ok(Bound { state, listener, addr })
}

// Terminals whose tab stopped polling (window closed, laptop asleep,
// client gone) would keep their shell alive forever on a server: reap
// any whose channel hasn't been polled for 90 s.
fn spawn_term_reaper(st: Arc<BridgeState>) {
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(20));
        let stale: Vec<(String, String)> = {
            let tc = st.term_channels.lock().map(|m| m.clone()).unwrap_or_default();
            let lp = st.last_poll.lock().map(|m| m.clone()).unwrap_or_default();
            tc.into_iter()
                .filter(|(_, ch)| lp.get(ch).is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(90)))
                .collect()
        };
        for (term, ch) in stale {
            let _ = crate::term_kill_logic(&st.terms, &term);
            if let Ok(mut tc) = st.term_channels.lock() {
                tc.remove(&term);
            }
            if let Ok(mut lp) = st.last_poll.lock() {
                lp.remove(&ch);
            }
            if let Ok(mut q) = st.events.lock() {
                q.remove(&ch);
            }
        }
        // reader queues whose client went away (window closed mid-turn)
        let lp = st.last_poll.lock().map(|m| m.clone()).unwrap_or_default();
        if let Ok(mut q) = st.events.lock() {
            q.retain(|k, _| {
                !k.contains(READER_SEP) || lp.get(k).is_some_and(|t| t.elapsed() <= std::time::Duration::from_secs(90))
            });
        }
    });
}

/// End everything this server started (browser daemon, terminals, background
/// jobs, agent shells). Sync version for the desktop app's Quit hook.
pub fn shutdown_state(st: &BridgeState) {
    crate::browser::kill_owned(&st.browser);
    crate::term_kill_all(&st.terms);
    crate::bg_kill_running(&st.bg);
    crate::kill_agent_shells(&st.shell);
}

/// Async graceful shutdown for `vicuna serve` / `vicuna bridge`: stops the
/// owned browser daemon gracefully (waiting for `lb web --port P stop` so its
/// Chromium is reaped and the port is released before exit), then tears down
/// terminals, background jobs, and agent shells.
pub async fn shutdown_state_async(st: &BridgeState) {
    crate::browser::stop_owned(&st.browser).await;
    crate::term_kill_all(&st.terms);
    crate::bg_kill_running(&st.bg);
    crate::kill_agent_shells(&st.shell);
}

impl Bound {
    /// Accept connections forever.
    pub async fn serve(self) {
        self.serve_until(std::future::pending()).await;
    }

    /// Accept connections until `shutdown` resolves, then tear down owned
    /// child processes (browser daemon, terminals, background jobs, agent
    /// shells) before returning.
    pub async fn serve_until<F: std::future::Future<Output = ()>>(self, shutdown: F) {
        let Bound { state, listener, .. } = self;
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                _ = &mut shutdown => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else {
                        continue;
                    };
                    let _ = stream.set_nodelay(true);
                    let st = state.clone();
                    tokio::spawn(async move {
                        let io = hyper_util::rt::TokioIo::new(stream);
                        let svc = hyper::service::service_fn(move |req| {
                            let st = st.clone();
                            async move { Ok::<_, std::convert::Infallible>(handle(st, req).await) }
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(io, svc)
                            .with_upgrades()
                            .await;
                    });
                }
            }
        }
        drop(listener);
        shutdown_state_async(&state).await;
    }
}

#[cfg(test)]
mod api_registry_tests {
    /// API_ACTIONS must list exactly the `dispatch` arms (adding an action
    /// without registering it, or the reverse, fails here).
    #[test]
    fn registry_matches_dispatch() {
        let src = include_str!("mod.rs");
        let start = src.find("async fn dispatch(").unwrap();
        let end = start + src[start..].find("unknown bridge command").unwrap();
        let mut arms: Vec<String> = Vec::new();
        for line in src[start..end].lines() {
            let t = line.trim_start();
            if !t.starts_with('"') || !t.contains("=>") {
                continue;
            }
            let head = &t[..t.find("=>").unwrap()];
            for part in head.split('|') {
                let p = part.trim().trim_matches('"');
                if !p.is_empty() && p.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    arms.push(p.to_string());
                }
            }
        }
        arms.sort();
        arms.dedup();
        let mut reg: Vec<String> = super::API_ACTIONS.iter().map(|s| s.to_string()).collect();
        reg.sort();
        assert_eq!(arms, reg, "API_ACTIONS out of sync with dispatch");
    }
}

#[cfg(test)]
mod plugin_panel_route_tests {
    /// Every panel response (found or not) carries the sandboxing CSP and the
    /// no-sniff / no-referrer / no-store headers, and needs no bearer token.
    #[test]
    fn panel_responses_carry_the_lockdown_headers() {
        let r = super::panel_resp("/plugin-panel/nope/0123/x.html");
        assert_eq!(r.status(), 404);
        let h = r.headers();
        assert_eq!(h["content-security-policy"], crate::plugins::PANEL_CSP);
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cache-control"], "no-store");
        assert!(h.get("access-control-allow-origin").is_none());
        assert!(h.get("x-frame-options").is_none(), "panels must stay frameable");
    }
}

#[cfg(test)]
mod event_queue_tests {
    use super::*;

    #[test]
    fn adjacent_text_events_merge_without_loss() {
        let q: EventQueues = Default::default();
        for i in 0..5000 {
            push_event(&q, "c", &format!(r#"{{"type":"token","text":"{}"}}"#, i % 10));
        }
        push_event(&q, "c", r#"{"type":"think","text":"a"}"#);
        push_event(&q, "c", r#"{"type":"think","text":"b"}"#);
        push_event(&q, "c", r#"{"type":"toolstart","name":"x","args":{}}"#);
        push_event(&q, "c", r#"{"type":"token","text":"z"}"#);
        let ev = drain_events(&q, "c");
        assert_eq!(ev.len(), 4);
        assert_eq!(ev[0]["text"].as_str().unwrap().len(), 5000);
        assert_eq!(ev[1]["text"], "ab");
        assert_eq!(ev[2]["type"], "toolstart");
        assert_eq!(ev[3]["text"], "z");
    }

    #[tokio::test]
    async fn push_event_wakes_waiter_immediately() {
        let q: EventQueues = Default::default();
        let q2 = q.clone();
        let start = tokio::time::Instant::now();
        let waiter = tokio::spawn(async move {
            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let notified = q2.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let batch = drain_events(&q2, "term-1");
                if !batch.is_empty() {
                    return batch;
                }
                if tokio::time::timeout_at(deadline, notified).await.is_err() {
                    return drain_events(&q2, "term-1");
                }
            }
        });
        tokio::task::yield_now().await;
        push_event(&q, "term-1", r#"{"type":"data","data":"a"}"#);
        let ev = waiter.await.unwrap();
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0]["data"], "a");
        assert!(start.elapsed() < std::time::Duration::from_millis(200));
    }
}


#[cfg(test)]
mod gzip_tests {
    use super::*;

    fn hdr(v: &str) -> hyper::HeaderMap {
        let mut h = hyper::HeaderMap::new();
        h.insert(hyper::header::ACCEPT_ENCODING, hyper::header::HeaderValue::from_str(v).unwrap());
        h
    }

    #[test]
    fn accept_encoding_parsing() {
        assert!(accepts_gzip(&hdr("gzip")));
        assert!(accepts_gzip(&hdr("br, gzip, deflate")));
        assert!(accepts_gzip(&hdr("gzip;q=0.5")));
        assert!(accepts_gzip(&hdr("*")));
        assert!(!accepts_gzip(&hdr("gzip;q=0")));
        assert!(!accepts_gzip(&hdr("br, deflate")));
        assert!(!accepts_gzip(&hyper::HeaderMap::new()));
    }

    #[tokio::test]
    async fn large_json_is_gzipped_small_and_foreign_are_not() {
        use http_body_util::BodyExt;
        use std::io::Read;
        let big = serde_json::json!({"ok": true, "data": "hello world ".repeat(2000)}).to_string();
        let resp = maybe_gzip(json_resp(200, big.clone())).await;
        assert_eq!(resp.headers().get(hyper::header::CONTENT_ENCODING).unwrap(), "gzip");
        let z = resp.into_body().collect().await.unwrap().to_bytes();
        assert!(z.len() < big.len() / 10);
        let mut out = String::new();
        flate2::read::GzDecoder::new(&z[..]).read_to_string(&mut out).unwrap();
        assert_eq!(out, big);

        let small = maybe_gzip(json_resp(200, r#"{"ok":true}"#)).await;
        assert!(small.headers().get(hyper::header::CONTENT_ENCODING).is_none());

        let html = hyper::Response::builder()
            .header("Content-Type", "text/html")
            .body(full_body("x".repeat(5000)))
            .unwrap();
        assert!(maybe_gzip(html).await.headers().get(hyper::header::CONTENT_ENCODING).is_none());
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    #[tokio::test]
    async fn serve_until_exits_and_stops_accepting_on_shutdown() {
        let bound = bind(&Config {
            bind: "127.0.0.1".into(),
            port: 0,
            ..Default::default()
        })
        .await
        .expect("bind ephemeral port");
        let addr = bound.addr;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let task = tokio::spawn(bound.serve_until(async move {
            let _ = rx.await;
        }));

        let client = reqwest::Client::new();
        let resp = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("health before shutdown");
        assert!(resp.status().is_success());

        let _ = tx.send(());
        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("serve_until should return promptly")
            .expect("serve_until task join");

        assert!(
            tokio::net::TcpStream::connect(addr).await.is_err(),
            "listener should be closed after shutdown"
        );
    }
}
