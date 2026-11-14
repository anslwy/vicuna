// Codex Work engine: the user's own `codex` CLI, driven through
// `codex app-server` (its official JSON-RPC protocol over stdio; the same
// one the Codex IDE extension and Kurisu use). Nothing bundled: the user's
// login (~/.codex/auth.json) and config.toml apply as in their terminal.
//
// Each Work turn starts `codex app-server` in the workspace and:
//   initialize -> thread/resume (saved thread) or thread/start
//                 (cwd, approval policy, sandbox, model, MCP servers as
//                 `config.mcp_servers`)
//   turn/start  -> streamed notifications:
//     item/agentMessage/delta                    -> Token
//     item/reasoning/summaryTextDelta|textDelta  -> Think
//     item/started commandExecution|fileChange|mcpToolCall|webSearch
//                                                -> ToolStart (real args)
//     item/completed (same types)                -> ToolEnd (real output)
//     thread/tokenUsage/updated + turn/completed -> RoundEnd
//   item/*/requestApproval (server request)      -> auto-accept, or the
//                                                   chat's ask card
// Messages queued during the turn run as the next turn/start on the same
// thread. Stop sends turn/interrupt (kill after a grace period). Models come
// from `model/list`; the user's config.toml model is the default when no
// model is picked in Vicuna (no model is sent then).
//
// Chosen over `codex exec --json`: app-server streams reasoning and approval
// requests and keeps one live thread for queued follow-ups, while exec has no
// approval round-trip (it can only run with a fixed policy).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

use crate::acp::{self, AcpState, McpSnapshot};
use crate::opencode::OpencodeModel;
use crate::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use crate::work_engine::Engine;
use crate::ChatMessage;

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const CANCEL_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 32 * 1024;
/// Test/override knob: a reasoning effort for every turn (e.g. "low" for the
/// acceptance script). Unset: the user's config.toml decides.
pub const EFFORT_ENV: &str = "VICUNA_CODEX_EFFORT";

// ---------- process + JSON-RPC ----------

fn base_command(cli: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(cli);
    cmd.arg("app-server");
    // the terminal's PATH (codex's own dir first), so MCP servers it spawns
    // resolve like in a shell even from a Finder launch
    if let Some(p) = crate::exe_resolve::child_path(cli.parent()) {
        cmd.env("PATH", p);
    }
    cmd.env("NO_COLOR", "1")
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

fn line(v: &Value) -> String {
    let mut s = v.to_string();
    s.push('\n');
    s
}

/// One running `codex app-server`: requests by id, everything else (notifications
/// and server->client requests) arrives on `events`.
struct Server {
    child: Child,
    writer: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    next_id: i64,
    events: mpsc::UnboundedReceiver<Value>,
    err_tail: Arc<Mutex<String>>,
}

impl Server {
    async fn start(cli: &Path, cwd: &Path) -> Result<Server, String> {
        let mut child = base_command(cli, cwd)
            .spawn()
            .map_err(|e| crate::work_engine::spawn_error("codex", cli, cwd, &e))?;
        let mut stdin = child.stdin.take().ok_or("codex: no stdin")?;
        let stdout = child.stdout.take().ok_or("codex: no stdout")?;
        let stderr = child.stderr.take().ok_or("codex: no stderr")?;
        let (writer, mut write_rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(l) = write_rx.recv().await {
                if stdin.write_all(l.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let err_tail = Arc::new(Mutex::new(String::new()));
        let tail = err_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(mut s) = tail.lock() {
                    s.push_str(&l);
                    s.push('\n');
                    if s.len() > 4000 {
                        let cut = s.len() - 4000;
                        let cut = (cut..s.len()).find(|i| s.is_char_boundary(*i)).unwrap_or(0);
                        s.drain(..cut);
                    }
                }
            }
        });
        let pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>> = Arc::default();
        let (ev_tx, events) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(read_loop(BufReader::new(stdout), pending.clone(), ev_tx));
        let mut s = Server { child, writer, pending, next_id: 1, events, err_tail };
        let info = json!({ "clientInfo": { "name": "vicuna", "title": "Vicuna", "version": env!("CARGO_PKG_VERSION") } });
        s.request_timeout("initialize", info, INIT_TIMEOUT).await?;
        s.notify("initialized", json!({}));
        Ok(s)
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.writer.send(line(&json!({ "method": method, "params": params })));
    }

    fn respond(&self, id: &Value, result: Value) {
        let _ = self.writer.send(line(&json!({ "id": id, "result": result })));
    }

    fn respond_error(&self, id: &Value, message: &str) {
        let _ = self.writer.send(line(&json!({ "id": id, "error": { "code": -32601, "message": message } })));
    }

    /// Send a request; the answer arrives on the returned receiver (the turn
    /// loop keeps reading events meanwhile).
    fn send_request(&mut self, method: &str, params: Value) -> oneshot::Receiver<Result<Value, String>> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.pending.lock() {
            p.insert(id, tx);
        }
        let _ = self.writer.send(line(&json!({ "id": id, "method": method, "params": params })));
        rx
    }

    async fn request_timeout(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let rx = self.send_request(method, params);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(self.exited_message(&format!("codex app-server exited during {method}"))),
            Err(_) => Err(format!("codex app-server did not answer {method} in time")),
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        self.request_timeout(method, params, REQUEST_TIMEOUT).await
    }

    fn stderr_tail(&self) -> String {
        let t = self.err_tail.lock().map(|s| s.trim().to_string()).unwrap_or_default();
        // codex prints this harmless line on some systems; it is never the reason
        let t: Vec<&str> = t.lines().filter(|l| !l.contains("failed to clean up stale arg0")).collect();
        crate::exe_resolve::strip_ansi(&t.join("\n"))
    }

    fn exited_message(&self, what: &str) -> String {
        let tail = self.stderr_tail();
        if tail.is_empty() {
            what.to_string()
        } else {
            format!("{what}:\n{tail}")
        }
    }

    async fn shutdown(mut self) {
        drop(self.writer);
        let _ = tokio::time::timeout(Duration::from_secs(3), self.child.wait()).await;
        let _ = self.child.start_kill();
    }
}

