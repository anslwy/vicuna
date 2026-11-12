// Claude Code Work engine: the user's own `claude` CLI, driven headless.
//
// No SDK and no adapter: each Work turn runs
//   claude -p --input-format stream-json --output-format stream-json
//          --verbose --include-partial-messages --permission-prompt-tool stdio
//          [--model M] [--resume SESSION] [--mcp-config JSON]
// in the workspace, writes the user's message as a stream-json `user` line and
// reads the CLI's structured events:
//   stream_event text/thinking deltas -> Token / Think (live streaming)
//   assistant tool_use blocks (complete input) -> ToolStart with real args
//   user tool_result blocks -> ToolEnd
//   result -> RoundEnd (time, cost); the session id is saved for --resume
//   control_request can_use_tool -> auto-allow, or the chat's ask card
// Messages queued during the turn are written to the same process as the
// next `user` line. Stop sends an `interrupt` control request (kill after a
// grace period). Models come from the CLI's own `initialize` control
// response (no prompt, nothing billed).
//
// Chosen over driving interactive `claude` in a hidden terminal + hooks
// (Kurisu-style): this is the CLI's official structured protocol, works the
// same on Windows/macOS/Linux (no tmux), and needs no screen or transcript
// scraping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};

use crate::acp::{self, AcpState, McpSnapshot};
use crate::opencode::OpencodeModel;
use crate::work_agent::{emit_ev, TurnState, WorkAgentEvent};
use crate::work_engine::Engine;
use crate::ChatMessage;

const INIT_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_GRACE: Duration = Duration::from_secs(20);
const OUTPUT_CAP: usize = 32 * 1024;

// ---------- process ----------

fn base_command(cli: &Path, cwd: &Path) -> Command {
    let mut cmd = Command::new(cli);
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-prompt-tool",
        "stdio",
    ]);
    // the terminal's PATH (claude's own dir first), so MCP servers it spawns
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

fn control_request(id: &str, request: Value) -> Value {
    json!({ "type": "control_request", "request_id": id, "request": request })
}

fn user_line(content: Vec<Value>) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null,
        "session_id": ""
    })
}

/// Text + images as Anthropic content blocks (images are data URLs).
pub fn content_blocks(text: &str, images: &Option<Vec<String>>) -> Vec<Value> {
    let mut blocks = vec![json!({ "type": "text", "text": text })];
    for url in images.iter().flatten() {
        let Some((meta, data)) = url.split_once(',') else { continue };
        let Some(mime) = meta.strip_prefix("data:").and_then(|m| m.split(';').next()) else { continue };
        blocks.push(json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": data } }));
    }
    blocks
}

// ---------- models (from the CLI's initialize response) ----------

static MODELS: Mutex<Option<(PathBuf, Vec<OpencodeModel>)>> = Mutex::new(None);

/// `models` from the initialize control response, minus the "default"
/// pseudo-entry (the picker has its own default row).
pub fn parse_models(init_response: &Value) -> Vec<OpencodeModel> {
    let mut out: Vec<OpencodeModel> = Vec::new();
    for m in init_response.get("models").and_then(|m| m.as_array()).into_iter().flatten() {
        let Some(id) = m.get("value").and_then(|v| v.as_str()).filter(|v| !v.is_empty() && *v != "default") else {
            continue;
        };
        if out.iter().any(|x| x.id == id) {
            continue;
        }
        out.push(OpencodeModel {
            id: id.to_string(),
            provider: "claude".to_string(),
            name: m.get("displayName").and_then(|v| v.as_str()).map(str::to_string),
        });
    }
    out
}

pub async fn list_models() -> Result<Vec<OpencodeModel>, String> {
    let cli = crate::claude::find_claude().ok_or_else(|| Engine::Claude.missing_message())?;
    if let Some((p, cached)) = MODELS.lock().ok().and_then(|g| g.clone()) {
        if p == cli {
            return Ok(cached);
        }
    }
    let home = crate::opencode::home_dir().unwrap_or_else(|| PathBuf::from("."));
    let mut child = base_command(&cli, &home)
        .spawn()
        .map_err(|e| format!("cannot start claude ({}): {e}", cli.display()))?;
    let mut stdin = child.stdin.take().ok_or("claude: no stdin")?;
    let stdout = child.stdout.take().ok_or("claude: no stdout")?;
    stdin
        .write_all(line(&control_request("vicuna-init", json!({ "subtype": "initialize" }))).as_bytes())
        .await
        .map_err(|e| format!("claude: {e}"))?;
    let mut lines = BufReader::new(stdout).lines();
    let read = async {
        while let Ok(Some(l)) = lines.next_line().await {
            let Ok(m) = serde_json::from_str::<Value>(&l) else { continue };
            if m.get("type").and_then(|t| t.as_str()) == Some("control_response") {
                return Ok(m.pointer("/response/response").cloned().unwrap_or(Value::Null));
            }
        }
        Err("claude exited before listing models (is it logged in? run `claude` once and /login)".to_string())
    };
    let resp = tokio::time::timeout(INIT_TIMEOUT, read)
        .await
        .map_err(|_| "claude did not answer initialize in time".to_string())??;
    drop(stdin);
    let _ = child.start_kill();
    let models = parse_models(&resp);
    if models.is_empty() {
        return Err("Claude Code reported no models (is it logged in? run `claude` once and /login)".into());
    }
    if let Ok(mut g) = MODELS.lock() {
        *g = Some((cli, models.clone()));
    }
    Ok(models)
}

// ---------- MCP: ACP-shaped server entries -> --mcp-config JSON ----------

fn name_values(v: Option<&Value>) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for e in v.and_then(|x| x.as_array()).into_iter().flatten() {
        if let (Some(n), Some(val)) = (e.get("name").and_then(|x| x.as_str()), e.get("value").and_then(|x| x.as_str())) {
            out.insert(n.to_string(), json!(val));
        }
    }
    out
}

