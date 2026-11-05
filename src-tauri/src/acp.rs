// Vicuna Work mode via the Agent Client Protocol (ACP).
//
// `fx acp` (Vercel Labs fx, bundled as a ~5 MB native sidecar) is an ACP
// agent server over stdio. This module is a deliberately small JSON-RPC
// client: initialize -> session/new|resume -> session/prompt, translating
// `session/update` notifications into the same WorkAgentEvent stream the
// built-in loop emits, so ChatApp's rendering is unchanged.
//
// No ACP SDK dependency: the wire format is newline-delimited JSON-RPC 2.0.
// Client-side fs/terminal capabilities are advertised as unsupported because
// fx runs its own tools; only permission requests need an answer, and those
// are auto-approved (allow_once -> allow_always -> first allow).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use super::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use super::ChatMessage;

const PROTOCOL_VERSION: u64 = 1;
const INIT_TIMEOUT: Duration = Duration::from_secs(30);
const SESSION_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 32 * 1024;

// ---------- state: fx session ids persisted across turns ----------

#[derive(Default)]
pub struct AcpState {
    sessions: Mutex<HashMap<String, String>>,
    loaded: AtomicBool,
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

    fn set(&self, key: String, fx_session: String) {
        let _ = self.load();
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.insert(key, fx_session.clone());
            if let Ok(path) = sessions_path() {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let doc = serde_json::to_string_pretty(&*sessions).unwrap_or_default();
                let _ = std::fs::write(path, doc);
            }
        }
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
        .join("fx-sessions.json"))
}

fn session_key(session_id: Option<&str>, workspace: &str, model: &str) -> Option<String> {
    session_id.map(|id| format!("{id}|{workspace}|{model}"))
}

// ---------- sidecar resolution ----------