async fn read_loop(
    out: BufReader<ChildStdout>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, String>>>>>,
    events: mpsc::UnboundedSender<Value>,
) {
    let mut lines = out.lines();
    while let Ok(Some(l)) = lines.next_line().await {
        let Ok(msg) = serde_json::from_str::<Value>(&l) else { continue };
        let id = msg.get("id").and_then(Value::as_i64);
        if let (Some(id), None) = (id, msg.get("method")) {
            if let Some(tx) = pending.lock().ok().and_then(|mut p| p.remove(&id)) {
                let res = match msg.get("error") {
                    Some(e) => Err(e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string())),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(res);
            }
            continue;
        }
        let _ = events.send(msg);
    }
    // EOF: wake waiting requests (dropping the senders) and the event loop
    if let Ok(mut p) = pending.lock() {
        p.clear();
    }
}

// ---------- models (model/list; nothing billed) ----------

static MODELS: Mutex<Option<(PathBuf, Vec<OpencodeModel>)>> = Mutex::new(None);

/// `model/list` pages -> picker rows (hidden models left out).
pub fn parse_models(pages: &[Value]) -> Vec<OpencodeModel> {
    let mut out: Vec<OpencodeModel> = Vec::new();
    for m in pages.iter().filter_map(|p| p.get("data").and_then(Value::as_array)).flatten() {
        if m.get("hidden").and_then(Value::as_bool).unwrap_or(false) {
            continue;
        }
        let Some(id) = m.get("model").or_else(|| m.get("id")).and_then(Value::as_str).filter(|v| !v.is_empty()) else {
            continue;
        };
        if out.iter().any(|x| x.id == id) {
            continue;
        }
        out.push(OpencodeModel {
            id: id.to_string(),
            provider: "codex".to_string(),
            name: m.get("displayName").and_then(Value::as_str).map(str::to_string),
        });
    }
    out
}

pub async fn list_models() -> Result<Vec<OpencodeModel>, String> {
    let cli = crate::codex::find_codex().ok_or_else(|| Engine::Codex.missing_message())?;
    if let Some((p, cached)) = MODELS.lock().ok().and_then(|g| g.clone()) {
        if p == cli {
            return Ok(cached);
        }
    }
    let home = crate::opencode::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut server = Server::start(&cli, &home).await?;
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..10 {
        let page = server.request_timeout("model/list", json!({ "cursor": cursor }), INIT_TIMEOUT).await?;
        cursor = page.get("nextCursor").and_then(Value::as_str).map(str::to_string);
        pages.push(page);
        if cursor.is_none() {
            break;
        }
    }
    server.shutdown().await;
    let models = parse_models(&pages);
    if models.is_empty() {
        return Err("Codex reported no models (is it logged in? run `codex login` in a terminal)".into());
    }
    if let Ok(mut g) = MODELS.lock() {
        *g = Some((cli, models.clone()));
    }
    Ok(models)
}

// ---------- MCP: ACP-shaped server entries -> config.mcp_servers ----------

fn name_values(v: Option<&Value>) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for e in v.and_then(|x| x.as_array()).into_iter().flatten() {
        if let (Some(n), Some(val)) = (e.get("name").and_then(|x| x.as_str()), e.get("value").and_then(|x| x.as_str())) {
            out.insert(n.to_string(), json!(val));
        }
    }
    out
}

/// Codex's config.toml `[mcp_servers.<name>]` shape, as a JSON override:
/// stdio `{command,args,env}` or streamable HTTP `{url,http_headers}`.
/// Names are limited to [A-Za-z0-9_-] (Codex rejects others).
/// `default_tools_approval_mode` follows the ask toggle: Codex otherwise
/// rejects MCP calls under the `never` policy ("MCP tool call requires
/// approval, but approval policy is never"); ask mode prompts per call.
pub fn mcp_servers(entries: &[Value], ask: bool) -> Option<Value> {
    let mut servers = serde_json::Map::new();
    for e in entries {
        let Some(name) = e.get("name").and_then(|n| n.as_str()) else { continue };
        let name: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
        let cfg = if let Some(url) = e.get("url").and_then(|u| u.as_str()) {
            let headers = name_values(e.get("headers"));
            if headers.is_empty() {
                json!({ "url": url })
            } else {
                json!({ "url": url, "http_headers": headers })
            }
        } else if let Some(cmd) = e.get("command").and_then(|c| c.as_str()) {
            let env = name_values(e.get("env"));
            let mut c = json!({ "command": cmd, "args": e.get("args").cloned().unwrap_or(json!([])) });
            if !env.is_empty() {
                c["env"] = Value::Object(env);
            }
            c
        } else {
            continue;
        };
        let mut cfg = cfg;
        // vicuna-mcp's ask_user is itself the question to the user: never
        // gate it behind an Allow card
        let approve = !ask || name == crate::vicuna_mcp::SERVER_NAME;
        cfg["default_tools_approval_mode"] = json!(if approve { "approve" } else { "prompt" });
        // ask_user waits for the user; Codex's default tool timeout is 60 s
        if name == crate::vicuna_mcp::SERVER_NAME {
            cfg["tool_timeout_sec"] = json!(crate::vicuna_mcp::ASK_TIMEOUT_SECS);
        }
        servers.insert(name, cfg);
    }
    (!servers.is_empty()).then_some(Value::Object(servers))
}

// ---------- approval policy ----------

