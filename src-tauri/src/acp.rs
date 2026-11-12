// Vicuna Work mode on opencode and Antigravity (agy), driven over the Agent
// Client Protocol (ACP). agy differs in three ways: one process per workspace
// (it runs tools in its own cwd), full model ids via session/set_model, and
// Allow/Deny-only permission requests (Vicuna adds "Always allow").
// (The Claude Code engine drives the `claude` CLI's stream-json protocol
// instead: claude_cli.rs; run_turn dispatches by engine. Both share this
// module's session store, MCP planning, prompts and turn helpers.)
//
// One long-lived `opencode acp` process (spawned on demand, respawned if it
// dies or the binary moves) serves every Work session: `initialize` once, then per session `session/new` (cwd +
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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, oneshot};

use super::opencode::{find_opencode, INSTALL_URL};
use super::work_engine::Engine;
use super::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use super::ChatMessage;

const PROTOCOL_VERSION: u64 = 1;
const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);
const CONFIG_TIMEOUT: Duration = Duration::from_secs(30);
const CANCEL_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 32 * 1024;
/// session/new tries for agy (see open_session_retrying).
const AGY_SESSION_ATTEMPTS: u32 = 3;

pub fn not_found_message() -> String {
    format!("OpenCode CLI not found, install it: {INSTALL_URL}")
}

// ---------- state: warm process + session ids persisted across turns ----------

struct Proc {
    client: Arc<AcpClient>,
    /// Approval mode the process was started under (opencode's config
    /// depends on it, so a toggle respawns).
    ask: bool,
    /// The opencode binary this process was started from.
    bin: PathBuf,
    /// OS pid (agy: also its process group id).
    pid: Option<u32>,
    // held so kill_on_drop ends the process when this Proc is replaced
    _child: Child,
}

#[derive(Default)]
pub struct AcpState {
    sessions: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
    /// (engine, workspace) -> warm process. The workspace is empty for
    /// engines with one shared process (opencode); agy runs its tools in its
    /// own working directory, so it gets one process per workspace.
    procs: tokio::sync::Mutex<HashMap<(Engine, String), Proc>>,
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

    pub(crate) fn get(&self, key: &str) -> Option<String> {
        self.load().ok()?;
        self.sessions.lock().ok()?.get(key).cloned()
    }

    pub(crate) fn set(&self, key: String, acp_session: String) {
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
    async fn client(&self, engine: Engine, workspace: &str) -> Result<Arc<AcpClient>, String> {
        let ask_now = super::work_engine::approval_ask();
        let (bin, cmd, ws) = match engine {
            Engine::Opencode => {
                let bin = find_opencode().ok_or_else(not_found_message)?;
                let mut cmd = super::opencode::command(&bin);
                cmd.arg("acp");
                (bin, cmd, String::new())
            }
            Engine::Agy => {
                if let Some(e) = super::agy::workspace_error(Path::new(workspace)) {
                    return Err(e);
                }
                let bin = super::agy::find_agy().ok_or_else(super::agy::missing_message)?;
                let cmd = super::agy::command(&bin, ask_now, Path::new(workspace));
                (bin, cmd, workspace.to_string())
            }
            Engine::Fx => {
                let bin = super::fx::find_fx().ok_or_else(super::fx::missing_message)?;
                let mut cmd = super::fx::command(&bin, ask_now);
                cmd.arg("acp");
                (bin, cmd, String::new())
            }
            _ => return Err(format!("{} does not run over ACP", engine.label())),
        };
        let key = (engine, ws);
        let mut guard = self.procs.lock().await;
        if let Some(p) = guard.get(&key) {
            if !p.client.closed.load(Ordering::Relaxed) && p.bin == bin && p.ask == ask_now {
                return Ok(p.client.clone());
            }
        }
        guard.remove(&key);
        let (client, child) = start_initialized(cmd, engine_process_label(engine), &bin).await?;
        guard.insert(
            key,
            Proc {
                client: client.clone(),
                ask: ask_now,
                pid: child.id(),
                bin,
                _child: child,
            },
        );
        Ok(client)
    }

    /// Drop a warm process (e.g. it failed to start a session) so the next
    /// call respawns it.
    async fn discard(&self, engine: Engine, workspace: &str) {
        let ws = if engine == Engine::Agy { workspace.to_string() } else { String::new() };
        self.procs.lock().await.remove(&(engine, ws));
    }

    /// Stop for agy: end its whole process group (agy leaves the command a
    /// cancelled call started running), so nothing outlives the turn. The
    /// next turn respawns agy and resumes the saved session.
    async fn kill_tree(&self, engine: Engine, workspace: &str) {
        let ws = if engine == Engine::Agy { workspace.to_string() } else { String::new() };
        if let Some(p) = self.procs.lock().await.remove(&(engine, ws)) {
            super::agy::kill_group(p.pid);
            // dropping `p` kills agy itself (kill_on_drop)
        }
    }

    /// Start the current engine's warm process ahead of the first Work turn
    /// (app startup). Silent when it is missing; the UI shows that state.
    pub async fn prewarm(&self) {
        let engine = super::work_engine::current_engine();
        // only the ACP engines keep a warm process; claude/codex start per turn
        if matches!(engine, Engine::Opencode | Engine::Fx) && engine.is_present() {
            let _ = self.client(engine, "").await;
        }
    }
}

/// Spawn an ACP agent and run `initialize`.
async fn start_initialized(
    cmd: tokio::process::Command,
    label: &'static str,
    bin: &Path,
) -> Result<(Arc<AcpClient>, Child), String> {
    let (client, child) = spawn_acp(cmd, label, bin)?;
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
    Ok((client, child))
}

fn engine_process_label(engine: Engine) -> &'static str {
    match engine {
        Engine::Opencode => "opencode acp",
        Engine::Claude => "claude",
        Engine::Codex => "codex app-server",
        Engine::Agy => "agy acp",
        Engine::Fx => "fx acp",
    }
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
pub(crate) fn session_key(engine: Engine, session_id: Option<&str>, workspace: &str) -> Option<String> {
    // opencode keeps the original key shape so sessions saved before Claude
    // existed still resume
    session_id.map(|id| match engine {
        Engine::Opencode => format!("{id}|{workspace}"),
        Engine::Claude => format!("claude|{id}|{workspace}"),
        Engine::Codex => format!("codex|{id}|{workspace}"),
        Engine::Agy => format!("agy|{id}|{workspace}"),
        Engine::Fx => format!("fx|{id}|{workspace}"),
    })
}

// ---------- session parameters + MCP servers handed to opencode ----------

/// Providers reject requests with more than this many tools (OpenAI-style
/// "Invalid value for 'tools': maximum number of items is 128").
pub const PROVIDER_TOOL_LIMIT: usize = 128;
/// Headroom kept for the engine's own built-in tools (~20 today) plus slack.
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
        "MCP servers not available this turn: {}. Together with the engine's own tools they would exceed the model's {}-tool limit; turn some servers off in the MCP dialog to make room.",
        list.join(", "),
        PROVIDER_TOOL_LIMIT
    ))
}

