// Vicuna Work mode = the user's agent CLI (opencode, or Claude Code through
// its ACP adapter), driven over the Agent Client Protocol (ACP).
//
// One long-lived ACP process per engine (`opencode acp`, or
// `claude-agent-acp` pointed at the user's `claude`; spawned on demand,
// respawned if it dies or the binary moves) serves every Work session: `initialize` once, then per session `session/new` (cwd +
// Vicuna's MCP servers) or `session/resume`/`session/load` for a saved
// session, `session/set_config_option` for the model picked in Vicuna, and
// `session/prompt`. `session/update` notifications are routed by sessionId and
// translated into the WorkAgentEvent stream ChatApp already renders.
//
// No ACP SDK dependency: the wire format is newline-delimited JSON-RPC 2.0.
// Client-side fs/terminal capabilities are advertised as unsupported because
// opencode runs its own tools; permission requests are auto-approved
// (allow_once -> allow_always -> first allow) and shown as a note.
// There is no fallback engine: without opencode, Work mode is disabled.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use super::opencode::{find_opencode, OpencodeModel, INSTALL_URL};
use super::work_engine::Engine;
use super::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use super::ChatMessage;

const PROTOCOL_VERSION: u64 = 1;
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const CONFIG_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 32 * 1024;

pub fn not_found_message() -> String {
    format!("opencode CLI not found, install it: {INSTALL_URL}")
}

// ---------- state: warm process + session ids persisted across turns ----------

struct Proc {
    client: Arc<AcpClient>,
    /// The binary this process was started from (opencode, or the adapter).
    bin: PathBuf,
    // held so kill_on_drop ends the process when this Proc is replaced
    _child: Child,
}

#[derive(Default)]
pub struct AcpState {
    sessions: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
    procs: tokio::sync::Mutex<HashMap<Engine, Proc>>,
    /// Claude's model list, read from a session's `model` config option;
    /// cleared when the Claude process is respawned.
    claude_models: tokio::sync::Mutex<Option<Vec<OpencodeModel>>>,
}

impl AcpState {
    fn load(&self) -> Result<(), String> {
        if self.loaded.swap(true, Ordering::Relaxed) {
            return Ok(());
        }
        let path = sessions_path()?;
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Ok(());
        };
        if let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&text) {
            if let Ok(mut sessions) = self.sessions.lock() {
                *sessions = map;
            }
        }
        Ok(())
    }

    fn get(&self, key: &str) -> Option<String> {
        self.load().ok()?;
        self.sessions.lock().ok()?.get(key).cloned()
    }

    fn set(&self, key: String, acp_session: String) {
        let _ = self.load();
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(key, acp_session);
            if let Ok(path) = sessions_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let doc = serde_json::to_string_pretty(&*sessions).unwrap_or_default();
                let _ = std::fs::write(path, doc);
            }
        }
    }

    /// The warm, initialized ACP client for `engine`; (re)spawned when there
    /// is none, it died, or detection now resolves a different binary.
    async fn client(&self, engine: Engine) -> Result<Arc<AcpClient>, String> {
        let (bin, cmd) = match engine {
            Engine::Opencode => {
                let bin = find_opencode().ok_or_else(not_found_message)?;
                let mut cmd = super::opencode::command(&bin);
                cmd.arg("acp");
                (bin, cmd)
            }
            Engine::Claude => {
                let (cli, adapter) = super::claude::find_parts()?;
                let cmd = super::claude::command(&adapter, &cli);
                (adapter, cmd)
            }
        };
        let mut guard = self.procs.lock().await;
        if let Some(p) = guard.get(&engine) {
            if !p.client.closed.load(Ordering::Relaxed) && p.bin == bin {
                return Ok(p.client.clone());
            }
        }
        guard.remove(&engine);
        if engine == Engine::Claude {
            *self.claude_models.lock().await = None;
        }
        let label = engine_process_label(engine);
        let (client, child) = spawn_acp(cmd, label, &bin)?;
        client
            .call(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "clientCapabilities": {
                        "fs": { "readTextFile": false, "writeTextFile": false },
                        "terminal": false
                    },
                    "clientInfo": { "name": "vicuna", "title": "Vicuna", "version": env!("CARGO_PKG_VERSION") }
                }),
                INIT_TIMEOUT,
            )
            .await
            .map_err(|e| format!("{label} initialize failed: {e}"))?;
        guard.insert(
            engine,
            Proc {
                client: client.clone(),
                bin,
                _child: child,
            },
        );
        Ok(client)
    }

    /// Start the current engine's warm process ahead of the first Work turn
    /// (app startup). Silent when it is missing; the UI shows that state.
    pub async fn prewarm(&self) {
        let engine = super::work_engine::current_engine();
        if engine.is_present() {
            let _ = self.client(engine).await;
        }
    }

    /// Claude's selectable models, detected (not hardcoded): the adapter
    /// lists them as the `model` config option of a session, so open a
    /// throwaway session (no prompt, nothing billed) and read it.
    pub async fn claude_models(&self) -> Result<Vec<OpencodeModel>, String> {
        let client = self.client(Engine::Claude).await?;
        if let Some(cached) = self.claude_models.lock().await.clone() {
            return Ok(cached);
        }
        let cwd = super::opencode::home_dir()
            .map(|h| h.display().to_string())
            .unwrap_or_else(|| ".".to_string());
        let r = client
            .call("session/new", session_params(&cwd, None, &[]), SESSION_TIMEOUT)
            .await
            .map_err(|e| format!("Claude Code could not list models: {e}"))?;
        if let Some(sid) = r.get("sessionId").and_then(|s| s.as_str()) {
            // best effort: the adapter supports session/close
            let _ = client.call("session/close", json!({ "sessionId": sid }), CONFIG_TIMEOUT).await;
        }
        let models = config_models(&r);
        if models.is_empty() {
            return Err("Claude Code reported no models (is `claude` logged in? run `claude` once and /login)".into());
        }
        *self.claude_models.lock().await = Some(models.clone());
        Ok(models)
    }
}

fn engine_process_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Opencode => "opencode acp",
        Engine::Claude => "claude-agent-acp",
    }
}

