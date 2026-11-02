// Vicuna Work agent loop (Tauri backend).
//
// The agent rounds live here now: the frontend sends one turn request, this
// module runs the round scheduler + tool dispatch + background-job tracking,
// and streams semantic events back over an EventSink. The webview stays the
// display layer — it mirrors events into the chat bubble with the existing
// <details> builders / ask cards, and answers `ask` + steering via commands.
//
// Event contract (frontend builds display markdown from these):
//   Token/Think      -> append to the live assistant bubble (batched ~120ms)
//   ToolStart        -> append formatToolCallHead (frontend helper)
//   ToolEnd          -> append formatToolCallTail
//   Status           -> toolStatus label ("Compacting…", "Waiting for …")
//   AskRequest       -> raise the ask card; frontend MUST call work_agent_answer
//   BgCompleted      -> append a collapsed result block for a finished bg job
//   ContextCompacted -> append the "> Context compacted" note to the bubble
//   Done/Error       -> settle the turn (mirror of the old JS loop exits)

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;
use crate::events::{encode, Sink};
use tokio::sync::oneshot;

use super::{
    bg_kill, bg_list, bg_snapshot, bg_start, parse_show_context_length,
    undo_begin_turn, undo_capture_prev, undo_key, undo_record, undo_record_write,
    upload_read_file_logic as upload_read_file, work_exec_logic as work_exec,
    work_fetch_url_logic as work_fetch_url, work_grep_logic as work_grep,
    work_list_dir_logic as work_list_dir, work_read_file_logic as work_read_file,
    work_read_image_logic as work_read_image, work_write_file_logic as work_write_file,
    BgExecState, ChatMessage, ollama_base, UndoOp, UndoState,
};
use crate::browser::{browser_mcp_command, browser_start_logic, lb_server_url, BrowserState};
use crate::mcp::{
    delete_file_server, list_file_servers, save_file_server, McpManager, McpServerConfig,
    McpServerInfo,
};

// ---------- public types ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatSnapshotMsg {
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ChatSnapshot {
    pub id: String,
    pub title: String,
    pub mode: Option<String>,
    pub messages: Vec<ChatSnapshotMsg>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct McpRoute {
    pub server_id: String,
    pub tool_name: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WorkAgentEvent {
    Token { text: String },
    Think { text: String },
    Status { label: Option<String> },
    RoundStart { mode: String },
    RoundEnd { gen_ms: u64, gen_tps: Option<f64>, gen_cost: Option<f64> },
    DisplayNote { text: String },
    QueueDrained { ids: Vec<String>, messages: Vec<ChatMessage> },
    ToolStart { name: String, args: serde_json::Value },
    ToolEnd { name: String, output: String },
    AskRequest { ask_id: String, question: String, options: Vec<String> },
    BgCompleted { job_id: String, command: String, output: String },
    ContextCompacted { note: String },
    Done { stopped: bool },
    Error { message: String },
}

/// Best-effort event send: when the webview is gone (e.g. refresh mid-turn)
/// the Channel breaks — mark the turn stopped so the backend loop unwinds
/// instead of running headless (mirrors the old `client_gone` drain break).
fn emit_ev(ch: &Sink, stop: &AtomicBool, ev: WorkAgentEvent) {
    if ch.send_json(&encode(&ev)).is_err() {
        stop.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub struct WorkAgentState {
    seq: AtomicU64,
    turns: Mutex<HashMap<String, Arc<TurnState>>>,
}

impl WorkAgentState {
    /// Register a new turn, prune old ones, and hand back its id.
    /// Shared by the Tauri command and the headless HTTP bridge.
    pub fn next_turn(&self, turn: Arc<TurnState>) -> String {
        let turn_id = format!("turn-{}", self.seq.fetch_add(1, Ordering::Relaxed) + 1);
        if let Ok(mut turns) = self.turns.lock() {
            turns.insert(turn_id.clone(), turn);
            while turns.len() > 20 {
                if let Some(k) = turns.keys().next().cloned() {
                    turns.remove(&k);
                } else {
                    break;
                }
            }
        }
        turn_id
    }

    pub fn get_turn(&self, turn_id: &str) -> Option<Arc<TurnState>> {
        self.turns.lock().ok()?.get(turn_id).cloned()
    }
}

pub struct TurnState {
    pub stop: AtomicBool,
    pub queue: Mutex<Vec<(String, ChatMessage)>>,
    pub ask_pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    pub ask_seq: AtomicU64,
}

impl TurnState {
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            queue: Mutex::new(Vec::new()),
            ask_pending: Mutex::new(HashMap::new()),
            ask_seq: AtomicU64::new(1),
        }
    }
}

// ---------- helpers (mirrors of the old JS harness) ----------

fn tool_key(name: &str, args: &serde_json::Value) -> String {
    format!("{name}\n{args}")
}

/// Repeat-guard key: `done` dedups by name alone. A model that signs off
/// with a fresh summary each round would otherwise never trip the guard
/// (every key differs) and — with no round cap — loop forever on the
/// stray-done error below. Goal-mode `done` always ends the turn on first
/// sight, so this only affects unoffered calls.
fn seen_key(name: &str, args: &serde_json::Value) -> String {
    if name == "done" {
        "done".to_string()
    } else {
        tool_key(name, args)
    }
}

/// Mirror of the frontend offer condition (`startGoal && !startGoal.paused`,
/// where chatGoal needs non-empty text): `done` is only a real tool while
/// an unpaused session goal is set.
fn goal_done_offered(goal_text: &Option<String>, goal_paused: bool) -> bool {
    goal_text.as_ref().is_some_and(|g| !g.is_empty()) && !goal_paused
}

fn estimate_tokens(messages: &[ChatMessage], tools: &Option<serde_json::Value>) -> usize {
    let mut n = 0usize;
    for m in messages {
        n += m.content.len() / 4;
        if let Some(imgs) = &m.images {
            n += imgs.len() * 1500;
        }
        if let Some(tc) = &m.tool_calls {
            n += tc.to_string().len() / 4;
        }
        n += 4;
    }
    if let Some(t) = tools {
        n += t.to_string().len() / 4;
    }
    n
}

fn extract_bg_id(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 3 < bytes.len() {
        if bytes[i..].starts_with(b"bg-") {
            let mut j = i + 3;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 3 {
                return Some(text[i..j].to_string());
            }
        }
        i += 1;
    }
    None
}

fn extract_viewed_images(text: &str) -> (String, Vec<String>) {
    let mut images = Vec::new();
    let mut clean = text.to_string();
    loop {
        let Some(start) = clean.find("[[VICUNA_IMAGE:") else {
            break;
        };
        let Some(end_rel) = clean[start..].find("]]") else {
            break;
        };
        let end = start + end_rel + 2;
        let url = clean[start + "[[VICUNA_IMAGE:".len()..start + end_rel].to_string();
        if url.starts_with("data:image/") {
            images.push(url);
        }
        clean.replace_range(start..end, "");
    }
    (clean.trim().to_string(), images)
}

fn search_sessions(chats: &[ChatSnapshot], query: &str, limit: usize) -> String {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return "Error: sessions needs {\"query\": \"...\"}.".to_string();
    }
    let mut out = Vec::new();
    for c in chats {
        if out.len() >= limit {
            break;
        }
        if c.id.to_lowercase().contains(&q) || c.title.to_lowercase().contains(&q) {
            let first = c
                .messages
                .iter()
                .find(|m| m.role == "user")
                .map(|m| m.content.clone())
                .unwrap_or_default();
            let excerpt: String = first.chars().take(200).collect();
            out.push(format!(
                "• \"{}\" [{}] id={}\n  {}",
                c.title,
                c.mode.clone().unwrap_or_else(|| "chat".to_string()),
                c.id,
                excerpt
            ));
            continue;
        }
        let mut hit: Option<String> = None;
        for m in &c.messages {
            let low = m.content.to_lowercase();
            if let Some(pos) = low.find(&q) {
                let start = pos.saturating_sub(60);
                let excerpt: String = m.content.chars().skip(start).take(pos - start + q.len() + 120).collect();
                hit = Some(excerpt);
                break;
            }
        }
        if let Some(ex) = hit {
            out.push(format!(
                "• \"{}\" [{}] id={}\n  {}",
                c.title,
                c.mode.clone().unwrap_or_else(|| "chat".to_string()),
                c.id,
                ex
            ));
        }
    }
    if out.is_empty() {
        "No matching sessions.".to_string()
    } else {
        out.join("\n")
    }
}

fn read_session(chats: &[ChatSnapshot], id: &str, limit: usize) -> String {
    let limit = limit.clamp(1, 100);
    match chats.iter().find(|c| c.id == id) {
        None => "No session found for that id.".to_string(),
        Some(c) => {
            let msgs: Vec<_> = c.messages.iter().rev().take(limit).rev().collect();
            let mut s = format!(
                "Session \"{}\" ({}), {} messages:\n\n",
                c.title,
                c.mode.clone().unwrap_or_else(|| "chat".to_string()),
                c.messages.len()
            );
            for m in msgs {
                let who = if m.role == "user" { "User" } else { "Assistant" };
                let body: String = m.content.chars().take(2000).collect();
                s.push_str(&format!("{who}: {body}\n\n"));
            }
            s
        }
    }
}

// ---------- agent MCP management (`mcp_list` / `mcp_add` / `mcp_remove`) ----------
//
// The Work agent manages its own MCP servers: the user says "add X, the
// endpoint is …" and the agent saves + connects + verifies on the spot. New
// tools join the NEXT turn (the tool list is fixed per turn); the add result
// carries the verified tool catalog as text so this turn can still report it.

/// The synthetic in-memory Browser row (`lb mcp`): enable/disable act on
/// its runtime connection (the daemon is ensured on enable, like the UI
/// toggle), but it has no file entry so add/remove stay refused.
const BROWSER_MCP_ID: &str = "browser";

fn norm_transport(t: &str) -> &str {
    match t {
        "local" | "stdio" => "stdio",
        "remote" | "http" => "http",
        other => other,
    }
}

/// In-memory `browser` config for the agent path. Mirrors the frontend's
/// `browserMcpConfig` exactly (same id/name/transport/command/LB_SERVER
/// env): the two must stay in sync or list/enable disagree about what
/// "browser" is.
fn synthetic_browser_config(lb_argv: Vec<String>) -> McpServerConfig {
    McpServerConfig {
        id: BROWSER_MCP_ID.to_string(),
        name: BROWSER_MCP_ID.to_string(),
        transport: "stdio".to_string(),
        command: Some(lb_argv),
        env: Some(HashMap::from([("LB_SERVER".to_string(), lb_server_url())])),
        url: None,
        headers: None,
        oauth: None,
        enabled: true,
    }
}

/// Config the agent's browser enable/disable/list acts on: a file entry
/// named `browser` wins (explicit config beats the default, like the
/// frontend's synthetic-row suppression), else the resolved `lb mcp` argv.
/// `None` = no browser at all (no `lb` binary anywhere).
fn browser_agent_config(
    file_servers: &[McpServerConfig],
    lb_argv: Option<Vec<String>>,
) -> Option<McpServerConfig> {
    if let Some(f) = file_servers
        .iter()
        .find(|s| s.name.to_lowercase() == BROWSER_MCP_ID)
    {
        return Some(f.clone());
    }
    lb_argv.map(synthetic_browser_config)
}

/// One-line endpoint summary for agent transcripts. Header/env values never
/// appear here (or in tool labels) — the transcript is not a secrets store.
fn describe_mcp_endpoint(c: &McpServerConfig) -> String {
    if norm_transport(&c.transport) == "http" {
        format!("remote {}", c.url.clone().unwrap_or_default())
    } else {
        format!("local {}", c.command.clone().unwrap_or_default().join(" "))
    }
}

/// Build a server config from `mcp_add` args. Exactly one of `url` (remote
/// streamable-HTTP endpoint) or `command` (local stdio argv) selects the
/// transport. Pure + total: every rejection happens before any disk write.
fn parse_mcp_add_args(args: &serde_json::Value) -> Result<McpServerConfig, String> {
    let name = str_arg(args, "name").trim().to_string();
    if name.is_empty() {
        return Err("Error: mcp_add needs a \"name\" for the server.".to_string());
    }
    if name.len() > 100 || name.contains('/') || name.contains('\\') {
        return Err("Error: server name must be ≤100 chars with no slashes.".to_string());
    }
    let url = str_arg(args, "url").trim().to_string();
    let command: Vec<String> = args
        .get("command")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .filter(|s| !s.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    match (url.is_empty(), command.is_empty()) {
        (true, true) => {
            return Err(
                "Error: mcp_add needs either \"url\" (remote http endpoint) or \"command\" (local argv array)."
                    .to_string(),
            )
        }
        (false, false) => {
            return Err(
                "Error: pass only one of \"url\" or \"command\" — url means a remote server, command a local one."
                    .to_string(),
            )
        }
        _ => {}
    }
    fn str_map(v: Option<&serde_json::Value>) -> Option<HashMap<String, String>> {
        let obj = v?.as_object()?;
        if obj.is_empty() {
            return None;
        }
        Some(
            obj.iter()
                .map(|(k, val)| {
                    (
                        k.clone(),
                        val.as_str().unwrap_or(&val.to_string()).to_string(),
                    )
                })
                .collect(),
        )
    }
    if command.is_empty() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err("Error: \"url\" must start with http:// or https://.".to_string());
        }
        Ok(McpServerConfig {
            id: name.clone(),
            name,
            transport: "http".to_string(),
            command: None,
            env: None,
            url: Some(url),
            headers: str_map(args.get("headers")),
            oauth: None,
            enabled: true,
        })
    } else {
        Ok(McpServerConfig {
            id: name.clone(),
            name,
            transport: "stdio".to_string(),
            command: Some(command),
            env: str_map(args.get("env")),
            url: None,
            headers: None,
            oauth: None,
            enabled: true,
        })
    }
}

