// Vicuna MCP client: connects to Model Context Protocol servers (local
// stdio commands like `lb mcp`, or remote streamable-HTTP endpoints) and
// exposes their tools to the chat tool loop. One method call at a time per
// server (a single manager-wide lock serializes requests), which keeps the
// newline-delimited stdio protocol trivially matchable by request id.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tokio::time::timeout;

const MCP_VERSION: &str = "2024-11-05";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Server config as stored in ~/vicuna/config/vicuna.json (opencode.json
/// shape: `mcp` maps name → {command, type local/remote, url, env,
/// enabled}). `transport` is "stdio" (spawn `command`) or "http" (POST
/// JSON-RPC to `url`). The opencode spellings "local"/"remote" are accepted
/// and normalized; `id` is the map key (= display name).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub id: String,
    pub name: String,
    pub transport: String,
    pub command: Option<Vec<String>>,
    pub env: Option<HashMap<String, String>>,
    pub url: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

impl McpServerConfig {
    fn normalized_transport(&self) -> &str {
        match self.transport.as_str() {
            "local" => "stdio",
            "remote" => "http",
            other => other,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct McpToolInfo {
    pub name: String,
    pub description: Option<String>,
    #[serde(rename = "inputSchema")]
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpServerInfo {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub tools: Vec<McpToolInfo>,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpCallResult {
    pub content: String,
    pub is_error: bool,
}

enum Backend {
    Stdio {
        child: Child,
        reader: BufReader<ChildStdout>,
        writer: ChildStdin,
        next_id: u64,
    },
    Remote {
        url: String,
        headers: HashMap<String, String>,
        next_id: u64,
    },
}

struct Connected {
    name: String,
    backend: Backend,
    tools: Vec<McpToolInfo>,
}

pub struct McpManager {
    inner: Mutex<HashMap<String, Connected>>,
}

impl Default for McpManager {
    fn default() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }
}

/// Resolve a bare binary name for spawning. Desktop shells get a full PATH,
/// but Finder-launched apps don't — so fall back to the usual install
/// locations (same problem as the `ollama` lookup in lib.rs).
fn resolve_command(prog: &str) -> Option<String> {
    if prog.contains('/') {
        let p = PathBuf::from(prog);
        return p.is_file().then(|| prog.to_string());
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs.iter()
        .map(|d| d.join(prog))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Write one JSON-RPC request line and read response lines until the matching
/// id arrives. Server notifications (no id) are skipped; server-to-client
/// requests get a "not supported" error reply so neither side hangs.
async fn stdio_request(
    reader: &mut BufReader<ChildStdout>,
    writer: &mut ChildStdin,
    id: u64,
    method: &str,
    params: serde_json::Value,
    per_call_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
    .to_string()
        + "\n";
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("mcp: failed writing to server: {e}"))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("mcp: failed flushing to server: {e}"))?;
    loop {
        let mut buf = String::new();
        timeout(per_call_timeout, reader.read_line(&mut buf))
            .await
            .map_err(|_| format!("mcp: timed out waiting for '{method}' response"))?
            .map_err(|e| format!("mcp: failed reading from server: {e}"))?;
        if buf.trim().is_empty() {
            continue;
        }
        let msg: serde_json::Value =
            serde_json::from_str(&buf).map_err(|e| format!("mcp: bad JSON from server: {e}"))?;
        let msg_id = msg.get("id");
        if msg.get("method").is_some() && msg_id.is_some() {
            // a request addressed at us (e.g. ping): decline politely
            let reply = serde_json::json!({
                "jsonrpc": "2.0",
                "id": msg_id,
                "error": { "code": -32601, "message": "not supported" },
            })
            .to_string()
                + "\n";
            let _ = writer.write_all(reply.as_bytes()).await;
            let _ = writer.flush().await;
            continue;
        }
        if msg_id.and_then(|v| v.as_u64()) != Some(id) {
            continue; // notification or someone else's response
        }
        if let Some(err) = msg.get("error") {
            return Err(format!("mcp: server error on '{method}': {err}"));
        }
        return Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null));
    }
}

async fn stdio_notify(
    writer: &mut ChildStdin,
    method: &str,
) -> Result<(), String> {
    let line = serde_json::json!({ "jsonrpc": "2.0", "method": method }).to_string() + "\n";
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("mcp: failed writing to server: {e}"))?;
    writer
        .flush()
        .await
        .map_err(|e| format!("mcp: failed flushing to server: {e}"))?;
    Ok(())
}

/// POST one JSON-RPC request to a streamable-HTTP endpoint. Accepts a plain
/// JSON body or an SSE stream (`data:` lines); single-flight per server, so
/// the last JSON payload wins. `bearer` (OAuth access token) is sent only
/// when the caller didn't configure an explicit Authorization header.
async fn remote_request(
    url: &str,
    headers: &HashMap<String, String>,
    bearer: Option<&str>,
    id: u64,
    method: &str,
    params: serde_json::Value,
    per_call_timeout: Duration,
) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(per_call_timeout)
        .build()
        .map_err(|e| format!("mcp: {e}"))?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
    let mut has_auth = false;
    for (k, v) in headers {
        if k.eq_ignore_ascii_case("authorization") {
            has_auth = true;
        }
        req = req.header(k, v);
    }
    if !has_auth {
        if let Some(token) = bearer.filter(|t| !t.is_empty()) {
            req = req.header("Authorization", format!("Bearer {token}"));
        }
    }
    let res = req
        .send()
        .await
        .map_err(|e| format!("mcp: request to {url} failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(300).collect();
        return Err(format!("mcp: {url} rejected '{method}': {status} {snippet}"));
    }
    let is_sse = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.contains("text/event-stream"));
    let body = res
        .text()
        .await
        .map_err(|e| format!("mcp: failed reading {url} response: {e}"))?;
    let payload = if is_sse {
        let mut last: Option<&str> = None;
        for line in body.lines() {
            let line = line.trim();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if !data.is_empty() && data != "[DONE]" {
                    last = Some(data);
                }
            }
        }
        last.ok_or_else(|| "mcp: empty SSE stream from server".to_string())?
    } else {
        body.trim()
    };
    let msg: serde_json::Value =
        serde_json::from_str(payload).map_err(|e| format!("mcp: bad JSON from {url}: {e}"))?;
    if let Some(err) = msg.get("error") {
        return Err(format!("mcp: server error on '{method}': {err}"));
    }
    Ok(msg.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

async fn remote_notify(url: &str, headers: &HashMap<String, String>, method: &str) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build();
    let Ok(client) = client else { return };
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({ "jsonrpc": "2.0", "method": method }));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let _ = req.send().await;
}

fn parse_tools(result: &serde_json::Value) -> Result<Vec<McpToolInfo>, String> {
    let arr = result
        .get("tools")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "mcp: tools/list returned no tools array".to_string())?;
    let mut tools = Vec::with_capacity(arr.len());
    for t in arr {
        let name = t
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "mcp: tool without a name".to_string())?;
        tools.push(McpToolInfo {
            name: name.to_string(),
            description: t
                .get("description")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
            input_schema: t.get("inputSchema").cloned().unwrap_or(serde_json::json!({
                "type": "object",
            })),
        });
    }
    Ok(tools)
}