/// Model rows from a session's `model` config option (Claude adapter), minus
/// the "default" pseudo-entry (the picker has its own default row).
fn config_models(result: &Value) -> Vec<OpencodeModel> {
    let Some(opt) = result
        .get("configOptions")
        .and_then(|o| o.as_array())
        .and_then(|a| a.iter().find(|o| o.get("id").and_then(|i| i.as_str()) == Some("model")))
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut push = |v: &Value| {
        let Some(id) = v.get("value").and_then(|x| x.as_str()).filter(|x| !x.is_empty() && *x != "default") else {
            return;
        };
        if out.iter().any(|m: &OpencodeModel| m.id == id) {
            return;
        }
        out.push(OpencodeModel {
            id: id.to_string(),
            provider: "claude".to_string(),
            name: v.get("name").and_then(|x| x.as_str()).map(str::to_string),
        });
    };
    for o in opt.get("options").and_then(|o| o.as_array()).into_iter().flatten() {
        // grouped options carry their own `options` list
        if let Some(group) = o.get("options").and_then(|g| g.as_array()) {
            group.iter().for_each(&mut push);
        } else {
            push(o);
        }
    }
    out
}

fn home_dir() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").map_err(|_| "cannot locate home folder".to_string())?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").map_err(|_| "cannot locate home folder".to_string())?;
    Ok(PathBuf::from(home))
}

fn sessions_path() -> Result<PathBuf, String> {
    Ok(home_dir()?
        .join("vicuna")
        .join("config")
        .join("opencode-sessions.json"))
}

/// Keyed by chat + workspace only: the model is switched inside the session
/// (set_config_option), so changing it keeps the conversation.
fn session_key(engine: Engine, session_id: Option<&str>, workspace: &str) -> Option<String> {
    // opencode keeps the original key shape so sessions saved before Claude
    // existed still resume
    session_id.map(|id| match engine {
        Engine::Opencode => format!("{id}|{workspace}"),
        Engine::Claude => format!("claude|{id}|{workspace}"),
    })
}

// ---------- session parameters + MCP servers handed to opencode ----------

/// Providers reject requests with more than this many tools (OpenAI-style
/// "Invalid value for 'tools': maximum number of items is 128").
pub const PROVIDER_TOOL_LIMIT: usize = 128;
/// Headroom kept for opencode's own built-in tools (~20 today) plus slack.
pub const OPENCODE_TOOL_RESERVE: usize = 32;
/// The MCP dialog's id for the synthetic built-in browser row.
pub const BROWSER_MCP_ID: &str = "browser";

/// What Vicuna's MCP dialog currently shows, captured when a turn starts:
/// connected server id -> advertised tool count, plus the absolute `lb mcp`
/// argv for the built-in browser row (when `lb` resolves).
#[derive(Debug, Clone, Default)]
pub struct McpSnapshot {
    pub tool_counts: HashMap<String, usize>,
    pub browser_command: Option<Vec<String>>,
    pub browser_server_url: String,
}

impl McpSnapshot {
    pub async fn capture(mcp: &crate::mcp::McpManager, resource_dir: Option<&Path>) -> Self {
        let tool_counts = mcp.tool_counts().await;
        let browser_command = if tool_counts.contains_key(BROWSER_MCP_ID) {
            crate::browser::browser_mcp_command(resource_dir).ok()
        } else {
            None
        };
        Self {
            tool_counts,
            browser_command,
            browser_server_url: crate::browser::lb_server_url(),
        }
    }
}

/// One MCP server Vicuna would hand to opencode.
#[derive(Debug, Clone)]
struct PlannedServer {
    name: String,
    entry: Value,
    /// Known tool count (server connected in Vicuna), else unknown.
    tools: Option<usize>,
}

fn name_value_list(map: Option<HashMap<String, String>>) -> Vec<Value> {
    let mut v: Vec<(String, String)> = map.unwrap_or_default().into_iter().collect();
    v.sort();
    v.into_iter().map(|(k, val)| json!({ "name": k, "value": val })).collect()
}

/// ACP `McpServer` entry for a stdio server (absolute command when it
/// resolves on PATH: macOS GUI apps and opencode may not share the shell's).
fn stdio_entry(name: &str, command: &[String], env: Option<HashMap<String, String>>) -> Option<Value> {
    let first = command.first().filter(|c| !c.trim().is_empty())?;
    let exe = resolve_executable(first).unwrap_or_else(|| first.clone());
    Some(json!({
        "name": name,
        "command": exe,
        "args": command[1..],
        "env": name_value_list(env),
    }))
}

/// ACP `McpServer` entry for a remote (streamable HTTP) server. A stored
/// Vicuna OAuth login rides along as a Bearer header unless the user set
/// their own Authorization header.
fn http_entry(name: &str, url: &str, headers: Option<HashMap<String, String>>, bearer: Option<String>) -> Value {
    let mut headers = headers.unwrap_or_default();
    let has_auth = headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
    if let (false, Some(token)) = (has_auth, bearer) {
        headers.insert("Authorization".to_string(), format!("Bearer {token}"));
    }
    json!({ "type": "http", "name": name, "url": url, "headers": name_value_list(Some(headers)) })
}

/// Keep servers in order while the known tool total stays within `budget`.
/// Servers with an unknown count (not connected in Vicuna) are kept; the
/// provider-error hint covers the rare case they push past the limit.
fn fit_tool_budget(servers: Vec<PlannedServer>, budget: usize) -> (Vec<PlannedServer>, Vec<PlannedServer>) {
    let mut used = 0usize;
    let mut kept = Vec::new();
    let mut skipped = Vec::new();
    for s in servers {
        let n = s.tools.unwrap_or(0);
        if used + n <= budget {
            used += n;
            kept.push(s);
        } else {
            skipped.push(s);
        }
    }
    (kept, skipped)
}

fn skipped_note(skipped: &[PlannedServer]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let list: Vec<String> = skipped
        .iter()
        .map(|s| format!("{} ({} tools)", s.name, s.tools.unwrap_or(0)))
        .collect();
    Some(format!(
        "MCP servers not available this turn: {}. Together with opencode's own tools they would exceed the model's {}-tool limit; turn some servers off in the MCP dialog to make room.",
        list.join(", "),
        PROVIDER_TOOL_LIMIT
    ))
}