#[derive(Debug)]
enum McpAddPlan {
    Fresh,
    Refresh,
    Overwrite,
}

/// What `mcp_add` should do about an existing same-name entry: an identical
/// endpoint just reconnects (refresh), a re-pointed one needs explicit
/// `overwrite: true` so a typo'd name can't silently kill a working server.
/// Header/env-only differences update freely (a credential refresh is a
/// routine update, not a re-point).
fn mcp_add_plan(
    existing: Option<&McpServerConfig>,
    new: &McpServerConfig,
    overwrite: bool,
) -> Result<McpAddPlan, String> {
    let Some(prev) = existing else {
        return Ok(McpAddPlan::Fresh);
    };
    let same_endpoint = norm_transport(&prev.transport) == norm_transport(&new.transport)
        && if norm_transport(&new.transport) == "http" {
            prev.url == new.url
        } else {
            prev.command == new.command
        };
    if same_endpoint {
        return Ok(McpAddPlan::Refresh);
    }
    if overwrite {
        return Ok(McpAddPlan::Overwrite);
    }
    Err(format!(
        "Error: a server named \"{}\" already exists ({}). Re-run with {{\"overwrite\": true}} to replace it, or pick another name.",
        prev.name,
        describe_mcp_endpoint(prev)
    ))
}

fn format_mcp_list(servers: &[McpServerConfig], connected: &HashSet<String>) -> String {
    if servers.is_empty() {
        return "No MCP servers configured. Add one with mcp_add (name + url or command) — ask the user for the endpoint first."
            .to_string();
    }
    servers
        .iter()
        .map(|s| {
            // the browser row is runtime-only (no enabled flag in the file
            // unless hand-added): "not connected" states the operative fact
            let status = if connected.contains(&s.id) {
                "connected"
            } else if s.id == BROWSER_MCP_ID && s.enabled {
                "not connected (built-in)"
            } else if s.enabled {
                "enabled, not connected"
            } else {
                "disabled"
            };
            format!("• \"{}\" [{}] — {status}", s.name, describe_mcp_endpoint(s))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Success record for `mcp_add`/`mcp_enable`: endpoint + verified tool catalog.
fn format_mcp_connected(action: &str, info: &McpServerInfo, cfg: &McpServerConfig) -> String {
    let n = info.tools.len();
    let mut s = format!(
        "{action} + connected \"{}\" ({}): {n} tool{}.",
        info.name,
        describe_mcp_endpoint(cfg),
        if n == 1 { "" } else { "s" }
    );
    if n > 0 {
        let names: Vec<&str> = info.tools.iter().take(20).map(|t| t.name.as_str()).collect();
        s.push_str(&format!("\nVerified tools: {}", names.join(", ")));
        if n > 20 {
            s.push_str(&format!(" (+{} more)", n - 20));
        }
    }
    s.push_str("\nThey join your tool list from your next reply onward — this turn can't call them yet.");
    s
}

/// Enable with no file entry: resolve `lb`, ensure the daemon (adopt a
/// running one or spawn ours, like opening the tab), connect the
/// in-memory config. Nothing is persisted — there is no entry for it.
async fn mcp_enable_synthetic_browser(
    mcp: &McpManager,
    browser_resource_dir: Option<&std::path::Path>,
    browser: &BrowserState,
) -> String {
    let Ok(argv) = browser_mcp_command(browser_resource_dir) else {
        return "Error: the built-in browser isn't available here (no `lb` engine found). It works in the Vicuna desktop app or wherever live-browser is installed."
            .to_string();
    };
    if let Err(e) = browser_start_logic(browser_resource_dir.map(|p| p.to_path_buf()), browser).await
    {
        return format!("Error: could not start the browser daemon ({e}).");
    }
    let cfg = synthetic_browser_config(argv);
    match mcp.connect(&cfg).await {
        Ok(info) => format_mcp_connected("Enabled", &info, &cfg),
        Err(e) => format!(
            "Error: the browser daemon is up but `lb mcp` would not connect ({e}). Ask the user to open the Browser tab once, then retry."
        ),
    }
}

/// Disable with no file entry: drop the runtime connection only. The
/// tab/daemon keeps running — the MCP link is separate from the tab.
async fn mcp_disable_synthetic_browser(mcp: &McpManager) -> String {
    match mcp.disconnect(BROWSER_MCP_ID).await {
        Ok(()) => "Disconnected the built-in Browser tools (the Browser tab/daemon keeps running — the MCP link is separate from the tab). Re-enable anytime with mcp_enable."
            .to_string(),
        Err(_) => "\"browser\" is already disconnected.".to_string(),
    }
}

fn extract_fences(text: &str) -> Vec<(String, serde_json::Value)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("```tool") {
        let after = &rest[start + "```tool".len()..];
        let Some(end) = after.find("```") else { break };
        let body = after[..end].trim();
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
            if let (Some(name), Some(args)) = (v.get("name").and_then(|x| x.as_str()), v.get("args")) {
                if args.is_object() {
                    out.push((name.to_string(), args.clone()));
                }
            }
        }
        rest = &after[end + 3..];
    }
    out
}

fn has_tool_fence(text: &str) -> bool {
    // loose port of the old /```+\s*tool\b/ detection
    let b = text.as_bytes();
    let mut i = 0;
    while i + 3 <= b.len() {
        if b[i] == b'`' && text[i..].starts_with("```") {
            let mut j = i;
            while j < b.len() && b[j] == b'`' {
                j += 1;
            }
            while j < b.len() && (b[j] == b' ' || b[j] == b'\t' || b[j] == b'\n' || b[j] == b'\r') {
                j += 1;
            }
            if text[j..].starts_with("tool")
                && text[j + 4..].chars().next().is_none_or(|c| !c.is_alphanumeric() && c != '_')
            {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

/// Per-round generation rate for the footer, mirroring the old streamInto
/// math (server eval stats when present, eval tokens over wall time for
/// cloud APIs that omit eval_duration).
fn round_tps(r: &RoundResult, wall_ms: u64) -> Option<f64> {
    if r.eval_duration_ns > 0 && r.eval_count > 0 {
        Some(r.eval_count as f64 / (r.eval_duration_ns as f64 / 1e9))
    } else if r.eval_count > 0 && wall_ms > 0 {
        Some(r.eval_count as f64 / (wall_ms as f64 / 1000.0))
    } else {
        None
    }
}

fn tool_label(name: &str, args: &serde_json::Value) -> String {
    let summary = match name {
        "exec" => args.get("command").and_then(|v| v.as_str()).unwrap_or(""),
        "exec_status" => args.get("id").and_then(|v| v.as_str()).unwrap_or("(list)"),
        "exec_kill" => args.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        "read" | "write" | "list" | "view_image" => args.get("path").and_then(|v| v.as_str()).unwrap_or("."),
        "grep" => args.get("pattern").and_then(|v| v.as_str()).unwrap_or(""),
        "sessions" => args.get("query").and_then(|v| v.as_str()).unwrap_or(""),
        "read_session" => args.get("id").and_then(|v| v.as_str()).unwrap_or(""),
        "read_upload" => args.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "say" => args.get("text").and_then(|v| v.as_str()).unwrap_or(""),
        "ask" => args.get("question").and_then(|v| v.as_str()).unwrap_or(""),
        "fetch_url" => args.get("url").and_then(|v| v.as_str()).unwrap_or(""),
        // MCP management labels show the name only — never headers/env: the
        // status line lands in the visible transcript, not a secrets store
        "mcp_add" | "mcp_remove" | "mcp_enable" | "mcp_disable" => args.get("name").and_then(|v| v.as_str()).unwrap_or(""),
        "mcp_list" => "",
        _ => "",
    };
    let short: String = summary.chars().take(120).collect();
    format!("{name} — {short}")
}

fn str_arg(args: &serde_json::Value, key: &str) -> String {
    args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string()
}

fn fmt_bg(s: &super::BgJobStatus) -> String {
    let mut parts = Vec::new();
    if !s.stdout.trim().is_empty() {
        parts.push(s.stdout.clone());
    }
    if !s.stderr.trim().is_empty() {
        parts.push(format!("[stderr]\n{}", s.stderr));
    }
    parts.push(if s.running {
        format!("[job {} still running]", s.id)
    } else {
        format!("[job {} exit {}]", s.id, s.exit_code.unwrap_or(-1))
    });
    parts.join("\n")
}

// ---------- Ollama ----------

struct RoundResult {
    text: String,
    tool_calls: Vec<serde_json::Value>,
    eval_count: u64,
    eval_duration_ns: u64,
    cost: Option<f64>,
}

/// One streaming /api/chat round with the same retry ladder as the old
/// frontend chatStream: text-only models (vision strip + note), plain models
/// (think:false), string think levels (think:true), models without tools
/// support (retry without tools — the caller falls back to ```tool fences).
async fn ollama_round(
    model: &str,
    messages: &[ChatMessage],
    think: &Option<serde_json::Value>,
    tools: &Option<serde_json::Value>,
    on_event: &Sink,
    stop: &AtomicBool,
) -> Result<RoundResult, String> {
    let mut think = think.clone();
    let mut tools = tools.clone();
    let mut strip_images = false;
    // attempt index mirrors the old ladder (0 = full request, then fallbacks)
    for _ in 0..4 {
        match ollama_round_attempt(model, messages, &think, &tools, strip_images, on_event, stop).await {
            Ok(r) => {
                return Ok(r);
            }
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    return Err(e);
                }
                if e.contains("does not support multimodal") && !strip_images {
                    strip_images = true;
                    emit_ev(
                        on_event,
                        stop,
                        WorkAgentEvent::DisplayNote {
                            text: format!(
                                "Note: {model} doesn't support images, so the attached picture was not sent."
                            ),
                        },
                    );
                    continue;
                }
                if e.contains("does not support thinking") && think != Some(serde_json::Value::Bool(false)) {
                    think = Some(serde_json::Value::Bool(false));
                    continue;
                }
                if e.contains("think")
                    && !e.contains("does not support thinking")
                    && think.as_ref().is_some_and(|v| v.is_string())
                {
                    think = Some(serde_json::Value::Bool(true));
                    continue;
                }
                {
                    let low = e.to_lowercase();
                    if low.contains("tool")
                        && (low.contains("not supported") || low.contains("does not support") || low.contains("unsupported"))
                        && tools.is_some()
                    {
                        tools = None;
                        continue;
                    }
                }
                return Err(e);
            }
        }
    }
    Err("ollama chat failed: retry ladder exhausted".to_string())
}

async fn ollama_round_attempt(
    model: &str,
    messages: &[ChatMessage],
    think: &Option<serde_json::Value>,
    tools: &Option<serde_json::Value>,
    strip_images: bool,
    on_event: &Sink,
    stop: &AtomicBool,
) -> Result<RoundResult, String> {
    let mut options = serde_json::Map::new();
    options.insert("num_ctx".to_string(), serde_json::json!(32768));
    let wire: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| {
            let mut v = serde_json::json!({ "role": m.role, "content": m.content });
            if !strip_images {
                if let Some(imgs) = &m.images {
                    v["images"] = serde_json::json!(imgs.iter().map(|u| {
                        match u.find(',') {
                            Some(i) => &u[i + 1..],
                            None => u.as_str(),
                        }
                    }).collect::<Vec<_>>());
                }
            }
            if let Some(tc) = &m.tool_calls {
                v["tool_calls"] = tc.clone();
            }
            if m.role == "tool" {
                if let Some(tn) = &m.tool_name {
                    v["tool_name"] = serde_json::json!(tn);
                }
            }
            v
        })
        .collect();
    let mut body = serde_json::json!({
        "model": model,
        "messages": wire,
        "stream": true,
        "think": think.clone().unwrap_or(serde_json::Value::Bool(true)),
        "options": options,
    });
    if let Some(t) = tools {
        if !t.is_null() {
            body["tools"] = t.clone();
        }
    }
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/api/chat", ollama_base()))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama chat failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("ollama chat failed: {status} {text}"));
    }
    use futures_util::StreamExt;
    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut eval_count: u64 = 0;
    let mut eval_duration_ns: u64 = 0;
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let chunk = match stream.next().await {
            Some(Ok(c)) => c,
            Some(Err(e)) => return Err(format!("ollama chat failed: {e}")),
            None => break,
        };
        buf.extend_from_slice(&chunk);
        let mut start = 0;
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                let line = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
                start = i + 1;
                if line.is_empty() {
                    continue;
                }
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
                    if eval_count == 0 {
                        eval_count = obj.get("eval_count").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                    if eval_duration_ns == 0 {
                        eval_duration_ns = obj.get("eval_duration").and_then(|v| v.as_u64()).unwrap_or(0);
                    }
                    if let Some(tc) = obj
                        .get("message")
                        .and_then(|m| m.get("tool_calls"))
                        .and_then(|v| v.as_array())
                    {
                        tool_calls.extend(tc.clone());
                    }
                    if let Some(t) = obj
                        .get("message")
                        .and_then(|m| m.get("thinking"))
                        .and_then(|v| v.as_str())
                    {
                        if !t.is_empty() {
                            emit_ev(on_event, stop, WorkAgentEvent::Think { text: t.to_string() });
                        }
                    }
                    if let Some(t) = obj
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        if !t.is_empty() {
                            full.push_str(t);
                            emit_ev(on_event, stop, WorkAgentEvent::Token { text: t.to_string() });
                        }
                    }
                }
            }
        }
        buf.drain(..start);
    }
    Ok(RoundResult { text: full, tool_calls, eval_count, eval_duration_ns, cost: None })
}