/// MCP `tools/call` content blocks flattened to plain text. Binary blocks
/// become placeholders so giant payloads never enter the model context.
fn flatten_content(result: &serde_json::Value) -> (String, bool) {
    let is_error = result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let mut parts = Vec::new();
    if let Some(items) = result.get("content").and_then(|v| v.as_array()) {
        for it in items {
            let kind = it.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match kind {
                "text" => parts.push(
                    it.get("text")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                ),
                "image" | "audio" => parts.push(format!("[{kind} content omitted]")),
                "resource" => {
                    let embedded = it.get("resource").cloned().unwrap_or_default();
                    let text = embedded
                        .get("text")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            embedded
                                .get("uri")
                                .and_then(|v| v.as_str())
                                .map(|u| format!("[resource: {u}]"))
                        })
                        .unwrap_or_else(|| "[resource]".to_string());
                    parts.push(text);
                }
                _ => parts.push(it.to_string()),
            }
        }
    }
    (parts.join("\n"), is_error)
}

impl McpManager {
    pub async fn connect(&self, cfg: &McpServerConfig) -> Result<McpServerInfo, String> {
        if cfg.id.trim().is_empty() {
            return Err("mcp: server id is required".to_string());
        }
        if cfg.name.trim().is_empty() {
            return Err("mcp: server name is required".to_string());
        }
        // connecting twice replaces the old connection (stale child killed)
        self.disconnect(&cfg.id).await.ok();
        let mut guard = self.inner.lock().await;
        match cfg.normalized_transport() {
            "stdio" => {
                let argv = cfg.command.clone().unwrap_or_default();
                if argv.is_empty() {
                    return Err(format!("mcp: '{}' needs a command to spawn", cfg.name));
                }
                let bin = resolve_command(&argv[0]).ok_or_else(|| {
                    format!(
                        "mcp: command '{}' not found (checked PATH, ~/.local/bin, /opt/homebrew/bin, /usr/local/bin)",
                        argv[0]
                    )
                })?;
                let mut cmd = Command::new(bin);
                cmd.args(&argv[1..])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .kill_on_drop(true);
                if let Some(env) = cfg.env.as_ref() {
                    cmd.envs(env);
                }
                let mut child = cmd.spawn().map_err(|e| {
                    format!("mcp: could not spawn '{}': {e}", argv.join(" "))
                })?;
                let stdout: ChildStdout = child.stdout.take().ok_or_else(|| {
                    "mcp: could not pipe server stdout".to_string()
                })?;
                let stdin: ChildStdin = child.stdin.take().ok_or_else(|| {
                    "mcp: could not pipe server stdin".to_string()
                })?;
                let mut reader = BufReader::new(stdout);
                let mut writer = stdin;
                let init = timeout(
                    CONNECT_TIMEOUT,
                    stdio_request(
                        &mut reader,
                        &mut writer,
                        0,
                        "initialize",
                        serde_json::json!({
                            "protocolVersion": MCP_VERSION,
                            "capabilities": {},
                            "clientInfo": { "name": "vicuna", "version": "0.1.0" },
                        }),
                        CONNECT_TIMEOUT,
                    ),
                )
                .await
                .map_err(|_| {
                    format!(
                        "mcp: '{}' did not answer initialize in time",
                        cfg.name
                    )
                })??;
                if init.get("protocolVersion").is_none() {
                    let _ = child.start_kill();
                    return Err(format!("mcp: '{}' gave a bad initialize reply", cfg.name));
                }
                stdio_notify(&mut writer, "notifications/initialized").await?;
                let listed = timeout(
                    CONNECT_TIMEOUT,
                    stdio_request(&mut reader, &mut writer, 1, "tools/list", serde_json::json!({}), CONNECT_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: '{}' did not answer tools/list in time", cfg.name))??;
                let tools = parse_tools(&listed).map_err(|e| {
                    let _ = child.start_kill();
                    e
                })?;
                guard.insert(
                    cfg.id.clone(),
                    Connected {
                        name: cfg.name.clone(),
                        backend: Backend::Stdio {
                            child,
                            reader,
                            writer,
                            next_id: 2,
                        },
                        tools: tools.clone(),
                    },
                );
                Ok(McpServerInfo {
                    id: cfg.id.clone(),
                    name: cfg.name.clone(),
                    connected: true,
                    tools,
                })
            }
            "http" => {
                let url = cfg.url.clone().unwrap_or_default();
                if !(url.starts_with("http://") || url.starts_with("https://")) {
                    return Err(format!(
                        "mcp: '{}' needs an http(s) URL for remote transport",
                        cfg.name
                    ));
                }
                let headers = cfg.headers.clone().unwrap_or_default();
                // OAuth: attach a fresh access token when one is stored
                // (refreshing it first when expired). A 401 here means the
                // token is missing or dead — never retry blindly.
                let bearer = http_bearer(&cfg.name).await;
                let init = timeout(
                    CONNECT_TIMEOUT,
                    remote_request(
                        &url,
                        &headers,
                        bearer.as_deref(),
                        0,
                        "initialize",
                        serde_json::json!({
                            "protocolVersion": MCP_VERSION,
                            "capabilities": {},
                            "clientInfo": { "name": "vicuna", "version": "0.1.0" },
                        }),
                        CONNECT_TIMEOUT,
                    ),
                )
                .await
                .map_err(|_| format!("mcp: {url} did not answer initialize in time"))?;
                let init = match init {
                    Ok(v) => v,
                    Err(e) => return Err(auth_aware_error(&cfg.name, bearer.is_some(), e)),
                };
                if init.get("protocolVersion").is_none() {
                    return Err(format!("mcp: {url} gave a bad initialize reply"));
                }
                remote_notify(&url, &headers, "notifications/initialized").await;
                let listed = timeout(
                    CONNECT_TIMEOUT,
                    remote_request(&url, &headers, bearer.as_deref(), 1, "tools/list", serde_json::json!({}), CONNECT_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: {url} did not answer tools/list in time"))?;
                let listed = match listed {
                    Ok(v) => v,
                    Err(e) => return Err(auth_aware_error(&cfg.name, bearer.is_some(), e)),
                };
                let tools = parse_tools(&listed)?;
                guard.insert(
                    cfg.id.clone(),
                    Connected {
                        name: cfg.name.clone(),
                        backend: Backend::Remote {
                            url,
                            headers,
                            next_id: 2,
                        },
                        tools: tools.clone(),
                    },
                );
                Ok(McpServerInfo {
                    id: cfg.id.clone(),
                    name: cfg.name.clone(),
                    connected: true,
                    tools,
                })
            }
            other => Err(format!("mcp: unknown transport '{other}' (use stdio or http)")),
        }
    }

    pub async fn disconnect(&self, id: &str) -> Result<(), String> {
        let mut guard = self.inner.lock().await;
        match guard.remove(id) {
            Some(conn) => {
                if let Backend::Stdio { mut child, .. } = conn.backend {
                    let _ = child.start_kill();
                }
                Ok(())
            }
            None => Err(format!("mcp: '{id}' is not connected")),
        }
    }

    pub async fn list_tools(&self, id: &str) -> Result<Vec<McpToolInfo>, String> {
        let mut guard = self.inner.lock().await;
        let conn = guard
            .get_mut(id)
            .ok_or_else(|| format!("mcp: '{id}' is not connected"))?;
        let name = conn.name.clone();
        let tools = match &mut conn.backend {
            Backend::Stdio { reader, writer, next_id, .. } => {
                let idn = *next_id;
                *next_id += 1;
                let listed = timeout(
                    CONNECT_TIMEOUT,
                    stdio_request(reader, writer, idn, "tools/list", serde_json::json!({}), CONNECT_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: '{name}' did not answer tools/list in time"))??;
                parse_tools(&listed)?
            }
            Backend::Remote { url, headers, next_id } => {
                let idn = *next_id;
                *next_id += 1;
                let bearer = http_bearer(&name).await;
                let listed = timeout(
                    CONNECT_TIMEOUT,
                    remote_request(url, headers, bearer.as_deref(), idn, "tools/list", serde_json::json!({}), CONNECT_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: '{name}' did not answer tools/list in time"))?;
                let listed = match listed {
                    Ok(v) => v,
                    Err(e) => return Err(auth_aware_error(&name, bearer.is_some(), e)),
                };
                parse_tools(&listed)?
            }
        };
        conn.tools = tools.clone();
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        server_id: &str,
        tool: &str,
        args: serde_json::Value,
    ) -> Result<McpCallResult, String> {
        let mut guard = self.inner.lock().await;
        let conn = guard
            .get_mut(server_id)
            .ok_or_else(|| format!("mcp: '{server_id}' is not connected"))?;
        let name = conn.name.clone();
        let params = serde_json::json!({ "name": tool, "arguments": args });
        let result = match &mut conn.backend {
            Backend::Stdio { reader, writer, next_id, .. } => {
                let idn = *next_id;
                *next_id += 1;
                timeout(
                    CALL_TIMEOUT,
                    stdio_request(reader, writer, idn, "tools/call", params, CALL_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: tool '{tool}' on '{name}' timed out"))??
            }
            Backend::Remote { url, headers, next_id } => {
                let idn = *next_id;
                *next_id += 1;
                let bearer = http_bearer(&name).await;
                timeout(
                    CALL_TIMEOUT,
                    remote_request(url, headers, bearer.as_deref(), idn, "tools/call", params, CALL_TIMEOUT),
                )
                .await
                .map_err(|_| format!("mcp: tool '{tool}' on '{name}' timed out"))?
                .map_err(|e| auth_aware_error(&name, bearer.is_some(), e))?
            }
        };
        let (content, is_error) = flatten_content(&result);
        Ok(McpCallResult { content, is_error })
    }
}

/// A 401/403 from the HTTP transport means the stored token is missing or
/// dead. `had_bearer` tells whether we sent one: if so it was rejected, so
/// drop it — the next attempt must go through sign-in, not a doomed retry.
fn auth_aware_error(server: &str, had_bearer: bool, err: String) -> String {
    if is_auth_failure(&err) {
        if had_bearer {
            invalidate_auth(server);
        }
        return format!("mcp: sign-in required for '{server}' ({err})");
    }
    err
}

fn is_auth_failure(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains(" 401 ") || e.contains(" 403 ") || e.contains("unauthorized") || e.contains("forbidden")
}

// ---------- MCP OAuth (remote servers like higgsfield) ----------
//
// Flow (MCP auth spec / OAuth 2.1, same shape as opencode's browser login):
// probe `initialize` → on 401 discover the protected-resource metadata →
// authorization-server metadata → dynamic client registration → PKCE
// authorize URL in the system browser → loopback redirect captures the code
// → token exchange. Tokens live in ~/vicuna/config/auth.json (0600),
// keyed by server name; vicuna.json keeps only the server config.

const OAUTH_CALLBACK_TIMEOUT: Duration = Duration::from_secs(180);
const OAUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct McpAuthEntry {
    #[serde(default)]
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(default)]
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    /// unix seconds when the access token expires (0 = unknown: use until 401)
    #[serde(default)]
    expires_at: u64,
    #[serde(default)]
    scopes: String,
    #[serde(default)]
    resource: String,
    #[serde(default)]
    token_endpoint: String,
}

fn auth_config_path() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").map_err(|_| "cannot locate home folder".to_string())?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").map_err(|_| "cannot locate home folder".to_string())?;
    Ok(PathBuf::from(home).join("vicuna").join("config").join("auth.json"))
}

fn read_auth_map_in(path: &Path) -> HashMap<String, McpAuthEntry> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if text.trim().is_empty() {
        return HashMap::new();
    }
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_auth_map_in(path: &Path, map: &HashMap<String, McpAuthEntry>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mcp: cannot create config dir: {e}"))?;
    }
    let text = serde_json::to_string_pretty(map).map_err(|e| format!("mcp: cannot encode auth: {e}"))?;
    std::fs::write(path, text + "\n").map_err(|e| format!("mcp: cannot write auth: {e}"))?;
    // tokens must not be world-readable (opencode's auth.json is 0600 too)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A usable access token for the server, refreshing first when expired.
/// Returns None when no login is stored (or the refresh token is dead, in
/// which case the entry is dropped so the UI offers a clean re-sign-in).
async fn http_bearer(server_name: &str) -> Option<String> {
    let path = auth_config_path().ok()?;
    http_bearer_in(server_name, &path).await
}

async fn http_bearer_in(server_name: &str, auth_path: &Path) -> Option<String> {
    let mut map = read_auth_map_in(auth_path);
    let entry = map.get(server_name)?.clone();
    if entry.access_token.is_empty() {
        return None;
    }
    if entry.expires_at == 0 || now_secs() + 60 < entry.expires_at {
        return Some(entry.access_token);
    }
    let refresh = entry.refresh_token.clone().unwrap_or_default();
    if refresh.is_empty() || entry.token_endpoint.is_empty() {
        return None;
    }
    match refresh_access_token(&entry, &refresh).await {
        Ok((access, expires_at)) => {
            if let Some(e) = map.get_mut(server_name) {
                e.access_token = access.clone();
                e.expires_at = expires_at;
            }
            let _ = write_auth_map_in(auth_path, &map);
            Some(access)
        }
        Err(dead) => {
            // invalid_grant / unknown client: the grant is dead, drop it so
            // the next attempt goes through sign-in instead of retrying
            if dead {
                map.remove(server_name);
                let _ = write_auth_map_in(auth_path, &map);
            }
            None
        }
    }
}

/// Drop the stored login (bad token, or explicit sign-out).
fn invalidate_auth(server_name: &str) {
    if let Ok(path) = auth_config_path() {
        let mut map = read_auth_map_in(&path);
        if map.remove(server_name).is_some() {
            let _ = write_auth_map_in(&path, &map);
        }
    }
}

pub fn auth_present(server_name: &str) -> bool {
    let Ok(path) = auth_config_path() else {
        return false;
    };
    read_auth_map_in(&path).contains_key(server_name)
}

pub fn delete_auth(server_name: &str) {
    invalidate_auth(server_name);
}

/// POST a form to the token endpoint. Err(dead) tells whether the grant
/// itself is dead (re-sign-in needed) vs a transient failure (keep tokens).
async fn token_exchange(
    token_endpoint: &str,
    fields: &[(&str, &str)],
    client_secret: Option<&str>,
) -> Result<(String, Option<String>, u64), bool> {
    let client = reqwest::Client::builder()
        .timeout(OAUTH_REFRESH_TIMEOUT)
        .build()
        .map_err(|_| false)?;
    let mut req = client.post(token_endpoint);
    if let Some(secret) = client_secret.filter(|s| !s.is_empty()) {
        // confidential client: secret goes in the Authorization header
        req = req.header("Authorization", format!("Basic {}", URL_SAFE_NO_PAD.encode(format!("x:{secret}"))));
    }
    let owned: Vec<(String, String)> = fields.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let res = req.form(&owned).send().await.map_err(|_| false)?;
    let status = res.status();
    let body: serde_json::Value = res.json().await.unwrap_or(serde_json::Value::Null);
    if !status.is_success() {
        let err = body.get("error").and_then(|e| e.as_str()).unwrap_or("");
        // dead grant: re-sign-in is the only way forward
        return Err(matches!(err, "invalid_grant" | "invalid_client" | "unauthorized_client"));
    }
    let access = body.get("access_token").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if access.is_empty() {
        return Err(false);
    }
    let refresh = body.get("refresh_token").and_then(|v| v.as_str()).map(|s| s.to_string());
    let expires_at = body
        .get("expires_in")
        .and_then(|v| v.as_u64())
        .filter(|s| *s > 0)
        .map(|s| now_secs() + s)
        .unwrap_or(0);
    Ok((access, refresh, expires_at))
}

async fn refresh_access_token(entry: &McpAuthEntry, refresh: &str) -> Result<(String, u64), bool> {
    let resource;
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh),
        ("client_id", entry.client_id.as_str()),
    ];
    if !entry.resource.is_empty() {
        resource = entry.resource.clone();
        fields.push(("resource", resource.as_str()));
    }
    let (access, _refresh, expires_at) =
        token_exchange(&entry.token_endpoint, &fields, entry.client_secret.as_deref()).await?;
    Ok((access, expires_at))
}

/// Raw probe: POST `initialize` without auth. Ok(result) = open server;
/// Err(challenge) carries the 401's WWW-Authenticate resource metadata (if
/// any); other failures are plain errors.
enum Probe {
    Open,
    Protected { resource_metadata: Option<String> },
}

async fn probe_resource(url: &str, headers: &HashMap<String, String>) -> Result<Probe, String> {
    let client = reqwest::Client::builder()
        .timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("mcp: {e}"))?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 0,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "vicuna", "version": "0.1.0" },
            },
        }));
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let res = req.send().await.map_err(|e| format!("mcp: request to {url} failed: {e}"))?;
    if res.status().is_success() {
        let body: serde_json::Value = res.json().await.map_err(|e| format!("mcp: bad JSON from {url}: {e}"))?;
        if let Some(err) = body.get("error") {
            return Err(format!("mcp: server error on 'initialize': {err}"));
        }
        return Ok(Probe::Open);
    }
    if res.status() == reqwest::StatusCode::UNAUTHORIZED || res.status() == reqwest::StatusCode::FORBIDDEN {
        let meta = res
            .headers()
            .get_all(reqwest::header::WWW_AUTHENTICATE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find_map(parse_resource_metadata)
            .map(|s| s.to_string());
        return Ok(Probe::Protected { resource_metadata: meta });
    }
    let status = res.status();
    let snippet: String = res.text().await.unwrap_or_default().chars().take(200).collect();
    Err(format!("mcp: {url} rejected 'initialize': {status} {snippet}"))
}