/// "Ask before running tools" -> Codex approval policy. Ask: `untrusted`
/// (anything not known-safe asks first). Auto: `never` (nothing asks; like
/// Claude's auto mode). The sandbox is the user's config.toml `sandbox_mode`
/// when set, else `workspace-write` (the agent edits its workspace).
pub fn thread_params(workspace: &str, ask: bool, user_sandbox: Option<&str>, model: Option<&str>, mcp: &[Value]) -> Value {
    let mut p = json!({
        "cwd": workspace,
        "approvalPolicy": if ask { "untrusted" } else { "never" },
        "sandbox": user_sandbox.unwrap_or("workspace-write"),
    });
    if let Some(m) = model {
        p["model"] = json!(m);
    }
    // Codex's own request_user_input question tool stays off outside Plan
    // mode (pinned here; Vicuna never runs Plan mode): questions go through
    // vicuna-mcp's ask_user, and a stray one is routed to the ask card.
    p["config"] = json!({ "features": { "default_mode_request_user_input": false } });
    if let Some(servers) = mcp_servers(mcp, ask) {
        p["config"]["mcp_servers"] = servers;
    }
    p
}

// ---------- tool cards ----------

fn cap(mut s: String) -> String {
    if s.chars().count() > OUTPUT_CAP {
        s = s.chars().take(OUTPUT_CAP).collect::<String>();
        s.push_str("\n[output truncated]");
    }
    s
}

/// Codex runs commands as `/usr/bin/bash -lc '<cmd>'`: show `<cmd>` (any
/// other shape is shown as is).
pub fn shell_inner(cmd: &str) -> String {
    for flag in [" -lc '", " -c '"] {
        if let Some(i) = cmd.find(flag) {
            let shell = cmd[..i].rsplit('/').next().unwrap_or("");
            let rest = &cmd[i + flag.len()..];
            if matches!(shell, "bash" | "sh" | "zsh") && rest.ends_with('\'') && !rest[..rest.len() - 1].contains('\'') {
                return rest[..rest.len() - 1].to_string();
            }
        }
    }
    cmd.to_string()
}

/// A Codex tool item -> Vicuna's card name + args (None: not a tool).
pub fn tool_card(item: &Value) -> Option<(String, Value)> {
    let s = |k: &str| item.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    match item.get("type")?.as_str()? {
        "commandExecution" => Some(("exec".into(), json!({ "command": shell_inner(&s("command")), "cwd": s("cwd") }))),
        "fileChange" => {
            let paths: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|c| c.get("path").and_then(Value::as_str)).collect())
                .unwrap_or_default();
            Some(("write".into(), json!({ "path": paths.join(", ") })))
        }
        "mcpToolCall" => Some((format!("mcp__{}__{}", s("server"), s("tool")), item.get("arguments").cloned().unwrap_or(json!({})))),
        "webSearch" => Some(("web_search".into(), json!({ "query": s("query") }))),
        _ => None,
    }
}

fn mcp_result_text(result: &Value) -> String {
    match result.get("content").and_then(Value::as_array) {
        Some(items) => items
            .iter()
            .filter_map(|i| match i.get("type").and_then(Value::as_str) {
                Some("text") => i.get("text").and_then(Value::as_str).map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        None if result.is_null() => String::new(),
        None => result.to_string(),
    }
}

/// The finished item's output for its card.
pub fn tool_output(item: &Value) -> String {
    let failed = matches!(item.get("status").and_then(Value::as_str), Some("failed" | "declined"));
    let mut out = match item.get("type").and_then(Value::as_str) {
        Some("commandExecution") => {
            let text = item.get("aggregatedOutput").and_then(Value::as_str).unwrap_or("").to_string();
            match item.get("exitCode").and_then(Value::as_i64) {
                Some(code) if code != 0 => format!("{text}\n[exit code {code}]"),
                _ => text,
            }
        }
        Some("mcpToolCall") => match item.get("error").filter(|e| !e.is_null()) {
            Some(e) => format!("Error: {}", e.get("message").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| e.to_string())),
            None => mcp_result_text(item.get("result").unwrap_or(&Value::Null)),
        },
        Some("fileChange") => item
            .get("changes")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|c| {
                        let kind = c.pointer("/kind/type").or_else(|| c.get("kind")).and_then(Value::as_str).unwrap_or("update");
                        format!("{kind} {}", c.get("path").and_then(Value::as_str).unwrap_or(""))
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default(),
        _ => String::new(),
    };
    if failed && !out.starts_with("Error:") {
        let status = item.get("status").and_then(Value::as_str).unwrap_or("failed");
        out = format!("Error: {status}{}{out}", if out.is_empty() { "" } else { "\n" });
    }
    if out.trim().is_empty() {
        out = "[completed]".to_string();
    }
    cap(out)
}

/// One app-server notification -> UI events. `tools` maps item ids to card
/// names (ToolEnd needs the name ToolStart used).
pub fn translate(method: &str, params: &Value, tools: &mut HashMap<String, String>, output_chars: &mut usize) -> Vec<WorkAgentEvent> {
    let mut out = Vec::new();
    match method {
        "item/agentMessage/delta" => {
            if let Some(t) = params.get("delta").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                *output_chars += t.chars().count();
                out.push(WorkAgentEvent::Token { text: t.to_string() });
            }
        }
        "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
            if let Some(t) = params.get("delta").and_then(Value::as_str).filter(|t| !t.is_empty()) {
                out.push(WorkAgentEvent::Think { text: t.to_string() });
            }
        }
        "item/reasoning/summaryPartAdded" => out.push(WorkAgentEvent::Think { text: "\n\n".into() }),
        "item/started" => {
            let item = params.get("item").unwrap_or(&Value::Null);
            let id = item.get("id").and_then(Value::as_str).unwrap_or("").to_string();
            if let Some((name, args)) = tool_card(item) {
                if !id.is_empty() && !tools.contains_key(&id) {
                    tools.insert(id, name.clone());
                    out.push(WorkAgentEvent::ToolStart { name, args });
                }
            } else if item.get("type").and_then(Value::as_str) == Some("agentMessage") && *output_chars > 0 {
                // a new message after earlier text: keep them apart
                out.push(WorkAgentEvent::Token { text: "\n\n".into() });
            }
        }
        "item/completed" => {
            let item = params.get("item").unwrap_or(&Value::Null);
            let id = item.get("id").and_then(Value::as_str).unwrap_or("");
            if let Some(name) = tools.remove(id) {
                out.push(WorkAgentEvent::ToolEnd { name, output: tool_output(item) });
            }
        }
        _ => {}
    }
    out
}