/// Vicuna's MCP dialog, as opencode `mcpServers`: every enabled vicuna.json
/// server plus the built-in browser row when it is connected, trimmed to
/// the tool budget. Returns the entries and an optional note for the user.
pub(crate) async fn mcp_servers_for_turn(snap: &McpSnapshot) -> (Vec<Value>, Option<String>) {
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
    let budget = PROVIDER_TOOL_LIMIT - OPENCODE_TOOL_RESERVE - crate::vicuna_mcp::TOOL_COUNT;
    let (kept, skipped) = fit_tool_budget(planned, budget);
    (kept.into_iter().map(|s| s.entry).collect(), skipped_note(&skipped))
}

fn resolve_executable(name: &str) -> Option<String> {
    let p = Path::new(name);
    if p.is_absolute() && p.is_file() {
        return Some(name.to_string());
    }
    // the terminal's PATH (login shell + process PATH): a GUI app's own PATH
    // misses nvm/uv/homebrew tools, and fx only accepts absolute commands
    let path = crate::exe_resolve::child_path(None).or_else(|| std::env::var_os("PATH"))?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().to_string());
        }
    }
    None
}

/// Stdio entries need an absolute command for fx; HTTP entries pass.
/// Returns (kept, names of the dropped servers).
fn split_absolute_commands(mcp: Vec<Value>) -> (Vec<Value>, Vec<String>) {
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for e in mcp {
        match e.get("command").and_then(Value::as_str) {
            Some(c) if !Path::new(c).is_absolute() => {
                dropped.push(e.get("name").and_then(Value::as_str).unwrap_or(c).to_string());
            }
            _ => kept.push(e),
        }
    }
    (kept, dropped)
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

fn config_value(result: &Value, id: &str) -> Option<String> {
    result
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| o.get("id").and_then(|i| i.as_str()) == Some(id))?
        .get("currentValue")?
        .as_str()
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

/// The model a session result reports: fx combines its `provider` and
/// `model` config options (`codex/gpt-6-luna`); agy's `models.currentModelId`
/// is the full id it lists (`gemini-3.8-flash-medium`; its `model` config
/// option drops the thinking level); the others report `configOptions.model`.
fn acp_model(engine: Engine, result: &Value) -> Option<String> {
    let model = if engine == Engine::Agy {
        result
            .pointer("/models/currentModelId")
            .and_then(Value::as_str)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
    } else {
        current_model(result)
    };
    let model = model?;
    if engine == Engine::Fx {
        return Some(match config_value(result, "provider") {
            Some(p) => format!("{p}/{model}"),
            None => model,
        });
    }
    Some(model)
}

/// agy sometimes fails its first session with "ACP session startup timed
/// out" (its backend warming up); a retry on the same process succeeds.
fn is_startup_timeout(e: &str) -> bool {
    let low = e.to_lowercase();
    low.contains("timed out") || low.contains("timeout")
}

/// Every model fx offers, as `provider/model`, from a throwaway ACP session:
/// the `provider` select lists fx's built-in and configured providers;
/// switching to each one (session-only, fx does not persist it) yields that
/// provider's `model` options. Providers that are not signed in are skipped.
async fn fx_list_models(client: &AcpClient, cwd: &str) -> Result<Vec<crate::opencode::OpencodeModel>, String> {
    let r = client
        .call("session/new", json!({ "cwd": cwd, "mcpServers": [] }), SESSION_TIMEOUT)
        .await
        .map_err(|e| format!("fx could not list models: {e}"))?;
    let sid = r.get("sessionId").and_then(|s| s.as_str()).unwrap_or_default().to_string();
    let providers: Vec<String> = r
        .get("configOptions")
        .and_then(|o| o.as_array())
        .and_then(|a| a.iter().find(|o| o.get("id").and_then(|i| i.as_str()) == Some("provider")))
        .and_then(|o| o.get("options"))
        .and_then(|o| o.as_array())
        .map(|a| a.iter().filter_map(|o| o.get("value")?.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    let mut out = Vec::new();
    let mut add = |provider: &str, res: &Value| {
        let opts = res
            .get("configOptions")
            .and_then(|o| o.as_array())
            .and_then(|a| a.iter().find(|o| o.get("id").and_then(|i| i.as_str()) == Some("model")))
            .and_then(|o| o.get("options"))
            .and_then(|o| o.as_array())
            .cloned()
            .unwrap_or_default();
        for o in opts {
            let Some(m) = o.get("value").and_then(|v| v.as_str()) else { continue };
            let id = format!("{provider}/{m}");
            if !out.iter().any(|x: &crate::opencode::OpencodeModel| x.id == id) {
                out.push(crate::opencode::OpencodeModel { id, provider: provider.to_string(), name: None });
            }
        }
    };
    let start = config_value(&r, "provider");
    if providers.is_empty() {
        if let Some(p) = &start {
            add(p, &r);
        }
    }
    for p in &providers {
        let res = if start.as_deref() == Some(p.as_str()) {
            r.clone()
        } else {
            match client
                .call(
                    "session/set_config_option",
                    json!({ "sessionId": sid, "configId": "provider", "value": p }),
                    CONFIG_TIMEOUT,
                )
                .await
            {
                Ok(res) => res,
                Err(_) => continue,
            }
        };
        add(p, &res);
    }
    if !sid.is_empty() {
        let _ = client.call("session/close", json!({ "sessionId": sid }), CONFIG_TIMEOUT).await;
    }
    Ok(out)
}

impl AcpState {
    /// fx's models (see fx_list_models), from a short-lived `fx acp` of its
    /// own: fx 0.0.16 keeps the last switched-to provider's model catalog
    /// process-wide, so after cycling providers a later session on the same
    /// process rejects valid models ("Model is not available for the active
    /// provider"). The warm turn process never cycles.
    pub async fn fx_models(&self) -> Result<Vec<crate::opencode::OpencodeModel>, String> {
        let bin = super::fx::find_fx().ok_or_else(super::fx::missing_message)?;
        let mut cmd = super::fx::command(&bin, false);
        cmd.arg("acp");
        let (client, _child) = start_initialized(cmd, "fx acp", &bin).await?;
        let cwd = home_dir()?.display().to_string();
        fx_list_models(&client, &cwd).await
        // _child is killed on drop
    }
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
    /// `sessionId|tool` pairs the user answered "Always allow" for, when the
    /// agent offers no allow_always option of its own (agy): later requests
    /// for that tool in that session are approved without a card.
    always: Arc<Mutex<HashSet<String>>>,
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

    /// Answer an agent -> client request (permission asked in the UI).
    fn respond(&self, id: Value, result: Value) -> Result<(), String> {
        let line = json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string();
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
    let always: Arc<Mutex<HashSet<String>>> = Arc::default();

    let (pending_r, routes_r, write_r, closed_r, always_r) =
        (pending.clone(), routes.clone(), write_tx.clone(), closed.clone(), always.clone());
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
                    // "ask" mode: hand the permission request to the turn
                    // that owns the session; it asks the user and responds.
                    let remembered = always_key(&params)
                        .is_some_and(|k| always_r.lock().map(|a| a.contains(&k)).unwrap_or(false));
                    if method == "session/request_permission"
                        && super::work_engine::approval_ask()
                        && has_route(&routes_r, &params)
                        && !remembered
                    {
                        let fwd = json!({ "vicuna_permission_request": { "id": id, "params": params.clone() } });
                        forward(&routes_r, &params, fwd);
                        continue;
                    }
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
        always,
    });
    Ok((client, child))
}

fn has_route(routes: &Routes, params: &Value) -> bool {
    params
        .get("sessionId")
        .and_then(|s| s.as_str())
        .is_some_and(|sid| routes.lock().map(|r| r.contains_key(sid)).unwrap_or(false))
}

/// Option labels for the ask card, and the response for a user's answer:
/// an exact label match, else an allow/deny reading of free text; anything
/// else (dismiss, Stop) rejects, or cancels when there is no reject option.
fn permission_options(params: &Value) -> Vec<(String, String, String)> {
    params
        .get("options")
        .and_then(|o| o.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    let id = o.get("optionId")?.as_str()?.to_string();
                    let kind = o.get("kind").and_then(|k| k.as_str()).unwrap_or("").to_string();
                    let name = o.get("name").and_then(|n| n.as_str()).map(str::to_string).unwrap_or_else(|| kind.clone());
                    Some((id, kind, name))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The label Vicuna adds when the agent has no allow_always option.
pub(crate) const ALWAYS_LABEL: &str = "Always allow";

/// Whether the ask card gets a synthesized "Always allow" (an allow option
/// exists but no allow_always one).
fn synth_always(params: &Value) -> bool {
    let opts = permission_options(params);
    opts.iter().any(|(_, k, _)| k.starts_with("allow")) && !opts.iter().any(|(_, k, _)| k == "allow_always")
}

/// Card labels: the agent's options, plus "Always allow" right after its
/// first allow option when the agent offers none.
fn permission_labels(params: &Value) -> Vec<String> {
    let opts = permission_options(params);
    let mut labels: Vec<String> = opts.iter().map(|(_, _, n)| n.clone()).collect();
    if synth_always(params) {
        let at = opts.iter().position(|(_, k, _)| k.starts_with("allow")).map_or(labels.len(), |i| i + 1);
        labels.insert(at, ALWAYS_LABEL.to_string());
    }
    labels
}

/// The remembered-approval key of a permission request: session + the
/// agent's tool name (agy `run_command`), else its kind.
fn always_key(params: &Value) -> Option<String> {
    let sid = params.get("sessionId")?.as_str()?;
    let tc = params.get("toolCall")?;
    let tool = tc
        .get("name")
        .or_else(|| tc.get("kind"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())?;
    Some(format!("{sid}|{tool}"))
}

/// True when `answer` is the synthesized "Always allow" for this request.
fn is_synth_always(params: &Value, answer: &str) -> bool {
    synth_always(params) && answer.trim().eq_ignore_ascii_case(ALWAYS_LABEL)
}

fn permission_answer(params: &Value, answer: &str) -> (Value, bool) {
    let opts = permission_options(params);
    let a = if is_synth_always(params, answer) { "allow".to_string() } else { answer.trim().to_lowercase() };
    let by_kind = |pref: &str| opts.iter().find(|(_, k, _)| k.starts_with(pref));
    let chosen = opts
        .iter()
        .find(|(_, _, n)| n.to_lowercase() == a)
        .or_else(|| {
            if ["allow", "yes", "y", "ok", "approve"].contains(&a.as_str()) {
                by_kind("allow_once").or_else(|| by_kind("allow"))
            } else {
                None
            }
        })
        .or_else(|| by_kind("reject"));
    match chosen {
        Some((id, kind, _)) => (
            json!({ "outcome": { "outcome": "selected", "optionId": id } }),
            kind.starts_with("allow"),
        ),
        None => (json!({ "outcome": { "outcome": "cancelled" } }), false),
    }
}

/// Ask the user (ask card) and answer the agent's permission request.
fn spawn_permission_ask(
    req: &Value,
    turn: Arc<TurnState>,
    client: Arc<AcpClient>,
    on_event: Channel<WorkAgentEvent>,
) {
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let title = params
        .pointer("/toolCall/title")
        .and_then(|t| t.as_str())
        .filter(|t| !t.trim().is_empty())
        .unwrap_or("a tool call")
        .to_string();
    let labels = permission_labels(&params);
    let ask_id = format!("perm-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(ask_id.clone(), tx);
    }
    emit_ev(
        &on_event,
        &turn.stop,
        WorkAgentEvent::AskRequest { ask_id, question: format!("Allow {title}?"), options: labels, multi: false },
    );
    tokio::spawn(async move {
        let answer = rx.await.unwrap_or_else(|_| "cancelled".to_string());
        let (result, allowed) = permission_answer(&params, &answer);
        let remember = allowed && is_synth_always(&params, &answer);
        if remember {
            if let (Some(k), Ok(mut a)) = (always_key(&params), client.always.lock()) {
                a.insert(k);
            }
        }
        let _ = client.respond(id, result);
        let verdict = if remember { "Always allowed" } else if allowed { "Approved" } else { "Denied" };
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> {verdict}: {title}\n") });
    });
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

pub(crate) fn tool_name_and_args(kind_hint: &str, title: &str, raw: &Value) -> (String, Value) {
    // agy spells its arguments in CamelCase (AbsolutePath, CommandLine, ...)
    let path = ["path", "file_path", "filePath", "AbsolutePath", "TargetFile", "DirectoryPath", "SearchPath", "SearchDirectory"]
        .iter()
        .find_map(|k| raw.get(*k).and_then(|p| p.as_str()))
        .unwrap_or(title);
    let command = raw
        .get("command")
        .or_else(|| raw.get("cmd"))
        .or_else(|| raw.get("CommandLine"))
        .and_then(|c| c.as_str());
    // opencode glob/search tools can arrive as kind "read" with a pattern in
    // rawInput; show them as a search instead of "read — <title>".
    let pattern = ["pattern", "glob", "query", "Pattern", "Query"]
        .iter()
        .find_map(|k| raw.get(*k).and_then(|v| v.as_str()));
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
        _ => {
            let mut r = raw.clone();
            if let Some(m) = r.as_object_mut() {
                m.remove("toolSummary");
            }
            r
        }
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
    /// ToolStart sent (deferred until the call's input is known).
    started: bool,
    ended: bool,
    kind: String,
    title: String,
    /// The agent's own tool name, when it sends one (fx).
    tool: Option<String>,
    raw: Value,
}

fn has_input(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Object(m) => !m.is_empty(),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    }
}

/// Tool-call updates -> ToolStart / ToolEnd. Agents often announce a call
/// before its input is known (`rawInput` empty, status pending) and fill it
/// in a later `tool_call_update`; the card waits for the input (or for the
/// call to start running / finish) so it shows real arguments, not `{}`.
fn tool_events(update: &Value, tools: &mut HashMap<String, ToolState>) -> Vec<WorkAgentEvent> {
    let mut out = Vec::new();
    let Some(id) = update.get("toolCallId").and_then(|i| i.as_str()).filter(|i| !i.is_empty()) else {
        return out;
    };
    let status = update.get("status").and_then(|s| s.as_str()).unwrap_or("");
    let entry = tools.entry(id.to_string()).or_insert_with(|| ToolState {
        name: String::new(),
        started: false,
        ended: false,
        kind: "other".to_string(),
        title: "tool".to_string(),
        tool: None,
        raw: json!({}),
    });
    if let Some(k) = update.get("kind").and_then(|k| k.as_str()) {
        entry.kind = k.to_string();
    }
    if let Some(t) = update.get("title").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty()) {
        entry.title = t.to_string();
    }
    // fx names each call (`name`: write_file, shell, an MCP tool); for kinds
    // without a native mapping that name reads better than a generic title
    if let Some(n) = update.get("name").and_then(|t| t.as_str()).filter(|t| !t.trim().is_empty()) {
        entry.tool = Some(n.to_string());
    }
    if let Some(r) = update.get("rawInput").filter(|r| has_input(r)) {
        entry.raw = r.clone();
    }
    let finished = matches!(status, "completed" | "failed" | "cancelled");
    if !entry.started && (has_input(&entry.raw) || status == "in_progress" || finished) {
        let native = matches!(entry.kind.as_str(), "execute" | "shell" | "read" | "edit" | "search" | "fetch");
        let title = match (&entry.tool, native) {
            (Some(t), false) => t.clone(),
            _ => entry.title.clone(),
        };
        let (name, args) = tool_name_and_args(&entry.kind, &title, &entry.raw);
        entry.name = name.clone();
        entry.started = true;
        out.push(WorkAgentEvent::ToolStart { name, args });
    }
    if finished && !entry.ended {
        entry.ended = true;
        let mut output = tool_output(update);
        if status == "failed" && !output.starts_with("Error:") {
            output = format!("Error: {output}");
        }
        if output.trim().is_empty() {
            output = format!("[{status}]");
        }
        out.push(WorkAgentEvent::ToolEnd { name: entry.name.clone(), output });
    }
    out
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
            for ev in tool_events(update, tools) {
                emit_ev(on_event, stop, ev);
            }
        }
        "plan" => {
            if let Some(note) = plan_note(update) {
                emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: note });
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

/// ACP `plan` update (entries with content + status) as a transcript checklist.
fn plan_note(update: &Value) -> Option<String> {
    let entries = update.get("entries").and_then(Value::as_array)?;
    if entries.is_empty() {
        return None;
    }
    let mut out = String::from("\n\n**Plan**\n");
    for e in entries {
        let mark = match e.get("status").and_then(Value::as_str) {
            Some("completed") => "[x]",
            Some("in_progress") => "[~]",
            _ => "[ ]",
        };
        out.push_str(&format!("- {mark} {}\n", e.get("content").and_then(Value::as_str).unwrap_or("").trim()));
    }
    out.push('\n');
    Some(out)
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

pub(crate) fn last_user_message(messages: &[ChatMessage]) -> Option<&ChatMessage> {
    messages.iter().rev().find(|m| m.role == "user")
}

/// On a brand-new opencode session with existing Vicuna history (fork, app
/// restart), hand opencode the visible transcript so the work continues in context.
pub(crate) fn history_preamble(messages: &[ChatMessage], last_user: &ChatMessage) -> String {
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

pub(crate) fn system_prompt(workspace: &str, goal: Option<&str>, session_id: Option<&str>) -> String {
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
- When you need a decision, a preference or missing information from the user, call the `ask_user` tool (vicuna-mcp server) with the question and a few short `choices`: it shows the user a question card and returns their answer. Do not ask such questions in plain text, and do not use any other question tool.\n\
{uploads}\n{goal}",
        goal = goal
            .filter(|g| !g.trim().is_empty())
            .map(|g| format!(
                "\nSESSION GOAL: {g}\nKeep working until the goal is fully achieved; do not stop at a partial result.\n"
            ))
            .unwrap_or_default()
    )
}

pub(crate) fn take_queue(turn: &TurnState) -> Vec<(String, ChatMessage)> {
    turn.queue
        .lock()
        .map(|mut q| std::mem::take(&mut *q))
        .unwrap_or_default()
}

/// Concatenate queued messages for a follow-up prompt. Images travel with it.
pub(crate) fn messages_text(messages: &[ChatMessage]) -> (String, Option<Vec<String>>) {
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
    // vicuna-mcp's ask_user reaches this turn while `_ask` lives. opencode
    // keeps MCP servers per workspace folder, so its route is the workspace;
    // each claude process and each codex app-server gets its own.
    // fx/agy: a resumed session keeps the MCP connections of the warm
    // process (agy: one per workspace), so the same stable per-workspace
    // route as opencode.
    let route = matches!(engine, Engine::Opencode | Engine::Agy | Engine::Fx).then(|| format!("{}|{workspace}", engine_process_label(engine)));
    let _ask = match crate::vicuna_mcp::register(route.as_deref(), turn.clone(), on_event.clone()).await {
        Ok(r) => Some(r),
        Err(e) => {
            eprintln!("{e}");
            None
        }
    };
    let ask_url = _ask.as_ref().map(|r| r.url.clone());
    let res = if engine == Engine::Claude {
        super::claude_cli::run_turn_inner(
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
            ask_url.as_deref(),
        )
        .await
    } else if engine == Engine::Codex {
        super::codex_cli::run_turn_inner(
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
            ask_url.as_deref(),
        )
        .await
    } else {
        run_turn_inner(
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
        ask_url.as_deref(),
    )
    .await
    };
    if let Err(message) = res {
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
                let model = acp_model(engine, &r);
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
    Ok((session_id_of(engine, &result)?, false, acp_model(engine, &result)))
}

fn session_id_of(engine: Engine, result: &Value) -> Result<String, String> {
    let sid = result
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("{}: session/new returned no sessionId", engine_process_label(engine)))?;
    Ok(sid)
}

/// open_session, retried for agy: its first session on a fresh process
/// sometimes fails with "ACP session startup timed out". Retry on the same
/// process once, then on a respawned one.
#[allow(clippy::too_many_arguments)]
async fn open_session_retrying(
    engine: Engine,
    state: &AcpState,
    client: Arc<AcpClient>,
    saved: Option<String>,
    workspace: &str,
    mcp: &[Value],
    on_event: &Channel<WorkAgentEvent>,
    stop: &AtomicBool,
) -> Result<(Arc<AcpClient>, (String, bool, Option<String>)), String> {
    let attempts = if engine == Engine::Agy { AGY_SESSION_ATTEMPTS } else { 1 };
    let mut client = client;
    let mut attempt = 1;
    loop {
        match open_session(engine, &client, saved.clone(), workspace, mcp).await {
            Ok(r) => return Ok((client, r)),
            Err(e) if attempt < attempts && !stop.load(Ordering::Relaxed) && (is_startup_timeout(&e) || client.closed.load(Ordering::Relaxed)) => {
                emit_ev(
                    on_event,
                    stop,
                    WorkAgentEvent::Status { label: Some(format!("{} session start failed, retrying ({}/{})", engine.label(), attempt + 1, attempts)) },
                );
                eprintln!("{}: {e}; retrying", engine_process_label(engine));
                if attempt + 1 == attempts || client.closed.load(Ordering::Relaxed) {
                    state.discard(engine, workspace).await;
                    client = state.client(engine, workspace).await?;
                }
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Model to run: the turn's explicit pick, else the one saved in Vicuna's
/// picker, else opencode's own default (None).
pub(crate) fn effective_model(engine: Engine, requested: Option<&str>) -> Option<String> {
    requested
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
        .or_else(|| engine.saved_model())
}

pub(crate) fn friendly_prompt_error(engine: Engine, e: String) -> String {
    let low = e.to_lowercase();
    if is_tool_limit_error(&e) {
        format!("{e}\n{TOOL_LIMIT_HINT}")
    } else if low.contains("auth") || low.contains("api key") || low.contains("unauthorized") || low.contains("401") || low.contains("login") {
        match engine {
            Engine::Opencode => format!("{e}\nThe selected provider needs credentials: run `opencode auth login` once, or pick another model."),
            Engine::Claude => format!("{e}\nClaude Code is not logged in: run `claude` in a terminal once and use /login."),
            Engine::Codex => format!("{e}\nCodex is not logged in: run `codex login` in a terminal once."),
            Engine::Agy => format!("{e}\nAntigravity is not logged in: run `agy` in a terminal once and sign in with Google."),
            Engine::Fx => format!("{e}\nfx needs a signed-in provider: run `fx login codex` (or set the provider's auth environment variable in ~/.fx/settings.json), or pick another model."),
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
    ask_url: Option<&str>,
) -> Result<(), String> {
    let stop = &turn.stop;
    let last_user = last_user_message(messages)
        .ok_or_else(|| "Work mode: no user message to send".to_string())?;
    let client = state.client(engine, workspace).await?;

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
    let mcp = crate::vicuna_mcp::with_ask_server(mcp, ask_url);
    // fx rejects the whole session when one stdio server's command is not an
    // absolute path; leave out the ones that did not resolve and say so
    let mcp = if engine == Engine::Fx {
        let (kept, dropped) = split_absolute_commands(mcp);
        if !dropped.is_empty() {
            let note = format!(
                "MCP servers not available to fx this turn (command not found on your PATH): {}",
                dropped.join(", ")
            );
            emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n\n> {note}\n\n") });
        }
        kept
    } else {
        mcp
    };
    let (client, (acp_session, resumed, mut used_model)) =
        open_session_retrying(engine, state, client, saved, workspace, &mcp, on_event, stop).await?;
    if let Some(k) = key {
        state.set(k, acp_session.clone());
    }
    let (mut updates, _route) = client.route(&acp_session);

    if let (Engine::Agy, Some(m)) = (engine, effective_model(engine, model)) {
        // agy lists full ids (`gemini-3.8-flash-medium`) for session/set_model;
        // its `model` config option only knows the family
        if used_model.as_deref() != Some(m.as_str()) {
            client
                .call("session/set_model", json!({ "sessionId": acp_session, "modelId": m }), CONFIG_TIMEOUT)
                .await
                .map_err(|e| friendly_prompt_error(engine, format!("Antigravity could not switch to model {m}: {e}")))?;
        }
        used_model = Some(m);
    } else if let (Engine::Fx, Some(m)) = (engine, effective_model(engine, model)) {
        // `provider/model`: switch the provider first (its model list
        // changes with it), then the model
        let (provider, bare) = super::fx::split_model(&m)
            .ok_or_else(|| format!("fx model ids are provider/model, got {m}"))?;
        for (id, value) in [("provider", provider), ("model", bare)] {
            let r = client
                .call(
                    "session/set_config_option",
                    json!({ "sessionId": acp_session, "configId": id, "value": value }),
                    CONFIG_TIMEOUT,
                )
                .await
                .map_err(|e| friendly_prompt_error(engine, format!("fx could not switch to {id} {value}: {e}")))?;
            used_model = acp_model(engine, &r).or(Some(m.clone()));
        }
    } else if let Some(m) = effective_model(engine, model) {
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
        // a tool call was still running when Stop was pressed
        let mut tool_running_at_stop = false;
        let mut cancel_at: Option<Instant> = None;
        let outcome: Result<Value, String> = loop {
            tokio::select! {
                update = updates.recv() => {
                    match update {
                        Some(msg) => {
                            if let Some(req) = msg.get("vicuna_permission_request") {
                                spawn_permission_ask(req, turn.clone(), client.clone(), on_event.clone());
                            } else {
                                handle_session_update(&msg, on_event, stop, &mut tools, &mut output_chars);
                            }
                        }
                        None => break Err(client.dead_error()),
                    }
                }
                result = &mut rx => break result.map_err(|_| "acp: response channel dropped".to_string()).and_then(|r| r),
                _ = tokio::time::sleep(Duration::from_millis(120)) => {
                    if stop.load(Ordering::Relaxed) && !cancelled {
                        cancelled = true;
                        tool_running_at_stop = tools.values().any(|t| t.started && !t.ended);
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
        if engine == Engine::Agy && cancelled && tool_running_at_stop {
            state.kill_tree(engine, workspace).await;
            emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: "\n> Stopped: ended the running command.\n".to_string() });
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
        assert_eq!(session_key(Engine::Codex, Some("chat1"), "/w"), Some("codex|chat1|/w".to_string()));
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

    /// fx reports provider and model as separate config options; Vicuna's
    /// id (and the reply label) is `provider/model`.
    #[test]
    fn fx_model_id_joins_provider_and_model() {
        let r = json!({ "configOptions": [
            { "id": "provider", "currentValue": "ollama" },
            { "id": "model", "currentValue": "deepseek-v4.1-flash:cloud" }
        ]});
        assert_eq!(acp_model(Engine::Fx, &r).as_deref(), Some("ollama/deepseek-v4.1-flash:cloud"));
        assert_eq!(acp_model(Engine::Opencode, &r).as_deref(), Some("deepseek-v4.1-flash:cloud"));
        assert_eq!(session_key(Engine::Fx, Some("c1"), "/ws").as_deref(), Some("fx|c1|/ws"));
        assert_eq!(engine_process_label(Engine::Fx), "fx acp");
    }

    #[test]
    fn fx_drops_stdio_servers_without_an_absolute_command() {
        let mcp = vec![
            json!({ "name": "a", "command": "/usr/bin/python3", "args": [], "env": [] }),
            json!({ "name": "b", "command": "uvx", "args": [], "env": [] }),
            json!({ "type": "http", "name": "vicuna-mcp", "url": "http://127.0.0.1:1/mcp/x", "headers": [] }),
        ];
        let (kept, dropped) = split_absolute_commands(mcp);
        assert_eq!(kept.iter().map(|e| e["name"].as_str().unwrap()).collect::<Vec<_>>(), ["a", "vicuna-mcp"]);
        assert_eq!(dropped, ["b"]);
    }

    /// fx sends its own tool name (`name`) next to a generic title; MCP tools
    /// (kind other) show that name, native kinds keep their mapping.
    #[test]
    fn fx_tool_names_and_plan_notes() {
        let mut tools = HashMap::new();
        let mcp = json!({ "sessionUpdate": "tool_call", "toolCallId": "m1", "name": "browser__navigate", "title": "Calling tool", "kind": "other", "status": "pending", "rawInput": { "url": "https://example.com" } });
        match &tool_events(&mcp, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "browser__navigate");
                assert_eq!(args["url"], "https://example.com");
            }
            other => panic!("unexpected {other:?}"),
        }
        let shell = json!({ "sessionUpdate": "tool_call", "toolCallId": "s1", "name": "shell", "title": "Running", "kind": "execute", "status": "pending", "rawInput": { "action": "run", "command": "ls" } });
        match &tool_events(&shell, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "ls");
            }
            other => panic!("unexpected {other:?}"),
        }
        let plan = json!({ "sessionUpdate": "plan", "entries": [
            { "content": "Read code", "status": "completed", "priority": "high" },
            { "content": "Fix bug", "status": "in_progress", "priority": "high" },
            { "content": "Test", "status": "pending", "priority": "low" }
        ]});
        let note = plan_note(&plan).unwrap();
        assert!(note.contains("- [x] Read code") && note.contains("- [~] Fix bug") && note.contains("- [ ] Test"), "{note}");
        assert_eq!(plan_note(&json!({ "entries": [] })), None);
    }

    /// QA FAIL (PR #6): cards showed `{}` because ToolStart went out with
    /// the first, input-less announcement. Now the card waits for the input.
    #[test]
    fn tool_card_waits_for_real_input() {
        let mut tools = HashMap::new();
        let announce = json!({ "sessionUpdate": "tool_call", "toolCallId": "t1", "title": "mcp__browser__open", "kind": "other", "status": "pending", "rawInput": {} });
        assert!(tool_events(&announce, &mut tools).is_empty(), "no card before the input is known");
        let filled = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "rawInput": { "url": "http://127.0.0.1:1421/health" } });
        let evs = tool_events(&filled, &mut tools);
        match &evs[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "mcp__browser__open");
                assert_eq!(args["url"], "http://127.0.0.1:1421/health");
            }
            other => panic!("{other:?}"),
        }
        let done = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed", "content": [{ "type": "content", "content": { "type": "text", "text": "opened" } }] });
        assert!(matches!(&tool_events(&done, &mut tools)[..], [WorkAgentEvent::ToolEnd { .. }]));
        // a call that never reports input still gets a card once it runs
        let bare = json!({ "sessionUpdate": "tool_call", "toolCallId": "t2", "title": "Run tests", "kind": "execute", "status": "pending" });
        assert!(tool_events(&bare, &mut tools).is_empty());
        let running = json!({ "sessionUpdate": "tool_call_update", "toolCallId": "t2", "status": "in_progress", "rawInput": { "command": "cargo test" } });
        match &tool_events(&running, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "cargo test");
            }
            other => panic!("{other:?}"),
        }
        // finishing without any start still yields start + end, in order
        let quick = json!({ "sessionUpdate": "tool_call", "toolCallId": "t3", "title": "x", "status": "completed" });
        let evs = tool_events(&quick, &mut tools);
        assert!(matches!(&evs[..], [WorkAgentEvent::ToolStart { .. }, WorkAgentEvent::ToolEnd { .. }]));
    }

    /// QA FAIL (PR #6): "ask" mode maps the user's choice onto the agent's
    /// permission options; dismiss/Stop rejects.
    #[test]
    fn permission_answers_map_to_options() {
        let params = json!({ "sessionId": "s", "toolCall": { "title": "Bash: rm -rf build" }, "options": [
            { "optionId": "a1", "kind": "allow_always", "name": "Always allow" },
            { "optionId": "a0", "kind": "allow_once", "name": "Allow" },
            { "optionId": "r0", "kind": "reject_once", "name": "Reject" }
        ]});
        let labels: Vec<_> = permission_options(&params).into_iter().map(|(_, _, n)| n).collect();
        assert_eq!(labels, ["Always allow", "Allow", "Reject"]);
        let pick = |a: &str| permission_answer(&params, a);
        assert_eq!(pick("Always allow"), (json!({ "outcome": { "outcome": "selected", "optionId": "a1" } }), true));
        assert_eq!(pick("allow").0["outcome"]["optionId"], "a0");
        assert_eq!(pick("yes").0["outcome"]["optionId"], "a0");
        assert_eq!(pick("Reject"), (json!({ "outcome": { "outcome": "selected", "optionId": "r0" } }), false));
        assert_eq!(pick("cancelled").0["outcome"]["optionId"], "r0");
        assert!(!pick("please no").1);
        let no_reject = json!({ "options": [{ "optionId": "a0", "kind": "allow_once", "name": "Allow" }] });
        assert_eq!(permission_answer(&no_reject, "cancelled"), (json!({ "outcome": { "outcome": "cancelled" } }), false));
    }

    /// agy offers only Allow / Deny: the card adds "Always allow", which
    /// answers allow_once and is remembered per session + tool name.
    #[test]
    fn agy_permission_gets_a_synthesized_always() {
        let params = json!({ "sessionId": "s9", "toolCall": { "name": "run_command", "kind": "execute", "title": "ls" }, "options": [
            { "optionId": "allow", "name": "Allow", "kind": "allow_once" },
            { "optionId": "deny", "name": "Deny", "kind": "reject_once" }
        ]});
        assert_eq!(permission_labels(&params), ["Allow", ALWAYS_LABEL, "Deny"]);
        assert!(is_synth_always(&params, "always allow"));
        assert_eq!(permission_answer(&params, ALWAYS_LABEL), (json!({ "outcome": { "outcome": "selected", "optionId": "allow" } }), true));
        assert_eq!(permission_answer(&params, "Deny").0["outcome"]["optionId"], "deny");
        assert_eq!(always_key(&params).as_deref(), Some("s9|run_command"));
        // an agent with its own allow_always keeps its labels untouched
        let oc = json!({ "options": [
            { "optionId": "a1", "kind": "allow_always", "name": "Always allow" },
            { "optionId": "a0", "kind": "allow_once", "name": "Allow" }
        ]});
        assert_eq!(permission_labels(&oc), ["Always allow", "Allow"]);
        assert!(!is_synth_always(&oc, "Always allow"));
    }

    /// agy: full model id from `models.currentModelId`, CamelCase tool args,
    /// MCP tools named by the agent, plan entries as a checklist.
    #[test]
    fn agy_models_tools_and_plans() {
        let r = json!({ "models": { "currentModelId": "gemini-3.8-flash-medium" },
            "configOptions": [{ "id": "model", "currentValue": "gemini-3.8-flash" }] });
        assert_eq!(acp_model(Engine::Agy, &r).as_deref(), Some("gemini-3.8-flash-medium"));
        assert_eq!(acp_model(Engine::Opencode, &r).as_deref(), Some("gemini-3.8-flash"));
        assert_eq!(session_key(Engine::Agy, Some("c1"), "/ws").as_deref(), Some("agy|c1|/ws"));
        assert!(is_startup_timeout("ACP session startup timed out"));
        assert!(!is_startup_timeout("permission denied"));

        let mut tools = HashMap::new();
        let shell = json!({ "sessionUpdate": "tool_call", "toolCallId": "a1", "name": "run_command", "kind": "execute", "title": "ls -la", "status": "in_progress",
            "rawInput": { "CommandLine": "ls -la", "WaitMsBeforeAsync": 5000, "toolSummary": "List" } });
        match &tool_events(&shell, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "ls -la");
            }
            other => panic!("unexpected {other:?}"),
        }
        let view = json!({ "sessionUpdate": "tool_call", "toolCallId": "a2", "name": "view_file", "kind": "read", "title": "Read main.rs", "status": "in_progress",
            "rawInput": { "AbsolutePath": "/ws/main.rs" } });
        match &tool_events(&view, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "read");
                assert_eq!(args["path"], "/ws/main.rs");
            }
            other => panic!("unexpected {other:?}"),
        }
        let mcp = json!({ "sessionUpdate": "tool_call", "toolCallId": "a3", "name": "mcp_browser_navigate", "kind": "other", "title": "Run mcp_browser_navigate", "status": "in_progress",
            "rawInput": { "toolSummary": "Open page", "url": "https://example.com" } });
        match &tool_events(&mcp, &mut tools)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "mcp_browser_navigate");
                assert_eq!(args, &json!({ "url": "https://example.com" }));
            }
            other => panic!("unexpected {other:?}"),
        }
        let plan = json!({ "sessionUpdate": "plan", "entries": [
            { "content": "Read code", "status": "completed" },
            { "content": "Fix bug", "status": "in_progress" },
            { "content": "Test", "status": "pending" }
        ]});
        let note = plan_note(&plan).unwrap();
        assert!(note.contains("- [x] Read code") && note.contains("- [~] Fix bug") && note.contains("- [ ] Test"), "{note}");
        assert_eq!(plan_note(&json!({ "entries": [] })), None);
    }

}