/// Pull `resource_metadata="..."` out of a WWW-Authenticate header,
/// tolerating commas inside the quoted URL and sibling challenges.
fn parse_resource_metadata(header: &str) -> Option<&str> {
    let mut rest = header;
    loop {
        let i = rest.find("resource_metadata")?;
        rest = &rest[i + "resource_metadata".len()..];
        rest = rest.trim_start_matches([' ', '\t', '=']);
        if rest.starts_with('"') {
            let end = rest[1..].find('"')?;
            return Some(&rest[1..1 + end]);
        }
    }
}

/// Insert `/.well-known/<suffix>` between origin and path (RFC 8414 §3.1 /
/// RFC 9728 §3.1), e.g. `https://x/mcp` + `oauth-protected-resource` →
/// `https://x/.well-known/oauth-protected-resource/mcp`. A bare origin
/// gets no trailing slash.
fn well_known(url: &str, suffix: &str) -> Option<String> {
    let scheme_end = url.find("://")?;
    let after = &url[scheme_end + 3..];
    let (host, path) = match after.find('/') {
        Some(i) => (&after[..i], &after[i..]),
        None => (after, ""),
    };
    let path = if path == "/" { "" } else { path };
    Some(format!("{}://{}/.well-known/{}{}", &url[..scheme_end], host, suffix, path))
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn random_b64url(n: usize) -> Result<String, String> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf).map_err(|e| format!("mcp: no randomness: {e}"))?;
    Ok(URL_SAFE_NO_PAD.encode(&buf))
}