async fn ollama_oneshot(model: &str, system: &str, user: &str, stop: &AtomicBool) -> Result<String, String> {
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
        "think": false,
        "options": { "num_ctx": 32768, "temperature": 0.2 },
    });
    let fut = client.post(format!("{}/api/chat", ollama_base())).json(&body).send();
    let res = tokio::select! {
        r = fut => r.map_err(|e| format!("ollama chat failed: {e}"))?,
        _ = async { while !stop.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => {
            return Err("stopped".to_string());
        }
    };
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(format!("ollama chat failed: {status} {text}"));
    }
    let data: serde_json::Value = res.json().await.map_err(|e| format!("ollama chat failed: {e}"))?;
    Ok(data
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string())
}

async fn model_context_length(model: &str) -> Option<u64> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/api/show", ollama_base()))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .ok()?;
    if !res.status().is_success() {
        return None;
    }
    let data: serde_json::Value = res.json().await.ok()?;
    data.get("model_info").and_then(parse_show_context_length)
}

// ---------- OpenRouter (Cloud models via the agent loop) ----------

const OPENROUTER_BASE: &str = "https://openrouter.ai/api/v1";

fn openrouter_error(status: reqwest::StatusCode, body: &str) -> String {
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("error").cloned())
        .and_then(|e| match e {
            serde_json::Value::String(s) => Some(s),
            other => other
                .get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string()),
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| body.chars().take(400).collect());
    format!("openrouter chat failed: {status} {detail}")
}