/// Vicuna's MCP dialog, as opencode `mcpServers`: every enabled vicuna.json
/// server plus the built-in browser row when it is connected, trimmed to
/// the tool budget. Returns the entries and an optional note for the user.
async fn mcp_servers_for_turn(snap: &McpSnapshot) -> (Vec<Value>, Option<String>) {
    let files = crate::mcp::list_file_servers().unwrap_or_default();
    let mut planned = Vec::new();
    for s in files.iter().filter(|s| s.enabled) {
        let tools = snap.tool_counts.get(&s.id).copied();
        let entry = if s.normalized_transport() == "stdio" {
            match s.command.as_deref().and_then(|c| stdio_entry(&s.name, c, s.env.clone())) {
                Some(e) => e,
                None => continue,
            }
        } else {
            let Some(url) = s.url.as_deref().filter(|u| !u.is_empty()) else { continue };
            http_entry(&s.name, url, s.headers.clone(), crate::mcp::bearer_token(&s.name).await)
        };
        planned.push(PlannedServer { name: s.name.clone(), entry, tools });
    }
    let file_has_browser = files.iter().any(|s| s.id == BROWSER_MCP_ID || s.name == BROWSER_MCP_ID);
    if !file_has_browser {
        if let Some(cmd) = &snap.browser_command {
            let env = HashMap::from([("LB_SERVER".to_string(), snap.browser_server_url.clone())]);
            if let Some(entry) = stdio_entry(BROWSER_MCP_ID, cmd, Some(env)) {
                planned.push(PlannedServer {
                    name: BROWSER_MCP_ID.to_string(),
                    entry,
                    tools: snap.tool_counts.get(BROWSER_MCP_ID).copied(),
                });
            }
        }
    }
    let (kept, skipped) = fit_tool_budget(planned, PROVIDER_TOOL_LIMIT - OPENCODE_TOOL_RESERVE);
    (kept.into_iter().map(|s| s.entry).collect(), skipped_note(&skipped))
}

fn resolve_executable(name: &str) -> Option<String> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(name.to_string());
    }
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// Params for session/new, resume and load.
fn session_params(workspace: &str, session_id: Option<&str>, mcp: &[Value]) -> Value {
    let mut v = json!({ "cwd": workspace, "mcpServers": mcp });
    if let Some(id) = session_id {
        v["sessionId"] = json!(id);
    }
    v
}

/// The model opencode reports as current in a configOptions-bearing result
/// (session/new, resume/load, set_config_option).
fn current_model(result: &Value) -> Option<String> {
    result
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| o.get("id").and_then(|i| i.as_str()) == Some("model"))?
        .get("currentValue")?
        .as_str()
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// Provider rejections caused by too many tools in the request (OpenAI-style
/// "maximum number of items is 128" on `tools`, and similar wordings).
fn is_tool_limit_error(e: &str) -> bool {
    let low = e.to_lowercase();
    low.contains("tools")
        && (low.contains("maximum number of items")
            || low.contains("too many tools")
            || low.contains("maximum of 128")
            || low.contains("at most 128"))
}

pub(crate) const TOOL_LIMIT_HINT: &str = "The model's provider rejected the request because it carried too many tools. \
Vicuna keeps the MCP servers it passes from its MCP dialog under the limit, so the rest come from MCP servers in your opencode config \
(see `opencode mcp list`; ~/.config/opencode/opencode.json, \"mcp\" section, or the workspace's opencode.json). \
Turn off some servers in Vicuna's MCP dialog or in your opencode config, or pick a different model.";

// ---------- JSON-RPC client ----------

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;
type Routes = Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>;

struct AcpClient {
    write_tx: mpsc::UnboundedSender<String>,
    pending: Pending,
    /// sessionId -> the running turn's update channel
    routes: Routes,
    next_id: AtomicU64,
    stderr_tail: Arc<Mutex<String>>,
    closed: Arc<AtomicBool>,
    label: &'static str,
}

/// Unregisters a session's update route when the turn ends.
struct RouteGuard {
    routes: Routes,
    session: String,
}

impl Drop for RouteGuard {
    fn drop(&mut self) {
        if let Ok(mut r) = self.routes.lock() {
            r.remove(&self.session);
        }
    }
}

impl AcpClient {
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, String> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(self.dead_error());
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "acp: state poisoned".to_string())?
            .insert(id, tx);
        let line = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        })
        .to_string();
        self.write_tx.send(line).map_err(|_| self.dead_error())?;
        Ok(rx)
    }

    async fn call(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let rx = self.request(method, params)?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err("acp: response channel dropped".to_string()),
            Err(_) => Err(format!("acp: {method} timed out")),
        }
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        let line = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        })
        .to_string();
        self.write_tx.send(line).map_err(|_| self.dead_error())
    }

    fn route(&self, session: &str) -> (mpsc::UnboundedReceiver<Value>, RouteGuard) {
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut r) = self.routes.lock() {
            r.insert(session.to_string(), tx);
        }
        (
            rx,
            RouteGuard {
                routes: self.routes.clone(),
                session: session.to_string(),
            },
        )
    }

    fn dead_error(&self) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if tail.is_empty() {
            format!("{} exited unexpectedly", self.label)
        } else {
            format!("{} exited unexpectedly:\n{tail}", self.label)
        }
    }
}