#[derive(Debug, Deserialize)]
struct ProtectedResourceMetadata {
    #[serde(default)]
    resource: String,
    #[serde(default)]
    authorization_servers: Vec<String>,
    #[serde(default)]
    scopes_supported: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AuthorizationServerMetadata {
    #[serde(default)]
    authorization_endpoint: String,
    #[serde(default)]
    token_endpoint: String,
    #[serde(default)]
    registration_endpoint: Option<String>,
}

async fn fetch_json(url: &str, what: &str) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::builder()
        .timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("mcp: {e}"))?;
    let res = client
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| format!("mcp: {what} fetch from {url} failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("mcp: {what} fetch from {url} failed: {}", res.status()));
    }
    res.json().await.map_err(|e| format!("mcp: bad {what} JSON from {url}: {e}"))
}

/// Full browser sign-in for a remote server. `open_browser` opens the
/// authorize URL (system browser in production; a test hook in tests).
/// Persists tokens to auth.json; the caller connects afterwards, which
/// picks them up. Errors when the server needs no auth so the caller can
/// fall back to a plain connect.
pub async fn oauth_login(
    cfg: &McpServerConfig,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let path = auth_config_path()?;
    oauth_login_in(cfg, &path, open_browser).await
}

async fn oauth_login_in(
    cfg: &McpServerConfig,
    auth_path: &Path,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    let url = cfg.url.clone().unwrap_or_default();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("mcp: '{}' needs an http(s) URL for remote transport", cfg.name));
    }
    let headers = cfg.headers.clone().unwrap_or_default();
    // 1. probe: an open server needs no sign-in
    match probe_resource(&url, &headers).await? {
        Probe::Open => {
            return Err(format!(
                "mcp: '{}' does not require sign-in (plain connect instead)",
                cfg.name
            ))
        }
        Probe::Protected { resource_metadata } => {
            oauth_protected(cfg, auth_path, &url, resource_metadata, open_browser).await
        }
    }
}