/// `think` → OpenRouter `reasoning`. Auto/true is left out so the provider's
/// model default applies (forcing `enabled: true` would switch reasoning on
/// for models that default it off).
fn openrouter_reasoning(think: &Option<serde_json::Value>) -> Option<serde_json::Value> {
    match think {
        Some(serde_json::Value::Bool(false)) => Some(serde_json::json!({ "enabled": false })),
        Some(serde_json::Value::String(level)) => {
            let l = level.to_lowercase();
            if matches!(l.as_str(), "minimal" | "low" | "medium" | "high" | "xhigh") {
                Some(serde_json::json!({ "effort": l }))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn json_piece(v: Option<&serde_json::Value>) -> String {
    match v {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// History → OpenAI chat messages. Tool results need a `tool_call_id`; the
/// shared ChatMessage only carries `tool_name`, so ids are assigned
/// deterministically (or reused from the raw call) and matched in order.
fn openrouter_wire(messages: &[ChatMessage], strip_images: bool) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut pending: Vec<(String, String)> = Vec::new();
    let mut seq = 0usize;
    for m in messages {
        match m.role.as_str() {
            "assistant" => {
                let mut v = serde_json::json!({ "role": "assistant", "content": m.content });
                if let Some(arr) = m.tool_calls.as_ref().and_then(|t| t.as_array()) {
                    let mut calls = Vec::new();
                    for tc in arr {
                        let name = tc
                            .get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args = match tc.get("function").and_then(|f| f.get("arguments")) {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(other) => other.to_string(),
                            None => "{}".to_string(),
                        };
                        let id = match tc.get("id").and_then(|v| v.as_str()) {
                            Some(s) if !s.is_empty() => s.to_string(),
                            _ => {
                                let id = format!("call_{seq}");
                                seq += 1;
                                id
                            }
                        };
                        if !name.is_empty() {
                            pending.push((name.clone(), id.clone()));
                        }
                        calls.push(serde_json::json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": args }
                        }));
                    }
                    v["tool_calls"] = serde_json::Value::Array(calls);
                }
                out.push(v);
            }
            "tool" => {
                let name = m.tool_name.clone().unwrap_or_default();
                let at = pending.iter().position(|(n, _)| n == &name).unwrap_or(0);
                let id = if pending.is_empty() {
                    let id = format!("call_unknown_{seq}");
                    seq += 1;
                    id
                } else {
                    pending.remove(at).1
                };
                out.push(serde_json::json!({
                    "role": "tool",
                    "tool_call_id": id,
                    "content": m.content
                }));
            }
            role => {
                let images = m.images.as_ref().filter(|i| !i.is_empty());
                if !strip_images && images.is_some() {
                    let mut parts = Vec::new();
                    if !m.content.is_empty() {
                        parts.push(serde_json::json!({ "type": "text", "text": m.content }));
                    }
                    for url in images.unwrap() {
                        parts.push(serde_json::json!({ "type": "image_url", "image_url": { "url": url } }));
                    }
                    out.push(serde_json::json!({ "role": role, "content": parts }));
                } else {
                    out.push(serde_json::json!({ "role": role, "content": m.content }));
                }
            }
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn openrouter_round(
    model: &str,
    api_key: &str,
    messages: &[ChatMessage],
    think: &Option<serde_json::Value>,
    tools: &Option<serde_json::Value>,
    on_event: &Sink,
    stop: &AtomicBool,
) -> Result<RoundResult, String> {
    let mut think = think.clone();
    let mut tools = tools.clone();
    let mut strip_images = false;
    let mut retried_5xx = false;
    let has_images = messages.iter().any(|m| m.images.as_ref().is_some_and(|i| !i.is_empty()));
    for _ in 0..4 {
        match openrouter_round_attempt(model, api_key, messages, &think, &tools, strip_images, on_event, stop).await {
            Ok(r) => return Ok(r),
            Err(e) => {
                if stop.load(Ordering::Relaxed) {
                    return Err(e);
                }
                // transient provider 5xx (same retry the chat transport does):
                // one immediate re-attempt before the fallback ladder runs
                if !retried_5xx && e.starts_with("openrouter chat failed: 5") {
                    retried_5xx = true;
                    continue;
                }
                let low = e.to_lowercase();
                if !strip_images
                    && has_images
                    && (low.contains("image") || low.contains("multimodal") || low.contains("modality"))
                {
                    strip_images = true;
                    emit_ev(
                        on_event,
                        stop,
                        WorkAgentEvent::DisplayNote {
                            text: format!(
                                "\n\n> Note: {model} doesn't accept images, so the attached picture was not sent."
                            ),
                        },
                    );
                    continue;
                }
                if low.contains("reasoning") && think.is_some() {
                    think = None;
                    continue;
                }
                if low.contains("tool")
                    && (low.contains("not supported")
                        || low.contains("does not support")
                        || low.contains("unsupported")
                        || low.contains("no endpoints"))
                    && tools.as_ref().is_some_and(|t| !t.is_null())
                {
                    tools = None;
                    continue;
                }
                return Err(e);
            }
        }
    }
    Err("openrouter chat failed: retry ladder exhausted".to_string())
}

#[allow(clippy::too_many_arguments)]
async fn openrouter_round_attempt(
    model: &str,
    api_key: &str,
    messages: &[ChatMessage],
    think: &Option<serde_json::Value>,
    tools: &Option<serde_json::Value>,
    strip_images: bool,
    on_event: &Sink,
    stop: &AtomicBool,
) -> Result<RoundResult, String> {
    if api_key.trim().is_empty() {
        return Err("No OpenRouter API key is set — add one in Settings → OpenRouter.".to_string());
    }
    let mut body = serde_json::json!({
        "model": model,
        "messages": openrouter_wire(messages, strip_images),
        "stream": true,
        "usage": { "include": true },
    });
    if let Some(reasoning) = openrouter_reasoning(think) {
        body["reasoning"] = reasoning;
    }
    if let Some(t) = tools {
        if !t.is_null() {
            body["tools"] = t.clone();
        }
    }
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OPENROUTER_BASE}/chat/completions"))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("X-Title", "Vicuna")
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("openrouter chat failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(openrouter_error(status, &text));
    }
    use futures_util::StreamExt;
    let mut stream = res.bytes_stream();
    let mut buf = String::new();
    let mut full = String::new();
    let mut calls: Vec<(u64, Option<String>, String, String)> = Vec::new();
    let mut eval_count: u64 = 0;
    let mut cost: Option<f64> = None;
    let mut first_token: Option<Instant> = None;
    'stream: loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let chunk = match stream.next().await {
            Some(Ok(c)) => c,
            Some(Err(e)) => return Err(format!("openrouter chat failed: {e}")),
            None => break,
        };
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(nl) = buf.find('\n') {
            let line = buf[..nl].trim_end_matches('\r').to_string();
            buf.drain(..=nl);
            let Some(payload) = line.strip_prefix("data:") else {
                continue;
            };
            let payload = payload.trim();
            if payload.is_empty() {
                continue;
            }
            if payload == "[DONE]" {
                break 'stream;
            }
            let Ok(obj) = serde_json::from_str::<serde_json::Value>(payload) else {
                continue;
            };
            if let Some(err) = obj.get("error") {
                let detail = err
                    .get("message")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
                    .or_else(|| err.as_str().map(|s| s.to_string()))
                    .unwrap_or_else(|| err.to_string());
                return Err(format!("openrouter chat failed: {detail}"));
            }
            if let Some(n) = obj
                .pointer("/usage/completion_tokens")
                .and_then(|v| v.as_u64())
            {
                eval_count = n;
            }
            if let Some(c) = obj.pointer("/usage/cost").and_then(|v| v.as_f64()) {
                cost = Some(c);
            }
            let delta = match obj.pointer("/choices/0/delta") {
                Some(d) => d,
                None => continue,
            };
            let think_piece = json_piece(delta.get("reasoning"));
            let think_piece = if think_piece.is_empty() {
                json_piece(delta.get("reasoning_content"))
            } else {
                think_piece
            };
            if !think_piece.is_empty() {
                first_token.get_or_insert_with(Instant::now);
                emit_ev(on_event, stop, WorkAgentEvent::Think { text: think_piece });
            }
            let piece = json_piece(delta.get("content"));
            if !piece.is_empty() {
                first_token.get_or_insert_with(Instant::now);
                full.push_str(&piece);
                emit_ev(on_event, stop, WorkAgentEvent::Token { text: piece });
            }
            if let Some(tcs) = delta.get("tool_calls").and_then(|v| v.as_array()) {
                first_token.get_or_insert_with(Instant::now);
                for tc in tcs {
                    let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(calls.len() as u64);
                    if !calls.iter().any(|c| c.0 == index) {
                        calls.push((index, None, String::new(), String::new()));
                    }
                    let slot = calls.iter_mut().find(|c| c.0 == index).unwrap();
                    if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            slot.1 = Some(id.to_string());
                        }
                    }
                    if let Some(f) = tc.get("function") {
                        if let Some(name) = f.get("name").and_then(|v| v.as_str()) {
                            slot.2.push_str(name);
                        }
                        match f.get("arguments") {
                            Some(serde_json::Value::String(s)) => slot.3.push_str(s),
                            Some(other) if !other.is_null() => slot.3.push_str(&other.to_string()),
                            _ => {}
                        }
                    }
                }
            }
        }
    }
    calls.sort_by_key(|c| c.0);
    let tool_calls = calls
        .into_iter()
        .filter(|c| !c.2.is_empty())
        .map(|(_, id, name, args)| {
            serde_json::json!({
                "id": id,
                "type": "function",
                "function": {
                    "name": name,
                    "arguments": if args.trim().is_empty() { "{}".to_string() } else { args }
                }
            })
        })
        .collect();
    let eval_duration_ns = first_token
        .map(|t| t.elapsed().as_nanos() as u64)
        .unwrap_or(0);
    Ok(RoundResult { text: full, tool_calls, eval_count, eval_duration_ns, cost })
}

async fn openrouter_oneshot(
    model: &str,
    api_key: &str,
    system: &str,
    user: &str,
    stop: &AtomicBool,
) -> Result<String, String> {
    if api_key.trim().is_empty() {
        return Err("No OpenRouter API key is set — add one in Settings → OpenRouter.".to_string());
    }
    let client = reqwest::Client::new();
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
    });
    let fut = client
        .post(format!("{OPENROUTER_BASE}/chat/completions"))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .header("X-Title", "Vicuna")
        .json(&body)
        .send();
    let mut res = tokio::select! {
        r = fut => r.map_err(|e| format!("openrouter chat failed: {e}"))?,
        _ = async { while !stop.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => {
            return Err("stopped".to_string());
        }
    };
    // one immediate retry on a transient provider 5xx (same policy as the
    // chat transport): the gateway re-routes, so the second attempt usually
    // lands on a healthy provider
    if res.status().is_server_error() && !stop.load(Ordering::Relaxed) {
        let fut2 = client
            .post(format!("{OPENROUTER_BASE}/chat/completions"))
            .header("Authorization", format!("Bearer {}", api_key.trim()))
            .header("X-Title", "Vicuna")
            .json(&body)
            .send();
        res = tokio::select! {
            r = fut2 => r.map_err(|e| format!("openrouter chat failed: {e}"))?,
            _ = async { while !stop.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => {
                return Err("stopped".to_string());
            }
        };
    }
    if !res.status().is_success() {
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        return Err(openrouter_error(status, &text));
    }
    let data: serde_json::Value = res.json().await.map_err(|e| format!("openrouter chat failed: {e}"))?;
    Ok(json_piece(data.pointer("/choices/0/message/content"))
        .trim()
        .to_string())
}

// ---------- tool dispatch (mirrors the old JS dispatchWorkTool) ----------

#[allow(clippy::too_many_arguments)]
async fn dispatch_tool(
    name: &str,
    args: &serde_json::Value,
    workspace: &str,
    session_id: Option<&str>,
    chats: &[ChatSnapshot],
    bg: &BgExecState,
    mcp: &McpManager,
    mcp_routes: &HashMap<String, McpRoute>,
    turn: &TurnState,
    turn_id: &str,
    on_event: &Sink,
    browser_resource_dir: Option<&std::path::Path>,
    browser: &BrowserState,
    undo_state: &UndoState,
    undo_key: &str,
) -> String {
    match name {
        "list" => match work_list_dir(workspace.to_string(), Some(str_arg(args, "path"))).await {
            Ok(entries) => {
                if entries.is_empty() {
                    "Empty directory.".to_string()
                } else {
                    entries
                        .iter()
                        .map(|e| {
                            if e.is_dir {
                                format!("{}/", e.path)
                            } else if let Some(s) = e.size {
                                format!("{} ({s}B)", e.path)
                            } else {
                                e.path.clone()
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            Err(e) => format!("Error: {e}"),
        },
        "read" => {
            let p = str_arg(args, "path");
            if p.is_empty() {
                return "Error: read needs {\"path\": \"...\"}.".to_string();
            }
            match work_read_file(workspace.to_string(), p.clone(), None).await {
                Ok(c) => format!(
                    "{}{}",
                    c.content,
                    if c.truncated {
                        format!("\n…[truncated, {}B total]", c.size)
                    } else {
                        String::new()
                    }
                ),
                Err(e) => format!("Error: {e}"),
            }
        }
        "grep" => {
            let p = str_arg(args, "pattern");
            if p.is_empty() {
                return "Error: grep needs {\"pattern\": \"...\"}.".to_string();
            }
            match work_grep(workspace.to_string(), p, Some(str_arg(args, "include"))).await {
                Ok(hits) => {
                    if hits.is_empty() {
                        "No matches.".to_string()
                    } else {
                        hits.iter().map(|h| format!("{}:{}: {}", h.path, h.line, h.text)).collect::<Vec<_>>().join("\n")
                    }
                }
                Err(e) => format!("Error: {e}"),
            }
        }
        "write" => {
            let p = str_arg(args, "path");
            if p.is_empty() {
                return "Error: write needs {\"path\": \"...\", \"content\": \"...\"}.".to_string();
            }
            match args.get("content").and_then(|v| v.as_str()) {
                Some(content) => {
                    // snapshot before clobbering so /undo can restore the old
                    // bytes and /redo can put the turn's bytes back
                    let prev = if undo_key.is_empty() {
                        None
                    } else {
                        undo_capture_prev(workspace, &p)
                    };
                    let res = work_write_file(workspace.to_string(), p.clone(), content.to_string()).await;
                    if res.is_ok() {
                        if let Some((existed, old)) = prev {
                            undo_record_write(undo_state, undo_key, &p, existed, old, content.as_bytes());
                        }
                    }
                    match res {
                        Ok(_) => format!("Wrote {} ({} chars).", p, content.len()),
                        Err(e) => format!("Error: {e}"),
                    }
                }
                None => "Error: write needs a string \"content\".".to_string(),
            }
        }
        "exec" => {
            let cmd = str_arg(args, "command");
            if cmd.is_empty() {
                return "Error: exec needs {\"command\": \"...\"}.".to_string();
            }
            // shell side effects aren't invertible — record the fact so
            // /undo can say so instead of silently pretending
            if !undo_key.is_empty() && !cmd.trim().is_empty() {
                undo_record(undo_state, undo_key, UndoOp::ExecNote { command: cmd.clone() });
            }
            if args.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
                match bg_start(bg, workspace.to_string(), cmd.clone()).await {
                    Ok(id) => format!(
                        "Started background job {id} for: {cmd}\nUse exec_status with {{\"id\": \"{id}\"}} to check output; you will also be notified automatically when it finishes. Continue with other work meanwhile."
                    ),
                    Err(e) => format!("Error: {e}"),
                }
            } else {
                let timeout = args.get("timeout_ms").and_then(|v| v.as_u64());
                match work_exec(workspace.to_string(), cmd, timeout).await {
                    Ok(r) => {
                        let mut parts = Vec::new();
                        if !r.stdout.trim().is_empty() {
                            parts.push(r.stdout);
                        }
                        if !r.stderr.trim().is_empty() {
                            parts.push(format!("[stderr]\n{}", r.stderr));
                        }
                        parts.push(if r.timed_out {
                            "[timed out]".to_string()
                        } else {
                            format!("[exit {}]", r.exit_code.unwrap_or(-1))
                        });
                        parts.join("\n")
                    }
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        "exec_status" => {
            if let Some(id) = args.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                match bg_snapshot(bg, id) {
                    Ok(s) => fmt_bg(&s),
                    Err(e) => format!("Error: {e}"),
                }
            } else {
                match bg_list(bg) {
                    Ok(jobs) => {
                        if jobs.is_empty() {
                            "No background jobs.".to_string()
                        } else {
                            jobs.iter()
                                .map(|j| {
                                    format!(
                                        "{} {} — {}",
                                        j.id,
                                        if j.running { "(running)" } else { "(done)" },
                                        j.command.chars().take(120).collect::<String>()
                                    )
                                })
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    Err(e) => format!("Error: {e}"),
                }
            }
        }
        "exec_kill" => {
            let id = str_arg(args, "id");
            if id.is_empty() {
                return "Error: exec_kill needs {\"id\": \"...\"}.".to_string();
            }
            match bg_kill(bg, &id) {
                Ok(s) => fmt_bg(&s),
                Err(e) => format!("Error: {e}"),
            }
        }
        "say" => {
            if str_arg(args, "text").trim().is_empty() {
                return "Error: say needs non-empty text.".to_string();
            }
            "Displayed to the user.".to_string()
        }
        "ask" => {
            let question = str_arg(args, "question");
            let options: Vec<String> = args
                .get("options")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                        .filter(|s| !s.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            if question.trim().is_empty() {
                return "Error: ask needs a non-empty question.".to_string();
            }
            if options.len() < 2 {
                return "Error: ask needs at least 2 options.".to_string();
            }
            let ask_id = format!("ask-{turn_id}-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = oneshot::channel::<String>();
            if turn.ask_pending.lock().map(|mut m| m.insert(ask_id.clone(), tx)).is_err() {
                return "Error: ask state poisoned".to_string();
            }
            emit_ev(on_event, &turn.stop, WorkAgentEvent::AskRequest {
                ask_id: ask_id.clone(),
                question,
                options: options.into_iter().take(8).collect(),
            });
            tokio::select! {
                r = rx => r.unwrap_or_else(|_| "cancelled".to_string()),
                _ = async { while !turn.stop.load(Ordering::Relaxed) { tokio::time::sleep(std::time::Duration::from_millis(100)).await; } } => "__CANCELLED__".to_string(),
            }
        }
        "sessions" => search_sessions(chats, &str_arg(args, "query"), 8),
        "read_session" => {
            let id = str_arg(args, "id");
            if id.is_empty() {
                return "Error: read_session needs {\"id\": \"...\"}.".to_string();
            }
            let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
            read_session(chats, &id, limit)
        }
        "read_upload" => {
            let name = str_arg(args, "name");
            if name.is_empty() {
                return "Error: read_upload needs {\"name\": \"...\"}.".to_string();
            }
            let Some(sid) = session_id.filter(|s| !s.is_empty()) else {
                return "Error: no active session for uploads.".to_string();
            };
            match upload_read_file(sid.to_string(), name).await {
                Ok(c) => format!(
                    "{}{}",
                    c.content,
                    if c.truncated {
                        format!("\n…[truncated, {}B total]", c.size)
                    } else {
                        String::new()
                    }
                ),
                Err(e) => format!("Error: {e}"),
            }
        }
        "view_image" => {
            let p = str_arg(args, "path");
            if p.is_empty() {
                return "Error: view_image needs {\"path\": \"...\"}.".to_string();
            }
            match work_read_image(workspace.to_string(), p.clone()).await {
                Ok(img) => format!(
                    "Viewed image {} ({}B, {}) — picture sent as vision input; describe what you see.\n[[VICUNA_IMAGE:data:{};base64,{}]]",
                    img.path, img.size, img.mime, img.mime, img.base64
                ),
                Err(e) => format!("Error: {e}"),
            }
        }
        "fetch_url" => {
            let u = str_arg(args, "url");
            if u.is_empty() {
                return "Error: fetch_url needs {\"url\": \"...\"}.".to_string();
            }
            match work_fetch_url(u).await {
                Ok(c) => format!(
                    "{}{}",
                    c.content,
                    if c.truncated {
                        format!("\n…[truncated, {} chars total]", c.size)
                    } else {
                        String::new()
                    }
                ),
                Err(e) => format!("Error: {e}"),
            }
        }
        "mcp_list" => match list_file_servers() {
            Ok(mut servers) => {
                // the built-in browser row isn't in the file: append it when
                // `lb` resolves (and no file entry already claims the name),
                // so the agent can discover + enable/disable it
                if !servers.iter().any(|s| s.name.to_lowercase() == BROWSER_MCP_ID) {
                    let argv = browser_mcp_command(browser_resource_dir).ok();
                    // in this branch only the synthetic row can come back
                    if let Some(cfg) = browser_agent_config(&servers, argv) {
                        servers.push(cfg);
                    }
                }
                let set: HashSet<String> = mcp.connected_ids().await.into_iter().collect();
                format_mcp_list(&servers, &set)
            }
            Err(e) => format!("Error: {e}"),
        },
        "mcp_add" => {
            let mut cfg = match parse_mcp_add_args(args) {
                Ok(c) => c,
                Err(e) => return e,
            };
            if cfg.name.to_lowercase() == BROWSER_MCP_ID {
                return "Error: \"browser\" is the built-in Browser-tab row — pick another name.".to_string();
            }
            let existing = match list_file_servers() {
                Ok(list) => list
                    .into_iter()
                    .find(|s| s.id == cfg.id || s.name.to_lowercase() == cfg.name.to_lowercase()),
                Err(e) => return format!("Error: {e}"),
            };
            // case-only renames reuse the stored key so "Foo" vs "foo" can't
            // fork into two entries (the UI refuses those outright)
            if let Some(prev) = existing.as_ref() {
                if prev.name.to_lowercase() == cfg.name.to_lowercase() {
                    cfg.id = prev.id.clone();
                    cfg.name = prev.name.clone();
                }
            }
            let overwrite = args.get("overwrite").and_then(|v| v.as_bool()).unwrap_or(false);
            if let Err(e) = mcp_add_plan(existing.as_ref(), &cfg, overwrite) {
                return e;
            }
            // connect BEFORE saving: a mistyped endpoint must not persist a
            // broken entry. A failed re-point rolls the previous config back
            // (connect drops the old connection first, so restore it too).
            match mcp.connect(&cfg).await {
                Ok(info) => match save_file_server(&cfg) {
                    Ok(_) => format_mcp_connected("Added", &info, &cfg),
                    Err(e) => format!(
                        "Connected \"{}\" and verified {} tools, but could not save it: {e}",
                        cfg.name,
                        info.tools.len()
                    ),
                },
                Err(e) => {
                    if let Some(prev) = existing {
                        let _ = save_file_server(&prev);
                        let _ = mcp.connect(&prev).await;
                        format!(
                            "Error: could not connect to the new endpoint ({e}). Previous \"{}\" config kept.",
                            prev.name
                        )
                    } else if e.to_lowercase().contains("sign-in required") {
                        // OAuth servers can't finish in-agent (browser flow) —
                        // save anyway so the user can sign in from the MCP popup
                        match save_file_server(&cfg) {
                            Ok(_) => format!(
                                "Saved \"{}\" ({}) but it needs browser sign-in: tell the user to open the MCP popup and choose \"Sign in required →\". Its tools join your list after they sign in.",
                                cfg.name,
                                describe_mcp_endpoint(&cfg)
                            ),
                            Err(se) => format!("Error: {e} (and could not save the server: {se})"),
                        }
                    } else {
                        format!("Error: could not connect ({e}). Nothing was saved — check the endpoint and retry.")
                    }
                }
            }
        }
        "mcp_remove" => {
            let name = str_arg(args, "name").trim().to_string();
            if name.is_empty() {
                return "Error: mcp_remove needs {\"name\": \"...\"}.".to_string();
            }
            if name.to_lowercase() == BROWSER_MCP_ID {
                return "Error: \"browser\" is the built-in Browser-tab row — it has no saved entry to remove. Use mcp_disable to disconnect it."
                    .to_string();
            }
            let existing = match list_file_servers() {
                Ok(list) => list
                    .into_iter()
                    .find(|s| s.id == name || s.name.to_lowercase() == name.to_lowercase()),
                Err(e) => return format!("Error: {e}"),
            };
            match existing {
                None => format!("No MCP server named \"{name}\" — nothing to remove."),
                Some(s) => {
                    let _ = mcp.disconnect(&s.id).await;
                    match delete_file_server(&s.id) {
                        Ok(_) => format!("Removed MCP server \"{}\" (disconnected and deleted).", s.name),
                        Err(e) => format!("Error: {e}"),
                    }
                }
            }
        }
        "mcp_enable" => {
            let name = str_arg(args, "name").trim().to_string();
            if name.is_empty() {
                return "Error: mcp_enable needs {\"name\": \"...\"}.".to_string();
            }
            let is_browser = name.to_lowercase() == BROWSER_MCP_ID;
            let existing = match list_file_servers() {
                Ok(list) => list
                    .into_iter()
                    .find(|s| s.id == name || s.name.to_lowercase() == name.to_lowercase()),
                Err(e) => return format!("Error: {e}"),
            };
            match existing {
                // no file entry: the built-in row still enables via `lb mcp`
                None if is_browser => {
                    mcp_enable_synthetic_browser(mcp, browser_resource_dir, browser).await
                }
                None => format!("No MCP server named \"{name}\" — add it with mcp_add first."),
                Some(s) => {
                    // same order as the UI toggle: persist enabled first, then
                    // connect, so a failure still leaves a visible row error
                    let cfg = McpServerConfig { enabled: true, ..s };
                    if let Err(e) = save_file_server(&cfg) {
                        return format!("Error: {e}");
                    }
                    if is_browser {
                        // `lb mcp` dials the daemon: ensure it first, like the
                        // UI toggle does for this id (a file entry named
                        // `browser` gets the same treatment)
                        if let Err(e) =
                            browser_start_logic(browser_resource_dir.map(|p| p.to_path_buf()), browser).await
                        {
                            return format!("Error: could not start the browser daemon ({e}).");
                        }
                    }
                    match mcp.connect(&cfg).await {
                        Ok(info) => format_mcp_connected("Enabled", &info, &cfg),
                        Err(e) => {
                            if e.to_lowercase().contains("sign-in required") {
                                format!("Enabled \"{}\" ({}) but it needs browser sign-in: tell the user to open the MCP popup and choose \"Sign in required →\".", cfg.name, describe_mcp_endpoint(&cfg))
                            } else {
                                format!("Error: could not connect to \"{}\" ({e}). It stays enabled and will retry on launch.", cfg.name)
                            }
                        }
                    }
                }
            }
        }
        "mcp_disable" => {
            let name = str_arg(args, "name").trim().to_string();
            if name.is_empty() {
                return "Error: mcp_disable needs {\"name\": \"...\"}.".to_string();
            }
            let is_browser = name.to_lowercase() == BROWSER_MCP_ID;
            let existing = match list_file_servers() {
                Ok(list) => list
                    .into_iter()
                    .find(|s| s.id == name || s.name.to_lowercase() == name.to_lowercase()),
                Err(e) => return format!("Error: {e}"),
            };
            match existing {
                // no file entry: the built-in row just drops its connection
                None if is_browser => mcp_disable_synthetic_browser(mcp).await,
                None => format!("No MCP server named \"{name}\" — nothing to disable."),
                Some(s) => {
                    // disconnect first (best-effort: it may never have
                    // connected), then persist disabled like the UI toggle
                    let _ = mcp.disconnect(&s.id).await;
                    if !s.enabled {
                        return format!("\"{}\" was already disabled (dropped any live connection).", s.name);
                    }
                    let cfg = McpServerConfig { enabled: false, ..s };
                    match save_file_server(&cfg) {
                        Ok(_) => format!(
                            "Disabled \"{}\" (disconnected; it won't reconnect on launch). Config kept — mcp_remove deletes it entirely.",
                            cfg.name
                        ),
                        Err(e) => format!("Error: {e}"),
                    }
                }
            }
        }
        _ => {
            if let Some(route) = mcp_routes.get(name) {
                match mcp.call_tool(&route.server_id, &route.tool_name, args.clone()).await {
                    Ok(r) => {
                        if r.is_error {
                            format!("Tool error: {}", r.content)
                        } else {
                            r.content
                        }
                    }
                    Err(e) => format!("Error: {e}"),
                }
            } else {
                let maybe_cmd = args.get("command").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                if !maybe_cmd.is_empty() {
                    format!(
                        "Error: unknown tool \"{name}\". There is no {name} tool — run shell commands with exec instead, e.g. {{\"command\": {}}}. Available: list, read, view_image, grep, write, exec, exec_status, exec_kill, sessions, read_session, read_upload, fetch_url, mcp_list, mcp_add, mcp_remove, mcp_enable, mcp_disable, say, ask.",
                        serde_json::json!(maybe_cmd)
                    )
                } else {
                    format!("Error: unknown tool \"{name}\". Available: list, read, view_image, grep, write, exec, exec_status, exec_kill, sessions, read_session, read_upload, fetch_url, mcp_list, mcp_add, mcp_remove, mcp_enable, mcp_disable, say, ask.")
                }
            }
        }
    }
}

// ---------- the loop ----------

#[allow(clippy::too_many_arguments)]
pub async fn run_loop(
    turn: Arc<TurnState>,
    model: String,
    provider: String,
    api_key: Option<String>,
    context_length: Option<u64>,
    workspace: String,
    mut native: Vec<ChatMessage>,
    think: Option<serde_json::Value>,
    tools: Option<serde_json::Value>,
    session_id: Option<String>,
    goal_text: Option<String>,
    goal_paused: bool,
    chats: Vec<ChatSnapshot>,
    mcp_routes: HashMap<String, McpRoute>,
    on_event: Sink,
    bg: &BgExecState,
    mcp: &McpManager,
    turn_id: &str,
    browser_resource_dir: Option<PathBuf>,
    browser: &BrowserState,
    undo_state: &UndoState,
) {
    let undo_key = undo_key(session_id.as_deref(), &workspace);
    undo_begin_turn(undo_state, &undo_key, &workspace);
    let mut seen: HashSet<String> = HashSet::new();
    let mut pending_bg: HashSet<String> = HashSet::new();
    let mut empty_rounds = 0u32;
    let mut got_native = false;
    let mut turn_done = false;
    let mut first_round = true;
    let goal_prefix = "Session goal check:";
    let mut goal_check_outstanding = false;

    loop {
        if turn.stop.load(Ordering::Relaxed) {
            break;
        }
        // steering: queued prompts join the context at the top of the round.
        // The frontend mirrors each drained message as a user bubble plus a
        // fresh assistant bubble (QueueDrained), so display and native stay
        // aligned exactly like the old in-thread loop did.
        {
            let q = turn.queue.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
            if !q.is_empty() {
                let ids: Vec<String> = q.iter().map(|(id, _)| id.clone()).collect();
                let msgs: Vec<ChatMessage> = q.into_iter().map(|(_, m)| m).collect();
                native.extend(msgs.clone());
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::QueueDrained { ids, messages: msgs });
            }
        }
        // finished background jobs join the transcript here
        {
            let mut finished = Vec::new();
            for id in pending_bg.clone() {
                match bg_snapshot(bg, &id) {
                    Ok(s) => {
                        if !s.running {
                            pending_bg.remove(&id);
                            finished.push((id, s.command.clone(), fmt_bg(&s)));
                        }
                    }
                    Err(_) => {
                        pending_bg.remove(&id);
                    }
                }
            }
            for (id, cmd, out) in finished {
                native.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!("Background job {id} finished (`{cmd}`):\n{out}"),
                    images: None,
                    tool_calls: None,
                    tool_name: None,
                });
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::BgCompleted { job_id: id, command: cmd, output: out });
            }
        }
        if turn.stop.load(Ordering::Relaxed) {
            break;
        }
        // context guard: 70% of the model's own window (32768 fallback)
        {
            let win = if provider == "openrouter" {
                context_length.unwrap_or(32768) as usize
            } else {
                model_context_length(&model).await.unwrap_or(32768) as usize
            };
            let target = (win as f64 * 0.7) as usize;
            if estimate_tokens(&native, &tools) > target && native.len() > 5 {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: Some("Compacting context…".to_string()) });
                let mut cut = 1usize;
                let last_allowed = native.len().saturating_sub(4);
                for i in 1..=last_allowed {
                    if native[i].role == "user" || native[i].role == "system" {
                        cut = i;
                        let kept = [&native[..1], &native[i..]].concat();
                        if estimate_tokens(&kept, &tools) <= target {
                            break;
                        }
                    }
                }
                let older: Vec<ChatMessage> = native[1..cut].to_vec();
                let transcript = older
                    .iter()
                    .map(|m| format!("{}: {}", m.role, m.content))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let summary = if provider == "openrouter" {
                    openrouter_oneshot(
                        &model,
                        api_key.as_deref().unwrap_or(""),
                        "Summarize this agent transcript excerpt concisely: files listed/read/written, commands run and their key output, decisions made, and what remains to do. Keep file paths and facts; drop chatter.",
                        &transcript.chars().take(60000).collect::<String>(),
                        &turn.stop,
                    )
                    .await
                } else {
                    ollama_oneshot(
                        &model,
                        "Summarize this agent transcript excerpt concisely: files listed/read/written, commands run and their key output, decisions made, and what remains to do. Keep file paths and facts; drop chatter.",
                        &transcript.chars().take(60000).collect::<String>(),
                        &turn.stop,
                    )
                    .await
                }
                .unwrap_or_default();
                if !turn.stop.load(Ordering::Relaxed) && !summary.is_empty() {
                    let mut proposed = vec![
                        native[0].clone(),
                        ChatMessage {
                            role: "user".to_string(),
                            content: format!("Earlier in this turn, summarized to save context:\n{summary}"),
                            images: None,
                            tool_calls: None,
                            tool_name: None,
                        },
                    ];
                    proposed.extend(native[cut..].iter().cloned());
                    native = proposed;
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::ContextCompacted {
                        note: "> Context compacted: older tool output summarized.".to_string(),
                    });
                }
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: None });
                if turn.stop.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
        // one chat round (RoundStart/End bracket the generation so the
        // frontend can mirror the old pending-stream marker + per-round stats)
        let mode = if first_round { "fresh" } else { "continue" };
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundStart { mode: mode.to_string() });
        let t0 = Instant::now();
        let round = match if provider == "openrouter" {
            openrouter_round(
                &model,
                api_key.as_deref().unwrap_or(""),
                &native,
                &think,
                &tools,
                &on_event,
                &turn.stop,
            )
            .await
        } else {
            ollama_round(&model, &native, &think, &tools, &on_event, &turn.stop).await
        } {
            Ok(r) => r,
            Err(e) => {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: 0, gen_tps: None, gen_cost: None });
                if turn.stop.load(Ordering::Relaxed) {
                    break;
                }
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Error { message: e });
                break;
            }
        };
        first_round = false;
        let wall_ms = t0.elapsed().as_millis() as u64;
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: wall_ms, gen_tps: round_tps(&round, wall_ms), gen_cost: round.cost });
        if turn.stop.load(Ordering::Relaxed) {
            break;
        }
        native.push(ChatMessage {
            role: "assistant".to_string(),
            content: round.text.clone(),
            images: None,
            tool_calls: if round.tool_calls.is_empty() {
                None
            } else {
                Some(serde_json::Value::Array(round.tool_calls.clone()))
            },
            tool_name: None,
        });
        let mut calls: Vec<(String, serde_json::Value)> = Vec::new();
        for tc in &round.tool_calls {
            if let Some(name) = tc.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) {
                let args = tc
                    .get("function")
                    .and_then(|f| f.get("arguments"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Object(Default::default()));
                let args = match &args {
                    serde_json::Value::String(s) if !s.trim().is_empty() => {
                        serde_json::from_str(s).unwrap_or(serde_json::Value::Object(Default::default()))
                    }
                    _ => args,
                };
                if !name.is_empty() {
                    calls.push((name.to_string(), args));
                }
            }
        }
        if !calls.is_empty() {
            got_native = true;
        }
        // fallback: models without tools support emit ```tool fences as text.
        // Once native calls have been seen, text fences never execute.
        if calls.is_empty() && !got_native {
            for cap in extract_fences(&round.text) {
                calls.push(cap);
            }
        }
        if calls.is_empty() {
            // No actual call this round but the model wrote a call-shaped
            // block: say so explicitly (display-only note) — otherwise
            // hallucinated "callings" look exactly like executed ones.
            if has_tool_fence(&round.text) {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::DisplayNote {
                    text: "\n\n> Not executed: the model wrote a tool block above that was not a valid call, so nothing ran.".to_string(),
                });
            }
            let round_empty = round.text.trim().is_empty() && round.tool_calls.is_empty();
            empty_rounds = if round_empty { empty_rounds + 1 } else { 0 };
            if round_empty && empty_rounds >= 2 {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Error {
                    message: "The model returned empty responses twice in a row — the context likely no longer fits even after compaction. Try /compact or a shorter task.".to_string(),
                });
                break;
            }
            // no calls but background jobs still run: wait for the next one
            if !pending_bg.is_empty() {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status {
                    label: Some("Waiting for background job…".to_string()),
                });
                let mut woke = false;
                while !pending_bg.is_empty() {
                    if turn.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    if !turn.queue.lock().map(|q| q.is_empty()).unwrap_or(true) {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if turn.stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let mut done_now = Vec::new();
                    for id in pending_bg.clone() {
                        match bg_snapshot(bg, &id) {
                            Ok(s) if !s.running => {
                                pending_bg.remove(&id);
                                done_now.push((id, s.command.clone(), fmt_bg(&s)));
                            }
                            Err(_) => {
                                pending_bg.remove(&id);
                            }
                            _ => {}
                        }
                    }
                    if !done_now.is_empty() {
                        for (id, cmd, out) in done_now {
                            native.push(ChatMessage {
                                role: "user".to_string(),
                                content: format!("Background job {id} finished (`{cmd}`):\n{out}"),
                                images: None,
                                tool_calls: None,
                                tool_name: None,
                            });
                            emit_ev(&on_event, &turn.stop, WorkAgentEvent::BgCompleted { job_id: id, command: cmd, output: out });
                        }
                        woke = true;
                        break;
                    }
                }
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: None });
                if woke {
                    continue;
                }
                if !turn.queue.lock().map(|q| q.is_empty()).unwrap_or(true) {
                    continue;
                }
            }
            // goal mode: the turn only ends on a met objective
            if let Some(g) = goal_text.clone().filter(|_| !goal_paused) {
                if !goal_check_outstanding {
                    let offer_done = tools
                        .as_ref()
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter().any(|x| {
                                x.get("function").and_then(|f| f.get("name")).and_then(|v| v.as_str()) == Some("done")
                            })
                        })
                        .unwrap_or(false);
                    native.push(ChatMessage {
                        role: "user".to_string(),
                        content: format!(
                            "{goal_prefix} The session goal is: {g}\n{}",
                            if offer_done {
                                "If it is fully achieved, call `done` with a brief completion summary (no other tool calls). Otherwise keep working toward it (emit tool calls)."
                            } else {
                                "If it is fully achieved, reply with a brief completion summary and NO tool calls. Otherwise keep working toward it (emit tool calls)."
                            }
                        ),
                        images: None,
                        tool_calls: None,
                        tool_name: None,
                    });
                    goal_check_outstanding = true;
                    continue;
                }
            }
            break;
        }
        goal_check_outstanding = false;
        // repeat guard: never re-run the same call twice
        let fresh: Vec<(String, serde_json::Value)> = calls
            .into_iter()
            .filter(|(n, a)| {
                let k = seen_key(n, a);
                if seen.contains(&k) {
                    false
                } else {
                    seen.insert(k);
                    true
                }
            })
            .collect();
        if fresh.is_empty() {
            native.push(ChatMessage {
                role: "user".to_string(),
                content: "Those tool calls already ran — their results are in the transcript above. Do not repeat them. Wrap up with a summary and no new tool calls.".to_string(),
                images: None,
                tool_calls: None,
                tool_name: None,
            });
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundStart { mode: "continue".to_string() });
            let t0 = Instant::now();
            let wrap = if provider == "openrouter" {
                openrouter_round(
                    &model,
                    api_key.as_deref().unwrap_or(""),
                    &native,
                    &think,
                    &tools,
                    &on_event,
                    &turn.stop,
                )
                .await
            } else {
                ollama_round(&model, &native, &think, &tools, &on_event, &turn.stop).await
            };
            let wall_ms = t0.elapsed().as_millis() as u64;
            match wrap {
                Ok(r) => {
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: wall_ms, gen_tps: round_tps(&r, wall_ms), gen_cost: r.cost });
                    native.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: r.text,
                        images: None,
                        tool_calls: None,
                        tool_name: None,
                    });
                }
                Err(_) => {
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: 0, gen_tps: None, gen_cost: None });
                }
            }
            break;
        }
        for (name, args) in fresh {
            if turn.stop.load(Ordering::Relaxed) {
                break;
            }
            if name == "done" {
                // Self-stop only exists in Goal Mode. Models sometimes emit
                // `done` + summary uninvited on goal-free turns (verified:
                // gemma4-cloud signs off that way) — honoring it would abort
                // the turn mid-round, skipping the remaining calls with no
                // wrap-up. Answer it as an error instead so the round
                // continues like any other unknown tool.
                if !goal_done_offered(&goal_text, goal_paused) {
                    let msg = "Error: there is no \"done\" tool in this turn (it only exists while a session goal is set). Do not call \"done\" again — continue with the available tools, or reply with no further tool calls when finished.".to_string();
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolStart { name: name.clone(), args: args.clone() });
                    native.push(ChatMessage {
                        role: "tool".to_string(),
                        content: msg.clone(),
                        images: None,
                        tool_calls: None,
                        tool_name: Some("done".to_string()),
                    });
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolEnd { name, output: msg });
                    continue;
                }
                let summary = args.get("summary").and_then(|v| v.as_str()).unwrap_or("Done.").trim().to_string();
                let summary = if summary.is_empty() { "Done.".to_string() } else { summary };
                native.push(ChatMessage {
                    role: "tool".to_string(),
                    content: summary.clone(),
                    images: None,
                    tool_calls: None,
                    tool_name: Some("done".to_string()),
                });
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolStart { name: name.clone(), args: args.clone() });
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolEnd { name, output: summary });
                turn_done = true;
                break;
            }
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: Some(tool_label(&name, &args)) });
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolStart { name: name.clone(), args: args.clone() });
            let out = dispatch_tool(&name, &args, &workspace, session_id.as_deref(), &chats, bg, mcp, &mcp_routes, &turn, turn_id, &on_event, browser_resource_dir.as_deref(), browser, undo_state, &undo_key).await;
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: None });
            if out == "__CANCELLED__" {
                break;
            }
            if name == "exec" && args.get("background").and_then(|v| v.as_bool()).unwrap_or(false) {
                if let Some(id) = extract_bg_id(&out) {
                    pending_bg.insert(id);
                }
            }
            let (clean, images) = extract_viewed_images(&out);
            let out_clean = if images.is_empty() { out.clone() } else { clean };
            native.push(ChatMessage {
                role: "tool".to_string(),
                content: out_clean.clone(),
                images: None,
                tool_calls: None,
                tool_name: Some(name.clone()),
            });
            if !images.is_empty() {
                native.push(ChatMessage {
                    role: "user".to_string(),
                    content: format!(
                        "Image {} shown above (vision input). Describe what you see and answer the user's question about it.",
                        args.get("path").and_then(|v| v.as_str()).unwrap_or("")
                    ),
                    images: Some(images),
                    tool_calls: None,
                    tool_name: None,
                });
            }
            emit_ev(&on_event, &turn.stop, WorkAgentEvent::ToolEnd { name: name.clone(), output: out_clean });
            // explicit single-job polls that finished settle the id
            if name == "exec_status" || name == "exec_kill" {
                if let Some(pid) = args
                    .get("id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                {
                    if pending_bg.contains(&pid) {
                        match bg_snapshot(bg, &pid) {
                            Ok(s) if s.running => {}
                            _ => {
                                pending_bg.remove(&pid);
                            }
                        }
                    }
                }
            }
        }
        if turn_done || turn.stop.load(Ordering::Relaxed) {
            break;
        }
    }
    let stopped = turn.stop.load(Ordering::Relaxed);
    emit_ev(&on_event, &turn.stop, WorkAgentEvent::Status { label: None });
    emit_ev(&on_event, &turn.stop, WorkAgentEvent::Done { stopped });
}