// ---------- user input ----------

/// Text + images (data URLs) as app-server `UserInput` items.
pub fn user_input(text: &str, images: &Option<Vec<String>>) -> Vec<Value> {
    let mut items = vec![json!({ "type": "text", "text": text, "text_elements": [] })];
    for url in images.iter().flatten().filter(|u| u.starts_with("data:")) {
        items.push(json!({ "type": "image", "url": url }));
    }
    items
}

// ---------- approvals ----------

pub fn approval_title(method: &str, params: &Value) -> String {
    if method.contains("commandExecution") || method == "execCommandApproval" {
        let cmd = match params.get("command") {
            Some(Value::String(s)) => shell_inner(s),
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" "),
            _ => String::new(),
        };
        if cmd.is_empty() {
            "a shell command".to_string()
        } else {
            format!("exec: {cmd}")
        }
    } else if method == "mcpServer/elicitation/request" {
        let server = params.get("serverName").and_then(Value::as_str).unwrap_or("an MCP server");
        let msg = params.get("message").and_then(Value::as_str).unwrap_or("").trim().trim_end_matches('?');
        let msg: String = msg.chars().take(200).collect();
        if msg.is_empty() {
            format!("MCP {server}")
        } else {
            format!("MCP {server}: {msg}")
        }
    } else if method.contains("fileChange") || method == "applyPatchApproval" {
        match params.get("grantRoot").and_then(Value::as_str) {
            Some(root) => format!("file changes under {root}"),
            None => "file changes".to_string(),
        }
    } else {
        "a tool".to_string()
    }
}

/// The answer for an approval request (v2 `decision` strings; the legacy
/// `execCommandApproval`/`applyPatchApproval` take `approved`/`denied`; an
/// MCP elicitation (Codex asks this way before some MCP tool calls) takes an
/// `action`; a URL elicitation can't be completed here, so it is declined).
pub fn approval_answer(method: &str, params: &Value, allow: bool) -> Value {
    if method == "mcpServer/elicitation/request" {
        let allow = allow && params.get("mode").and_then(Value::as_str) != Some("url");
        if allow {
            json!({ "action": "accept", "content": {}, "_meta": null })
        } else {
            json!({ "action": "decline", "content": null, "_meta": null })
        }
    } else if method == "execCommandApproval" || method == "applyPatchApproval" {
        json!({ "decision": if allow { "approved" } else { "denied" } })
    } else {
        json!({ "decision": if allow { "accept" } else { "decline" } })
    }
}

fn is_approval(method: &str) -> bool {
    method.ends_with("/requestApproval")
        && method != "item/permissions/requestApproval" // a permission-profile grant: different answer shape
        || matches!(method, "execCommandApproval" | "applyPatchApproval" | "mcpServer/elicitation/request")
}