/// `{"mcpServers": {name: {command,args,env} | {type:"http",url,headers}}}`
pub fn mcp_config(entries: &[Value]) -> Option<String> {
    let mut servers = serde_json::Map::new();
    for e in entries {
        let Some(name) = e.get("name").and_then(|n| n.as_str()) else { continue };
        let cfg = if let Some(url) = e.get("url").and_then(|u| u.as_str()) {
            json!({ "type": "http", "url": url, "headers": name_values(e.get("headers")) })
        } else if let Some(cmd) = e.get("command").and_then(|c| c.as_str()) {
            json!({ "command": cmd, "args": e.get("args").cloned().unwrap_or(json!([])), "env": name_values(e.get("env")) })
        } else {
            continue;
        };
        servers.insert(name.to_string(), cfg);
    }
    (!servers.is_empty()).then(|| json!({ "mcpServers": servers }).to_string())
}

/// AskUserQuestion can't be answered through `-p` (its can_use_tool request
/// carries no answers, so the model reads "The user did not answer"); it is
/// switched off and vicuna-mcp's ask_user, pre-approved so "ask before
/// running tools" doesn't gate the question itself, takes its place.
pub fn question_tool_args(mcp: &[Value]) -> Vec<String> {
    let mut args = vec!["--disallowedTools".to_string(), "AskUserQuestion".to_string()];
    if mcp.iter().any(|e| e.get("name").and_then(|n| n.as_str()) == Some(crate::vicuna_mcp::SERVER_NAME)) {
        args.push("--allowedTools".to_string());
        args.push(crate::vicuna_mcp::CLAUDE_TOOL_ID.to_string());
    }
    args
}

// ---------- tool cards ----------

/// Claude tool names -> Vicuna's card names/args (unknown/MCP: as-is).
pub fn tool_card(name: &str, input: &Value) -> (String, Value) {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "Bash" | "BashOutput" => ("exec".into(), json!({ "command": s("command") })),
        "Read" | "NotebookRead" => ("read".into(), json!({ "path": s("file_path") })),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let p = if input.get("file_path").is_some() { s("file_path") } else { s("notebook_path") };
            ("write".into(), json!({ "path": p }))
        }
        "Grep" => ("grep".into(), json!({ "pattern": s("pattern"), "path": s("path") })),
        "Glob" => ("grep".into(), json!({ "pattern": s("pattern"), "path": s("path") })),
        "WebFetch" => ("fetch_url".into(), json!({ "url": s("url") })),
        _ => (name.to_string(), input.clone()),
    }
}