// ---------- shared turn operations (Tauri commands + headless bridge) ----------

/// Interrupt a turn: set the stop flag and unblock any waiting `ask`.
pub fn agent_stop(turn: &Arc<TurnState>) {
    turn.stop.store(true, Ordering::Relaxed);
    if let Ok(mut m) = turn.ask_pending.lock() {
        for (_, tx) in m.drain() {
            let _ = tx.send("cancelled".to_string());
        }
    }
}

pub fn agent_queue_msg(turn: &Arc<TurnState>, id: String, message: ChatMessage) -> Result<(), String> {
    turn.queue
        .lock()
        .map(|mut q| {
            q.push((id, message));
        })
        .map_err(|_| "agent state poisoned".to_string())
}

pub fn agent_remove_msg(turn: &Arc<TurnState>, id: &str) -> Result<(), String> {
    turn.queue
        .lock()
        .map(|mut q| {
            q.retain(|(qid, _)| qid != id);
        })
        .map_err(|_| "agent state poisoned".to_string())
}

pub fn agent_answer(turn: &Arc<TurnState>, ask_id: &str, answer: String) -> Result<(), String> {
    let tx = turn
        .ask_pending
        .lock()
        .map_err(|_| "agent state poisoned".to_string())?
        .remove(ask_id);
    match tx {
        Some(tx) => {
            let _ = tx.send(answer);
            Ok(())
        }
        None => Err("unknown question".to_string()),
    }
}


