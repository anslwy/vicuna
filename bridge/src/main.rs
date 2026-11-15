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

mod cli;

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeResponseBody};

use vicuna_lib::acp::AcpState;
use vicuna_lib::browser::BrowserState;
use vicuna_lib::mcp::McpManager;
use vicuna_lib::work_agent::{
    agent_answer, agent_queue_msg, agent_remove_msg, agent_stop, TurnState, WorkAgentState, WorkAgentEvent,
};
use vicuna_lib::{AgentShellState, BgExecState, BgJobStatus, BgJobSummary, ChatMessage, UndoState};

const EVENT_CAP: usize = 2000;
const BODY_CAP: u64 = 32 * 1024 * 1024;

type EventQueues = Arc<Mutex<HashMap<String, VecDeque<serde_json::Value>>>>;

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
    pub db: Option<vicuna_lib::server_store::Pool>,
    /// terminal tabs (remote-server terminals): ptys + term id -> channel id
    pub terms: Arc<vicuna_lib::TermState>,
    pub term_channels: Arc<Mutex<HashMap<String, String>>>,
    /// channel id -> last time a client polled it (orphan-terminal reaper)
    pub last_poll: Arc<Mutex<HashMap<String, std::time::Instant>>>,
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
            terms: Arc::new(vicuna_lib::TermState::default()),
            term_channels: Arc::new(Mutex::new(HashMap::new())),
            last_poll: Arc::new(Mutex::new(HashMap::new())),
            events: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

fn push_event(queues: &EventQueues, id: &str, raw: &str) {
    let v: serde_json::Value =
        serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()));
    if let Ok(mut map) = queues.lock() {
        let q = map.entry(id.to_string()).or_default();
        q.push_back(v);
        while q.len() > EVENT_CAP {
            q.pop_front();
        }
    }
}

fn drain_events(queues: &EventQueues, id: &str) -> Vec<serde_json::Value> {
    if let Ok(mut map) = queues.lock() {
        if let Some(q) = map.get_mut(id) {
            return q.drain(..).collect();
        }
    }
    Vec::new()
}