fn result_text(content: &Value) -> String {
    let mut out = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|i| match i.get("type").and_then(|t| t.as_str()) {
                Some("text") => i.get("text").and_then(|t| t.as_str()).map(str::to_string),
                Some("image") => Some("[image]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if out.chars().count() > OUTPUT_CAP {
        out = out.chars().take(OUTPUT_CAP).collect::<String>();
        out.push_str("\n[output truncated]");
    }
    out
}

/// One stream-json line -> UI events. `tools` maps tool_use ids to card
/// names (ToolEnd needs the name ToolStart used).
pub fn translate(msg: &Value, tools: &mut HashMap<String, String>, output_chars: &mut usize) -> Vec<WorkAgentEvent> {
    let mut out = Vec::new();
    match msg.get("type").and_then(|t| t.as_str()) {
        Some("stream_event") => {
            let ev = msg.get("event").unwrap_or(&Value::Null);
            if ev.get("type").and_then(|t| t.as_str()) == Some("content_block_delta") {
                let d = ev.get("delta").unwrap_or(&Value::Null);
                match d.get("type").and_then(|t| t.as_str()) {
                    Some("text_delta") => {
                        if let Some(t) = d.get("text").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                            *output_chars += t.chars().count();
                            out.push(WorkAgentEvent::Token { text: t.to_string() });
                        }
                    }
                    Some("thinking_delta") => {
                        if let Some(t) = d.get("thinking").and_then(|t| t.as_str()).filter(|t| !t.is_empty()) {
                            out.push(WorkAgentEvent::Think { text: t.to_string() });
                        }
                    }
                    _ => {}
                }
            }
        }
        // complete assistant message: tool_use blocks carry the full input
        // (text/thinking already streamed as deltas)
        Some("assistant") => {
            for b in msg.pointer("/message/content").and_then(|c| c.as_array()).into_iter().flatten() {
                if b.get("type").and_then(|t| t.as_str()) != Some("tool_use") {
                    continue;
                }
                let id = b.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();
                if id.is_empty() || tools.contains_key(&id) {
                    continue;
                }
                let name = b.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let (card, args) = tool_card(name, b.get("input").unwrap_or(&json!({})));
                tools.insert(id, card.clone());
                out.push(WorkAgentEvent::ToolStart { name: card, args });
            }
        }
        Some("user") => {
            for b in msg.pointer("/message/content").and_then(|c| c.as_array()).into_iter().flatten() {
                if b.get("type").and_then(|t| t.as_str()) != Some("tool_result") {
                    continue;
                }
                let id = b.get("tool_use_id").and_then(|i| i.as_str()).unwrap_or("");
                let Some(name) = tools.get(id).cloned() else { continue };
                let mut output = result_text(b.get("content").unwrap_or(&Value::Null));
                if b.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false) && !output.starts_with("Error:") {
                    output = format!("Error: {output}");
                }
                if output.trim().is_empty() {
                    output = "[completed]".to_string();
                }
                out.push(WorkAgentEvent::ToolEnd { name, output });
            }
        }
        _ => {}
    }
    out
}

// ---------- permissions ----------

pub fn permission_response(request_id: &str, input: &Value, allow: bool) -> Value {
    let response = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": "The user denied this tool call in Vicuna." })
    };
    json!({
        "type": "control_response",
        "response": { "subtype": "success", "request_id": request_id, "response": response }
    })
}

pub fn is_allow(answer: &str) -> bool {
    matches!(answer.trim().to_lowercase().as_str(), "allow" | "yes" | "y" | "ok" | "approve" | "allow once")
}

fn permission_title(req: &Value) -> String {
    let tool = req.get("tool_name").and_then(|t| t.as_str()).unwrap_or("a tool");
    let input = req.get("input").unwrap_or(&Value::Null);
    let detail = ["command", "file_path", "url", "pattern", "path"]
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .map(|d| format!(": {d}"))
        .unwrap_or_default();
    format!("{tool}{detail}")
}