/// Binaries are arch-suffixed on macOS (universal app ships both); Linux uses
/// `fx`. Debug builds prefer the source tree over `resource_dir()` so a
/// rebuild can't race a stale copy (same reasoning as the lb sidecar).
pub fn fx_binary_path(resource_dir: Option<&Path>) -> Option<PathBuf> {
    if let Ok(p) = std::env::var("VICUNA_FX_PATH") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let mut dirs: Vec<PathBuf> = Vec::new();
    #[cfg(debug_assertions)]
    dirs.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fx-sidecar"));
    if let Some(dir) = resource_dir {
        dirs.push(dir.join("fx-sidecar"));
        dirs.push(dir.to_path_buf());
    }
    for dir in dirs {
        if let Some(p) = fx_binary_in(&dir) {
            return Some(p);
        }
    }
    // Last resort: a user-installed `fx` on PATH.
    let name = if cfg!(windows) { "fx.exe" } else { "fx" };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn fx_binary_in(dir: &Path) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let names: &[&str] = if cfg!(target_arch = "aarch64") {
        &["fx-arm64", "fx"]
    } else {
        &["fx-x64", "fx"]
    };
    #[cfg(not(target_os = "macos"))]
    let names: &[&str] = if cfg!(windows) { &["fx.exe"] } else { &["fx"] };
    for name in names {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

pub fn fx_available(resource_dir: Option<&Path>) -> bool {
    fx_binary_path(resource_dir).is_some()
}

/// Diagnostics for Settings → General and the Work header: which engine a
/// Work turn will actually use, where it came from, and its version.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkEngineInfo {
    pub engine: String,
    pub fx_available: bool,
    pub fx_path: Option<String>,
    pub fx_version: Option<String>,
    pub settings_path: Option<String>,
    pub log_path: Option<String>,
}

pub fn engine_info(resource_dir: Option<&Path>) -> WorkEngineInfo {
    let path = fx_binary_path(resource_dir);
    let fx_available = path.is_some();
    let fx_version = path.as_deref().and_then(fx_version_of);
    WorkEngineInfo {
        engine: if fx_available { "fx" } else { "native" }.to_string(),
        fx_available,
        fx_path: path.as_ref().map(|p| p.display().to_string()),
        fx_version,
        settings_path: home_dir()
            .ok()
            .map(|h| h.join(".fx").join("settings.json").display().to_string()),
        log_path: fx_log_path().ok().map(|p| p.display().to_string()),
    }
}

fn fx_version_of(bin: &Path) -> Option<String> {
    // Bundled sidecars carry a pin marker next to the binary.
    if let Some(dir) = bin.parent() {
        if let Ok(v) = std::fs::read_to_string(dir.join("FX_VERSION")) {
            let v = v.trim().to_string();
            if !v.is_empty() {
                return Some(v);
            }
        }
    }
    // PATH install: `fx --version` is a ~2 ms native call.
    let out = std::process::Command::new(bin)
        .arg("--version")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let first = text.lines().next().unwrap_or("").trim().to_string();
    if first.is_empty() {
        None
    } else {
        Some(first)
    }
}

// ---------- fx profile for Ollama ----------

/// `http://host:11434` -> `http://host:11434/v1` (idempotent).
pub fn ollama_v1_base(raw: &str) -> String {
    let trimmed = raw.trim().trim_end_matches('/');
    let base = if trimmed.is_empty() {
        "http://localhost:11434"
    } else {
        trimmed
    };
    if base.ends_with("/v1") {
        base.to_string()
    } else {
        format!("{base}/v1")
    }
}

/// fx v0.0.16 predates `FX_HOME`, so Work mode registers an additive
/// `vicuna-local` connection in the real `~/.fx/settings.json` and selects it
/// with process-local `FX_PROVIDER`/`FX_MODEL` env vars. The user's own
/// `provider`/`models.*` entries are preserved.
fn fx_provider_entry(model: &str, base: &str, context_length: Option<u64>) -> Value {
    let ctx = context_length.unwrap_or(32768).clamp(4096, 2_000_000);
    let max_out = (ctx / 2).clamp(1024, 8192);
    json!({
        "protocol": "openai-chat-completions",
        "base_url": base,
        "auth": { "type": "none" },
        "model_metadata": {
            (model): {
                "context_window": ctx,
                "max_output_tokens": max_out,
                "supports_tool_use": true,
                "supports_vision": true
            }
        }
    })
}

fn apply_fx_settings(doc: &mut Value, model: &str, base: &str, context_length: Option<u64>) {
    let obj = doc.as_object_mut().expect("settings is an object");
    obj.entry("providers")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("providers is an object")
        .insert("vicuna-local".to_string(), fx_provider_entry(model, base, context_length));
    obj.entry("models")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("models is an object")
        .insert("vicuna-local".to_string(), json!(model));
}

fn write_fx_settings(model: &str, base: &str, context_length: Option<u64>) -> Result<PathBuf, String> {
    let home = home_dir()?;
    let dir = home.join(".fx");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create fx config folder: {e}"))?;
    let path = dir.join("settings.json");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&text)
            .map_err(|_| "fx settings.json is not valid JSON; refusing to overwrite it".to_string())?
    };
    if !doc.is_object() {
        return Err("fx settings.json is not a JSON object; refusing to overwrite it".to_string());
    }
    // Do not clobber a non-object providers/models value either.
    for key in ["providers", "models"] {
        if doc.get(key).is_some_and(|v| !v.is_object()) {
            return Err(format!("fx settings.json has a non-object `{key}`; refusing to overwrite it"));
        }
    }
    apply_fx_settings(&mut doc, model, base, context_length);
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| format!("cannot write fx settings: {e}"))?;
    Ok(path)
}

fn fx_log_path() -> Result<PathBuf, String> {
    let dir = home_dir()?.join("vicuna").join("config");
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create Vicuna config folder: {e}"))?;
    Ok(dir.join("fx-acp.log"))
}

// ---------- MCP servers for the ACP session ----------

