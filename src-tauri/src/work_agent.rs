// Vicuna Work agent loop (Tauri backend).
//
// The agent rounds live here now: the frontend sends one turn request, this
// module runs the round scheduler + tool dispatch + background-job tracking,
// and streams semantic events back over a Channel. The webview stays the
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
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc, Mutex,
};
use std::time::Instant;
use tauri::{ipc::Channel, AppHandle, Manager, State};
use tokio::sync::oneshot;

use super::{
    bg_kill, bg_list, bg_snapshot, bg_start, parse_show_context_length,
    upload_read_file_logic as upload_read_file, work_exec_logic as work_exec,
    work_grep_logic as work_grep, work_list_dir_logic as work_list_dir,
    work_read_file_logic as work_read_file, work_read_image_logic as work_read_image,
    work_write_file_logic as work_write_file, BgExecState, ChatMessage, OLLAMA_BASE,
};
use crate::mcp::McpManager;

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
    RoundEnd { gen_ms: u64, gen_tps: Option<f64> },
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
fn emit_ev(ch: &Channel<WorkAgentEvent>, stop: &AtomicBool, ev: WorkAgentEvent) {
    if ch.send(ev).is_err() {
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
    on_event: &Channel<WorkAgentEvent>,
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
    on_event: &Channel<WorkAgentEvent>,
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
        .post(format!("{OLLAMA_BASE}/api/chat"))
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
    Ok(RoundResult { text: full, tool_calls, eval_count, eval_duration_ns })
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
    let fut = client.post(format!("{OLLAMA_BASE}/api/chat")).json(&body).send();
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
        .post(format!("{OLLAMA_BASE}/api/show"))
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
    on_event: &Channel<WorkAgentEvent>,
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
                Some(content) => match work_write_file(workspace.to_string(), p.clone(), content.to_string()).await {
                    Ok(_) => format!("Wrote {} ({} chars).", p, content.len()),
                    Err(e) => format!("Error: {e}"),
                },
                None => "Error: write needs a string \"content\".".to_string(),
            }
        }
        "exec" => {
            let cmd = str_arg(args, "command");
            if cmd.is_empty() {
                return "Error: exec needs {\"command\": \"...\"}.".to_string();
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
                        "Error: unknown tool \"{name}\". There is no {name} tool — run shell commands with exec instead, e.g. {{\"command\": {}}}. Available: list, read, view_image, grep, write, exec, exec_status, exec_kill, sessions, read_session, read_upload, say, ask.",
                        serde_json::json!(maybe_cmd)
                    )
                } else {
                    format!("Error: unknown tool \"{name}\". Available: list, read, view_image, grep, read, view_image, grep, write, exec, exec_status, exec_kill, sessions, read_session, read_upload, say, ask.")
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
    workspace: String,
    mut native: Vec<ChatMessage>,
    think: Option<serde_json::Value>,
    tools: Option<serde_json::Value>,
    session_id: Option<String>,
    goal_text: Option<String>,
    goal_paused: bool,
    chats: Vec<ChatSnapshot>,
    mcp_routes: HashMap<String, McpRoute>,
    on_event: Channel<WorkAgentEvent>,
    bg: &BgExecState,
    mcp: &McpManager,
    turn_id: &str,
) {
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
            let win = model_context_length(&model).await.unwrap_or(32768) as usize;
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
                let summary = ollama_oneshot(
                    &model,
                    "Summarize this agent transcript excerpt concisely: files listed/read/written, commands run and their key output, decisions made, and what remains to do. Keep file paths and facts; drop chatter.",
                    &transcript.chars().take(60000).collect::<String>(),
                    &turn.stop,
                )
                .await
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
        let round = match ollama_round(&model, &native, &think, &tools, &on_event, &turn.stop).await {
            Ok(r) => r,
            Err(e) => {
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: 0, gen_tps: None });
                if turn.stop.load(Ordering::Relaxed) {
                    break;
                }
                emit_ev(&on_event, &turn.stop, WorkAgentEvent::Error { message: e });
                break;
            }
        };
        first_round = false;
        let wall_ms = t0.elapsed().as_millis() as u64;
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: wall_ms, gen_tps: round_tps(&round, wall_ms) });
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
                let k = tool_key(n, a);
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
            let wrap = ollama_round(&model, &native, &think, &tools, &on_event, &turn.stop).await;
            let wall_ms = t0.elapsed().as_millis() as u64;
            match wrap {
                Ok(r) => {
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: wall_ms, gen_tps: round_tps(&r, wall_ms) });
                    native.push(ChatMessage {
                        role: "assistant".to_string(),
                        content: r.text,
                        images: None,
                        tool_calls: None,
                        tool_name: None,
                    });
                }
                Err(_) => {
                    emit_ev(&on_event, &turn.stop, WorkAgentEvent::RoundEnd { gen_ms: 0, gen_tps: None });
                }
            }
            break;
        }
        for (name, args) in fresh {
            if turn.stop.load(Ordering::Relaxed) {
                break;
            }
            if name == "done" {
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
            let out = dispatch_tool(&name, &args, &workspace, session_id.as_deref(), &chats, bg, mcp, &mcp_routes, &turn, turn_id, &on_event).await;
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

fn get_turn(agent_state: &WorkAgentState, turn_id: &str) -> Result<Arc<TurnState>, String> {
    agent_state.get_turn(turn_id).ok_or_else(|| "unknown turn".to_string())
}

// ---------- commands ----------

#[tauri::command]
pub async fn work_agent_start(
    app: AppHandle,
    model: String,
    workspace: String,
    messages: Vec<ChatMessage>,
    think: Option<serde_json::Value>,
    tools: Option<serde_json::Value>,
    session_id: Option<String>,
    goal_text: Option<String>,
    goal_paused: Option<bool>,
    chats: Option<Vec<ChatSnapshot>>,
    mcp_routes: Option<HashMap<String, McpRoute>>,
    on_event: Channel<WorkAgentEvent>,
    agent_state: State<'_, WorkAgentState>,
) -> Result<String, String> {
    let turn = Arc::new(TurnState::new());
    let turn_id = agent_state.next_turn(turn.clone());
    // The loop runs detached so this command resolves with the turn id
    // immediately — stop/queue/answer operate on the shared TurnState while
    // the task below owns an AppHandle for State access.
    let tid = turn_id.clone();
    tokio::spawn(async move {
        let bg = app.state::<BgExecState>();
        let mcp = app.state::<McpManager>();
        // Hold the guards for the whole turn.
        let (bg_ref, mcp_ref) = (&*bg, &*mcp);
        run_loop(
            turn,
            model,
            workspace,
            messages,
            think,
            tools,
            session_id,
            goal_text,
            goal_paused.unwrap_or(true),
            chats.unwrap_or_default(),
            mcp_routes.unwrap_or_default(),
            on_event,
            bg_ref,
            mcp_ref,
            &tid,
        )
        .await;
    });
    Ok(turn_id)
}

#[tauri::command]
pub async fn work_agent_stop(turn_id: String, agent_state: State<'_, WorkAgentState>) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_stop(&turn);
    Ok(())
}

#[tauri::command]
pub async fn work_agent_queue(
    turn_id: String,
    id: String,
    message: ChatMessage,
    agent_state: State<'_, WorkAgentState>,
) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_queue_msg(&turn, id, message)
}

/// Drop a still-queued steering message (composer chip removed). Already
/// drained messages are untouched — they have bubbles by then.
#[tauri::command]
pub async fn work_agent_remove(
    turn_id: String,
    id: String,
    agent_state: State<'_, WorkAgentState>,
) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_remove_msg(&turn, &id)
}

#[tauri::command]
pub async fn work_agent_answer(
    turn_id: String,
    ask_id: String,
    answer: String,
    agent_state: State<'_, WorkAgentState>,
) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_answer(&turn, &ask_id, answer)
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
        let sink = events.clone();
        let ch = Channel::new(move |body| {
            if let tauri::ipc::InvokeResponseBody::Json(s) = body {
                if let Ok(ev) = serde_json::from_str::<WorkAgentEvent>(&s) {
                    sink.lock().unwrap().push(ev);
                }
            }
            Ok(())
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
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(240),
            run_loop(
                turn,
                "beam:cloud".to_string(),
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