fn spawn_acp(
    mut cmd: tokio::process::Command,
    label: &'static str,
    bin: &Path,
) -> Result<(Arc<AcpClient>, Child), String> {
    cmd.stdin(std::process::Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {label} ({}): {e}", bin.display()))?;
    let mut stdin = child.stdin.take().ok_or(format!("{label}: no stdin"))?;
    let stdout = child.stdout.take().ok_or(format!("{label}: no stdout"))?;
    let stderr = child.stderr.take().ok_or(format!("{label}: no stderr"))?;

    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = write_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err()
                || stdin.write_all(b"\n").await.is_err()
                || stdin.flush().await.is_err()
            {
                break;
            }
        }
    });

    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let closed = Arc::new(AtomicBool::new(false));

    let (pending_r, routes_r, write_r, closed_r) =
        (pending.clone(), routes.clone(), write_tx.clone(), closed.clone());
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let id = msg.get("id").cloned().filter(|v| !v.is_null());
            let method = msg.get("method").and_then(|m| m.as_str()).map(str::to_string);
            match (id, method) {
                (Some(id), Some(method)) => {
                    // Agent -> client request.
                    let params = msg.get("params").cloned().unwrap_or(Value::Null);
                    let reply = match handle_agent_request(&method, &params) {
                        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                        Err(error) => json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": { "code": -32601, "message": error }
                        }),
                    };
                    let _ = write_r.send(reply.to_string());
                    if method == "session/request_permission" {
                        if let Some(note) = permission_note(&params) {
                            forward(&routes_r, &params, note);
                        }
                    }
                }
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else { continue };
                    if let Some(tx) = pending_r.lock().ok().and_then(|mut m| m.remove(&id)) {
                        let result = match msg.get("error") {
                            Some(err) => Err(jsonrpc_error_text(err)),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(result);
                    }
                }
                (None, Some(method)) => {
                    if method == "session/update" {
                        let params = msg.get("params").cloned().unwrap_or(Value::Null);
                        forward(&routes_r, &params, msg);
                    }
                }
                (None, None) => {}
            }
        }
        // EOF: the process is gone. Unblock every waiter and every turn; the
        // next turn respawns.
        closed_r.store(true, Ordering::Relaxed);
        if let Ok(mut m) = pending_r.lock() {
            for (_, tx) in m.drain() {
                let _ = tx.send(Err(format!("{label} closed the connection")));
            }
        }
        if let Ok(mut r) = routes_r.lock() {
            r.clear();
        }
    });

    let tail = stderr_tail.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(mut s) = tail.lock() {
                s.push_str(&line);
                s.push('\n');
                if s.len() > 4000 {
                    let keep = s.len() - 4000;
                    let cut = s
                        .char_indices()
                        .find(|(i, _)| *i >= keep)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    s.drain(..cut);
                }
            }
        }
    });

    let client = Arc::new(AcpClient {
        write_tx,
        pending,
        routes,
        next_id: AtomicU64::new(0),
        stderr_tail,
        closed,
        label,
    });
    Ok((client, child))
}

/// Deliver a message to the turn that owns `params.sessionId` (dropped when no
/// turn is listening, e.g. history replayed by session/load).
fn forward(routes: &Routes, params: &Value, msg: Value) {
    let Some(sid) = params.get("sessionId").and_then(|s| s.as_str()) else {
        return;
    };
    if let Some(tx) = routes.lock().ok().and_then(|r| r.get(sid).cloned()) {
        let _ = tx.send(msg);
    }
}

fn jsonrpc_error_text(err: &Value) -> String {
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown error");
    let detail = err
        .get("data")
        .and_then(|d| d.get("message").or_else(|| d.get("details")))
        .and_then(|m| m.as_str())
        .filter(|d| !d.is_empty() && *d != message);
    let text = match detail {
        Some(d) => format!("{message}: {d}"),
        None => message.to_string(),
    };
    super::opencode::clean_error(&text)
}

/// Client-side fs/terminal capabilities are not advertised, so only
/// permission requests should arrive. Auto-approve the narrowest allow.
fn handle_agent_request(method: &str, params: &Value) -> Result<Value, String> {
    match method {
        "session/request_permission" => {
            let options = params
                .get("options")
                .and_then(|o| o.as_array())
                .cloned()
                .unwrap_or_default();
            let kind_of = |opt: &Value| opt.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
            let id_of = |opt: &Value| opt.get("optionId").and_then(|id| id.as_str()).map(str::to_string);
            let pick = |kind: &str| options.iter().find(|o| kind_of(o) == kind).and_then(id_of);
            let id = pick("allow_once")
                .or_else(|| pick("allow_always"))
                .or_else(|| options.iter().find(|o| kind_of(o).starts_with("allow")).and_then(id_of));
            match id {
                Some(option_id) => Ok(json!({
                    "outcome": { "outcome": "selected", "optionId": option_id }
                })),
                None => Ok(json!({ "outcome": { "outcome": "cancelled" } })),
            }
        }
        other => Err(format!("{other} is not supported by Vicuna")),
    }
}

/// A synthetic update so the transcript says what was auto-approved.
fn permission_note(params: &Value) -> Option<Value> {
    let title = params
        .pointer("/toolCall/title")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .trim();
    if title.is_empty() {
        return None;
    }
    Some(json!({
        "params": {
            "sessionId": params.get("sessionId").cloned().unwrap_or(Value::Null),
            "update": { "sessionUpdate": "vicuna_permission", "title": title }
        }
    }))
}

// ---------- update translation ----------

fn content_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().filter_map(content_text).collect();
            if parts.is_empty() {
                None
            } else {
                Some(parts.join(""))
            }
        }
        Value::Object(obj) => match obj.get("type").and_then(|t| t.as_str()) {
            Some("text") => obj
                .get("text")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string()),
            Some("content") => obj.get("content").and_then(content_text),
            Some("diff") => {
                let path = obj.get("path").and_then(|p| p.as_str()).unwrap_or("file");
                let old = obj.get("oldText").and_then(|t| t.as_str()).unwrap_or("");
                let new = obj.get("newText").and_then(|t| t.as_str()).unwrap_or("");
                Some(format!("--- {path}\n+++ {path}\n-old: {old}\n+new: {new}"))
            }
            _ => None,
        },
        _ => None,
    }
}

fn tool_output(update: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = update.get("content").and_then(content_text) {
        parts.push(content);
    }
    if let Some(raw) = update.get("rawOutput").and_then(content_text) {
        parts.push(raw);
    }
    if parts.is_empty() {
        if let Some(status) = update.get("status").and_then(|s| s.as_str()) {
            if status != "completed" {
                parts.push(format!("[tool {status}]"));
            }
        }
    }
    let mut out = parts.join("\n");
    if out.chars().count() > OUTPUT_CAP {
        out = out.chars().take(OUTPUT_CAP).collect::<String>();
        out.push_str("\n[output truncated]");
    }
    out
}