fn mcp_servers_for_fx() -> Vec<Value> {
    let Ok(servers) = crate::mcp::list_file_servers() else {
        return vec![];
    };
    servers
        .into_iter()
        .filter(|s| s.enabled)
        .filter_map(|s| {
            if s.transport == "stdio" {
                let command = s.command.clone()?;
                if command.is_empty() {
                    return None;
                }
                let exe = resolve_executable(&command[0])?;
                let mut v = json!({
                    "name": s.name,
                    "command": exe,
                    "args": command[1..],
                });
                if let Some(env) = s.env {
                    let env: Vec<Value> = env
                        .into_iter()
                        .map(|(k, val)| json!({ "name": k, "value": val }))
                        .collect();
                    v["env"] = Value::Array(env);
                }
                Some(v)
            } else {
                let url = s.url?;
                let mut v = json!({ "type": "http", "name": s.name, "url": url });
                if let Some(headers) = s.headers {
                    let headers: Vec<Value> = headers
                        .into_iter()
                        .map(|(k, val)| json!({ "name": k, "value": val }))
                        .collect();
                    v["headers"] = Value::Array(headers);
                }
                Some(v)
            }
        })
        .collect()
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

// ---------- JSON-RPC client ----------

struct AcpClient {
    write_tx: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: AtomicU64,
    stderr_tail: Arc<Mutex<String>>,
}

impl AcpClient {
    fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<oneshot::Receiver<Result<Value, String>>, String> {
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
        self.write_tx
            .send(line)
            .map_err(|_| self.dead_error())?;
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

    fn dead_error(&self) -> String {
        let tail = self
            .stderr_tail
            .lock()
            .ok()
            .map(|s| s.trim().to_string())
            .unwrap_or_default();
        if tail.is_empty() {
            "fx acp exited unexpectedly".to_string()
        } else {
            format!("fx acp exited unexpectedly:\n{tail}")
        }
    }
}

fn spawn_fx(
    bin: &Path,
    workspace: &str,
    model: &str,
    log_path: &Path,
) -> Result<(Arc<AcpClient>, mpsc::UnboundedReceiver<Value>, Child), String> {
    let mut cmd = Command::new(bin);
    // Project instructions auto-load in fx (the native loop left them to the
    // model). Cap them so a huge AGENTS.md can't crowd out the 32K work
    // window; the agent can still read the full file with its tools.
    cmd.arg("--context-limit")
        .arg("project_instruction_file_bytes=32768")
        .arg("--context-limit")
        .arg("project_instructions_total_bytes=65536")
        .arg("acp")
        .arg("--log-file")
        .arg(log_path)
        .current_dir(workspace)
        .env("FX_PROVIDER", "vicuna-local")
        .env("FX_MODEL", model)
        .env("FX_SOUND", "off")
        .env("FX_AUTO_UPGRADE", "0")
        .env("NO_COLOR", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start fx acp ({}): {e}", bin.display()))?;
    let mut stdin = child.stdin.take().ok_or("fx acp: no stdin")?;
    let stdout = child.stdout.take().ok_or("fx acp: no stdout")?;
    let stderr = child.stderr.take().ok_or("fx acp: no stderr")?;

    let (write_tx, mut write_rx) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(line) = write_rx.recv().await {
            if stdin.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if stdin.write_all(b"\n").await.is_err() {
                break;
            }
            if stdin.flush().await.is_err() {
                break;
            }
        }
    });

    let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let stderr_tail = Arc::new(Mutex::new(String::new()));
    let (updates_tx, updates_rx) = mpsc::unbounded_channel::<Value>();

    let pending_reader = pending.clone();
    let write_reader = write_tx.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let id = msg.get("id").and_then(|v| v.as_u64());
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
                    let _ = write_reader.send(reply.to_string());
                }
                (Some(id), None) => {
                    if let Some(tx) = pending_reader.lock().ok().and_then(|mut m| m.remove(&id)) {
                        let result = match msg.get("error") {
                            Some(err) => Err(jsonrpc_error_text(err)),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(result);
                    }
                }
                (None, Some(method)) => {
                    if method == "session/update" {
                        let _ = updates_tx.send(msg);
                    }
                }
                (None, None) => {}
            }
        }
        // EOF: unblock every waiter.
        if let Ok(mut m) = pending_reader.lock() {
            for (_, tx) in m.drain() {
                let _ = tx.send(Err("fx acp closed the connection".to_string()));
            }
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
        next_id: AtomicU64::new(0),
        stderr_tail,
    });
    Ok((client, updates_rx, child))
}