/// Auto mode accepts at once (noted in the transcript); ask mode raises the
/// chat's ask card and answers when the user picks (Stop/dismiss declines).
fn handle_approval(server: &Server, id: &Value, method: &str, params: &Value, turn: &Arc<TurnState>, on_event: &Channel<WorkAgentEvent>) {
    let title = approval_title(method, params);
    if !crate::work_engine::approval_ask() {
        server.respond(id, approval_answer(method, params, true));
        emit_ev(on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> Auto-approved: {title}\n") });
        return;
    }
    let ask_id = format!("perm-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(ask_id.clone(), tx);
    }
    emit_ev(
        on_event,
        &turn.stop,
        WorkAgentEvent::AskRequest { ask_id, question: format!("Allow {title}?"), options: vec!["Allow".into(), "Deny".into()], multi: false },
    );
    let (writer, on_event, turn, id, method, params) =
        (server.writer.clone(), on_event.clone(), turn.clone(), id.clone(), method.to_string(), params.clone());
    tokio::spawn(async move {
        let answer = rx.await.unwrap_or_else(|_| "cancelled".to_string());
        let allow = crate::claude_cli::is_allow(&answer);
        let _ = writer.send(line(&json!({ "id": id, "result": approval_answer(&method, &params, allow) })));
        let verdict = if allow { "Approved" } else { "Denied" };
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> {verdict}: {title}\n") });
    });
}

/// Codex's native question (`item/tool/requestUserInput`, normally Plan mode
/// only): each question becomes the chat's ask card in turn; the answers go
/// back as `{answers: {<id>: {answers: [..]}}}`. Stop answers nothing.
fn handle_user_input(server: &Server, id: &Value, params: &Value, turn: &Arc<TurnState>, on_event: &Channel<WorkAgentEvent>) {
    let questions = params.get("questions").and_then(Value::as_array).cloned().unwrap_or_default();
    let (writer, on_event, turn, id) = (server.writer.clone(), on_event.clone(), turn.clone(), id.clone());
    tokio::spawn(async move {
        let mut answers = serde_json::Map::new();
        for q in questions {
            if turn.stop.load(Ordering::Relaxed) {
                break;
            }
            let (qid, question, options) = user_input_question(&q);
            let ask_id = format!("ask-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = oneshot::channel::<String>();
            if let Ok(mut m) = turn.ask_pending.lock() {
                m.insert(ask_id.clone(), tx);
            }
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::AskRequest { ask_id: ask_id.clone(), question, options, multi: false });
            let answer = rx.await.ok();
            if let Ok(mut m) = turn.ask_pending.lock() {
                m.remove(&ask_id);
            }
            match answer {
                Some(a) if !turn.stop.load(Ordering::Relaxed) => {
                    // native questions take text only (pasted images dropped)
                    let a = crate::vicuna_mcp::parse_ask_answer(&a).text;
                    answers.insert(qid, json!({ "answers": [a.trim()] }));
                }
                _ => break,
            }
        }
        let _ = writer.send(line(&json!({ "id": id, "result": { "answers": answers } })));
    });
}

/// (id, question text, option labels) of one request_user_input question.
fn user_input_question(q: &Value) -> (String, String, Vec<String>) {
    let s = |k: &str| q.get(k).and_then(Value::as_str).unwrap_or("").trim().to_string();
    let (header, text) = (s("header"), s("question"));
    let question = if header.is_empty() || text.contains(&header) { text } else { format!("{header}: {text}") };
    let options = q
        .get("options")
        .and_then(Value::as_array)
        .map(|o| o.iter().filter_map(|x| x.get("label").and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default();
    (s("id"), question, options)
}

/// `thread/goal/updated` (Codex 0.16x's goal tools, which replaced
/// update_plan) as a transcript note, or None when objective + status are
/// unchanged (the notification also fires for token/time counters).
fn goal_note(params: &Value, last: &mut Option<(String, String)>) -> Option<String> {
    let g = params.get("goal")?;
    let objective = g.get("objective").and_then(Value::as_str)?.trim().to_string();
    let status = g.get("status").and_then(Value::as_str).unwrap_or("active").to_string();
    if objective.is_empty() || last.as_ref() == Some(&(objective.clone(), status.clone())) {
        return None;
    }
    *last = Some((objective.clone(), status.clone()));
    let label = match status.as_str() {
        "complete" => "Goal complete",
        "paused" => "Goal paused",
        "blocked" => "Goal blocked",
        "usageLimited" | "budgetLimited" => "Goal stopped (limit reached)",
        _ => "Goal",
    };
    Some(format!("\n> **{label}:** {objective}\n"))
}

/// `turn/plan/updated` as a transcript checklist (older Codex update_plan).
fn plan_note(params: &Value) -> Option<String> {
    let steps = params.get("plan").and_then(Value::as_array)?;
    if steps.is_empty() {
        return None;
    }
    let mut out = String::from("\n\n**Plan**\n");
    if let Some(e) = params.get("explanation").and_then(Value::as_str).map(str::trim).filter(|e| !e.is_empty()) {
        out.push_str(&format!("{e}\n"));
    }
    for st in steps {
        let mark = match st.get("status").and_then(Value::as_str) {
            Some("completed") => "[x]",
            Some("inProgress") => "[~]",
            _ => "[ ]",
        };
        out.push_str(&format!("- {mark} {}\n", st.get("step").and_then(Value::as_str).unwrap_or("").trim()));
    }
    out.push('\n');
    Some(out)
}

// ---------- one Work turn ----------

/// The user's config.toml `sandbox_mode`, via `config/read` (so Vicuna never
/// widens or narrows a sandbox the user chose).
async fn user_sandbox(server: &mut Server, workspace: &str) -> Option<String> {
    let r = server.request("config/read", json!({ "cwd": workspace })).await.ok()?;
    r.pointer("/config/sandbox_mode").and_then(Value::as_str).map(str::to_string)
}

#[allow(clippy::too_many_arguments)]
pub async fn run_turn_inner(
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
    let cli = crate::codex::find_codex().ok_or_else(|| Engine::Codex.missing_message())?;
    let last_user = acp::last_user_message(messages).ok_or_else(|| "Work mode: no user message to send".to_string())?;
    let key = acp::session_key(Engine::Codex, session_id, workspace);
    let saved = if fresh_session { None } else { key.as_deref().and_then(|k| state.get(k)) };

    let (mcp, mcp_note) = acp::mcp_servers_for_turn(mcp_snapshot).await;
    if let Some(note) = mcp_note {
        emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n\n> {note}\n\n") });
    }
    let mcp = crate::vicuna_mcp::with_ask_server(mcp, ask_url);
    let picked = acp::effective_model(Engine::Codex, model);
    if let Some(m) = &picked {
        emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Codex.reply_label(m) });
    }

    let goal_note = goal_text
        .filter(|g| !g.trim().is_empty())
        .map(|g| format!("\n\n[Session goal: {g} — keep working until it is fully achieved.]"))
        .unwrap_or_default();
    let base_text = format!("{}{}", last_user.content, goal_note);
    let fresh_text = format!(
        "{}\n\n---\n{}{}",
        acp::system_prompt(workspace, None, session_id).trim(),
        acp::history_preamble(messages, last_user),
        base_text
    );

    let mut server = Server::start(&cli, Path::new(workspace)).await.map_err(|e| acp::friendly_prompt_error(Engine::Codex, e))?;
    let sandbox = user_sandbox(&mut server, workspace).await;
    let params = thread_params(workspace, crate::work_engine::approval_ask(), sandbox.as_deref(), picked.as_deref(), &mcp);

    // resume the chat's thread; a thread Codex no longer has starts over with
    // the history preamble
    let mut opened: Option<(String, Value, String)> = None;
    if let Some(thread) = saved {
        let mut p = params.clone();
        p["threadId"] = json!(thread);
        p["excludeTurns"] = json!(true);
        if let Ok(r) = server.request("thread/resume", p).await {
            opened = Some((thread, r, base_text.clone()));
        }
    }
    let (thread, opened_resp, first_text) = match opened {
        Some(o) => o,
        None => {
            let r = server
                .request("thread/start", params)
                .await
                .map_err(|e| acp::friendly_prompt_error(Engine::Codex, format!("Codex could not start a thread: {e}")))?;
            let id = r.pointer("/thread/id").and_then(Value::as_str).ok_or("codex: thread/start returned no thread id")?.to_string();
            (id, r, fresh_text)
        }
    };
    if let Some(k) = &key {
        state.set(k.clone(), thread.clone());
    }
    if picked.is_none() {
        // the user's config.toml default: label the reply with what runs
        if let Some(m) = opened_resp.get("model").and_then(Value::as_str) {
            emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Codex.reply_label(m) });
        }
    }

    let result = run_turns(&mut server, &thread, picked.as_deref(), &first_text, last_user, turn, on_event).await;
    server.shutdown().await;
    result?;
    emit_ev(on_event, stop, WorkAgentEvent::Done { stopped: stop.load(Ordering::Relaxed) });
    Ok(())
}