async fn oauth_protected(
    cfg: &McpServerConfig,
    auth_path: &Path,
    url: &str,
    resource_metadata: Option<String>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<(), String> {
    // 2. protected-resource metadata: header first, well-known fallback
    let prm_url = resource_metadata.or_else(|| well_known(url, "oauth-protected-resource"));
    let prm_url = prm_url.ok_or_else(|| format!("mcp: '{}' demands auth but gave no discovery URL", cfg.name))?;
    let prm: ProtectedResourceMetadata =
        serde_json::from_value(fetch_json(&prm_url, "protected-resource metadata").await?)
            .map_err(|e| format!("mcp: bad protected-resource metadata from {prm_url}: {e}"))?;
    let issuer = prm.authorization_servers.first().cloned().ok_or_else(|| {
        format!("mcp: '{0}' names no authorization server (metadata at {prm_url})", cfg.name)
    })?;
    let resource = if prm.resource.is_empty() { url.to_string() } else { prm.resource.clone() };
    // 3. authorization-server metadata (oauth first, OpenID fallback)
    let mut as_meta: Option<AuthorizationServerMetadata> = None;
    let mut as_err = String::new();
    for suffix in ["oauth-authorization-server", "openid-configuration"] {
        if let Some(meta_url) = well_known(&issuer, suffix) {
            match fetch_json(&meta_url, "authorization-server metadata").await {
                Ok(v) => match serde_json::from_value::<AuthorizationServerMetadata>(v) {
                    Ok(m) => {
                        as_meta = Some(m);
                        break;
                    }
                    Err(e) => as_err = format!("mcp: bad authorization-server metadata from {meta_url}: {e}"),
                },
                Err(e) => as_err = e,
            }
        }
    }
    let as_meta = as_meta.ok_or_else(|| {
        if as_err.is_empty() {
            format!("mcp: cannot discover authorization server for '{}'", cfg.name)
        } else {
            as_err
        }
    })?;
    if as_meta.authorization_endpoint.is_empty() || as_meta.token_endpoint.is_empty() {
        return Err(format!("mcp: authorization server for '{}' lacks endpoints", cfg.name));
    }
    // 4. dynamic client registration (public client + PKCE, no secret)
    let reg = as_meta.registration_endpoint.ok_or_else(|| {
        format!(
            "mcp: '{0}' requires OAuth but its server offers no dynamic client registration",
            cfg.name
        )
    })?;
    let client = reqwest::Client::builder()
        .timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|e| format!("mcp: {e}"))?;
    let reg_res = client
        .post(&reg)
        .json(&serde_json::json!({
            "redirect_uris": ["http://127.0.0.1/callback"],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "client_name": "Vicuna",
        }))
        .send()
        .await
        .map_err(|e| format!("mcp: client registration with {reg} failed: {e}"))?;
    if !reg_res.status().is_success() {
        let snippet: String = reg_res.text().await.unwrap_or_default().chars().take(200).collect();
        return Err(format!("mcp: client registration failed: {snippet}"));
    }
    let reg_body: serde_json::Value = reg_res.json().await.map_err(|e| format!("mcp: bad registration reply: {e}"))?;
    let client_id = reg_body.get("client_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
    if client_id.is_empty() {
        return Err("mcp: registration gave no client_id".to_string());
    }
    let client_secret = reg_body.get("client_secret").and_then(|v| v.as_str()).map(|s| s.to_string());
    // 5. PKCE + loopback redirect, then open the system browser
    let verifier = random_b64url(32)?;
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_b64url(16)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("mcp: cannot listen for login callback: {e}"))?;
    let port = listener.local_addr().map(|a| a.port()).map_err(|e| format!("mcp: {e}"))?;
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let mut auth_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&resource={}",
        as_meta.authorization_endpoint,
        percent_encode(&client_id),
        percent_encode(&redirect_uri),
        percent_encode(&state),
        percent_encode(&challenge),
        percent_encode(&resource),
    );
    if !prm.scopes_supported.is_empty() {
        auth_url.push_str(&format!("&scope={}", percent_encode(&prm.scopes_supported.join(" "))));
    }
    // 6. wait for the redirect with the code. The accept task starts BEFORE
    //    opening the browser: the browser (or a script driving the URL)
    //    may hit the loopback before open_browser returns.
    let accept_task = tokio::spawn(wait_for_callback(listener));
    open_browser(&auth_url)?;
    let (code, returned_state) = match timeout(
        OAUTH_CALLBACK_TIMEOUT + Duration::from_secs(30),
        accept_task,
    )
    .await
    {
        Err(_) => return Err("mcp: login timed out waiting for the browser (try again)".to_string()),
        Ok(Err(e)) => return Err(format!("mcp: login callback failed: {e}")),
        Ok(Ok(inner)) => inner?,
    };
    if returned_state != state {
        return Err("mcp: login callback state mismatch (possible CSRF — aborted)".to_string());
    }
    // 7. exchange the code for tokens (single exchange: the code is one-shot)
    let fields = [
        ("grant_type", "authorization_code"),
        ("code", code.as_str()),
        ("redirect_uri", redirect_uri.as_str()),
        ("client_id", client_id.as_str()),
        ("code_verifier", verifier.as_str()),
        ("resource", resource.as_str()),
    ];
    let (access, refresh, expires_at) =
        token_exchange(&as_meta.token_endpoint, &fields, client_secret.as_deref()).await.map_err(|dead| {
            if dead {
                "mcp: login failed: authorization grant rejected".to_string()
            } else {
                "mcp: login failed: token exchange failed".to_string()
            }
        })?;
    let mut map = read_auth_map_in(auth_path);
    map.insert(
        cfg.name.clone(),
        McpAuthEntry {
            client_id,
            client_secret,
            access_token: access,
            refresh_token: refresh,
            expires_at,
            scopes: prm.scopes_supported.join(" "),
            resource,
            token_endpoint: as_meta.token_endpoint,
        },
    );
    write_auth_map_in(auth_path, &map)?;
    Ok(())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(*b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Accept one loopback connection and pull `code` + `state` from its query.
/// Replies with a "done, close this tab" page so the browser isn't left
/// hanging, then returns.
async fn wait_for_callback(listener: tokio::net::TcpListener) -> Result<(String, String), String> {
    let (mut stream, _) = timeout(OAUTH_CALLBACK_TIMEOUT, listener.accept())
        .await
        .map_err(|_| "mcp: login timed out waiting for the browser (try again)".to_string())?
        .map_err(|e| format!("mcp: login callback failed: {e}"))?;
    let mut buf = vec![0u8; 8192];
    let mut len = 0;
    timeout(Duration::from_secs(60), async {
        loop {
            let n = stream.read(&mut buf[len..]).await.map_err(|e| format!("mcp: login callback failed: {e}"))?;
            if n == 0 {
                break;
            }
            len += n;
            if len >= 4 && buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
            if len >= buf.len() {
                break;
            }
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| "mcp: login callback timed out".to_string())??;
    let head = String::from_utf8_lossy(&buf[..len]).into_owned();
    let target = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .ok_or_else(|| "mcp: bad login callback request".to_string())?;
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "code" => code = Some(percent_decode(v)),
            "state" => state = Some(percent_decode(v)),
            "error" => error = Some(percent_decode(v)),
            _ => {}
        }
    }
    let page = if error.is_none() && code.is_some() {
        "<html><body style='font-family:sans-serif;padding:40px'><h2>Signed in to Vicuna</h2><p>You can close this tab and return to the app.</p></body></html>"
    } else {
        "<html><body style='font-family:sans-serif;padding:40px'><h2>Sign-in failed</h2><p>Return to Vicuna and try again.</p></body></html>"
    };
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        page.len(),
        page
    );
    let _ = stream.write_all(resp.as_bytes()).await;
    if let Some(e) = error {
        return Err(format!("mcp: login denied by the server ({e})"));
    }
    match (code, state) {
        (Some(c), Some(s)) if !c.is_empty() => Ok((c, s)),
        _ => Err("mcp: login callback carried no code".to_string()),
    }
}

// ---------- vicuna.json config file (source of truth) ----------
//
// `~/vicuna/config/vicuna.json` mirrors opencode.json: top-level `mcp` maps
// name → { command?, type local/remote, url?, env?, enabled? }. Other
// top-level keys are preserved verbatim on write. The manager key (`id`) is
// the map key (= display name).