fn jsonrpc_error_text(err: &Value) -> String {
    let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
    let message = err
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown error");
    format!("fx acp error {code}: {message}")
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
            let pick = |kind: &str| {
                options.iter().find_map(|opt| {
                    let k = opt.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                    if k == kind {
                        opt.get("optionId")
                            .and_then(|id| id.as_str())
                            .map(|id| id.to_string())
                    } else {
                        None
                    }
                })
            };
            let id = pick("allow_once")
                .or_else(|| pick("allow_always"))
                .or_else(|| {
                    options.iter().find_map(|opt| {
                        let k = opt.get("kind").and_then(|k| k.as_str()).unwrap_or("");
                        if k.starts_with("allow") {
                            opt.get("optionId")
                                .and_then(|id| id.as_str())
                                .map(|id| id.to_string())
                        } else {
                            None
                        }
                    })
                });
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

// ---------- prompt + update translation ----------

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
    // fx's glob/search tools can arrive as kind "read" with a pattern in
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

#[allow(clippy::too_many_arguments)]
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
                // Defensive: fx versions that ignore session.notices send the
                // context-truncation diagnostic as plain message text.
                let trimmed = text.trim_start();
                if trimmed.starts_with("[context]")
                    && (trimmed.contains("truncated") || trimmed.contains("project instruction"))
                {
                    return;
                }
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
            if kind == "tool_call" && !tools.contains_key(&id) {
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
            let terminal = matches!(status, "completed" | "failed" | "cancelled");
            if terminal {
                let entry = tools.entry(id).or_insert_with(|| ToolState {
                    name: tool_name_and_args(kind_hint, title, &json!({})).0,
                    ended: false,
                });
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
        "notice" => {
            let severity = update
                .get("severity")
                .and_then(|s| s.as_str())
                .unwrap_or("info");
            let title = update
                .get("title")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            let desc = update
                .get("description")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .trim();
            // fx's project-instruction/context truncation notice is noisy in
            // the chat (it repeats on every turn for big AGENTS.md files).
            // It is still in the fx log when someone needs to raise the limit.
            let noise = format!("{title} {desc}").to_lowercase();
            let context_noise = noise.contains("project instruction")
                || noise.contains("truncated")
                || noise.contains("context limit")
                || noise.contains("context-limit");
            if severity == "info" || context_noise {
                return;
            }
            let text = match (title.is_empty(), desc.is_empty()) {
                (false, false) => format!("\n> {title} — {desc}\n"),
                (false, true) => format!("\n> {title}\n"),
                (true, false) => format!("\n> {desc}\n"),
                (true, true) => String::new(),
            };
            if !text.is_empty() {
                emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text });
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

/// On a brand-new fx session with existing Vicuna history (fork, app
/// restart), hand fx the visible transcript so the work continues in context.
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

/// fx steering accepts text blocks only; over-long or image-bearing queued
/// messages are sent as a normal follow-up prompt instead.
fn steer_body(messages: &[ChatMessage]) -> Option<String> {
    if messages
        .iter()
        .any(|m| m.images.as_ref().is_some_and(|imgs| !imgs.is_empty()))
    {
        return None;
    }
    let (text, _) = messages_text(messages);
    if text.trim().is_empty() || text.chars().count() > 60_000 {
        None
    } else {
        Some(text)
    }
}

fn stop_and_wait(child: &mut Child) {
    let _ = child.start_kill();
}

// ---------- public entry point ----------

/// Run one Work turn through `fx acp`. Emits the same event stream as the
/// built-in loop; internal failures are surfaced as an Error event so the
/// frontend always settles.
#[allow(clippy::too_many_arguments)]
pub async fn run_fx_turn(
    turn: Arc<TurnState>,
    model: String,
    workspace: String,
    messages: Vec<ChatMessage>,
    session_id: Option<String>,
    goal_text: Option<String>,
    context_length: Option<u64>,
    ollama_base: Option<String>,
    fresh_session: bool,
    on_event: Channel<WorkAgentEvent>,
    resource_dir: Option<PathBuf>,
    state: &AcpState,
) {
    if let Err(message) = run_fx_turn_inner(
        &turn,
        &turn.stop,
        &model,
        &workspace,
        &messages,
        session_id.as_deref(),
        goal_text.as_deref(),
        context_length,
        ollama_base.as_deref(),
        fresh_session,
        &on_event,
        resource_dir.as_deref(),
        state,
    )
    .await
    {
        let _ = on_event.send(WorkAgentEvent::Error { message });
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_fx_turn_inner(
    turn: &Arc<TurnState>,
    stop: &AtomicBool,
    model: &str,
    workspace: &str,
    messages: &[ChatMessage],
    session_id: Option<&str>,
    goal_text: Option<&str>,
    context_length: Option<u64>,
    ollama_base: Option<&str>,
    fresh_session: bool,
    on_event: &Channel<WorkAgentEvent>,
    resource_dir: Option<&Path>,
    state: &AcpState,
) -> Result<(), String> {
    let bin = fx_binary_path(resource_dir).ok_or_else(|| {
        "fx acp sidecar not found. Run scripts/fetch-fx-sidecar.sh (or install fx).".to_string()
    })?;
    let last_user = last_user_message(messages)
        .ok_or_else(|| "fx acp: no user message to send".to_string())?;
    let base = ollama_v1_base(ollama_base.unwrap_or(""));
    write_fx_settings(model, &base, context_length)?;
    let log_path = fx_log_path()?;

    let mcp = mcp_servers_for_fx();
    let (client, mut updates, mut child) = spawn_fx(&bin, workspace, model, &log_path)?;

    // initialize
    let init = client
        .call(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL_VERSION,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                    // Structured notices keep fx's context/limit diagnostics
                    // out of the assistant transcript (we can filter them).
                    "session": { "notices": {} }
                },
                "clientInfo": { "name": "vicuna", "title": "Vicuna", "version": "0.1.0" }
            }),
            INIT_TIMEOUT,
        )
        .await
        .map_err(|e| {
            format!("fx acp initialize failed: {e}. Check the Ollama endpoint and that the bundled fx sidecar runs.")
        })?;
    let _ = init;

    let key = session_key(session_id, workspace, model);
    let mut resumed = false;
    let fx_session: String;
    if let Some(prev) = if fresh_session {
        None
    } else {
        key.as_deref().and_then(|k| state.get(k))
    } {
        let resume = client
            .call(
                "session/resume",
                json!({
                    "sessionId": prev,
                    "cwd": workspace,
                    "mcpServers": mcp
                }),
                SESSION_TIMEOUT,
            )
            .await;
        match resume {
            Ok(_) => {
                fx_session = prev;
                resumed = true;
            }
            Err(_) => {
                // Saved fx session disappeared (profile purge / version
                // change). Start clean; history preamble keeps context.
                fx_session = new_session(&client, workspace, mcp.clone(), goal_text, session_id).await?;
            }
        }
    } else {
        fx_session = new_session(&client, workspace, mcp.clone(), goal_text, session_id).await?;
    }
    if let Some(k) = key {
        state.set(k, fx_session.clone());
    }

    // First prompt: on a fresh fx session, prepend the visible transcript
    // (forks/restarts). Resumed sessions already hold the conversation.
    // The goal is repeated on every turn because a resumed fx session keeps
    // its original system prompt.
    let goal_note = goal_text
        .filter(|g| !g.trim().is_empty())
        .map(|g| format!("\n\n[Session goal: {g} — keep working until it is fully achieved.]"))
        .unwrap_or_default();
    let base_text = format!("{}{}", last_user.content, goal_note);
    let text = if resumed {
        base_text
    } else {
        format!("{}{}", history_preamble(messages, last_user), base_text)
    };
    let mut blocks = prompt_blocks(&text, &last_user.images);

    let mut first_round = true;
    let mut carry: Vec<ChatMessage> = Vec::new();
    let (steer_tx, mut steer_results) = mpsc::unbounded_channel::<(u64, Result<Value, String>)>();
    let mut steer_seq = 0u64;
    let mut steer_in_flight: Option<u64> = None;
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
            json!({ "sessionId": fx_session, "prompt": blocks }),
        )?;
        tokio::pin!(rx);
        let mut output_chars = 0usize;
        let mut tools: HashMap<String, ToolState> = HashMap::new();
        let mut cancelled = false;
        let mut cancel_deadline: Option<Instant> = None;
        let mut steer_msgs: Vec<ChatMessage> = Vec::new();
        let outcome: Result<Value, String> = loop {
            tokio::select! {
                update = updates.recv() => {
                    match update {
                        Some(msg) => handle_session_update(
                            &msg, on_event, stop, &mut tools, &mut output_chars
                        ),
                        None => break Err("fx acp closed the connection".to_string()),
                    }
                }
                result = &mut rx => break result.map_err(|_| "acp: response channel dropped".to_string()).and_then(|r| r),
                steer = steer_results.recv() => {
                    if let Some((id, res)) = steer {
                        if steer_in_flight == Some(id) {
                            steer_in_flight = None;
                            let absorbed = match &res {
                                Ok(v) => v.pointer("/_meta/fx/steering").and_then(|s| s.as_str()) != Some("dropped"),
                                Err(_) => false,
                            };
                            if absorbed {
                                steer_msgs.clear();
                            } else {
                                carry.extend(std::mem::take(&mut steer_msgs));
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(120)) => {
                    if stop.load(Ordering::Relaxed) && !cancelled {
                        cancelled = true;
                        cancel_deadline = Some(Instant::now());
                        let _ = client.notify("session/cancel", json!({ "sessionId": fx_session }));
                    }
                    if let Some(deadline) = cancel_deadline {
                        if deadline.elapsed() > CANCEL_GRACE {
                            break Ok(json!({ "stopReason": "cancelled" }));
                        }
                    }
                    // Mid-turn steering: fx accepts a second session/prompt
                    // with _meta.fx.steer:true and folds it into the running
                    // turn. Only one may be in flight; dropped steers become
                    // the next prompt in this turn.
                    if !stop.load(Ordering::Relaxed) && steer_in_flight.is_none() {
                        let queued = take_queue(turn);
                        if !queued.is_empty() {
                            let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
                            let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
                            let body = steer_body(&drained);
                            emit_ev(
                                on_event,
                                stop,
                                WorkAgentEvent::QueueDrained { ids, messages: drained.clone() },
                            );
                            match body {
                                Some(text) => {
                                    match client.request(
                                        "session/prompt",
                                        json!({
                                            "sessionId": fx_session,
                                            "prompt": [{ "type": "text", "text": text }],
                                            "_meta": { "fx": { "steer": true } }
                                        }),
                                    ) {
                                        Ok(srx) => {
                                            steer_seq += 1;
                                            let id = steer_seq;
                                            steer_in_flight = Some(id);
                                            steer_msgs = drained;
                                            let tx = steer_tx.clone();
                                            tokio::spawn(async move {
                                                let res = match srx.await {
                                                    Ok(r) => r,
                                                    Err(_) => Err("acp: steering response dropped".to_string()),
                                                };
                                                let _ = tx.send((id, res));
                                            });
                                        }
                                        Err(_) => carry.extend(drained),
                                    }
                                }
                                None => carry.extend(drained),
                            }
                        }
                    }
                }
            }
        };
        // A steer still in flight when the main prompt settled: wait briefly
        // for its verdict; dropped messages re-run as the next prompt.
        if let Some(id) = steer_in_flight.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut absorbed = false;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                match tokio::time::timeout(left, steer_results.recv()).await {
                    Ok(Some((got, Ok(v)))) if got == id => {
                        absorbed = v
                            .pointer("/_meta/fx/steering")
                            .and_then(|s| s.as_str())
                            != Some("dropped");
                        break;
                    }
                    Ok(Some(_)) => continue, // stale result from an earlier steer
                    _ => break,
                }
            }
            if absorbed {
                steer_msgs.clear();
            } else {
                carry.extend(std::mem::take(&mut steer_msgs));
            }
        }
        // Drain trailing updates that arrived before the prompt response.
        while let Ok(msg) = updates.try_recv() {
            handle_session_update(&msg, on_event, stop, &mut tools, &mut output_chars);
        }
        let gen_ms = started.elapsed().as_millis() as u64;
        let secs = (gen_ms as f64 / 1000.0).max(0.001);
        let gen_tps = if output_chars > 0 {
            Some((output_chars as f64 / 4.0) / secs)
        } else {
            None
        };
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
                return Err(e);
            }
        }

        // Anything queued after the last poll: fresh bubbles, emit them.
        let queued = take_queue(turn);
        if !queued.is_empty() {
            let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
            let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
            emit_ev(
                on_event,
                stop,
                WorkAgentEvent::QueueDrained { ids, messages: drained.clone() },
            );
            carry.extend(drained);
        }
        if carry.is_empty() {
            break;
        }
        let drained = std::mem::take(&mut carry);
        let (next_text, next_images) = messages_text(&drained);
        blocks = prompt_blocks(&next_text, &next_images);
        first_round = false;
    }

    stop_and_wait(&mut child);
    emit_ev(
        on_event,
        stop,
        WorkAgentEvent::Done {
            stopped: stop.load(Ordering::Relaxed),
        },
    );
    Ok(())
}