/// can_use_tool: auto mode allows at once; ask mode raises the chat's ask
/// card and answers when the user picks (Stop/dismiss denies).
fn handle_permission(
    msg: &Value,
    turn: &Arc<TurnState>,
    on_event: &Channel<WorkAgentEvent>,
    writer: &mpsc::UnboundedSender<String>,
) {
    let request_id = msg.get("request_id").and_then(|r| r.as_str()).unwrap_or("").to_string();
    let req = msg.get("request").cloned().unwrap_or(Value::Null);
    let input = req.get("input").cloned().unwrap_or(json!({}));
    let title = permission_title(&req);
    if !crate::work_engine::approval_ask() {
        let _ = writer.send(line(&permission_response(&request_id, &input, true)));
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
    let (writer, on_event, turn) = (writer.clone(), on_event.clone(), turn.clone());
    tokio::spawn(async move {
        let answer = rx.await.unwrap_or_else(|_| "cancelled".to_string());
        let allow = is_allow(&answer);
        let _ = writer.send(line(&permission_response(&request_id, &input, allow)));
        let verdict = if allow { "Approved" } else { "Denied" };
        emit_ev(&on_event, &turn.stop, WorkAgentEvent::DisplayNote { text: format!("\n> {verdict}: {title}\n") });
    });
}

// ---------- one Work turn ----------

fn result_error(msg: &Value) -> Option<String> {
    let is_err = msg.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false)
        || msg.get("subtype").and_then(|s| s.as_str()).is_some_and(|s| s.starts_with("error"));
    if !is_err {
        return None;
    }
    let text = msg
        .get("result")
        .and_then(|r| r.as_str())
        .map(str::to_string)
        .or_else(|| msg.get("errors").map(|e| e.to_string()))
        .unwrap_or_else(|| msg.get("subtype").and_then(|s| s.as_str()).unwrap_or("error").to_string());
    Some(crate::exe_resolve::strip_ansi(&text))
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
    let cli = crate::claude::find_claude().ok_or_else(|| Engine::Claude.missing_message())?;
    let last_user = acp::last_user_message(messages).ok_or_else(|| "Work mode: no user message to send".to_string())?;
    let key = acp::session_key(Engine::Claude, session_id, workspace);
    let saved = if fresh_session { None } else { key.as_deref().and_then(|k| state.get(k)) };

    let (mcp, mcp_note) = acp::mcp_servers_for_turn(mcp_snapshot).await;
    if let Some(note) = mcp_note {
        emit_ev(on_event, stop, WorkAgentEvent::DisplayNote { text: format!("\n\n> {note}\n\n") });
    }
    let mcp = crate::vicuna_mcp::with_ask_server(mcp, ask_url);
    let picked = acp::effective_model(Engine::Claude, model);
    if let Some(m) = &picked {
        emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Claude.reply_label(m) });
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

    // a saved session that no longer exists: start over with the preamble
    let attempts: Vec<(Option<String>, String)> = match saved {
        Some(s) => vec![(Some(s), base_text.clone()), (None, fresh_text.clone())],
        None => vec![(None, fresh_text.clone())],
    };
    let mut last_err = String::new();
    for (resume, text) in attempts {
        match run_process(&cli, workspace, resume.as_deref(), picked.as_deref(), &mcp, &text, last_user, turn, on_event, state, key.as_deref()).await {
            Ok(()) => {
                emit_ev(on_event, stop, WorkAgentEvent::Done { stopped: stop.load(Ordering::Relaxed) });
                return Ok(());
            }
            Err(RunError::ResumeMissing(e)) => last_err = e,
            Err(RunError::Fatal(e)) => return Err(e),
        }
    }
    Err(last_err)
}

enum RunError {
    /// `--resume` named a session the CLI no longer has (before any output).
    ResumeMissing(String),
    Fatal(String),
}