fn vicuna_config_path() -> Result<PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").map_err(|_| "cannot locate home folder".to_string())?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").map_err(|_| "cannot locate home folder".to_string())?;
    Ok(PathBuf::from(home).join("vicuna").join("config").join("vicuna.json"))
}

fn entry_to_config(name: &str, v: &serde_json::Value) -> Option<McpServerConfig> {
    let obj = v.as_object()?;
    let ty = obj.get("type").and_then(|t| t.as_str()).unwrap_or("local");
    let transport = match ty {
        "local" | "stdio" => "stdio",
        "remote" | "http" => "http",
        _ => return None,
    }
    .to_string();
    let command = obj
        .get("command")
        .and_then(|c| c.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect::<Vec<_>>()
        })
        .filter(|a| !a.is_empty());
    let env = obj.get("env").and_then(|e| e.as_object()).map(|m| {
        m.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str().unwrap_or(&v.to_string()).to_string(),
                )
            })
            .collect::<HashMap<String, String>>()
    });
    let url = obj.get("url").and_then(|u| u.as_str()).map(|s| s.to_string());
    let headers = obj.get("headers").and_then(|e| e.as_object()).map(|m| {
        m.iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    v.as_str().unwrap_or(&v.to_string()).to_string(),
                )
            })
            .collect::<HashMap<String, String>>()
    });
    let enabled = obj.get("enabled").and_then(|e| e.as_bool()).unwrap_or(true);
    if transport == "stdio" && command.as_ref().map(|c| c.is_empty()).unwrap_or(true) {
        return None;
    }
    if transport == "http" && url.as_deref().unwrap_or("").is_empty() {
        return None;
    }
    Some(McpServerConfig {
        id: name.to_string(),
        name: name.to_string(),
        transport,
        command,
        env,
        url,
        headers,
        enabled,
    })
}

fn config_to_entry(c: &McpServerConfig) -> serde_json::Value {
    let mut m = serde_json::Map::new();
    let ty = match c.transport.as_str() {
        "http" | "remote" => "remote",
        _ => "local",
    };
    m.insert("type".to_string(), serde_json::json!(ty));
    if let Some(cmd) = c.command.as_ref().filter(|c| !c.is_empty()) {
        m.insert("command".to_string(), serde_json::json!(cmd));
    }
    if let Some(env) = c.env.as_ref().filter(|e| !e.is_empty()) {
        m.insert("env".to_string(), serde_json::json!(env));
    }
    if let Some(url) = c.url.as_ref().filter(|u| !u.is_empty()) {
        m.insert("url".to_string(), serde_json::json!(url));
    }
    if let Some(h) = c.headers.as_ref().filter(|h| !h.is_empty()) {
        m.insert("headers".to_string(), serde_json::json!(h));
    }
    m.insert("enabled".to_string(), serde_json::json!(c.enabled));
    serde_json::Value::Object(m)
}

/// All MCP servers in vicuna.json (empty when the file is missing).
pub fn list_file_servers() -> Result<Vec<McpServerConfig>, String> {
    let path = vicuna_config_path()?;
    list_file_servers_in(&path)
}

/// Insert/replace one server entry, preserving every other key in the file.
pub fn save_file_server(cfg: &McpServerConfig) -> Result<Vec<McpServerConfig>, String> {
    let path = vicuna_config_path()?;
    save_file_server_in(&path, cfg)
}

/// Remove one server entry (no-op when absent).
pub fn delete_file_server(id: &str) -> Result<Vec<McpServerConfig>, String> {
    let path = vicuna_config_path()?;
    delete_file_server_in(&path, id)
}

fn read_doc_in(path: &std::path::Path) -> serde_json::Value {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if text.trim().is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(&text).unwrap_or(serde_json::json!({}))
}

fn write_doc_in(path: &std::path::Path, doc: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mcp: cannot create config dir: {e}"))?;
    }
    let text = serde_json::to_string_pretty(doc).map_err(|e| format!("mcp: cannot encode config: {e}"))?;
    std::fs::write(path, text + "\n").map_err(|e| format!("mcp: cannot write config: {e}"))?;
    Ok(())
}

fn list_file_servers_in(path: &std::path::Path) -> Result<Vec<McpServerConfig>, String> {
    let doc = read_doc_in(path);
    let Some(map) = doc.get("mcp").and_then(|m| m.as_object()) else {
        return Ok(vec![]);
    };
    let mut out: Vec<McpServerConfig> = map
        .iter()
        .filter_map(|(k, v)| entry_to_config(k, v))
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

fn save_file_server_in(
    path: &std::path::Path,
    cfg: &McpServerConfig,
) -> Result<Vec<McpServerConfig>, String> {
    let name = cfg.name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err("mcp: server name is required".to_string());
    }
    if name.contains('/') || name.contains('\\') {
        return Err("mcp: server name cannot contain slashes".to_string());
    }
    let mut doc = read_doc_in(path);
    if !doc.is_object() {
        doc = serde_json::json!({});
    }
    let map = doc
        .as_object_mut()
        .expect("object")
        .entry("mcp".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if !map.is_object() {
        *map = serde_json::json!({});
    }
    let stored = McpServerConfig {
        id: name.to_string(),
        name: name.to_string(),
        ..cfg.clone()
    };
    // keep keys the UI doesn't manage (e.g. oauth tokens) when overwriting
    // an existing entry; stale transport keys are dropped by rebuilding
    // the known set from scratch first.
    const KNOWN: [&str; 6] = ["type", "command", "env", "url", "headers", "enabled"];
    let mut merged = config_to_entry(&stored);
    if let (Some(obj), Some(prev)) = (
        merged.as_object_mut(),
        map.get(name).and_then(|v| v.as_object()),
    ) {
        for (k, v) in prev {
            if !KNOWN.contains(&k.as_str()) {
                obj.insert(k.clone(), v.clone());
            }
        }
    }
    map.as_object_mut()
        .expect("object")
        .insert(name.to_string(), merged);
    write_doc_in(path, &doc)?;
    list_file_servers_in(path)
}

fn delete_file_server_in(path: &std::path::Path, id: &str) -> Result<Vec<McpServerConfig>, String> {
    let mut doc = read_doc_in(path);
    if let Some(map) = doc.get_mut("mcp").and_then(|m| m.as_object_mut()) {
        map.remove(id);
        map.remove(id.trim());
    }
    write_doc_in(path, &doc)?;
    list_file_servers_in(path)
}

#[cfg(test)]
mod config_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_config(tag: &str) -> PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("vicuna-mcp-test-{tag}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p.join("vicuna.json")
    }

    fn local(name: &str, cmd: &[&str], enabled: bool) -> McpServerConfig {
        McpServerConfig {
            id: name.to_string(),
            name: name.to_string(),
            transport: "stdio".to_string(),
            command: Some(cmd.iter().map(|s| s.to_string()).collect()),
            env: None,
            url: None,
            headers: None,
            enabled,
        }
    }

    #[test]
    fn save_list_delete_roundtrip_and_opencode_shape() {
        let path = tmp_config("roundtrip");
        // missing file reads empty
        assert!(list_file_servers_in(&path).unwrap().is_empty());
        save_file_server_in(&path, &local("live-browser", &["lb", "mcp"], true)).unwrap();
        save_file_server_in(
            &path,
            &McpServerConfig {
                id: "higgsfield".to_string(),
                name: "higgsfield".to_string(),
                transport: "http".to_string(),
                command: None,
                env: None,
                url: Some("https://mcp.higgsfield.ai/mcp".to_string()),
                headers: None,
                enabled: true,
            },
        )
        .unwrap();
        // disabled local server (opencode browsermcp shape)
        save_file_server_in(&path, &local("browsermcp", &["npx", "@browsermcp/mcp@latest"], false)).unwrap();
        let list = list_file_servers_in(&path).unwrap();
        assert_eq!(list.len(), 3);
        // sorted case-insensitively
        assert_eq!(list[0].name, "browsermcp");
        assert!(!list[0].enabled);
        assert_eq!(list[1].name, "higgsfield");
        assert_eq!(list[1].transport, "http");
        assert_eq!(list[2].command, Some(vec!["lb".to_string(), "mcp".to_string()]));
        // file itself uses opencode spellings and keeps unknown top-level keys
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["mcp"]["live-browser"]["type"], "local");
        assert_eq!(raw["mcp"]["higgsfield"]["type"], "remote");
        assert_eq!(raw["mcp"]["browsermcp"]["enabled"], false);
        drop(list);
        // delete removes just that entry
        let list = delete_file_server_in(&path, "higgsfield").unwrap();
        assert_eq!(list.len(), 2);
        assert!(list.iter().all(|s| s.name != "higgsfield"));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn unknown_entry_keys_survive_save() {
        let path = tmp_config("unknown");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcp": {"higgsfield": {"type": "remote", "url": "https://x", "enabled": true, "oauth": {"token": "abc"}}}}"#,
        )
        .unwrap();
        save_file_server_in(
            &path,
            &McpServerConfig {
                id: "higgsfield".to_string(),
                name: "higgsfield".to_string(),
                transport: "http".to_string(),
                command: None,
                env: None,
                url: Some("https://mcp.higgsfield.ai/mcp".to_string()),
                headers: None,
                enabled: false,
            },
        )
        .unwrap();
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["mcp"]["higgsfield"]["oauth"]["token"], "abc");
        assert_eq!(raw["mcp"]["higgsfield"]["enabled"], false);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn invalid_entries_are_skipped_and_names_rejected() {
        let path = tmp_config("invalid");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"mcp": {"broken": {"type": "warp"}, "nourl": {"type": "remote", "enabled": true}}}"#,
        )
        .unwrap();
        assert!(list_file_servers_in(&path).unwrap().is_empty());
        assert!(save_file_server_in(&path, &local("a/b", &["x"], true)).is_err());
        assert!(save_file_server_in(
            &path,
            &McpServerConfig {
                id: "".to_string(),
                name: "  ".to_string(),
                transport: "stdio".to_string(),
                command: Some(vec!["x".to_string()]),
                env: None,
                url: None,
                headers: None,
                enabled: true,
            },
        )
        .is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}