async fn new_session(
    client: &AcpClient,
    workspace: &str,
    mcp: Vec<Value>,
    goal_text: Option<&str>,
    session_id: Option<&str>,
) -> Result<String, String> {
    let prompt = system_prompt(workspace, goal_text, session_id);
    let result = client
        .call(
            "session/new",
            json!({
                "cwd": workspace,
                "mcpServers": mcp,
                "systemPrompt": [{ "type": "text", "text": prompt }]
            }),
            SESSION_TIMEOUT,
        )
        .await?;
    result
        .get("sessionId")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "fx acp: session/new returned no sessionId".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ollama_base_normalization() {
        assert_eq!(ollama_v1_base(""), "http://localhost:11434/v1");
        assert_eq!(
            ollama_v1_base("http://localhost:11434/"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            ollama_v1_base("http://localhost:11434/v1"),
            "http://localhost:11434/v1"
        );
        assert_eq!(
            ollama_v1_base("https://box.ts.net/ollama"),
            "https://box.ts.net/ollama/v1"
        );
    }

    #[test]
    fn settings_add_vicuna_provider_without_touching_user_choices() {
        let mut doc = json!({
            "provider": "gateway",
            "models": { "gateway": "spacexai/grok-4.7" },
            "providers": { "work": { "protocol": "openai-chat-completions", "base_url": "https://example.test/v1" } }
        });
        apply_fx_settings(&mut doc, "qwen3.8:27b", "http://localhost:11434/v1", Some(65536));
        assert_eq!(doc["provider"], "gateway");
        assert_eq!(doc["models"]["gateway"], "spacexai/grok-4.7");
        assert_eq!(doc["models"]["vicuna-local"], "qwen3.8:27b");
        assert_eq!(
            doc["providers"]["vicuna-local"]["base_url"],
            "http://localhost:11434/v1"
        );
        assert_eq!(
            doc["providers"]["vicuna-local"]["protocol"],
            "openai-chat-completions"
        );
        assert_eq!(
            doc["providers"]["vicuna-local"]["model_metadata"]["qwen3.8:27b"]["context_window"],
            65536
        );
        assert_eq!(doc["providers"]["work"]["base_url"], "https://example.test/v1");
    }

    #[test]
    fn binary_lookup_prefers_arch_suffix() {
        let dir = std::env::temp_dir().join(format!("vicuna-fx-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let name = if cfg!(target_os = "macos") {
            if cfg!(target_arch = "aarch64") { "fx-arm64" } else { "fx-x64" }
        } else if cfg!(windows) {
            "fx.exe"
        } else {
            "fx"
        };
        let path = dir.join(name);
        std::fs::write(&path, b"x").unwrap();
        assert_eq!(fx_binary_in(&dir), Some(path.clone()));
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn tool_mapping_reads_like_native_names() {
        let (name, args) = tool_name_and_args("execute", "Run tests", &json!({ "command": "cargo test" }));
        assert_eq!(name, "exec");
        assert_eq!(args["command"], "cargo test");
        let (name, args) = tool_name_and_args("read", "src/main.rs", &json!({ "path": "src/main.rs" }));
        assert_eq!(name, "read");
        assert_eq!(args["path"], "src/main.rs");
        let (name, args) = tool_name_and_args(
            "read",
            "Matching",
            &json!({ "pattern": "**/*.md", "path": "." }),
        );
        assert_eq!(name, "grep");
        assert_eq!(args["pattern"], "**/*.md");
    }

    #[test]
    fn permission_auto_approves_allow_once() {
        let params = json!({
            "options": [
                { "kind": "allow_always", "optionId": "always" },
                { "kind": "allow_once", "optionId": "once" }
            ]
        });
        let out = handle_agent_request("session/request_permission", &params).unwrap();
        assert_eq!(out["outcome"]["optionId"], "once");
    }
}