fn channel_for<T: tauri::ipc::IpcResponse>(queues: &EventQueues, uuid: &str) -> Channel<T> {
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
fn test_resource_dir() -> Option<std::path::PathBuf> {
    std::env::var("VICUNA_TEST_RESOURCE_DIR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    st: &BridgeState,
    cmd: &str,
    args: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let s = |k: &str| arg_str(args, k);
    match cmd {
        // ----- work agent turn -----
        "work_agent_start" => {
            let uuid = args
                .get("onEvent")
                .and_then(|v| v.get("__bridge_channel"))
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing channel marker onEvent".to_string())?
                .to_string();
            let on_event: Channel<WorkAgentEvent> = channel_for(&st.events, &uuid);
            let engine = vicuna_lib::work_engine::effective_engine(arg_opt_str(args, "engine").as_deref());
            if !engine.is_present() {
                return Err(engine.missing_message());
            }
            let turn = Arc::new(TurnState::new());
            let turn_id = st.agent.next_turn(turn.clone());
            let acp = st.acp.clone();
            let model = arg_opt_str(args, "model");
            let fresh_session = args.get("freshSession").and_then(|v| v.as_bool()).unwrap_or(false);
            let workspace = s("workspace")?;
            let messages: Vec<ChatMessage> =
                serde_json::from_value(args.get("messages").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad messages: {e}"))?;
            let session_id = arg_opt_str(args, "sessionId");
            let goal_text = arg_opt_str(args, "goalText");
            let mcp_snapshot =
                vicuna_lib::acp::McpSnapshot::capture(&st.mcp, test_resource_dir().as_deref()).await;
            tokio::spawn(async move {
                vicuna_lib::acp::run_turn(
                    turn, model, workspace, messages, session_id, goal_text, fresh_session, on_event, &acp,
                    mcp_snapshot, engine,
                )
                .await;
            });
            Ok(serde_json::json!(turn_id))
        }
        "work_engine_info" => Ok(serde_json::to_value(vicuna_lib::work_engine::engines_info().await)
            .map_err(|e| e.to_string())?),
        "work_engine_select" => Ok(serde_json::to_value(
            vicuna_lib::work_engine::select_engine(&s("engine")?).await?,
        )
        .map_err(|e| e.to_string())?),
        "work_models" => {
            let engine = vicuna_lib::work_engine::effective_engine(arg_opt_str(args, "engine").as_deref());
            Ok(serde_json::to_value(vicuna_lib::work_engine::list_models(engine, &st.acp).await?)
                .map_err(|e| e.to_string())?)
        }
        "work_set_approval" => {
            let ask = args.get("ask").and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::to_value(vicuna_lib::work_engine::set_approval(ask).await?).map_err(|e| e.to_string())?)
        }
        "work_set_model" => {
            vicuna_lib::work_engine::set_model(
                arg_opt_str(args, "engine").as_deref(),
                arg_opt_str(args, "model").as_deref(),
            )?;
            Ok(serde_json::Value::Null)
        }
        "work_usage" => Ok(serde_json::to_value(
            vicuna_lib::work_usage::fetch_usage_logic(arg_opt_str(args, "engine")).await?,
        )
        .map_err(|e| e.to_string())?),
        "opencode_models" => Ok(serde_json::to_value(vicuna_lib::opencode::list_models().await?)
            .map_err(|e| e.to_string())?),
        "opencode_set_model" => {
            vicuna_lib::opencode::save_model(arg_opt_str(args, "model").as_deref())?;
            Ok(serde_json::Value::Null)
        }
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
            agent_answer(&turn, &s("askId")?, s("answer")?)?;
            Ok(serde_json::Value::Null)
        }
        "work_agent_background" => {
            let turn = st.agent.get_turn(&s("turnId")?).ok_or("unknown turn")?;
            vicuna_lib::work_agent::agent_background(&turn, &s("jobId")?)?;
            Ok(serde_json::Value::Null)
        }
        "work_run_status" => Ok(serde_json::json!(vicuna_lib::bg_snapshot(vicuna_lib::vicuna_mcp::jobs(), &s("id")?)?)),
        "work_run_kill" => Ok(serde_json::json!(vicuna_lib::bg_kill(vicuna_lib::vicuna_mcp::jobs(), &s("id")?)?)),
        // ----- workspace validation / tools -----
        "work_validate_root" => {
            Ok(serde_json::json!(vicuna_lib::work_validate_root_logic(s("path")?).await?))
        }
        "work_list_dir" => {
            let path = arg_opt_str(args, "path");
            Ok(serde_json::json!(vicuna_lib::work_list_dir_logic(s("root")?, path).await?))
        }
        "work_tree" => {
            let max = args.get("maxDepth").and_then(|v| v.as_u64()).map(|n| n as usize);
            Ok(serde_json::json!(vicuna_lib::work_tree_logic(s("root")?, max).await?))
        }
        "work_read_file" => {
            let max = args.get("maxBytes").and_then(|v| v.as_u64());
            let offset = args.get("offset").and_then(|v| v.as_u64());
            let limit = args.get("limit").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                vicuna_lib::work_read_file_logic(s("root")?, s("path")?, max, offset, limit).await?
            ))
        }
        "work_read_image" => {
            Ok(serde_json::json!(vicuna_lib::work_read_image_logic(s("root")?, s("path")?).await?))
        }
        "work_grep" => {
            let include = arg_opt_str(args, "include");
            Ok(serde_json::json!(
                vicuna_lib::work_grep_logic(s("root")?, s("pattern")?, include).await?
            ))
        }
        "work_fetch_url" => {
            Ok(serde_json::json!(vicuna_lib::work_fetch_url_logic(s("url")?).await?))
        }
        "work_write_file" => {
            Ok(serde_json::json!(
                vicuna_lib::work_write_file_logic(s("root")?, s("path")?, s("content")?).await?
            ))
        }
        "work_exec" => {
            let timeout = args.get("timeoutMs").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                vicuna_lib::work_exec_logic(s("root")?, s("command")?, timeout).await?
            ))
        }
        "work_exec_start" => {
            Ok(serde_json::json!(vicuna_lib::bg_start(&st.bg, s("root")?, s("command")?).await?))
        }
        "work_exec_status" => {
            let r: BgJobStatus = vicuna_lib::bg_snapshot(&st.bg, &s("id")?)?;
            Ok(serde_json::json!(r))
        }
        "work_shell_exec" => {
            let timeout = args.get("timeoutMs").and_then(|v| v.as_u64());
            Ok(serde_json::json!(
                vicuna_lib::agent_shell_exec(
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
            Ok(serde_json::json!(vicuna_lib::agent_shell_status(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
            )
            .await?))
        }
        "work_shell_input" => {
            Ok(serde_json::json!(vicuna_lib::agent_shell_input(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
                s("text")?,
            )
            .await?))
        }
        "work_shell_interrupt" => {
            Ok(serde_json::json!(vicuna_lib::agent_shell_interrupt(
                &st.shell,
                s("root")?,
                arg_opt_str(args, "key").unwrap_or_else(|| s("root").unwrap_or_default()),
            )
            .await?))
        }
        "work_exec_list" => {
            let l: Vec<BgJobSummary> = vicuna_lib::bg_list(&st.bg)?;
            Ok(serde_json::json!(l))
        }
        "work_exec_kill" => {
            let r: BgJobStatus = vicuna_lib::bg_kill(&st.bg, &s("id")?)?;
            Ok(serde_json::json!(r))
        }
        // ----- git panel -----
        "git_status" => Ok(serde_json::json!(vicuna_lib::git_panel::git_status_logic(s("root")?).await?)),
        "git_diff" => {
            let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            Ok(serde_json::json!(
                vicuna_lib::git_panel::git_diff_logic(s("root")?, s("path")?, b("staged"), b("untracked")).await?
            ))
        }
        "git_stage" => {
            vicuna_lib::git_panel::git_stage_logic(s("root")?, arg_str_list(args, "paths")).await?;
            Ok(serde_json::Value::Null)
        }
        "git_unstage" => {
            vicuna_lib::git_panel::git_unstage_logic(s("root")?, arg_str_list(args, "paths")).await?;
            Ok(serde_json::Value::Null)
        }
        "git_discard" => {
            vicuna_lib::git_panel::git_discard_logic(s("root")?, arg_str_list(args, "paths"), arg_str_list(args, "untracked"))
                .await?;
            Ok(serde_json::Value::Null)
        }
        // ----- folder picker (remote servers / browsers) -----
        "fs_list_dirs" => vicuna_lib::fs_list_dirs_logic(
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
            let ch: Channel<vicuna_lib::TermEvent> = channel_for(&st.events, &uuid);
            let id = vicuna_lib::term_spawn_logic(&st.terms, &s("root")?, cols, rows, ch)?;
            if let Ok(mut lp) = st.last_poll.lock() {
                lp.insert(uuid.clone(), std::time::Instant::now());
            }
            if let Ok(mut tc) = st.term_channels.lock() {
                tc.insert(id.clone(), uuid);
            }
            Ok(serde_json::json!(id))
        }
        "term_write" => {
            vicuna_lib::term_write_logic(&st.terms, &s("id")?, &s("data")?)?;
            Ok(serde_json::Value::Null)
        }
        "term_resize" => {
            let cols = args.get("cols").and_then(|v| v.as_u64()).unwrap_or(80) as u16;
            let rows = args.get("rows").and_then(|v| v.as_u64()).unwrap_or(24) as u16;
            vicuna_lib::term_resize_logic(&st.terms, &s("id")?, cols, rows)?;
            Ok(serde_json::Value::Null)
        }
        "term_kill" => {
            let id = s("id")?;
            if let Ok(mut tc) = st.term_channels.lock() {
                tc.remove(&id);
            }
            vicuna_lib::term_kill_logic(&st.terms, &id)?;
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
        })),
        "chats_load" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            vicuna_lib::server_store::load_chats(db).await
        }
        "chats_merge" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let chats = args.get("chats").cloned().unwrap_or(serde_json::Value::Array(vec![]));
            vicuna_lib::server_store::merge_json(db, &chats, arg_str_list(args, "deleted")).await?;
            Ok(serde_json::Value::Null)
        }
        // cross-host Cmd+P: another window searches this server's sessions
        "server_search_chats" => {
            let db = st.db.as_ref().ok_or("This bridge stores no chats (run `vicuna serve`).")?;
            let query = arg_opt_str(args, "query").unwrap_or_default();
            let limit = args.get("limit").and_then(|v| v.as_i64()).unwrap_or(10);
            vicuna_lib::server_store::search_chats(db, &query, &arg_str_list(args, "ids"), limit).await
        }
        "git_apply" => {
            let b = |k: &str| args.get(k).and_then(|v| v.as_bool()).unwrap_or(false);
            vicuna_lib::git_panel::git_apply_logic(
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
            Ok(serde_json::json!(vicuna_lib::work_rename_logic(s("root")?, s("path")?, s("newName")?).await?))
        }
        "work_delete" => {
            vicuna_lib::work_delete_logic(s("root")?, s("path")?).await?;
            Ok(serde_json::Value::Null)
        }
        "work_undo" => {
            let sid = arg_opt_str(args, "sessionId")
                .or_else(|| arg_opt_str(args, "session_id"))
                .or_else(|| arg_opt_str(args, "id"))
                .ok_or_else(|| "missing arg sessionId".to_string())?;
            Ok(serde_json::json!(vicuna_lib::work_undo_logic(&st.undo, sid).await?))
        }
        "work_redo" => {
            let sid = arg_opt_str(args, "sessionId")
                .or_else(|| arg_opt_str(args, "session_id"))
                .or_else(|| arg_opt_str(args, "id"))
                .ok_or_else(|| "missing arg sessionId".to_string())?;
            Ok(serde_json::json!(vicuna_lib::work_redo_logic(&st.undo, sid).await?))
        }
        "work_import_file" => {
            Ok(serde_json::json!(
                vicuna_lib::work_import_file_logic(s("root")?, s("path")?, s("base64")?).await?
            ))
        }
        "upload_stage_file" => {
            Ok(serde_json::json!(
                vicuna_lib::upload_stage_file_logic(s("sessionId")?, s("fileName")?, s("base64")?).await?
            ))
        }
        "upload_read_file" => {
            Ok(serde_json::json!(
                vicuna_lib::upload_read_file_logic(s("sessionId")?, s("name")?).await?
            ))
        }
        // ----- MCP -----
        "mcp_list_servers" => Ok(serde_json::json!(vicuna_lib::mcp::list_file_servers()?)),
        "mcp_save_server" => {
            let cfg: vicuna_lib::mcp::McpServerConfig =
                serde_json::from_value(args.get("config").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad config: {e}"))?;
            Ok(serde_json::json!(vicuna_lib::mcp::save_file_server(&cfg)?))
        }
        "mcp_delete_server" => {
            Ok(serde_json::json!(vicuna_lib::mcp::delete_file_server(&s("id")?)?))
        }
        "mcp_connect" => {
            let cfg: vicuna_lib::mcp::McpServerConfig =
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
            let name = vicuna_lib::mcp::list_file_servers()?
                .into_iter()
                .find(|cf| cf.id == id)
                .map(|cf| cf.name)
                .unwrap_or(id.clone());
            vicuna_lib::mcp::delete_auth(&name);
            let _ = st.mcp.disconnect(&id).await;
            Ok(serde_json::Value::Null)
        }
        "mcp_login_present" => {
            let id = s("id")?;
            let name = vicuna_lib::mcp::list_file_servers()?
                .into_iter()
                .find(|cf| cf.id == id)
                .map(|cf| cf.name)
                .unwrap_or(id);
            Ok(serde_json::json!(vicuna_lib::mcp::auth_present(&name)))
        }
        "mcp_signin" => Err("browser sign-in needs the Vicuna desktop app".to_string()),
        // ----- in-app updates -----
        "app_build_info" => Ok(vicuna_lib::app_build_info_logic().await?),
        "github_latest_release" => Ok(vicuna_lib::github_latest_release_logic().await?),
        "github_download_asset" => {
            Ok(serde_json::json!(vicuna_lib::github_download_asset_logic(s("url")?, s("fileName")?).await?))
        }
        "bridge_open_path" => {
            vicuna_lib::open_installer(&s("path")?)?;
            Ok(serde_json::Value::Null)
        }
        // ----- registry fetch (no CORS in Rust) -----
        "browser_status" => {
            Ok(serde_json::json!(
                vicuna_lib::browser::browser_status_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_start" => {
            Ok(serde_json::json!(
                vicuna_lib::browser::browser_start_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_stop" => Ok(serde_json::json!(vicuna_lib::browser::browser_stop_logic(&st.browser).await?)),
        "browser_restart" => {
            Ok(serde_json::json!(
                vicuna_lib::browser::browser_restart_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_ensure_engine" => {
            Ok(serde_json::json!(
                vicuna_lib::browser::browser_ensure_engine_logic(test_resource_dir()).await?
            ))
        }
        "browser_takeover" => {
            Ok(serde_json::json!(
                vicuna_lib::browser::browser_takeover_logic(test_resource_dir(), &st.browser).await?
            ))
        }
        "browser_mcp_command" => {
            Ok(serde_json::json!(vicuna_lib::browser::browser_mcp_command(test_resource_dir().as_deref())?))
        }
        "ollama_fetch_text" => {
            Ok(serde_json::json!(vicuna_lib::ollama_fetch_text_logic(s("url")?).await?))
        }
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
    let port = vicuna_lib::browser::lb_port();
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

async fn handle(st: Arc<BridgeState>, req: hyper::Request<hyper::body::Incoming>) -> hyper::Response<BoxBody> {
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
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        let mut events = drain_events(&st.events, &id);
        while events.is_empty() && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(15)).await;
            events = drain_events(&st.events, &id);
        }
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

fn server_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).unwrap_or_else(|_| ".".into());
    std::path::PathBuf::from(home).join("vicuna").join("server")
}

/// `VICUNA_SERVER_TOKEN`, else `~/vicuna/server/token` (created 0600 with a
/// fresh 128-bit token on first run).
fn server_token() -> String {
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

pub struct Config {
    pub bind: String,
    pub port: u16,
    pub token: Option<String>,
    pub db: Option<std::path::PathBuf>,
    /// `--tailscale [--https-port N]`: expose over `tailscale serve` (HTTPS,
    /// tailnet only) after binding, and print the URL to paste in the app
    pub tailscale: bool,
    pub https_port: Option<u16>,
}

/// Serve forever on <bind>:<port>. Called from `main` below.
pub fn run(cfg: Config) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("bridge runtime");
    let mut state = BridgeState::default();
    if let Some(path) = &cfg.db {
        let pool = rt.block_on(vicuna_lib::server_store::open(path)).expect("open server db");
        vicuna_lib::vicuna_mcp::set_db_pool_override(pool.clone());
        state.db = Some(pool);
    }
    state.token = cfg.token.clone();
    let state = Arc::new(state);
    // Terminals whose tab stopped polling (window closed, laptop asleep,
    // client gone) would keep their shell alive forever on a server: reap
    // any whose channel hasn't been polled for 90 s.
    {
        let st = state.clone();
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
                let _ = vicuna_lib::term_kill_logic(&st.terms, &term);
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
        });
    }
    let addr = format!("{}:{}", cfg.bind, cfg.port);
    let listener = rt
        .block_on(tokio::net::TcpListener::bind(&addr))
        .unwrap_or_else(|e| panic!("bridge bind {addr}: {e}"));
    match (&cfg.token, &cfg.db) {
        (Some(t), Some(db)) => {
            println!("vicuna server {} on http://{addr}", env!("CARGO_PKG_VERSION"));
            println!("  chats: {}", db.display());
            println!("  token: {t}");
            if cfg.tailscale {
                match cli::tailscale_expose(cfg.port, cfg.https_port) {
                    Ok(url) => {
                        println!("  tailnet URL: {url}");
                        println!("In Vicuna: sidebar server menu → Connect to new server… → {url} + the token above.");
                    }
                    Err(e) => println!("  tailscale: {e}"),
                }
            } else {
                println!("Expose it to your tailnet (HTTPS) with `vicuna serve --tailscale`, or by hand:");
                println!("  tailscale serve --bg --https=8443 http://127.0.0.1:{}", cfg.port);
            }
            if cfg.bind != "127.0.0.1" && cfg.bind != "localhost" {
                println!("WARNING: listening on {} over plain HTTP; prefer --bind 127.0.0.1 behind tailscale serve.", cfg.bind);
            }
        }
        _ => println!("vicuna bridge on http://{addr}"),
    }
    rt.block_on(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                continue;
            };
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
    });
}

const USAGE: &str = "vicuna — Vicuna's command line

usage:
  vicuna serve [--port N] [--bind ADDR] [--db PATH] [--tailscale [--https-port N]]
        Run a Vicuna server for the desktop app (token auth; chats in ~/vicuna/server).
        --tailscale also exposes it over `tailscale serve` (HTTPS, tailnet only)
        and prints the URL to paste into the app. Default 127.0.0.1:7421.
  vicuna install-service [serve options]   run `vicuna serve` in the background
        on login/boot (systemd user unit on Linux, launchd agent on macOS)
  vicuna uninstall-service
  vicuna token                             print the server token
  vicuna open [path]                       open a folder in the Vicuna desktop app
  vicuna [path]                            same as `vicuna open [path]`
  vicuna bridge [port]                     dev bridge for browser testing (127.0.0.1, no auth, 1421)
  vicuna --version";

fn die(msg: &str) -> ! {
    eprintln!("{msg}\n\n{USAGE}");
    std::process::exit(2)
}

/// Parse `serve` options (shared by `serve` and `install-service`).
fn parse_serve(args: &[String]) -> Config {
    let mut cfg = Config { bind: "127.0.0.1".into(), port: 7421, token: None, db: None, tailscale: false, https_port: None };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || it.next().cloned().unwrap_or_else(|| die(&format!("{a} needs a value")));
        match a.as_str() {
            "--port" => cfg.port = val().parse().unwrap_or_else(|_| die("bad --port")),
            "--bind" => cfg.bind = val(),
            "--db" => cfg.db = Some(std::path::PathBuf::from(val())),
            "--tailscale" => cfg.tailscale = true,
            "--https-port" => cfg.https_port = Some(val().parse().unwrap_or_else(|_| die("bad --https-port"))),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0)
            }
            other => die(&format!("unknown option {other}")),
        }
    }
    cfg
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = args.get(1..).unwrap_or(&[]).to_vec();
    match args.first().map(String::as_str) {
        Some("serve") => {
            let mut cfg = parse_serve(&rest);
            cfg.token = Some(server_token());
            if cfg.db.is_none() {
                cfg.db = Some(server_dir().join("vicuna.db"));
            }
            run(cfg);
        }
        Some("install-service") => {
            let _ = parse_serve(&rest); // validate before writing anything
            match cli::install_service(&rest) {
                Ok(m) => {
                    println!("{m}");
                    println!("Token: {}", server_token());
                }
                Err(e) => {
                    eprintln!("install-service failed: {e}");
                    std::process::exit(1)
                }
            }
        }
        Some("uninstall-service") => match cli::uninstall_service() {
            Ok(m) => println!("{m}"),
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1)
            }
        },
        Some("token") => println!("{}", server_token()),
        Some("open") => match cli::open_in_app(rest.first().map(String::as_str)) {
            Ok(p) => println!("Opened {p} in Vicuna."),
            Err(e) => {
                eprintln!("vicuna open: {e}");
                std::process::exit(1)
            }
        },
        Some("bridge") => {
            let port = rest.first().map(|p| p.parse().unwrap_or_else(|_| die("bad port"))).unwrap_or(1421);
            run(Config { bind: "127.0.0.1".into(), port, token: None, db: None, tailscale: false, https_port: None });
        }
        Some("-h") | Some("--help") | Some("help") => println!("{USAGE}"),
        Some("-V") | Some("--version") | Some("version") => println!("vicuna {}", env!("CARGO_PKG_VERSION")),
        // `cargo run -- <port>` (the old bridge invocation) keeps working
        Some(p) if p.parse::<u16>().is_ok() => {
            run(Config { bind: "127.0.0.1".into(), port: p.parse().unwrap(), token: None, db: None, tailscale: false, https_port: None })
        }
        // `vicuna [path]` — the old desktop launcher's behavior
        Some(p) if !p.starts_with('-') => match cli::open_in_app(Some(p)) {
            Ok(p) => println!("Opened {p} in Vicuna."),
            Err(e) => {
                eprintln!("vicuna: {e}");
                std::process::exit(1)
            }
        },
        Some(other) => die(&format!("unknown option {other}")),
        // no args: the dev bridge, as before (docs/scripts run `cargo run` here)
        None => run(Config { bind: "127.0.0.1".into(), port: 1421, token: None, db: None, tailscale: false, https_port: None }),
    }
}