#[cfg(test)]
mod oauth_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn pkce_matches_rfc7636_vector() {
        // RFC 7636 Appendix B: verifier → S256 challenge
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
    }

    #[test]
    fn percent_roundtrip() {
        let s = "https://x.test/cb?a=b c+d/e?f";
        assert_eq!(percent_decode(&percent_encode(s)), s);
        assert_eq!(percent_decode("a%2Fb%20c+d"), "a/b c d");
        assert_eq!(percent_decode("%zz"), "%zz"); // invalid escape stays literal
    }

    #[test]
    fn resource_metadata_parsing() {
        let h = r#"Bearer resource_metadata="https://x.test/.well-known/oauth-protected-resource/mcp", scope="a,b""#;
        assert_eq!(
            parse_resource_metadata(h),
            Some("https://x.test/.well-known/oauth-protected-resource/mcp")
        );
        // sibling challenges before/after, commas inside the quoted URL
        let h2 = r#"Basic realm="x", Bearer resource_metadata="https://y.test/a,b/c", Bearer error="invalid_token""#;
        assert_eq!(parse_resource_metadata(h2), Some("https://y.test/a,b/c"));
        assert_eq!(parse_resource_metadata(r#"Bearer error="invalid_token""#), None);
    }

    #[test]
    fn well_known_building() {
        assert_eq!(
            well_known("https://x.test/mcp", "oauth-protected-resource"),
            Some("https://x.test/.well-known/oauth-protected-resource/mcp".to_string())
        );
        assert_eq!(
            well_known("https://x.test", "oauth-authorization-server"),
            Some("https://x.test/.well-known/oauth-authorization-server".to_string())
        );
        assert_eq!(
            well_known("https://x.test/", "oauth-authorization-server"),
            Some("https://x.test/.well-known/oauth-authorization-server".to_string())
        );
        assert_eq!(well_known("notaurl", "x"), None);
    }

    fn tmp_auth(tag: &str) -> PathBuf {
        let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let p = std::env::temp_dir().join(format!("vicuna-auth-test-{tag}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p.join("auth.json")
    }

    fn test_cfg(url: String) -> McpServerConfig {
        McpServerConfig {
            id: "fake".to_string(),
            name: "fake".to_string(),
            transport: "http".to_string(),
            command: None,
            env: None,
            url: Some(url),
            headers: None,
            enabled: true,
        }
    }

    /// Fake OAuth + MCP server: 401s without a known bearer, serves PRM / AS
    /// metadata / DCR / authorize-redirect / token exchange, and answers
    /// initialize + tools/list for valid bearers.
    fn fake_route(
        base: &str,
        method: &str,
        path: &str,
        query: &str,
        headers: &HashMap<String, String>,
        body: &[u8],
    ) -> String {
        fn json(status: &str, extra: &str, payload: &str) -> String {
            format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{payload}",
                payload.len()
            )
        }
        fn text(status: &str, payload: &str) -> String {
            format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            )
        }
        let bearer_ok = headers.get("authorization").is_some_and(|a| {
            a == "Bearer test-access-1" || a == "Bearer test-access-2"
        });
        if method == "POST" && path == "/mcp" {
            if !bearer_ok {
                return json(
                    "401 Unauthorized",
                    &format!("WWW-Authenticate: Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource\"\r\n"),
                    r#"{"error":"unauthorized"}"#,
                );
            }
            let v: serde_json::Value = serde_json::from_slice(body).unwrap_or_default();
            let id = v.get("id").cloned().unwrap_or(serde_json::json!(0));
            let result = if v.get("method").and_then(|m| m.as_str()) == Some("initialize") {
                serde_json::json!({"protocolVersion": "2024-11-05", "capabilities": {}})
            } else {
                serde_json::json!({"tools": [{"name": "probe", "description": "d", "inputSchema": {"type": "object"}}]})
            };
            let payload = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
            return json("200 OK", "", &payload);
        }
        if method == "GET" && path == "/.well-known/oauth-protected-resource" {
            let payload = serde_json::json!({
                "resource": format!("{base}/mcp"),
                "authorization_servers": [base],
            })
            .to_string();
            return json("200 OK", "", &payload);
        }
        if method == "GET" && path == "/.well-known/oauth-authorization-server" {
            let payload = serde_json::json!({
                "issuer": base,
                "authorization_endpoint": format!("{base}/authorize"),
                "token_endpoint": format!("{base}/token"),
                "registration_endpoint": format!("{base}/register"),
            })
            .to_string();
            return json("200 OK", "", &payload);
        }
        if method == "POST" && path == "/register" {
            return json("200 OK", "", r#"{"client_id":"test-client"}"#);
        }
        if method == "GET" && path == "/authorize" {
            let mut redirect = String::new();
            let mut state = String::new();
            for pair in query.split('&') {
                let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
                if k == "redirect_uri" {
                    redirect = percent_decode(v);
                } else if k == "state" {
                    state = percent_decode(v);
                }
            }
            let sep = if redirect.contains('?') { "&" } else { "?" };
            let loc = format!("{redirect}{sep}code=test-code&state={state}");
            return format!("HTTP/1.1 302 Found\r\nLocation: {loc}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        }
        if method == "POST" && path == "/token" {
            let form = String::from_utf8_lossy(body).into_owned();
            let grant = form.split('&').find_map(|p| {
                let (k, v) = p.split_once('=').unwrap_or((p, ""));
                (k == "grant_type").then(|| v.to_string())
            });
            let val = |key: &str| {
                form.split('&').find_map(|p| {
                    let (k, v) = p.split_once('=').unwrap_or((p, ""));
                    (k == key).then(|| percent_decode(v))
                })
            };
            match grant.as_deref() {
                Some("authorization_code") if val("code").as_deref() == Some("test-code") => {
                    return json(
                        "200 OK",
                        "",
                        r#"{"access_token":"test-access-1","refresh_token":"test-refresh","expires_in":3600,"token_type":"Bearer"}"#,
                    );
                }
                Some("refresh_token") if val("refresh_token").as_deref() == Some("test-refresh") => {
                    return json(
                        "200 OK",
                        "",
                        r#"{"access_token":"test-access-2","expires_in":3600,"token_type":"Bearer"}"#,
                    );
                }
                _ => return json("400 Bad Request", "", r#"{"error":"invalid_grant"}"#),
            }
        }
        text("404 Not Found", "nope")
    }

    /// "Browser" hook for tests: GET the authorize URL with redirects followed,
    /// which lands on the loopback callback the login flow is listening on.
    /// The accept task is already running (spawned before open), so this can
    /// block for the full fetch.
    fn visit_in_thread(url: &str) -> Result<(), String> {
        let url = url.to_string();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            rt.block_on(async {
                let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().unwrap();
                let _ = client.get(&url).send().await;
            });
        })
        .join()
        .map_err(|_| "mcp: test browser thread panicked".to_string())
    }

    // NOTE: multi_thread — the "browser" hook below blocks its thread in
    // join() while the loopback accept task needs executor polling. A
    // single-thread runtime would starve it (production uses Tauri's
    // multi-thread runtime, so this is test-only).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn full_login_refresh_and_dead_grant() {
        // bind here so we know the base URL (spawn_fake can't return it after
        // moving the listener into the task)
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task_base = base.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                let base = task_base.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16384];
                    let mut len = 0;
                    loop {
                        match s.read(&mut buf[len..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => {
                                len += n;
                                if len >= 4 && buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                                    break;
                                }
                                if len >= buf.len() {
                                    return;
                                }
                            }
                        }
                    }
                    let head = String::from_utf8_lossy(&buf[..len]).into_owned();
                    let mut lines = head.lines();
                    let request = lines.next().unwrap_or("").to_string();
                    let mut headers = HashMap::new();
                    for l in lines {
                        if let Some((k, v)) = l.split_once(':') {
                            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
                        }
                    }
                    let cl: usize = headers.get("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
                    // the first read(s) may already hold body bytes past the headers
                    let hlen = buf[..len].windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4).unwrap_or(len);
                    let mut body = vec![0u8; cl];
                    let buffered = (len - hlen).min(cl);
                    body[..buffered].copy_from_slice(&buf[hlen..hlen + buffered]);
                    let mut got = buffered;
                    while got < cl {
                        match s.read(&mut body[got..]).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => got += n,
                        }
                    }
                    let parts: Vec<&str> = request.split_whitespace().collect();
                    let (method, target) = (parts.first().copied().unwrap_or(""), parts.get(1).copied().unwrap_or(""));
                    let (path, query) = target.split_once('?').unwrap_or((target, ""));
                    let resp = fake_route(&base, method, path, query, &headers, &body);
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });

        let auth_path = tmp_auth("full");
        let cfg = test_cfg(format!("{base}/mcp"));
        // probe sees the 401 + resource metadata, not an open server
        match probe_resource(&format!("{base}/mcp"), &HashMap::new()).await.unwrap() {
            Probe::Protected { resource_metadata } => {
                assert!(resource_metadata.unwrap().contains("oauth-protected-resource"));
            }
            Probe::Open => panic!("fake should demand auth"),
        }
        // full dance through the fake browser
        oauth_login_in(&cfg, &auth_path, visit_in_thread).await.unwrap();
        let map = read_auth_map_in(&auth_path);
        let entry = map.get("fake").expect("token stored");
        assert_eq!(entry.client_id, "test-client");
        assert_eq!(entry.access_token, "test-access-1");
        assert_eq!(entry.refresh_token.as_deref(), Some("test-refresh"));
        assert_eq!(entry.resource, format!("{base}/mcp"));
        assert!(entry.token_endpoint.ends_with("/token"));
        assert!(entry.expires_at > now_secs());
        // fresh token is served without another refresh
        assert_eq!(http_bearer_in("fake", &auth_path).await.as_deref(), Some("test-access-1"));
        // expired token refreshes and persists the new one
        let mut map = read_auth_map_in(&auth_path);
        map.get_mut("fake").unwrap().expires_at = 1;
        write_auth_map_in(&auth_path, &map).unwrap();
        assert_eq!(http_bearer_in("fake", &auth_path).await.as_deref(), Some("test-access-2"));
        assert_eq!(
            read_auth_map_in(&auth_path).get("fake").unwrap().access_token,
            "test-access-2"
        );
        // dead refresh token drops the entry → clean re-sign-in afterwards
        let mut map = read_auth_map_in(&auth_path);
        let e = map.get_mut("fake").unwrap();
        e.expires_at = 1;
        e.refresh_token = Some("bad".to_string());
        write_auth_map_in(&auth_path, &map).unwrap();
        assert_eq!(http_bearer_in("fake", &auth_path).await, None);
        assert!(!read_auth_map_in(&auth_path).contains_key("fake"));
        std::fs::remove_dir_all(auth_path.parent().unwrap()).ok();
    }

    #[tokio::test]
    async fn open_server_needs_no_signin() {
        // initialize 200 without auth → the dance bails so the caller can
        // fall back to a plain connect
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else { break };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    let mut len = 0;
                    while len < 4 || !buf[..len].windows(4).any(|w| w == b"\r\n\r\n") {
                        match s.read(&mut buf[len..]).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => len += n,
                        }
                        if len >= buf.len() {
                            return;
                        }
                    }
                    let payload = r#"{"jsonrpc":"2.0","id":0,"result":{"protocolVersion":"2024-11-05"}}"#;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        let auth_path = tmp_auth("open");
        let cfg = test_cfg(format!("{base}/mcp"));
        let err = oauth_login_in(&cfg, &auth_path, visit_in_thread).await.unwrap_err();
        assert!(err.contains("does not require sign-in"), "{err}");
        std::fs::remove_dir_all(auth_path.parent().unwrap()).ok();
    }
}