#[cfg(test)]
mod agent_tests {
    use super::*;

    #[test]
    fn bg_id_extraction_matches_frontend() {
        assert_eq!(extract_bg_id("Started background job bg-3 for: make"), Some("bg-3".to_string()));
        assert_eq!(extract_bg_id("no job here"), None);
    }

    #[test]
    fn token_estimate_counts_tools() {
        let msgs = vec![ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
            images: None,
            tool_calls: None,
            tool_name: None,
        }];
        let with_tools = estimate_tokens(&msgs, &Some(serde_json::json!([{"a": 1}])));
        let without = estimate_tokens(&msgs, &None);
        assert!(with_tools >= without);
    }

    #[test]
    fn fence_extraction_parses_calls() {
        let text = "thinking\n```tool\n{\"name\": \"read\", \"args\": {\"path\": \"a.txt\"}}\n```";
        let calls = extract_fences(text);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "read");
    }

    #[test]
    fn session_search_finds_by_title() {
        let chats = vec![ChatSnapshot {
            id: "abc".to_string(),
            title: "my project".to_string(),
            mode: Some("work".to_string()),
            messages: vec![ChatSnapshotMsg { role: "user".to_string(), content: "hello".to_string() }],
        }];
        let out = search_sessions(&chats, "project", 8);
        assert!(out.contains("abc"));
        assert_eq!(search_sessions(&chats, "zzz-no-match", 8), "No matching sessions.");
    }

    #[test]
    fn done_offered_only_with_unpaused_goal() {
        assert!(goal_done_offered(&Some("ship it".to_string()), false));
        assert!(!goal_done_offered(&None, false));
        assert!(!goal_done_offered(&Some("ship it".to_string()), true));
        assert!(!goal_done_offered(&Some(String::new()), false));
    }

    #[test]
    fn done_dedups_by_name_alone() {
        let a = serde_json::json!({"summary": "first"});
        let b = serde_json::json!({"summary": "second"});
        assert_eq!(seen_key("done", &a), seen_key("done", &b));
        let r1 = serde_json::json!({"path": "a.txt"});
        let r2 = serde_json::json!({"path": "b.txt"});
        assert_ne!(seen_key("read", &r1), seen_key("read", &r2));
        assert_eq!(seen_key("read", &r1), tool_key("read", &r1));
    }

    fn mcp_cfg(name: &str, transport: &str, url: Option<&str>, command: Option<Vec<&str>>) -> McpServerConfig {
        McpServerConfig {
            id: name.to_string(),
            name: name.to_string(),
            transport: transport.to_string(),
            command: command.map(|c| c.iter().map(|s| s.to_string()).collect()),
            env: None,
            url: url.map(|s| s.to_string()),
            headers: None,
            oauth: None,
            enabled: true,
        }
    }

    #[test]
    fn mcp_add_args_pick_transport() {
        let remote = parse_mcp_add_args(
            &serde_json::json!({"name": "x", "url": "https://a/b", "headers": {"Authorization": "k"}}),
        )
        .unwrap();
        assert_eq!(remote.transport, "http");
        assert_eq!(remote.url.as_deref(), Some("https://a/b"));
        assert_eq!(
            remote.headers.as_ref().and_then(|h| h.get("Authorization")).map(|s| s.as_str()),
            Some("k")
        );
        assert!(remote.command.is_none());

        let local = parse_mcp_add_args(
            &serde_json::json!({"name": "y", "command": ["lb", "mcp"], "env": {"A": "b"}}),
        )
        .unwrap();
        assert_eq!(local.transport, "stdio");
        assert_eq!(local.command, Some(vec!["lb".to_string(), "mcp".to_string()]));
        assert!(local.url.is_none());
    }

    #[test]
    fn mcp_add_args_reject_before_any_io() {
        assert!(parse_mcp_add_args(&serde_json::json!({"url": "https://a"}))
            .unwrap_err()
            .contains("needs a \"name\""));
        assert!(parse_mcp_add_args(&serde_json::json!({"name": "a/b", "url": "https://a"}))
            .unwrap_err()
            .contains("no slashes"));
        assert!(parse_mcp_add_args(&serde_json::json!({"name": "x"}))
            .unwrap_err()
            .contains("either \"url\""));
        assert!(parse_mcp_add_args(&serde_json::json!({"name": "x", "url": "https://a", "command": ["y"]}))
            .unwrap_err()
            .contains("only one of"));
        assert!(parse_mcp_add_args(&serde_json::json!({"name": "x", "url": "ftp://a"}))
            .unwrap_err()
            .contains("http:// or https://"));
    }

    #[test]
    fn mcp_add_plan_guards_repoints() {
        let new = mcp_cfg("s", "http", Some("https://new"), None);
        assert!(matches!(mcp_add_plan(None, &new, false), Ok(McpAddPlan::Fresh)));
        // identical endpoint reconnects freely (refresh)
        let same = mcp_cfg("s", "http", Some("https://new"), None);
        assert!(matches!(mcp_add_plan(Some(&same), &new, false), Ok(McpAddPlan::Refresh)));
        // header-only drift is a credential refresh, not a re-point
        let mut keyed = same.clone();
        keyed.headers = Some(HashMap::from([("Authorization".to_string(), "old".to_string())]));
        assert!(matches!(mcp_add_plan(Some(&keyed), &new, false), Ok(McpAddPlan::Refresh)));
        // re-pointed endpoint needs explicit overwrite …
        let old = mcp_cfg("s", "http", Some("https://old"), None);
        let err = mcp_add_plan(Some(&old), &new, false).unwrap_err();
        assert!(err.contains("already exists") && err.contains("overwrite"), "got {err:?}");
        assert!(matches!(mcp_add_plan(Some(&old), &new, true), Ok(McpAddPlan::Overwrite)));
        // … as does a transport flip
        let stdio = mcp_cfg("s", "stdio", None, Some(vec!["lb", "mcp"]));
        assert!(mcp_add_plan(Some(&stdio), &new, false).is_err());
    }

    #[test]
    fn mcp_list_format_shows_status() {
        assert!(format_mcp_list(&[], &HashSet::new()).starts_with("No MCP servers"));
        let servers = vec![
            mcp_cfg("up", "http", Some("https://a"), None),
            McpServerConfig { enabled: false, ..mcp_cfg("off", "stdio", None, Some(vec!["x"])) },
        ];
        let out = format_mcp_list(&servers, &HashSet::from(["up".to_string()]));
        assert!(out.contains("\"up\"") && out.contains("connected"), "got {out:?}");
        assert!(out.contains("\"off\"") && out.contains("disabled"), "got {out:?}");
        assert!(out.contains("remote https://a") && out.contains("local x"), "got {out:?}");
        // the built-in row states the operative fact, not a flag it lacks
        let synth = synthetic_browser_config(vec!["/x/lb".to_string(), "mcp".to_string()]);
        let out = format_mcp_list(&[synth.clone()], &HashSet::new());
        assert!(out.contains("\"browser\"") && out.contains("not connected (built-in)"), "got {out:?}");
        let out = format_mcp_list(&[synth], &HashSet::from(["browser".to_string()]));
        assert!(out.contains("connected") && !out.contains("built-in"), "got {out:?}");
    }

    #[test]
    fn browser_agent_config_prefers_file_then_synthetic() {
        // hand-added file entry wins (case-insensitive), like the frontend
        let file = mcp_cfg("Browser", "stdio", None, Some(vec!["custom"]));
        let got = browser_agent_config(&[file], Some(vec!["/x/lb".to_string(), "mcp".to_string()])).unwrap();
        assert_eq!(got.command, Some(vec!["custom".to_string()]));
        // otherwise the resolved `lb mcp` argv becomes the synthetic row
        let got =
            browser_agent_config(&[], Some(vec!["/x/lb".to_string(), "mcp".to_string()])).unwrap();
        assert_eq!(got.id, "browser");
        assert_eq!(got.transport, "stdio");
        assert_eq!(
            got.env.as_ref().and_then(|e| e.get("LB_SERVER")).map(|s| s.as_str()),
            Some(lb_server_url().as_str())
        );
        // neither: no browser at all
        assert!(browser_agent_config(&[], None).is_none());
    }

    #[test]
    fn mcp_added_format_verifies_without_secrets() {
        use crate::mcp::McpToolInfo;
        let mut cfg = mcp_cfg("s", "http", Some("https://a"), None);
        cfg.headers = Some(HashMap::from([("Authorization".to_string(), "Bearer s3krit".to_string())]));
        let info = McpServerInfo {
            id: "s".to_string(),
            name: "s".to_string(),
            connected: true,
            tools: vec![
                McpToolInfo { name: "alpha".to_string(), description: None, input_schema: serde_json::json!({}) },
                McpToolInfo { name: "beta".to_string(), description: None, input_schema: serde_json::json!({}) },
            ],
        };
        let out = format_mcp_connected("Added", &info, &cfg);
        assert!(out.contains("alpha") && out.contains("beta"), "got {out:?}");
        assert!(out.contains("next reply"), "got {out:?}");
        assert!(!out.contains("s3krit"), "secret leaked: {out:?}");
        let out = format_mcp_connected("Enabled", &info, &cfg);
        assert!(out.starts_with("Enabled + connected"), "got {out:?}");
        assert_eq!(tool_label("mcp_add", &serde_json::json!({"name": "s", "headers": {"Authorization": "Bearer s3krit"}})), "mcp_add — s");
    }

    #[tokio::test]
    async fn manager_starts_with_no_connections() {
        assert!(McpManager::default().connected_ids().await.is_empty());
    }

    /// Dispatch-level rejections for the MCP tools that fail before touching
    /// the config file or the network — hermetic, no temp HOME needed.
    #[tokio::test]
    async fn dispatch_mcp_management_rejects_before_io() {
        let bg = BgExecState::default();
        let mcp = McpManager::default();
        let turn = TurnState::new();
        let routes = HashMap::new();
        let ch = crate::events::sink(|_| {});
        let browser = BrowserState::default();
        let undo = UndoState::default();
        macro_rules! run {
            ($name:expr, $args:expr) => {
                dispatch_tool($name, &$args, "/tmp", None, &[], &bg, &mcp, &routes, &turn, "turn-test", &ch, None, &browser, &undo, "").await
            };
        }
        let out = run!("mcp_add", serde_json::json!({"url": "https://example.com/mcp"}));
        assert!(out.contains("needs a \"name\""), "got {out:?}");
        let out = run!("mcp_add", serde_json::json!({"name": "x", "url": "https://a", "command": ["y"]}));
        assert!(out.contains("only one of"), "got {out:?}");
        let out = run!("mcp_add", serde_json::json!({"name": "browser", "url": "https://a"}));
        assert!(out.contains("built-in"), "got {out:?}");
        let out = run!("mcp_remove", serde_json::json!({}));
        assert!(out.contains("mcp_remove needs"), "got {out:?}");
        let out = run!("mcp_remove", serde_json::json!({"name": "browser"}));
        assert!(out.contains("built-in") && out.contains("mcp_disable"), "got {out:?}");
        let out = run!("mcp_add", serde_json::json!({"name": "browser", "url": "https://a"}));
        assert!(out.contains("built-in"), "got {out:?}");
        let out = run!("mcp_enable", serde_json::json!({}));
        assert!(out.contains("mcp_enable needs"), "got {out:?}");
        let out = run!("mcp_disable", serde_json::json!({}));
        assert!(out.contains("mcp_disable needs"), "got {out:?}");
        // NOTE: enable/disable("browser") are live paths now (daemon ensure
        // may spawn Chromium on a machine with `lb`), so they stay out of
        // this hermetic test — covered by scratch round-trips instead.
    }

    /// Headless end-to-end: drive the real loop (real Ollama, real FS tools)
    /// in a temp workspace and assert the event contract holds. Needs the
    /// local daemon plus a working cloud model; skips gracefully otherwise
    /// so plain `cargo test` stays green offline.
    #[tokio::test]
    async fn agent_loop_lists_and_writes_headless() {
        let client = reqwest::Client::new();
        let version_ok = client
            .get("http://localhost:11434/api/version")
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        if !version_ok {
            eprintln!("skipping agent e2e: no local ollama daemon");
            return;
        }
        let dir = std::env::temp_dir().join(format!(
            "vicuna-agent-e2e-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("seed.txt"), "seed\n").unwrap();
        let root = dir.to_string_lossy().to_string();

        let events = std::sync::Arc::new(std::sync::Mutex::new(Vec::<WorkAgentEvent>::new()));
        let collected = events.clone();
        let ch = crate::events::sink(move |json| {
            if let Ok(ev) = serde_json::from_str::<WorkAgentEvent>(json) {
                collected.lock().unwrap().push(ev);
            }
        });
        let turn = std::sync::Arc::new(TurnState {
            stop: AtomicBool::new(false),
            queue: Mutex::new(Vec::new()),
            ask_pending: Mutex::new(HashMap::new()),
            ask_seq: AtomicU64::new(1),
        });
        let tools = serde_json::json!([
            {"type":"function","function":{"name":"list","description":"List a workspace-relative directory.","parameters":{"type":"object","properties":{"path":{"type":"string"}}}}},
            {"type":"function","function":{"name":"read","description":"Read a text file.","parameters":{"type":"object","required":["path"],"properties":{"path":{"type":"string"}}}}},
            {"type":"function","function":{"name":"write","description":"Write a file.","parameters":{"type":"object","required":["path","content"],"properties":{"path":{"type":"string"},"content":{"type":"string"}}}}}
        ]);
        let native = vec![
            ChatMessage { role: "system".to_string(), content: "You operate inside a workspace. Use the tools.".to_string(), images: None, tool_calls: None, tool_name: None },
            ChatMessage { role: "user".to_string(), content: "List the files here, then write hello.txt containing exactly: hi-from-agent. Reply with one short line when done.".to_string(), images: None, tool_calls: None, tool_name: None },
        ];
        let bg = BgExecState::default();
        let mcp = McpManager::default();
        let browser = BrowserState::default();
        let undo = UndoState::default();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(240),
            run_loop(
                turn,
                "beam:cloud".to_string(),
                "ollama".to_string(),
                None,
                None,
                root.clone(),
                native,
                Some(serde_json::Value::Bool(false)),
                Some(tools),
                None,
                None,
                true,
                vec![],
                HashMap::new(),
                ch,
                &bg,
                &mcp,
                "turn-e2e",
                None,
                &browser,
                &undo,
            ),
        )
        .await;
        assert!(outcome.is_ok(), "agent loop timed out");
        let evs = events.lock().unwrap();
        let kinds: Vec<&str> = evs
            .iter()
            .map(|e| match e {
                WorkAgentEvent::Token { .. } => "token",
                WorkAgentEvent::ToolStart { .. } => "toolstart",
                WorkAgentEvent::ToolEnd { .. } => "toolend",
                WorkAgentEvent::Done { .. } => "done",
                WorkAgentEvent::Error { .. } => "error",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"toolstart"), "expected tool calls, got {kinds:?}");
        assert!(kinds.contains(&"done"), "expected done, got {kinds:?}");
        assert!(!kinds.contains(&"error"), "unexpected error events: {evs:?}");
        let written = std::fs::read_to_string(dir.join("hello.txt")).unwrap_or_default();
        assert!(written.contains("hi-from-agent"), "hello.txt missing agent content: {written:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