#[allow(clippy::too_many_arguments)]
async fn run_process(
    cli: &Path,
    workspace: &str,
    resume: Option<&str>,
    model: Option<&str>,
    mcp: &[Value],
    first_text: &str,
    last_user: &ChatMessage,
    turn: &Arc<TurnState>,
    on_event: &Channel<WorkAgentEvent>,
    state: &AcpState,
    key: Option<&str>,
) -> Result<(), RunError> {
    let stop = &turn.stop;
    let mut cmd = base_command(cli, Path::new(workspace));
    if let Some(m) = model {
        cmd.args(["--model", m]);
    }
    if let Some(r) = resume {
        cmd.args(["--resume", r]);
    }
    if let Some(cfg) = mcp_config(mcp) {
        cmd.args(["--mcp-config", &cfg]);
    }
    cmd.args(question_tool_args(mcp));
    let mut child = cmd
        .spawn()
        .map_err(|e| RunError::Fatal(crate::work_engine::spawn_error("claude", cli, Path::new(workspace), &e)))?;
    let mut stdin = child.stdin.take().ok_or(RunError::Fatal("claude: no stdin".into()))?;
    let stdout = child.stdout.take().ok_or(RunError::Fatal("claude: no stdout".into()))?;
    let stderr = child.stderr.take().ok_or(RunError::Fatal("claude: no stderr".into()))?;

    // every write goes through one channel (permission answers come from
    // spawned ask tasks); dropping all senders closes stdin -> CLI exits
    let (writer, mut write_rx) = mpsc::unbounded_channel::<String>();
    let write_task = tokio::spawn(async move {
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

    let _ = writer.send(line(&control_request("vicuna-init", json!({ "subtype": "initialize" }))));
    let _ = writer.send(line(&user_line(content_blocks(first_text, &last_user.images))));
    emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "fresh".into() });

    let mut lines = BufReader::new(stdout).lines();
    let mut tools: HashMap<String, String> = HashMap::new();
    let mut output_chars = 0usize;
    let mut started = Instant::now();
    let mut got_output = false;
    let mut interrupted_at: Option<Instant> = None;
    let mut writer = Some(writer);
    let outcome: Result<(), RunError> = loop {
        tokio::select! {
            next = lines.next_line() => {
                let Ok(Some(l)) = next else {
                    // process ended
                    if stop.load(Ordering::Relaxed) { break Ok(()); }
                    let tail = err_tail.lock().map(|s| s.trim().to_string()).unwrap_or_default();
                    let tail = crate::exe_resolve::strip_ansi(&tail);
                    if resume.is_some() && !got_output {
                        break Err(RunError::ResumeMissing(format!("claude could not resume the session: {tail}")));
                    }
                    if writer.is_none() { break Ok(()); }
                    break Err(RunError::Fatal(acp::friendly_prompt_error(Engine::Claude,
                        if tail.is_empty() { "claude exited unexpectedly".to_string() } else { format!("claude exited unexpectedly:\n{tail}") })));
                };
                let Ok(msg) = serde_json::from_str::<Value>(&l) else { continue };
                match msg.get("type").and_then(|t| t.as_str()) {
                    Some("system") if msg.get("subtype").and_then(|s| s.as_str()) == Some("init") => {
                        got_output = true;
                        if let (Some(k), Some(sid)) = (key, msg.get("session_id").and_then(|s| s.as_str())) {
                            state.set(k.to_string(), sid.to_string());
                        }
                        if model.is_none() {
                            if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
                                emit_ev(on_event, stop, WorkAgentEvent::Model { model: Engine::Claude.reply_label(m) });
                            }
                        }
                    }
                    Some("control_request") => {
                        if msg.pointer("/request/subtype").and_then(|s| s.as_str()) == Some("can_use_tool") {
                            if let Some(w) = &writer {
                                handle_permission(&msg, turn, on_event, w);
                            }
                        }
                    }
                    Some("result") => {
                        got_output = true;
                        let gen_ms = msg.get("duration_ms").and_then(|d| d.as_u64())
                            .unwrap_or_else(|| started.elapsed().as_millis() as u64);
                        let out_tokens = msg.pointer("/usage/output_tokens").and_then(|t| t.as_f64());
                        let secs = (gen_ms as f64 / 1000.0).max(0.001);
                        let gen_tps = out_tokens.map(|t| t / secs)
                            .or_else(|| (output_chars > 0).then(|| (output_chars as f64 / 4.0) / secs));
                        let gen_cost = msg.get("total_cost_usd").and_then(|c| c.as_f64());
                        emit_ev(on_event, stop, WorkAgentEvent::RoundEnd { gen_ms, gen_tps, gen_cost });
                        if let Some(e) = result_error(&msg) {
                            if !stop.load(Ordering::Relaxed) && interrupted_at.is_none() {
                                break Err(RunError::Fatal(acp::friendly_prompt_error(Engine::Claude, e)));
                            }
                        }
                        if stop.load(Ordering::Relaxed) {
                            writer = None;
                            continue;
                        }
                        // queued messages run as the next prompt of this turn
                        let queued = acp::take_queue(turn);
                        if queued.is_empty() {
                            writer = None; // close stdin: the CLI exits
                            continue;
                        }
                        let ids: Vec<String> = queued.iter().map(|(id, _)| id.clone()).collect();
                        let drained: Vec<ChatMessage> = queued.into_iter().map(|(_, m)| m).collect();
                        emit_ev(on_event, stop, WorkAgentEvent::QueueDrained { ids, messages: drained.clone() });
                        let (t, imgs) = acp::messages_text(&drained);
                        if let Some(w) = &writer {
                            let _ = w.send(line(&user_line(content_blocks(&t, &imgs))));
                        }
                        output_chars = 0;
                        started = Instant::now();
                        emit_ev(on_event, stop, WorkAgentEvent::RoundStart { mode: "continue".into() });
                    }
                    _ => {
                        for ev in translate(&msg, &mut tools, &mut output_chars) {
                            got_output = true;
                            emit_ev(on_event, stop, ev);
                        }
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(120)) => {
                if stop.load(Ordering::Relaxed) && interrupted_at.is_none() {
                    interrupted_at = Some(Instant::now());
                    if let Some(w) = &writer {
                        let _ = w.send(line(&control_request("vicuna-stop", json!({ "subtype": "interrupt" }))));
                    }
                }
                if interrupted_at.is_some_and(|t| t.elapsed() > CANCEL_GRACE) {
                    let _ = child.start_kill();
                    break Ok(());
                }
            }
        }
    };
    drop(writer);
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    let _ = child.start_kill();
    write_task.abort();
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_come_from_initialize_response() {
        // shape captured from claude 2.1.322 (`initialize` control response)
        let r = json!({ "models": [
            { "value": "default", "displayName": "Default (recommended)" },
            { "value": "opus", "displayName": "Opus 5.5" },
            { "value": "haiku", "displayName": "Haiku 5.5" },
            { "value": "haiku", "displayName": "dup" }
        ]});
        let m: Vec<_> = parse_models(&r).into_iter().map(|m| (m.id, m.name)).collect();
        assert_eq!(m, vec![("opus".into(), Some("Opus 5.5".into())), ("haiku".into(), Some("Haiku 5.5".into()))]);
        assert!(parse_models(&json!({})).is_empty());
    }

    /// Captured from a real `claude -p --output-format stream-json
    /// --include-partial-messages` turn: deltas stream, the tool card gets
    /// the complete input, the result closes it.
    #[test]
    fn stream_json_turn_maps_to_ui_events() {
        let mut tools = HashMap::new();
        let mut chars = 0;
        let start = json!({ "type": "stream_event", "event": { "type": "content_block_start", "content_block": { "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {} } } });
        assert!(translate(&start, &mut tools, &mut chars).is_empty(), "no empty-args card from the start event");
        let partial = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "input_json_delta", "partial_json": "{\"command\": \"echo" } } });
        assert!(translate(&partial, &mut tools, &mut chars).is_empty());
        let full = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "toolu_1", "name": "Bash", "input": { "command": "echo probe-ok", "description": "Print" } }] } });
        match &translate(&full, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "exec");
                assert_eq!(args["command"], "echo probe-ok");
            }
            other => panic!("{other:?}"),
        }
        assert!(translate(&full, &mut tools, &mut chars).is_empty(), "a repeated assistant message adds no second card");
        let res = json!({ "type": "user", "message": { "content": [{ "type": "tool_result", "tool_use_id": "toolu_1", "content": "probe-ok", "is_error": false }] } });
        match &translate(&res, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolEnd { name, output }] => {
                assert_eq!(name, "exec");
                assert_eq!(output, "probe-ok");
            }
            other => panic!("{other:?}"),
        }
        let think = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "hmm" } } });
        assert!(matches!(&translate(&think, &mut tools, &mut chars)[..], [WorkAgentEvent::Think { .. }]));
        let empty_think = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "thinking_delta", "thinking": "" } } });
        assert!(translate(&empty_think, &mut tools, &mut chars).is_empty());
        let text = json!({ "type": "stream_event", "event": { "type": "content_block_delta", "delta": { "type": "text_delta", "text": "DONE" } } });
        assert!(matches!(&translate(&text, &mut tools, &mut chars)[..], [WorkAgentEvent::Token { text }] if text == "DONE"));
        assert_eq!(chars, 4);
        // the final assistant text block is not re-emitted (already streamed)
        let final_text = json!({ "type": "assistant", "message": { "content": [{ "type": "text", "text": "DONE" }] } });
        assert!(translate(&final_text, &mut tools, &mut chars).is_empty());
        // MCP tools keep their name and real args
        let mcp = json!({ "type": "assistant", "message": { "content": [{ "type": "tool_use", "id": "toolu_2", "name": "mcp__browser__open", "input": { "url": "http://x" } }] } });
        match &translate(&mcp, &mut tools, &mut chars)[..] {
            [WorkAgentEvent::ToolStart { name, args }] => {
                assert_eq!(name, "mcp__browser__open");
                assert_eq!(args["url"], "http://x");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn ask_user_replaces_ask_user_question() {
        let none = question_tool_args(&[]);
        assert_eq!(none, vec!["--disallowedTools", "AskUserQuestion"]);
        let mcp = crate::vicuna_mcp::with_ask_server(vec![], Some("http://127.0.0.1:9/mcp/t"));
        assert_eq!(
            question_tool_args(&mcp),
            vec!["--disallowedTools", "AskUserQuestion", "--allowedTools", "mcp__vicuna-mcp__ask_user"]
        );
        let cfg: Value = serde_json::from_str(&mcp_config(&mcp).unwrap()).unwrap();
        assert_eq!(cfg["mcpServers"]["vicuna-mcp"], json!({ "type": "http", "url": "http://127.0.0.1:9/mcp/t", "headers": {} }));
    }

    #[test]
    fn mcp_config_converts_acp_entries() {
        let cfg = mcp_config(&[
            json!({ "name": "probe", "command": "/usr/bin/python3", "args": ["s.py", "3"], "env": [{ "name": "K", "value": "v" }] }),
            json!({ "type": "http", "name": "searchapi", "url": "https://m/mcp", "headers": [{ "name": "Authorization", "value": "Bearer t" }] }),
        ])
        .unwrap();
        let v: Value = serde_json::from_str(&cfg).unwrap();
        assert_eq!(v["mcpServers"]["probe"]["command"], "/usr/bin/python3");
        assert_eq!(v["mcpServers"]["probe"]["args"], json!(["s.py", "3"]));
        assert_eq!(v["mcpServers"]["probe"]["env"]["K"], "v");
        assert_eq!(v["mcpServers"]["searchapi"]["type"], "http");
        assert_eq!(v["mcpServers"]["searchapi"]["headers"]["Authorization"], "Bearer t");
        assert_eq!(mcp_config(&[]), None);
    }

    #[test]
    fn permission_responses_and_errors() {
        let allow = permission_response("r1", &json!({ "command": "ls" }), true);
        assert_eq!(allow["response"]["response"]["behavior"], "allow");
        assert_eq!(allow["response"]["response"]["updatedInput"]["command"], "ls");
        assert_eq!(allow["response"]["request_id"], "r1");
        assert_eq!(permission_response("r1", &json!({}), false)["response"]["response"]["behavior"], "deny");
        assert!(is_allow("Allow") && is_allow("yes") && !is_allow("Deny") && !is_allow("cancelled"));
        assert_eq!(result_error(&json!({ "type": "result", "subtype": "success", "is_error": false })), None);
        assert_eq!(result_error(&json!({ "type": "result", "subtype": "error_during_execution", "is_error": true, "result": "boom" })).as_deref(), Some("boom"));
        assert_eq!(permission_title(&json!({ "tool_name": "Bash", "input": { "command": "rm x" } })), "Bash: rm x");
    }

    #[test]
    fn images_become_base64_blocks() {
        let b = content_blocks("hi", &Some(vec!["data:image/png;base64,AAAA".into(), "bogus".into()]));
        assert_eq!(b.len(), 2);
        assert_eq!(b[1]["source"]["media_type"], "image/png");
        assert_eq!(b[1]["source"]["data"], "AAAA");
    }
}