fn turn_start_params(thread: &str, model: Option<&str>, input: Vec<Value>) -> Value {
    let mut p = json!({ "threadId": thread, "input": input });
    if let Some(m) = model {
        p["model"] = json!(m);
    }
    if let Some(e) = std::env::var(EFFORT_ENV).ok().filter(|e| !e.trim().is_empty()) {
        p["effort"] = json!(e.trim());
    }
    p
}

/// turn/start, stream until turn/completed; queued messages run as further
/// turns on the same thread.
async fn run_turns(
    server: &mut Server,
    thread: &str,
    model: Option<&str>,
    first_text: &str,
    last_user: &ChatMessage,
    turn: &Arc<TurnState>,
    on_event: &Channel<WorkAgentEvent>,
) -> Result<(), String> {
    let stop = &turn.stop;
    let mut start_rx = Some(server.send_request("turn/start", turn_start_params(thread, model, user_input(first_text, &last_user.images))));
    emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "fresh".into() });
    let mut turn_id: Option<String> = None;
    let mut tools: HashMap<String, String> = HashMap::new();
    let mut output_chars = 0usize;
    let mut out_tokens: Option<f64> = None;
    let mut last_goal: Option<(String, String)> = None;
    let mut started = Instant::now();
    let mut interrupted_at: Option<Instant> = None;
    loop {
        tokio::select! {
            r = async { start_rx.as_mut().unwrap().await }, if start_rx.is_some() => {
                start_rx = None;
                match r {
                    Ok(Ok(v)) => {
                        if let Some(t) = v.pointer("/turn/id").and_then(Value::as_str) {
                            turn_id = Some(t.to_string());
                        }
                    }
                    Ok(Err(e)) => return Err(acp::friendly_prompt_error(Engine::Codex, format!("Codex rejected the turn: {e}"))),
                    Err(_) => return Err(acp::friendly_prompt_error(Engine::Codex, server.exited_message("codex app-server exited"))),
                }
            }
            msg = server.events.recv() => {
                let Some(msg) = msg else {
                    if stop.load(Ordering::Relaxed) { return Ok(()); }
                    return Err(acp::friendly_prompt_error(Engine::Codex, server.exited_message("codex app-server exited unexpectedly")));
                };
                let method = msg.get("method").and_then(Value::as_str).unwrap_or("").to_string();
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                if let Some(t) = params.get("threadId").and_then(Value::as_str) {
                    if t != thread { continue; } // another thread (none expected)
                }
                if let Some(id) = msg.get("id") {
                    // server -> client request
                    if is_approval(&method) {
                        handle_approval(server, id, &method, &params, turn, on_event);
                    } else if method == "item/tool/requestUserInput" {
                        handle_user_input(server, id, &params, turn, on_event);
                    } else {
                        server.respond_error(id, "not supported by Vicuna");
                    }
                    continue;
                }
                match method.as_str() {
                    "turn/started" => {
                        if let Some(t) = params.pointer("/turn/id").and_then(Value::as_str) {
                            turn_id = Some(t.to_string());
                        }
                    }
                    "thread/tokenUsage/updated" => {
                        out_tokens = params.pointer("/tokenUsage/last/outputTokens").and_then(Value::as_f64);
                    }
                    "error" => {
                        if !params.get("willRetry").and_then(Value::as_bool).unwrap_or(false) {
                            if let Some(m) = params.pointer("/error/message").and_then(Value::as_str) {
                                emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n> Codex: {m}\n") });
                            }
                        }
                    }
                    "thread/goal/updated" => {
                        if let Some(text) = goal_note(&params, &mut last_goal) {
                            emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text });
                        }
                    }
                    "turn/plan/updated" => {
                        if let Some(text) = plan_note(&params) {
                            emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text });
                        }
                    }
                    "turn/completed" => {
                        let t = params.get("turn").unwrap_or(&Value::Null);
                        let gen_ms = t.get("durationMs").and_then(Value::as_u64).unwrap_or_else(|| started.elapsed().as_millis() as u64);
                        let secs = (gen_ms as f64 / 1000.0).max(0.001);
                        let gen_tps = out_tokens.map(|n| n / secs).or_else(|| (output_chars > 0).then(|| (output_chars as f64 / 4.0) / secs));
                        emit_ev(on_event, stop, WorkAgentEvent::RoundEnd { gen_ms, gen_tps, gen_cost: None });
                        let status = t.get("status").and_then(Value::as_str).unwrap_or("completed");
                        if status == "failed" && !stop.load(Ordering::Relaxed) {
                            let detail = t.pointer("/error/message").and_then(Value::as_str).unwrap_or("").trim().to_string();
                            let msg = if detail.is_empty() { server.exited_message("Codex turn failed") } else { format!("Codex turn failed: {detail}") };
                            return Err(acp::friendly_prompt_error(Engine::Codex, msg));
                        }
                        if stop.load(Ordering::Relaxed) || status == "interrupted" {
                            return Ok(());
                        }
                        let queued = acp::take_queue(turn);
                        if queued.is_empty() {
                            return Ok(());
                        }
                        let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
                        let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
                        emit_ev(on_event, stop, WorkAgentEvent::QueueDrained { ids, messages: drained.clone() });
                        let (text, imgs) = acp::messages_text(&drained);
                        turn_id = None;
                        tools.clear();
                        output_chars = 0;
                        out_tokens = None;
                        started = Instant::now();
                        start_rx = Some(server.send_request("turn/start", turn_start_params(thread, model, user_input(&text, &imgs))));
                        emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "continue".into() });
                    }
                    _ => {
                        for ev in translate(&method, &params, &mut tools, &mut output_chars) {
                            emit_ev(on_event, stop, ev);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                if stop.load(Ordering::Relaxed) {
                    if interrupted_at.is_none() {
                        if let Some(t) = &turn_id {
                            interrupted_at = Some(Instant::now());
                            let _ = server.send_request("turn/interrupt", json!({ "threadId": thread, "turnId": t }));
                        }
                    }
                    let since = interrupted_at.map(|t| t.elapsed()).unwrap_or_default();
                    if since > CANCEL_GRACE || (interrupted_at.is_none() && turn_id.is_none() && start_rx.is_none()) {
                        let _ = server.child.start_kill();
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_come_from_model_list() {
        // shape captured from codex-cli 0.166.0 `model/list`
        let page = json!({ "data": [
            { "id": "gpt-6.1-sol", "model": "gpt-6.1-sol", "displayName": "GPT-6.1-Sol", "hidden": false, "isDefault": true },
            { "id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "GPT-6-Luna", "hidden": false, "isDefault": false },
            { "id": "secret", "model": "secret", "displayName": "S", "hidden": true, "isDefault": false },
            { "id": "gpt-6-luna", "model": "gpt-6-luna", "displayName": "dup", "hidden": false }
        ], "nextCursor": null });
        let m: Vec<_> = parse_models(&[page]).into_iter().map(|m| (m.id, m.name)).collect();
        assert_eq!(m, vec![("gpt-6.1-sol".into(), Some("GPT-6.1-Sol".into())), ("gpt-6-luna".into(), Some("GPT-6-Luna".into()))]);
        assert!(parse_models(&[json!({})]).is_empty());
    }

    #[test]
    fn notifications_map_to_ui_events() {
        let mut tools = HashMap::new();
        let mut chars = 0;
        let d = translate("item/agentMessage/delta", &json!({ "threadId": "t", "delta": "PO" }), &mut tools, &mut chars);
        assert!(matches!(&d[..], [WorkAgentEvent::Token { text }] if text == "PO"));
        let r = translate("item/reasoning/summaryTextDelta", &json!({ "delta": "thinking" }), &mut tools, &mut chars);
        assert!(matches!(&r[..], [WorkAgentEvent::Think { text }] if text == "thinking"));
        assert!(translate("item/reasoning/textDelta", &json!({ "delta": "" }), &mut tools, &mut chars).is_empty());
        // command: card with the real command at start, output + exit code at the end
        let start = json!({ "item": { "type": "commandExecution", "id": "c1", "command": "/bin/bash -lc 'echo hi'", "cwd": "/w", "status": "inProgress" } });
        match &translate("item/started", &start, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "echo hi");
            }
            other => panic!("{other:?}"),
        }
        assert!(translate("item/started", &start, &mut tools, &mut chars).is_empty(), "no duplicate card");
        let done = json!({ "item": { "type": "commandExecution", "id": "c1", "command": "x", "status": "failed", "aggregatedOutput": "boom\n", "exitCode": 2 } });
        match &translate("item/completed", &done, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolEnd { name, output }] => {
                assert_eq!(name, "exec");
                assert!(output.starts_with("Error: failed") && output.contains("boom") && output.contains("[exit code 2]"), "{output}");
            }
            other => panic!("{other:?}"),
        }
        // MCP keeps server/tool and real arguments; result text on completion
        let mcp = json!({ "item": { "type": "mcpToolCall", "id": "m1", "server": "probe", "tool": "echo_args", "arguments": { "note": "x" }, "status": "inProgress" } });
        match &translate("item/started", &mcp, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "mcp__probe__echo_args");
                assert_eq!(args["note"], "x");
            }
            other => panic!("{other:?}"),
        }
        let mcp_done = json!({ "item": { "type": "mcpToolCall", "id": "m1", "server": "probe", "tool": "echo_args", "status": "completed", "result": { "content": [{ "type": "text", "text": "{\"note\":\"x\"}" }] }, "error": null } });
        assert!(matches!(&translate("item/completed", &mcp_done, &mut tools, &mut chars)[..], [WorkAgentEvent::ToolEnd { output, .. }] if output == "{\"note\":\"x\"}"));
        // file change: path on the card
        let fc = json!({ "item": { "type": "fileChange", "id": "f1", "changes": [{ "path": "/w/a.txt", "kind": { "type": "add" }, "diff": "" }], "status": "inProgress" } });
        assert!(matches!(&translate("item/started", &fc, &mut tools, &mut chars)[..], [WorkAgentEvent::ToolStart { name, args }] if name == "write" && args["path"] == "/w/a.txt"));
        // non-tool items: nothing; an agent message after text gets a separator
        assert!(translate("item/completed", &json!({ "item": { "type": "reasoning", "id": "r" } }), &mut tools, &mut chars).is_empty());
        assert!(matches!(&translate("item/started", &json!({ "item": { "type": "agentMessage", "id": "a2" } }), &mut tools, &mut chars)[..], [WorkAgentEvent::Token { text }] if text == "\n\n"));
    }

    #[test]
    fn ask_user_server_is_never_gated_and_native_question_is_pinned_off() {
        let mcp = crate::vicuna_mcp::with_ask_server(vec![json!({ "name": "p", "command": "x" })], Some("http://127.0.0.1:9/mcp/t"));
        let p = thread_params("/w", true, None, None, &mcp);
        assert_eq!(p["config"]["mcp_servers"]["vicuna-mcp"], json!({ "url": "http://127.0.0.1:9/mcp/t", "default_tools_approval_mode": "approve", "tool_timeout_sec": 86_400 }));
        assert_eq!(p["config"]["mcp_servers"]["p"]["default_tools_approval_mode"], "prompt");
        assert_eq!(p["config"]["features"]["default_mode_request_user_input"], false);
        assert_eq!(thread_params("/w", false, None, None, &[])["config"], json!({ "features": { "default_mode_request_user_input": false } }));
    }

    #[test]
    fn native_question_maps_to_the_ask_card() {
        let q = json!({ "id": "fruit", "header": "Fruit", "question": "Which fruit?", "isOther": true, "isSecret": false,
            "options": [{ "label": "Mango", "description": "" }, { "label": "Kiwi", "description": "green" }] });
        assert_eq!(user_input_question(&q), ("fruit".to_string(), "Fruit: Which fruit?".to_string(), vec!["Mango".to_string(), "Kiwi".to_string()]));
        let bare = json!({ "id": "x", "header": "", "question": "Name?", "options": null });
        assert_eq!(user_input_question(&bare).2, Vec::<String>::new());
    }

    #[test]
    fn goal_updates_render_once_per_change() {
        let mut last = None;
        let ev = |status: &str, used: u64| json!({ "threadId": "t", "turnId": null, "goal": {
            "threadId": "t", "objective": "ship the fix", "status": status, "tokenBudget": null, "tokensUsed": used,
            "timeUsedSeconds": 1, "createdAt": 0, "updatedAt": 0 } });
        assert_eq!(goal_note(&ev("active", 1), &mut last).as_deref(), Some("\n> **Goal:** ship the fix\n"));
        assert_eq!(goal_note(&ev("active", 50), &mut last), None, "counter-only updates stay quiet");
        assert_eq!(goal_note(&ev("complete", 90), &mut last).as_deref(), Some("\n> **Goal complete:** ship the fix\n"));
    }

    #[test]
    fn plan_updates_render_as_a_checklist() {
        let n = plan_note(&json!({ "explanation": "two steps", "plan": [
            { "step": "read", "status": "completed" }, { "step": "edit", "status": "inProgress" }, { "step": "test", "status": "pending" } ] })).unwrap();
        assert!(n.contains("**Plan**\ntwo steps\n- [x] read\n- [~] edit\n- [ ] test\n"), "{n}");
        assert_eq!(plan_note(&json!({ "plan": [] })), None);
    }

    #[test]
    fn mcp_servers_become_config_overrides() {
        let cfg = mcp_servers(&[
            json!({ "name": "probe", "command": "/usr/bin/python3", "args": ["s.py", "3"], "env": [{ "name": "K", "value": "v" }] }),
            json!({ "type": "http", "name": "search api", "url": "https://m/mcp", "headers": [{ "name": "Authorization", "value": "Bearer t" }] }),
        ], false)
        .unwrap();
        assert_eq!(cfg["probe"]["default_tools_approval_mode"], "approve");
        assert_eq!(mcp_servers(&[json!({ "name": "p", "command": "x" })], true).unwrap()["p"]["default_tools_approval_mode"], "prompt");
        assert_eq!(cfg["probe"]["command"], "/usr/bin/python3");
        assert_eq!(cfg["probe"]["args"], json!(["s.py", "3"]));
        assert_eq!(cfg["probe"]["env"]["K"], "v");
        assert_eq!(cfg["search_api"]["url"], "https://m/mcp");
        assert_eq!(cfg["search_api"]["http_headers"]["Authorization"], "Bearer t");
        assert_eq!(mcp_servers(&[], false), None);
    }

    #[test]
    fn approval_toggle_maps_to_codex_policy() {
        let auto = thread_params("/w", false, None, None, &[]);
        assert_eq!(auto["approvalPolicy"], "never");
        assert_eq!(auto["sandbox"], "workspace-write");
        assert!(auto.get("model").is_none(), "no pick: the user's config.toml model runs");
        assert!(auto["config"].get("mcp_servers").is_none());
        let ask = thread_params("/w", true, Some("read-only"), Some("gpt-6-luna"), &[json!({ "name": "p", "command": "x" })]);
        assert_eq!(ask["approvalPolicy"], "untrusted");
        assert_eq!(ask["sandbox"], "read-only", "the user's sandbox_mode is kept");
        assert_eq!(ask["model"], "gpt-6-luna");
        assert_eq!(ask["config"]["mcp_servers"]["p"]["command"], "x");
        let none = json!({});
        assert_eq!(approval_answer("item/commandExecution/requestApproval", &none, true)["decision"], "accept");
        assert_eq!(approval_answer("item/fileChange/requestApproval", &none, false)["decision"], "decline");
        assert_eq!(approval_answer("execCommandApproval", &none, true)["decision"], "approved");
        let form = json!({ "serverName": "probe", "mode": "form", "message": "Allow noop_001?" });
        assert_eq!(approval_answer("mcpServer/elicitation/request", &form, true)["action"], "accept");
        assert_eq!(approval_answer("mcpServer/elicitation/request", &json!({ "mode": "url" }), true)["action"], "decline");
        assert_eq!(approval_title("mcpServer/elicitation/request", &form), "MCP probe: Allow noop_001");
        assert!(is_approval("item/commandExecution/requestApproval") && is_approval("mcpServer/elicitation/request"));
        assert!(!is_approval("item/permissions/requestApproval") && !is_approval("item/tool/requestUserInput"));
        assert_eq!(approval_title("item/commandExecution/requestApproval", &json!({ "command": "rm x" })), "exec: rm x");
    }

    #[test]
    fn shell_wrapper_is_unwrapped_for_cards() {
        assert_eq!(shell_inner("/usr/bin/bash -lc 'mkdir -p a && cat a/x'"), "mkdir -p a && cat a/x");
        assert_eq!(shell_inner("ls -la"), "ls -la");
        assert_eq!(shell_inner(r#"/bin/zsh -lc 'a'b'"#), r#"/bin/zsh -lc 'a'b'"#, "inner quotes: shown as is");
        assert_eq!(approval_title("mcpServer/elicitation/request", &json!({ "serverName": "p", "message": "Run x?" })), "MCP p: Run x");
    }

    #[test]
    fn images_become_user_input() {
        let i = user_input("hi", &Some(vec!["data:image/png;base64,AAAA".into(), "bogus".into()]));
        assert_eq!(i.len(), 2);
        assert_eq!(i[0]["type"], "text");
        assert_eq!(i[1]["url"], "data:image/png;base64,AAAA");
    }
}