fn tool_name_and_args(kind_hint: &str, title: &str, raw: &Value) -> (String, Value) {
    let path = raw
        .get("path")
        .or_else(|| raw.get("file_path"))
        .or_else(|| raw.get("filePath"))
        .and_then(|p| p.as_str())
        .unwrap_or(title);
    let command = raw
        .get("command")
        .or_else(|| raw.get("cmd"))
        .and_then(|c| c.as_str());
    // opencode glob/search tools can arrive as kind "read" with a pattern in
    // rawInput; show them as a search instead of "read — <title>".
    let pattern = raw
        .get("pattern")
        .or_else(|| raw.get("glob"))
        .or_else(|| raw.get("query"))
        .and_then(|v| v.as_str());
    if let Some(pattern) = pattern {
        if kind_hint != "edit" {
            return (
                "grep".to_string(),
                json!({ "pattern": pattern, "path": path }),
            );
        }
    }
    let args = match kind_hint {
        "execute" | "shell" => json!({ "command": command.unwrap_or(title) }),
        "read" => json!({ "path": path }),
        "edit" => json!({ "path": path }),
        "search" => json!({ "pattern": title }),
        "fetch" => json!({ "url": raw.get("url").and_then(|v| v.as_str()).unwrap_or(title) }),
        _ => raw.clone(),
    };
    let name = match kind_hint {
        "execute" | "shell" => "exec",
        "read" => "read",
        "edit" => "write",
        "search" => "grep",
        "fetch" => "fetch_url",
        _ => title,
    };
    (name.to_string(), args)
}

struct ToolState {
    name: String,
    ended: bool,
}

fn handle_session_update(
    msg: &Value,
    on_event: &Channel<WorkAgentEvent>,
    stop: &AtomicBool,
    tools: &mut HashMap<String, ToolState>,
    output_chars: &mut usize,
) {
    let Some(update) = msg.get("params").and_then(|p| p.get("update")) else {
        return;
    };
    let kind = update
        .get("sessionUpdate")
        .and_then(|s| s.as_str())
        .unwrap_or("");
    match kind {
        "agent_message_chunk" => {
            if let Some(text) = update.get("content").and_then(content_text) {
                *output_chars += text.chars().count();
                emit_ev(on_event, stop, WorkAgentEvent::Token { text });
            }
        }
        "agent_thought_chunk" => {
            if let Some(text) = update.get("content").and_then(content_text) {
                emit_ev(on_event, stop, WorkAgentEvent::Think { text });
            }
        }
        "tool_call" | "tool_call_update" => {
            let id = update
                .get("toolCallId")
                .and_then(|i| i.as_str())
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                return;
            }
            let status = update
                .get("status")
                .and_then(|s| s.as_str())
                .unwrap_or("pending");
            let title = update
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("tool");
            let kind_hint = update
                .get("kind")
                .and_then(|k| k.as_str())
                .unwrap_or("other");
            if !tools.contains_key(&id) {
                let raw = update.get("rawInput").cloned().unwrap_or_else(|| json!({}));
                let (name, args) = tool_name_and_args(kind_hint, title, &raw);
                tools.insert(
                    id.clone(),
                    ToolState {
                        name: name.clone(),
                        ended: false,
                    },
                );
                emit_ev(on_event, stop, WorkAgentEvent::ToolStart { name, args });
            }
            if matches!(status, "completed" | "failed" | "cancelled") {
                let entry = tools.get_mut(&id).expect("inserted above");
                if !entry.ended {
                    entry.ended = true;
                    let mut output = tool_output(update);
                    if status == "failed" && !output.starts_with("Error:") {
                        output = format!("Error: {output}");
                    }
                    if output.trim().is_empty() {
                        output = format!("[{status}]");
                    }
                    let name = entry.name.clone();
                    emit_ev(on_event, stop, WorkAgentEvent::ToolEnd { name, output });
                }
            }
        }
        "vicuna_permission" => {
            if let Some(title) = update.get("title").and_then(|t| t.as_str()) {
                emit_ev(
                    on_event,
                    stop,
                    WorkAgentEvent::DisplayNote {
                        text: format!("\n> Auto-approved: {title}\n"),
                    },
                );
            }
        }
        _ => {}
    }
}

