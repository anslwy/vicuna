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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeResponseBody};

use vicuna_lib::mcp::McpManager;
use vicuna_lib::work_agent::{
    agent_answer, agent_queue_msg, agent_remove_msg, agent_stop, run_loop, ChatSnapshot,
    McpRoute, TurnState, WorkAgentState, WorkAgentEvent,
};
use vicuna_lib::{BgExecState, BgJobStatus, BgJobSummary, ChatMessage};

const EVENT_CAP: usize = 2000;
const BODY_CAP: u64 = 32 * 1024 * 1024;

type EventQueues = Arc<Mutex<HashMap<String, VecDeque<serde_json::Value>>>>;

pub struct BridgeState {
    pub bg: Arc<BgExecState>,
    pub mcp: McpManager,
    pub agent: WorkAgentState,
    pub events: EventQueues,
}

impl Default for BridgeState {
    fn default() -> Self {
        Self {
            bg: Arc::new(BgExecState::default()),
            mcp: McpManager::default(),
            agent: WorkAgentState::default(),
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

fn channel_for(queues: &EventQueues, uuid: &str) -> Channel<WorkAgentEvent> {
    // Real Tauri Channel wired into the poll queue: run_loop is unchanged,
    // its Token/Think/Tool/Done sends land here as JSON for GET /poll/<id>.
    let queues = queues.clone();
    let id = uuid.to_string();
    Channel::<WorkAgentEvent>::new(move |body| {
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
            let on_event = channel_for(&st.events, &uuid);
            let turn = Arc::new(TurnState::new());
            let turn_id = st.agent.next_turn(turn.clone());
            let bg = st.bg.clone();
            let mcp = st.mcp.clone();
            let tid = turn_id.clone();
            let model = s("model")?;
            let workspace = s("workspace")?;
            let messages: Vec<ChatMessage> =
                serde_json::from_value(args.get("messages").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad messages: {e}"))?;
            let think = args.get("think").cloned();
            let tools = args.get("tools").cloned();
            let session_id = arg_opt_str(args, "sessionId");
            let goal_text = arg_opt_str(args, "goalText");
            let goal_paused = args.get("goalPaused").and_then(|v| v.as_bool()).unwrap_or(true);
            let chats: Vec<ChatSnapshot> =
                serde_json::from_value(args.get("chats").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad chats: {e}"))?;
            let mcp_routes: HashMap<String, McpRoute> =
                serde_json::from_value(args.get("mcpRoutes").cloned().unwrap_or_default())
                    .map_err(|e| format!("bad mcpRoutes: {e}"))?;
            tokio::spawn(async move {
                run_loop(
                    turn, model, workspace, messages, think, tools, session_id,
                    goal_text, goal_paused, chats, mcp_routes, on_event, &bg, &mcp, &tid,
                )
                .await;
            });
            Ok(serde_json::json!(turn_id))
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
            Ok(serde_json::json!(
                vicuna_lib::work_read_file_logic(s("root")?, s("path")?, max).await?
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
        "work_exec_list" => {
            let l: Vec<BgJobSummary> = vicuna_lib::bg_list(&st.bg)?;
            Ok(serde_json::json!(l))
        }
        "work_exec_kill" => {
            let r: BgJobStatus = vicuna_lib::bg_kill(&st.bg, &s("id")?)?;
            Ok(serde_json::json!(r))
        }
        "work_rename" => {
            Ok(serde_json::json!(vicuna_lib::work_rename_logic(s("root")?, s("path")?, s("newName")?).await?))
        }
        "work_delete" => {
            vicuna_lib::work_delete_logic(s("root")?, s("path")?).await?;
            Ok(serde_json::Value::Null)
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
        // ----- registry fetch (no CORS in Rust) -----
        "ollama_fetch_text" => {
            Ok(serde_json::json!(vicuna_lib::ollama_fetch_text_logic(s("url")?).await?))
        }
        _ => Err(format!("unknown bridge command {cmd}")),
    }
}

fn json_resp(body: String) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    use tiny_http::Header;
    let h = |n: &[u8], v: &[u8]| Header::from_bytes(n, v).unwrap();
    tiny_http::Response::from_string(body)
        .with_header(h(b"Access-Control-Allow-Origin", b"*"))
        .with_header(h(b"Access-Control-Allow-Methods", b"GET, POST, OPTIONS"))
        .with_header(h(b"Access-Control-Allow-Headers", b"Content-Type"))
        .with_header(h(b"Content-Type", b"application/json"))
}

fn handle(rt: &tokio::runtime::Handle, st: &Arc<BridgeState>, mut req: tiny_http::Request) {
    use tiny_http::Method::{Get, Options, Post};
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if *req.method() == Options {
        let _ = req.respond(json_resp(String::new()));
        return;
    }
    if *req.method() == Get && path == "/health" {
        let _ = req.respond(json_resp(r#"{"ok":true,"bridge":1}"#.to_string()));
        return;
    }
    if *req.method() == Get && path.starts_with("/poll/") {
        let id = path.trim_start_matches("/poll/").to_string();
        let events = drain_events(&st.events, &id);
        let body = serde_json::json!({"events": events}).to_string();
        let _ = req.respond(json_resp(body));
        return;
    }
    if *req.method() == Post && path == "/invoke" {
        let mut body: Vec<u8> = Vec::new();
        let mut ok = true;
        {
            let reader = req.as_reader();
            let mut buf = [0u8; 8192];
            loop {
                match std::io::Read::read(reader, &mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if body.len() + n > BODY_CAP as usize {
                            ok = false;
                            break;
                        }
                        body.extend_from_slice(&buf[..n]);
                    }
                    Err(_) => {
                        ok = false;
                        break;
                    }
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
                    match rt.block_on(dispatch(st, cmd, &args)) {
                        Ok(data) => serde_json::json!({"ok": true, "data": data}).to_string(),
                        Err(e) => serde_json::json!({"ok": false, "error": e}).to_string(),
                    }
                }
            }
        };
        let _ = req.respond(json_resp(out));
        return;
    }
    let _ = req.respond(json_resp(r#"{"ok":false,"error":"not found"}"#.to_string()));
}

/// Serve forever on 127.0.0.1:<port>. Called from `main` below.
pub fn run(port: u16) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("bridge runtime");
    let state = Arc::new(BridgeState::default());
    let server =
        tiny_http::Server::http(format!("127.0.0.1:{port}")).expect("bridge bind 127.0.0.1");
    println!("vicuna bridge on http://127.0.0.1:{port}");
    for req in server.incoming_requests() {
        let st = state.clone();
        let h = rt.handle().clone();
        std::thread::spawn(move || handle(&h, &st, req));
    }
}

fn main() {
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(1421);
    run(port);
}