fn image_blocks(images: &Option<Vec<String>>) -> Vec<Value> {
    images
        .as_ref()
        .map(|list| {
            list.iter()
                .filter_map(|url| {
                    let (meta, data) = url.split_once(',')?;
                    let mime = meta.strip_prefix("data:")?.split(';').next()?;
                    Some(json!({
                        "type": "image",
                        "data": data,
                        "mimeType": mime
                    }))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn prompt_blocks(text: &str, images: &Option<Vec<String>>) -> Vec<Value> {
    let mut blocks = vec![json!({ "type": "text", "text": text })];
    blocks.extend(image_blocks(images));
    blocks
}

fn last_user_message(messages: &[ChatMessage]) -> Option<&ChatMessage> {
    messages.iter().rev().find(|m| m.role == "user")
}

/// On a brand-new opencode session with existing Vicuna history (fork, app
/// restart), hand opencode the visible transcript so the work continues in context.
fn history_preamble(messages: &[ChatMessage], last_user: &ChatMessage) -> String {
    let mut lines: Vec<String> = Vec::new();
    for m in messages {
        if m.role == "system" || std::ptr::eq(m, last_user) {
            continue;
        }
        let text = m.content.trim();
        if text.is_empty() {
            continue;
        }
        let role = match m.role.as_str() {
            "assistant" => "Assistant",
            "user" => "User",
            _ => continue,
        };
        lines.push(format!("{role}: {text}"));
    }
    if lines.is_empty() {
        return String::new();
    }
    let mut joined = lines.join("\n\n");
    if joined.chars().count() > 40_000 {
        joined = joined
            .chars()
            .skip(joined.chars().count() - 40_000)
            .collect::<String>();
        joined.insert_str(0, "[earlier history omitted]\n");
    }
    format!("Conversation so far (for context):\n{joined}\n\n---\n")
}

fn system_prompt(workspace: &str, goal: Option<&str>, session_id: Option<&str>) -> String {
    let uploads = session_id
        .map(|id| format!("Files the user attached to this session live in ~/vicuna/uploads/{id}/ — read them directly when the prompt mentions attachments."))
        .unwrap_or_default();
    format!(
        "You are Vicuna Work mode, operating inside this workspace: {workspace}\n\
\n\
Use your own built-in tools to inspect and change files and run commands. The workspace is your boundary:\n\
- Do not read, list, search, write, or delete outside it unless the user explicitly asks; ask first with a clear reason. Scratch files belong in the system temp dir, never in the repo.\n\
- Never crawl from / or the home folder (`find /`, `find ~`, `grep -r /`): on macOS that triggers a consent dialog for every protected folder. Use targeted paths.\n\
- Keep the user oriented: write one short line before a batch of tool calls saying what you are doing and why, then after the work summarize what changed with paths and commands that matter.\n\
- Read before overwriting a file. Prefer targeted reads/searches over dumping large files.\n\
{uploads}\n{goal}",
        goal = goal
            .filter(|g| !g.trim().is_empty())
            .map(|g| format!(
                "\nSESSION GOAL: {g}\nKeep working until the goal is fully achieved; do not stop at a partial result.\n"
            ))
            .unwrap_or_default()
    )
}

fn take_queue(turn: &TurnState) -> Vec<(String, ChatMessage)> {
    turn.queue
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Concatenate queued messages for a follow-up prompt. Images travel with it.
fn messages_text(messages: &[ChatMessage]) -> (String, Option<Vec<String>>) {
    let mut text = String::new();
    let mut images: Option<Vec<String>> = None;
    for m in messages {
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&m.content);
        if let Some(imgs) = &m.images {
            images.get_or_insert_with(Vec::new).extend(imgs.clone());
        }
    }
    (text, images)
}


// ---------- public entry point ----------

/// Run one Work turn through the warm `opencode acp` process. Internal
/// failures are surfaced as an Error event so the frontend always settles.
#[allow(clippy::too_many_arguments)]
pub async fn run_turn(
    turn: Arc<TurnState>,
    model: Option<String>,
    workspace: String,
    messages: Vec<ChatMessage>,
    session_id: Option<String>,
    goal_text: Option<String>,
    fresh_session: bool,
    on_event: Channel<WorkAgentEvent>,
    state: &AcpState,
    mcp_snapshot: McpSnapshot,
    engine: Engine,
) {
    if let Err(message) = run_turn_inner(
        engine,
        &turn,
        model.as_deref(),
        &workspace,
        &messages,
        session_id.as_deref(),
        goal_text.as_deref(),
        fresh_session,
        &on_event,
        state,
        &mcp_snapshot,
    )
    .await
    {
        let _ = on_event.send(WorkAgentEvent::Error { message });
    }
}

/// Resume the chat's saved opencode session (resume, then load), else start
/// a new one. Returns (sessionId, resumed).
async fn open_session(
    engine: Engine,
    client: &AcpClient,
    saved: Option<String>,
    workspace: &str,
    mcp: &[Value],
) -> Result<(String, bool, Option<String>), String> {
    if let Some(prev) = saved {
        for method in ["session/resume", "session/load"] {
            if let Ok(r) = client
                .call(method, session_params(workspace, Some(&prev), mcp), SESSION_TIMEOUT)
                .await
            {
                let model = current_model(&r);
                return Ok((prev, true, model));
            }
        }
        // Saved session disappeared (data purge / version change): start
        // clean; the history preamble keeps the context.
    }
    let result = client
        .call("session/new", session_params(workspace, None, mcp), SESSION_TIMEOUT)
        .await
        .map_err(|e| format!("{} could not start a session: {e}", engine.label()))?;
    let sid = result
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{}: session/new returned no sessionId", engine_process_label(engine)))?;
    Ok((sid, false, current_model(&result)))
}

/// Model to run: the turn's explicit pick, else the one saved in Vicuna's
/// picker, else opencode's own default (None).
fn effective_model(engine: Engine, requested: Option<&str>) -> Option<String> {
    requested
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| engine.saved_model())
}

fn friendly_prompt_error(engine: Engine, e: String) -> String {
    let low = e.to_lowercase();
    if is_tool_limit_error(&e) {
        format!("{e}\n{TOOL_LIMIT_HINT}")
    } else if low.contains("auth") || low.contains("api key") || low.contains("unauthorized") || low.contains("401") || low.contains("login") {
        match engine {
            Engine::Opencode => format!("{e}\nThe selected provider needs credentials: run `opencode auth login` once, or pick another model."),
            Engine::Claude => format!("{e}\nClaude Code is not logged in: run `claude` in a terminal once and use /login."),
        }
    } else {
        e
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_turn_inner(
    engine: Engine,
    turn: &Arc<TurnState>,
    model: Option<&str>,
    workspace: &str,
    messages: &[ChatMessage],
    session_id: Option<&str>,
    goal_text: Option<&str>,
    fresh_session: bool,
    on_event: &Channel<WorkAgentEvent>,
    state: &AcpState,
    mcp_snapshot: &McpSnapshot,
) -> Result<(), String> {
    let stop = &turn.stop;
    let last_user = last_user_message(messages)
        .ok_or_else(|| "Work mode: no user message to send".to_string())?;
    let client = state.client(engine).await?;

    let key = session_key(engine, session_id, workspace);
    let saved = if fresh_session {
        None
    } else {
        key.as_deref().and_then(|k| state.get(k))
    };
    let (mcp, mcp_note) = mcp_servers_for_turn(mcp_snapshot).await;
    if let Some(note) = mcp_note {
        emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n\n> {note}\n\n") });
    }
    let (acp_session, resumed, mut used_model) = open_session(engine, &client, saved, workspace, &mcp).await?;
    if let Some(k) = key {
        state.set(k, acp_session.clone());
    }
    let (mut updates, _route) = client.route(&acp_session);

    if let Some(m) = effective_model(engine, model) {
        let r = client
            .call(
                "session/set_config_option",
                json!({ "sessionId": acp_session, "configId": "model", "value": m }),
                CONFIG_TIMEOUT,
            )
            .await
            .map_err(|e| format!("{} could not switch to model {m}: {e}", engine.label()))?;
        used_model = current_model(&r).or(Some(m));
    }
    // Tell the UI which opencode model this turn really runs on (the reply
    // label must never show the Chat-mode model).
    if let Some(m) = used_model {
        emit_ev(on_event, stop, WorkAgentEvent::Model { model: engine.reply_label(&m) });
    }

    // First prompt: on a fresh session, Vicuna's workspace rules plus the
    // visible transcript (forks/restarts). Resumed sessions already hold the
    // conversation. The goal is repeated every turn.
    let goal_note = goal_text
        .filter(|g| !g.trim().is_empty())
        .map(|g| format!("\n\n[Session goal: {g} — keep working until it is fully achieved.]"))
        .unwrap_or_default();
    let base_text = format!("{}{}", last_user.content, goal_note);
    let text = if resumed {
        base_text
    } else {
        format!(
            "{}\n\n---\n{}{}",
            system_prompt(workspace, None, session_id).trim(),
            history_preamble(messages, last_user),
            base_text
        )
    };
    let mut blocks = prompt_blocks(&text, &last_user.images);

    let mut first_round = true;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        emit_ev(
            on_event,
            stop,
            WorkAgentEvent::RoundStart {
                mode: if first_round { "fresh" } else { "continue" }.to_string(),
            },
        );
        let started = Instant::now();
        let rx = client.request(
            "session/prompt",
            json!({ "sessionId": acp_session, "prompt": blocks }),
        )?;
        tokio::pin!(rx);
        let mut output_chars = 0usize;
        let mut tools: HashMap<String, ToolState> = HashMap::new();
        let mut cancelled = false;
        let mut cancel_at: Option<Instant> = None;
        let outcome: Result<Value, String> = loop {
            tokio::select! {
                update = updates.recv() => {
                    match update {
                        Some(msg) => handle_session_update(&msg, on_event, stop, &mut tools, &mut output_chars),
                        None => break Err(client.dead_error()),
                    }
                }
                result = &mut rx => break result.map_err(|_| "acp: response channel dropped".to_string()).and_then(|r| r),
                _ = tokio::time::sleep(Duration::from_millis(120)) => {
                    if stop.load(Ordering::Relaxed) && !cancelled {
                        cancelled = true;
                        cancel_at = Some(Instant::now());
                        let _ = client.notify("session/cancel", json!({ "sessionId": acp_session }));
                    }
                    if cancel_at.is_some_and(|t| t.elapsed() > CANCEL_GRACE) {
                        break Ok(json!({ "stopReason": "cancelled" }));
                    }
                }
            }
        };
        // Drain trailing updates that arrived before the prompt response.
        while let Ok(msg) = updates.try_recv() {
            handle_session_update(&msg, on_event, stop, &mut tools, &mut output_chars);
        }
        let gen_ms = started.elapsed().as_millis() as u64;
        let secs = (gen_ms as f64 / 1000.0).max(0.001);
        let gen_tps = (output_chars > 0).then(|| (output_chars as f64 / 4.0) / secs);
        emit_ev(
            on_event,
            stop,
            WorkAgentEvent::RoundEnd {
                gen_ms,
                gen_tps,
                gen_cost: None,
            },
        );

        match outcome {
            Ok(result) => {
                let stop_reason = result
                    .get("stopReason")
                    .and_then(|r| r.as_str())
                    .unwrap_or("end_turn");
                if stop_reason == "cancelled" || stop.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
            Err(e) => {
                if cancelled || stop.load(Ordering::Relaxed) {
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
                return Err(friendly_prompt_error(engine, e));
            }
        }

        // opencode has no mid-turn steering: messages queued while it worked
        // run as the next prompt of this same turn.
        let queued = take_queue(turn);
        if queued.is_empty() {
            break;
        }
        let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
        let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
        emit_ev(
            on_event,
            stop,
            WorkAgentEvent::QueueDrained { ids, messages: drained.clone() },
        );
        let (next_text, next_images) = messages_text(&drained);
        blocks = prompt_blocks(&next_text, &next_images);
        first_round = false;
    }

    emit_ev(
        on_event,
        stop,
        WorkAgentEvent::Done {
            stopped: stop.load(Ordering::Relaxed),
        },
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_mapping_reads_like_native_names() {
        let (name, args) = tool_name_and_args("execute", "Run tests", &json!({ "command": "cargo test" }));
        assert_eq!(name, "exec");
        assert_eq!(args["command"], "cargo test");
        let (name, args) = tool_name_and_args("read", "src/main.rs", &json!({ "filePath": "src/main.rs" }));
        assert_eq!(name, "read");
        assert_eq!(args["path"], "src/main.rs");
        let (name, args) = tool_name_and_args("search", "Matching", &json!({ "pattern": "**/*.md", "path": "." }));
        assert_eq!(name, "grep");
        assert_eq!(args["pattern"], "**/*.md");
    }

    #[test]
    fn permission_auto_approves_allow_once() {
        let params = json!({
            "sessionId": "s1",
            "toolCall": { "title": "bash: rm -rf build" },
            "options": [
                { "kind": "allow_always", "optionId": "always" },
                { "kind": "allow_once", "optionId": "once" },
                { "kind": "reject_once", "optionId": "reject" }
            ]
        });
        let out = handle_agent_request("session/request_permission", &params).unwrap();
        assert_eq!(out["outcome"]["optionId"], "once");
        let note = permission_note(&params).unwrap();
        assert_eq!(note["params"]["sessionId"], "s1");
        assert_eq!(note["params"]["update"]["title"], "bash: rm -rf build");
    }

    #[test]
    fn permission_without_allow_cancels() {
        let params = json!({ "options": [{ "kind": "reject_once", "optionId": "r" }] });
        let out = handle_agent_request("session/request_permission", &params).unwrap();
        assert_eq!(out["outcome"]["outcome"], "cancelled");
        assert!(handle_agent_request("fs/read_text_file", &json!({})).is_err());
    }

    #[test]
    fn updates_route_by_session_and_drop_unrouted() {
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        routes.lock().unwrap().insert("a".into(), tx);
        forward(&routes, &json!({ "sessionId": "a" }), json!(1));
        forward(&routes, &json!({ "sessionId": "b" }), json!(2));
        forward(&routes, &json!({}), json!(3));
        assert_eq!(rx.try_recv().unwrap(), json!(1));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn route_guard_unregisters() {
        let routes: Routes = Arc::new(Mutex::new(HashMap::new()));
        routes.lock().unwrap().insert("a".into(), mpsc::unbounded_channel().0);
        drop(RouteGuard { routes: routes.clone(), session: "a".into() });
        assert!(routes.lock().unwrap().is_empty());
    }

    #[test]
    fn session_key_ignores_model() {
        assert_eq!(session_key(Engine::Opencode, Some("chat1"), "/w"), Some("chat1|/w".to_string()));
        assert_eq!(session_key(Engine::Opencode, None, "/w"), None);
        // engines never share a session id (switching engine starts fresh)
        assert_eq!(session_key(Engine::Claude, Some("chat1"), "/w"), Some("claude|chat1|/w".to_string()));
    }

    /// Claude's models come from the adapter's `model` config option (shape
    /// captured from claude-agent-acp 0.99.1), never a hardcoded list.
    #[test]
    fn claude_models_detected_from_config_option() {
        let r = json!({ "sessionId": "s", "configOptions": [
            { "id": "mode", "currentValue": "default", "options": [{ "value": "default", "name": "Default" }] },
            { "id": "model", "currentValue": "opus", "options": [
                { "value": "default", "name": "Default (recommended)" },
                { "value": "opus", "name": "Opus" },
                { "value": "haiku", "name": "Haiku" },
                { "group": "Pinned", "name": "Pinned", "options": [{ "value": "claude-haiku-4-5-20251001", "name": "Haiku 4.5" }] },
                { "value": "haiku", "name": "dup" }
            ]}
        ]});
        let ids: Vec<_> = config_models(&r).into_iter().map(|m| (m.id, m.name, m.provider)).collect();
        assert_eq!(ids, vec![
            ("opus".to_string(), Some("Opus".to_string()), "claude".to_string()),
            ("haiku".to_string(), Some("Haiku".to_string()), "claude".to_string()),
            ("claude-haiku-4-5-20251001".to_string(), Some("Haiku 4.5".to_string()), "claude".to_string()),
        ]);
        assert!(config_models(&json!({ "sessionId": "s" })).is_empty());
    }

    #[test]
    fn claude_login_errors_point_at_claude() {
        let m = friendly_prompt_error(Engine::Claude, "Invalid API key · Please run /login".into());
        assert!(m.contains("run `claude`") && m.contains("/login"), "{m}");
        assert!(!m.contains("opencode"));
    }

    #[test]
    fn explicit_model_wins_and_blank_is_ignored() {
        assert_eq!(effective_model(Engine::Opencode, Some(" ollama/x ")), Some("ollama/x".to_string()));
    }

    #[test]
    fn rpc_error_text_includes_detail() {
        let e = json!({ "code": -32603, "message": "Internal error", "data": { "message": "model not found" } });
        assert_eq!(jsonrpc_error_text(&e), "Internal error: model not found");
        assert_eq!(jsonrpc_error_text(&json!({ "message": "x" })), "x");
        assert!(friendly_prompt_error(Engine::Opencode, "401 Unauthorized".into()).contains("opencode auth login"));
    }

    #[test]
    fn session_params_carry_mcp_servers() {
        let mcp = vec![json!({ "name": "searchapi", "type": "http", "url": "https://x", "headers": [] })];
        let p = session_params("/w", None, &mcp);
        assert_eq!(p["mcpServers"], json!(mcp));
        assert_eq!(p["cwd"], "/w");
        assert!(p.get("sessionId").is_none());
        assert_eq!(session_params("/w", Some("s1"), &[])["sessionId"], "s1");
    }

    fn planned(name: &str, tools: Option<usize>) -> PlannedServer {
        PlannedServer { name: name.into(), entry: json!({ "name": name }), tools }
    }

    /// Regression for the 128-tool error: servers past the budget are
    /// dropped (in dialog order) and named in a note; unknown counts pass.
    #[test]
    fn tool_budget_skips_servers_that_would_overflow() {
        let budget = PROVIDER_TOOL_LIMIT - OPENCODE_TOOL_RESERVE;
        let (kept, skipped) = fit_tool_budget(
            vec![planned("searchapi", Some(3)), planned("big", Some(140)), planned("browser", Some(33)), planned("unknown", None)],
            budget,
        );
        let names: Vec<_> = kept.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["searchapi", "browser", "unknown"]);
        assert_eq!(skipped.len(), 1);
        let note = skipped_note(&skipped).unwrap();
        assert!(note.contains("big (140 tools)") && note.contains("MCP dialog"), "{note}");
        assert!(skipped_note(&[]).is_none());
        assert!(kept.iter().map(|s| s.tools.unwrap_or(0)).sum::<usize>() + OPENCODE_TOOL_RESERVE <= PROVIDER_TOOL_LIMIT);
    }

    #[test]
    fn mcp_entries_match_acp_shape() {
        let e = stdio_entry("browser", &["/abs/lb".into(), "mcp".into()], Some(HashMap::from([("LB_SERVER".into(), "http://127.0.0.1:18765".into())]))).unwrap();
        assert_eq!(e["command"], "/abs/lb");
        assert_eq!(e["args"], json!(["mcp"]));
        assert_eq!(e["env"], json!([{ "name": "LB_SERVER", "value": "http://127.0.0.1:18765" }]));
        assert!(stdio_entry("x", &[], None).is_none());
        let h = http_entry("searchapi", "https://mcp.example/mcp", None, Some("tok".into()));
        assert_eq!(h["type"], "http");
        assert_eq!(h["headers"], json!([{ "name": "Authorization", "value": "Bearer tok" }]));
        // a user-set Authorization header wins over the stored login
        let own = http_entry("s", "https://u", Some(HashMap::from([("authorization".into(), "Key k".into())])), Some("tok".into()));
        assert_eq!(own["headers"], json!([{ "name": "authorization", "value": "Key k" }]));
    }

    /// Regression: the exact upstream error the owner hit must carry the
    /// opencode-config hint instead of a bare provider message.
    #[test]
    fn tool_limit_error_points_at_opencode_config() {
        let raw = "Internal error: Upstream request failed: [invalid_value] Invalid value for 'tools': maximum number of items is 128.";
        assert!(is_tool_limit_error(raw));
        let msg = friendly_prompt_error(Engine::Opencode, raw.into());
        assert!(msg.starts_with(raw));
        assert!(msg.contains("opencode mcp list") && msg.contains("MCP dialog"), "{msg}");
        assert!(!is_tool_limit_error("Internal error: model not found"));
        assert_eq!(friendly_prompt_error(Engine::Opencode, "model not found".into()), "model not found");
    }

    /// Regression: the reply label showed the Chat model. The model id the
    /// turn reports comes from opencode's own configOptions.
    #[test]
    fn current_model_reads_config_options() {
        let r = json!({ "sessionId": "s", "configOptions": [
            { "id": "effort", "currentValue": "default" },
            { "id": "model", "currentValue": "opencode/beam-free", "options": [] }
        ]});
        assert_eq!(current_model(&r).as_deref(), Some("opencode/beam-free"));
        assert_eq!(current_model(&json!({ "sessionId": "s" })), None);
        assert_eq!(current_model(&json!({ "configOptions": [{ "id": "model", "currentValue": "" }] })), None);
    }
}
