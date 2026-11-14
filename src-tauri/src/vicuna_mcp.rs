// vicuna-mcp: Vicuna's own MCP server, handed to every Work engine so the
// agent can ask the user a question mid-turn.
//
// Why: neither engine's native question tool works under Vicuna. opencode's
// `question` tool only renders when the ACP client advertises form
// elicitation (it fails at once with "The question couldn't be shown to the
// user in this client"), and Claude Code's AskUserQuestion under `claude -p`
// arrives as a `can_use_tool` permission request that Vicuna allowed with no
// answers ("The user did not answer the questions."). Both engines speak MCP,
// so one `ask_user` tool covers both: a call raises the chat's ask card
// (AskRequest), waits for the user's answer and returns it as the tool result.
//
// Transport: streamable HTTP on 127.0.0.1 (random port, started on first use)
// with plain JSON responses. Each route is `/mcp/<token>` (128-bit random) and
// maps to the live turn(s) registered for it; a call to an unknown or idle
// token returns a tool error. opencode keeps MCP servers per workspace folder,
// so its turns share one token per workspace (the newest live turn there gets
// the question); Claude gets a token per process.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::ipc::Channel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::work_agent::{emit_ev, TurnState, WorkAgentEvent};

/// Server name as the engines see it (Claude: `mcp__vicuna-mcp__ask_user`).
pub const SERVER_NAME: &str = "vicuna-mcp";
pub const TOOL_NAME: &str = "ask_user";
/// Tools this server adds to the engine's tool budget.
pub const TOOL_COUNT: usize = 1;
/// Claude's tool id, pre-approved so ask mode doesn't gate the question.
pub const CLAUDE_TOOL_ID: &str = "mcp__vicuna-mcp__ask_user";

const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 4 * 1024 * 1024;
const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// How often a waiting `ask_user` call sends a progress heartbeat on its SSE
/// stream. Clients' idle timers (Claude: 5 min for HTTP servers, reset on
/// progress) must never fire while the user is still typing.
const HEARTBEAT: Duration = if cfg!(test) { Duration::from_millis(100) } else { Duration::from_secs(15) };
/// Per-call timeout written into the engines' config for vicuna-mcp (Claude
/// `timeout` ms, Codex `tool_timeout_sec`): a question waits for the user, so
/// the engines' 60 s defaults would cut a slow answer off mid-typing.
pub const ASK_TIMEOUT_SECS: u64 = 24 * 60 * 60;

const INSTRUCTIONS: &str = "Vicuna shows questions to the user as a card in the chat. \
When you need a decision, a preference or missing information from the user, call ask_user \
(with short choices when there are a few likely answers) instead of asking in plain text or \
using a built-in question tool; it waits for the answer and returns it.";

#[derive(Clone)]
struct Target {
    seq: u64,
    turn: Arc<TurnState>,
    on_event: Channel<WorkAgentEvent>,
}

#[derive(Default)]
struct Registry {
    /// route key (e.g. "opencode|<workspace>") -> its token
    tokens: HashMap<String, String>,
    /// token -> live turns, oldest first (the newest answers)
    targets: HashMap<String, Vec<Target>>,
}

type SharedRegistry = Arc<Mutex<Registry>>;

struct Server {
    port: u16,
    registry: SharedRegistry,
}

static SERVER: std::sync::OnceLock<Result<Server, String>> = std::sync::OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(1);

/// The process-wide server, started on first use on its own thread and
/// runtime (so it outlives whichever runtime asked first).
fn server() -> Result<&'static Server, String> {
    SERVER.get_or_init(start).as_ref().map_err(Clone::clone)
}

fn start() -> Result<Server, String> {
    let fail = |e: std::io::Error| format!("vicuna-mcp: cannot listen on 127.0.0.1: {e}");
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(fail)?;
    std_listener.set_nonblocking(true).map_err(fail)?;
    let port = std_listener.local_addr().map_err(fail)?.port();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("vicuna-mcp")
        .enable_all()
        .build()
        .map_err(fail)?;
    let registry: SharedRegistry = Arc::default();
    let reg = registry.clone();
    std::thread::Builder::new()
        .name("vicuna-mcp".into())
        .spawn(move || {
            rt.block_on(async move {
                match TcpListener::from_std(std_listener) {
                    Ok(l) => serve(l, reg).await,
                    Err(e) => eprintln!("vicuna-mcp: {e}"),
                }
            })
        })
        .map_err(fail)?;
    Ok(Server { port, registry })
}

fn random_token() -> String {
    let mut b = [0u8; 16];
    if getrandom::fill(&mut b).is_err() {
        // never expected; still unguessable enough for a loopback route
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        b = (t ^ (SEQ.fetch_add(1, Ordering::Relaxed) as u128).rotate_left(64)).to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// A turn's place on the server; dropping it unregisters the turn.
pub struct AskRegistration {
    registry: SharedRegistry,
    token: String,
    seq: u64,
    pub url: String,
}

impl Drop for AskRegistration {
    fn drop(&mut self) {
        if let Ok(mut r) = self.registry.lock() {
            if let Some(v) = r.targets.get_mut(&self.token) {
                v.retain(|t| t.seq != self.seq);
                if v.is_empty() {
                    r.targets.remove(&self.token);
                }
            }
        }
    }
}

/// Register a running turn under `route` (None = a route of its own) and get
/// the URL the engine should use. Err only when the loopback server could not
/// start; the turn then simply runs without ask_user.
pub async fn register(
    route: Option<&str>,
    turn: Arc<TurnState>,
    on_event: Channel<WorkAgentEvent>,
) -> Result<AskRegistration, String> {
    let srv = server()?;
    Ok(register_on(&srv.registry, srv.port, route, turn, on_event))
}

fn register_on(
    registry: &SharedRegistry,
    port: u16,
    route: Option<&str>,
    turn: Arc<TurnState>,
    on_event: Channel<WorkAgentEvent>,
) -> AskRegistration {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut r = registry.lock().unwrap_or_else(|p| p.into_inner());
    let token = match route {
        Some(k) => r.tokens.entry(k.to_string()).or_insert_with(random_token).clone(),
        None => random_token(),
    };
    r.targets.entry(token.clone()).or_default().push(Target { seq, turn, on_event });
    AskRegistration {
        registry: registry.clone(),
        url: format!("http://127.0.0.1:{port}/mcp/{token}"),
        token,
        seq,
    }
}

fn target_for(registry: &SharedRegistry, token: &str) -> Option<Option<Target>> {
    let r = registry.lock().ok()?;
    if let Some(v) = r.targets.get(token) {
        return Some(v.last().cloned());
    }
    // a known route with no live turn right now
    r.tokens.values().any(|t| t == token).then_some(None)
}

/// ACP-shaped MCP entry for the server (opencode's `mcpServers`; Claude's
/// `--mcp-config` is converted from the same shape).
pub fn server_entry(url: &str) -> Value {
    json!({ "type": "http", "name": SERVER_NAME, "url": url, "headers": [] })
}

/// The turn's MCP list with vicuna-mcp first (a user server of the same name
/// is replaced).
pub fn with_ask_server(mcp: Vec<Value>, url: Option<&str>) -> Vec<Value> {
    let Some(url) = url else { return mcp };
    let mut out = vec![server_entry(url)];
    out.extend(mcp.into_iter().filter(|e| e.get("name").and_then(|n| n.as_str()) != Some(SERVER_NAME)));
    out
}

fn tool_schema() -> Value {
    json!({
        "name": TOOL_NAME,
        "title": "Ask the user",
        "description": "Ask the user a question in Vicuna's chat and wait for the answer. Use this whenever you need a decision, a preference or missing information from the user mid-task, instead of asking in plain text and ending your turn, and instead of any built-in question tool. Offer `choices` when there are a few likely answers (the user can always type their own). Set `multiple` to let the user pick several choices. Returns the user's answer.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "question": { "type": "string", "description": "The question, one or two sentences." },
                "choices": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional answer choices shown as buttons (2-6 short labels)."
                },
                "multiple": { "type": "boolean", "description": "Let the user select more than one choice (default false)." }
            },
            "required": ["question"]
        }
    })
}

fn text_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

/// Parsed ask_user arguments: (question, choices, multiple).
pub fn parse_ask_args(args: &Value) -> Result<(String, Vec<String>, bool), String> {
    let question = args
        .get("question")
        .and_then(|q| q.as_str())
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .ok_or_else(|| "ask_user needs a non-empty `question`".to_string())?
        .to_string();
    // accept `options` too, and {label} objects (opencode/Claude habits)
    let raw = args.get("choices").or_else(|| args.get("options"));
    let mut choices: Vec<String> = Vec::new();
    for c in raw.and_then(|c| c.as_array()).into_iter().flatten() {
        let label = c.as_str().or_else(|| c.get("label").and_then(|l| l.as_str())).map(str::trim);
        if let Some(l) = label.filter(|l| !l.is_empty()) {
            if !choices.iter().any(|x| x == l) {
                choices.push(l.to_string());
            }
        }
    }
    let multiple = ["multiple", "multiSelect", "multi_select"]
        .iter()
        .find_map(|k| args.get(*k).and_then(|v| v.as_bool()))
        .unwrap_or(false);
    let multiple = multiple && !choices.is_empty();
    Ok((question, choices, multiple))
}

/// The ask card's answer: plain text, or (when the user pasted images) the
/// envelope `{"vicunaAskAnswer": {"text": "...", "images": ["data:image/...;base64,..."]}}`.
#[derive(Debug, Default, PartialEq)]
pub struct AskAnswer {
    pub text: String,
    /// (mime type, base64 data)
    pub images: Vec<(String, String)>,
}

pub fn parse_ask_answer(raw: &str) -> AskAnswer {
    let env = serde_json::from_str::<Value>(raw.trim()).ok();
    let Some(inner) = env.as_ref().and_then(|v| v.get("vicunaAskAnswer")) else {
        return AskAnswer { text: raw.to_string(), images: Vec::new() };
    };
    let text = inner.get("text").and_then(Value::as_str).unwrap_or("").to_string();
    let images = inner
        .get("images")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| i.as_str())
        .filter_map(|url| {
            let rest = url.strip_prefix("data:")?;
            let (mime, data) = rest.split_once(";base64,")?;
            let data: String = data.chars().filter(|c| !c.is_whitespace()).collect();
            (mime.starts_with("image/") && !data.is_empty()).then(|| (mime.to_string(), data))
        })
        .collect();
    AskAnswer { text, images }
}

/// The tool result text for the user's answer. Multi-select answers arrive as
/// a JSON array of labels; a dismissal arrives as the UI's own sentence.
pub fn format_answer(answer: &str, multiple: bool) -> String {
    let a = answer.trim();
    if a.starts_with("User dismissed the question") {
        return a.to_string();
    }
    if multiple {
        if let Ok(list) = serde_json::from_str::<Vec<String>>(a) {
            return if list.is_empty() {
                "The user selected none of the choices.".to_string()
            } else {
                format!("The user selected: {}", list.join("; "))
            };
        }
    }
    format!("The user answered: {a}")
}

/// Raise the ask card on the turn and wait for the answer (or Stop).
async fn ask(target: &Target, args: &Value, client_gone: impl std::future::Future<Output = ()>) -> Value {
    let (question, choices, multiple) = match parse_ask_args(args) {
        Ok(v) => v,
        Err(e) => return text_result(&e, true),
    };
    let turn = &target.turn;
    if turn.stop.load(Ordering::Relaxed) {
        return text_result("The user stopped the turn; no question was shown.", true);
    }
    let ask_id = format!("ask-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(ask_id.clone(), tx);
    }
    emit_ev(
        &target.on_event,
        &turn.stop,
        WorkAgentEvent::AskRequest { ask_id: ask_id.clone(), question, options: choices, multi: multiple },
    );
    let answer = tokio::select! {
        a = rx => a.ok(),
        _ = client_gone => None,
    };
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.remove(&ask_id);
    }
    match answer {
        Some(a) if !turn.stop.load(Ordering::Relaxed) => answer_result(&parse_ask_answer(&a), multiple),
        _ => text_result("The user stopped the turn before answering.", true),
    }
}

/// Tool result for an answer: the text, then any pasted images as MCP image
/// content blocks (vision models see them like a pasted picture).
fn answer_result(a: &AskAnswer, multiple: bool) -> Value {
    let n = a.images.len();
    let s = if n == 1 { "" } else { "s" };
    let text = if a.text.trim().is_empty() && n > 0 {
        format!("The user answered with {n} image{s} (attached below).")
    } else if n > 0 {
        format!("{} (The user also attached {n} image{s}, below.)", format_answer(&a.text, multiple))
    } else {
        format_answer(&a.text, multiple)
    };
    let mut content = vec![json!({ "type": "text", "text": text })];
    for (mime, data) in &a.images {
        content.push(json!({ "type": "image", "data": data, "mimeType": mime }));
    }
    json!({ "content": content, "isError": false })
}

/// One JSON-RPC message -> its response (None for notifications/responses).
async fn handle_rpc(
    msg: &Value,
    target: Option<&Target>,
    client_gone: impl std::future::Future<Output = ()>,
) -> Option<Value> {
    let id = msg.get("id").cloned().filter(|i| !i.is_null())?;
    let method = msg.get("method").and_then(|m| m.as_str())?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("");
            let version = PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(PROTOCOL_VERSIONS[1]);
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": SERVER_NAME, "title": "Vicuna", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [tool_schema()] }),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            if name != TOOL_NAME {
                return Some(rpc_error(id, -32602, &format!("unknown tool: {name}")));
            }
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match target {
                Some(t) => ask(t, &args, client_gone).await,
                None => text_result("No Vicuna Work turn is running for this chat, so the question cannot be shown.", true),
            }
        }
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        _ => return Some(rpc_error(id, -32601, &format!("method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

fn rpc_error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

// ---------- minimal HTTP/1.1 ----------

async fn serve(listener: TcpListener, registry: SharedRegistry) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        let registry = registry.clone();
        tokio::spawn(async move {
            let _ = handle_conn(stream, registry).await;
        });
    }
}

struct Request {
    method: String,
    path: String,
    accept: String,
    body: Vec<u8>,
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn decode_chunked(mut data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let eol = find(data, b"\r\n")?;
        let size_txt = std::str::from_utf8(&data[..eol]).ok()?;
        let size = usize::from_str_radix(size_txt.split(';').next()?.trim(), 16).ok()?;
        data = &data[eol + 2..];
        if size == 0 {
            return Some(out);
        }
        if data.len() < size + 2 {
            return None;
        }
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

/// Read one request (Content-Length or chunked body).
async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER {
            return None;
        }
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..n]);
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split_whitespace();
    let method = first.next()?.to_string();
    let path = first.next()?.to_string();
    let mut content_length = 0usize;
    let mut chunked = false;
    let mut accept = String::new();
    for l in lines {
        let Some((k, v)) = l.split_once(':') else { continue };
        let (k, v) = (k.trim().to_ascii_lowercase(), v.trim());
        if k == "accept" {
            accept = v.to_ascii_lowercase();
        } else if k == "content-length" {
            content_length = v.parse().ok()?;
        } else if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
            chunked = true;
        }
    }
    let mut body = buf[head_end + 4..].to_vec();
    if chunked {
        loop {
            if let Some(b) = decode_chunked(&body) {
                return Some(Request { method, path, accept, body: b });
            }
            if body.len() > MAX_BODY {
                return None;
            }
            let n = stream.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            body.extend_from_slice(&tmp[..n]);
        }
    }
    if content_length > MAX_BODY {
        return None;
    }
    while body.len() < content_length {
        let n = stream.read(&mut tmp).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);
    Some(Request { method, path, accept, body })
}

async fn respond(stream: &mut TcpStream, status: &str, body: Option<&Value>, extra: &str) -> std::io::Result<()> {
    let payload = body.map(|b| b.to_string()).unwrap_or_default();
    let ctype = if body.is_some() { "Content-Type: application/json\r\n" } else { "" };
    let head = format!(
        "HTTP/1.1 {status}\r\n{ctype}{extra}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(payload.as_bytes()).await?;
    stream.flush().await
}

async fn handle_conn(mut stream: TcpStream, registry: SharedRegistry) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream).await else {
        return respond(&mut stream, "400 Bad Request", None, "").await;
    };
    let token = req.path.split('?').next().unwrap_or("").strip_prefix("/mcp/").unwrap_or("").to_string();
    let Some(target) = (!token.is_empty()).then(|| target_for(&registry, &token)).flatten() else {
        return respond(&mut stream, "404 Not Found", None, "").await;
    };
    match req.method.as_str() {
        "POST" => {}
        "DELETE" => return respond(&mut stream, "200 OK", None, "").await,
        // no server-initiated stream: plain JSON responses only
        _ => return respond(&mut stream, "405 Method Not Allowed", None, "Allow: POST, DELETE\r\n").await,
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&req.body) else {
        let err = rpc_error(Value::Null, -32700, "parse error");
        return respond(&mut stream, "400 Bad Request", Some(&err), "").await;
    };
    // A waiting ask ends early when the engine drops the request (its own
    // cancel): the client closes the socket and the read returns 0/err.
    let (read_half, mut write_half) = stream.into_split();
    let gone = async move {
        let mut b = [0u8; 256];
        let mut r = read_half;
        loop {
            match r.read(&mut b).await {
                Ok(0) | Err(_) => return,
                Ok(_) => continue,
            }
        }
    };
    // A question waits for the user, possibly for many minutes. Answer it as
    // an SSE stream: headers go out now (Claude aborts a POST whose headers
    // take over 60 s with "The operation timed out."), then progress
    // heartbeats keep idle timers from firing, then the result.
    let is_call = msg.get("method").and_then(Value::as_str) == Some("tools/call")
        && msg.get("id").is_some_and(|i| !i.is_null());
    if is_call && req.accept.contains("text/event-stream") {
        return stream_call(&msg, target.as_ref(), gone, &mut write_half).await;
    }
    let reply = match &msg {
        Value::Array(batch) => {
            let gone = futures_util::future::FutureExt::shared(Box::pin(gone));
            let mut out = Vec::new();
            for m in batch {
                if let Some(r) = handle_rpc(m, target.as_ref(), gone.clone()).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => handle_rpc(m, target.as_ref(), gone).await,
    };
    let (status, payload) = match &reply {
        Some(v) => ("200 OK", v.to_string()),
        None => ("202 Accepted", String::new()),
    };
    let ctype = if reply.is_some() { "Content-Type: application/json\r\n" } else { "" };
    let head = format!(
        "HTTP/1.1 {status}\r\n{ctype}Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    write_half.write_all(head.as_bytes()).await?;
    write_half.write_all(payload.as_bytes()).await?;
    write_half.flush().await
}

fn sse_event(v: &Value) -> String {
    format!("event: message\ndata: {v}\n\n")
}

/// Run one tools/call and stream it as SSE: progress notifications every
/// HEARTBEAT (a comment line when the client sent no progressToken), then the
/// JSON-RPC response, then close.
async fn stream_call(
    msg: &Value,
    target: Option<&Target>,
    gone: impl std::future::Future<Output = ()>,
    out: &mut tokio::net::tcp::OwnedWriteHalf,
) -> std::io::Result<()> {
    let head = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n";
    out.write_all(head.as_bytes()).await?;
    out.flush().await?;
    let token = msg.pointer("/params/_meta/progressToken").cloned().filter(|t| !t.is_null());
    let call = handle_rpc(msg, target, gone);
    tokio::pin!(call);
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + HEARTBEAT, HEARTBEAT);
    let mut beats: u64 = 0;
    let reply = loop {
        tokio::select! {
            r = &mut call => break r,
            _ = tick.tick() => {
                beats += 1;
                let frame = match &token {
                    Some(t) => sse_event(&json!({
                        "jsonrpc": "2.0",
                        "method": "notifications/progress",
                        "params": { "progressToken": t, "progress": beats, "message": "Waiting for the user's answer" }
                    })),
                    None => ": waiting for the user\n\n".to_string(),
                };
                // a failed write means the client left; the read half's EOF
                // (the `gone` future) ends the ask itself
                let _ = out.write_all(frame.as_bytes()).await;
                let _ = out.flush().await;
            }
        }
    };
    if let Some(r) = reply {
        out.write_all(sse_event(&r).as_bytes()).await?;
    }
    out.flush().await?;
    out.shutdown().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::ipc::InvokeResponseBody;

    fn capture() -> (Channel<WorkAgentEvent>, Arc<Mutex<Vec<Value>>>) {
        let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
        let s = seen.clone();
        let ch = Channel::<WorkAgentEvent>::new(move |body| {
            if let InvokeResponseBody::Json(j) = body {
                s.lock().unwrap().push(serde_json::from_str(&j).unwrap());
            }
            Ok(())
        });
        (ch, seen)
    }

    async fn post(url: &str, body: Value) -> (u16, Option<Value>) {
        let r = reqwest::Client::new()
            .post(url)
            .header("accept", "application/json, text/event-stream")
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        let sse = r.headers().get("content-type").is_some_and(|c| c.to_str().unwrap().starts_with("text/event-stream"));
        let text = r.text().await.unwrap();
        if sse {
            // the response is the last event carrying an id
            let resp = sse_messages(&text).into_iter().rev().find(|m| m.get("id").is_some());
            return (status, resp);
        }
        (status, (!text.is_empty()).then(|| serde_json::from_str(&text).unwrap()))
    }

    fn sse_messages(text: &str) -> Vec<Value> {
        text.lines().filter_map(|l| l.strip_prefix("data: ")).map(|d| serde_json::from_str(d).unwrap()).collect()
    }

    #[tokio::test]
    async fn slow_answer_streams_headers_first_then_heartbeats() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let mut body = call("Take your time?", json!({}));
        body["params"]["_meta"] = json!({ "progressToken": 42 });
        // headers must arrive while the question is still unanswered (Claude
        // aborts a POST whose headers take over 60 s)
        let resp = tokio::time::timeout(
            Duration::from_secs(5),
            reqwest::Client::new().post(&reg.url).header("accept", "application/json, text/event-stream").json(&body).send(),
        )
        .await
        .expect("headers before the answer")
        .unwrap();
        assert_eq!(resp.headers()["content-type"], "text/event-stream");
        let ev = wait_ask(&seen).await;
        tokio::time::sleep(HEARTBEAT * 3 + Duration::from_millis(50)).await;
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), "Later".into()).unwrap();
        let msgs = sse_messages(&resp.text().await.unwrap());
        let progress: Vec<&Value> = msgs.iter().filter(|m| m["method"] == "notifications/progress").collect();
        assert!(progress.len() >= 2, "heartbeats while waiting: {msgs:?}");
        assert_eq!(progress[0]["params"]["progressToken"], 42);
        assert!(progress[1]["params"]["progress"].as_u64() > progress[0]["params"]["progress"].as_u64());
        let last = msgs.last().unwrap();
        assert_eq!(last["id"], 7);
        assert_eq!(last["result"]["content"][0]["text"], "The user answered: Later");
    }

    #[tokio::test]
    async fn json_only_clients_still_get_plain_json() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move {
            let r = reqwest::Client::new().post(&url).header("accept", "application/json").json(&call("Q?", json!({}))).send().await.unwrap();
            (r.headers()["content-type"].to_str().unwrap().to_string(), r.json::<Value>().await.unwrap())
        });
        let ev = wait_ask(&seen).await;
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), "ok".into()).unwrap();
        let (ctype, r) = pending.await.unwrap();
        assert_eq!(ctype, "application/json");
        assert_eq!(r["result"]["content"][0]["text"], "The user answered: ok");
    }

    #[tokio::test]
    async fn pasted_images_come_back_as_image_blocks() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, call("Show me?", json!({ "choices": ["A", "B"] }))).await });
        let ev = wait_ask(&seen).await;
        let answer = json!({ "vicunaAskAnswer": { "text": "A, but like this", "images": ["data:image/jpeg;base64,QUJD\n", "not-a-data-url"] } });
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), answer.to_string()).unwrap();
        let r = pending.await.unwrap().1.unwrap();
        let content = r["result"]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["text"], "The user answered: A, but like this (The user also attached 1 image, below.)");
        assert_eq!(content[1], json!({ "type": "image", "data": "QUJD", "mimeType": "image/jpeg" }));
    }

    #[test]
    fn answer_envelope_parses_and_plain_text_passes_through() {
        assert_eq!(parse_ask_answer("plain"), AskAnswer { text: "plain".into(), images: vec![] });
        // a multi-select array is not an envelope
        assert_eq!(parse_ask_answer(r#"["A"]"#).text, r#"["A"]"#);
        let a = parse_ask_answer(r#"{"vicunaAskAnswer":{"text":"","images":["data:image/png;base64,eA=="]}}"#);
        assert_eq!(a.images, vec![("image/png".to_string(), "eA==".to_string())]);
        let r = answer_result(&a, false);
        assert_eq!(r["content"][0]["text"], "The user answered with 1 image (attached below).");
    }

    fn call(question: &str, extra: Value) -> Value {
        let mut args = json!({ "question": question });
        if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
            a.extend(e.clone());
        }
        json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": "ask_user", "arguments": args } })
    }

    async fn wait_ask(seen: &Arc<Mutex<Vec<Value>>>) -> Value {
        for _ in 0..200 {
            if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == "askrequest").cloned() {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no askrequest event");
    }

    #[tokio::test]
    async fn handshake_lists_ask_user() {
        let (ch, _) = capture();
        let reg = register(None, Arc::new(TurnState::new()), ch).await.unwrap();
        let (s, r) = post(&reg.url, json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2025-06-18" } })).await;
        let r = r.unwrap();
        assert_eq!(s, 200);
        assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(r["result"]["serverInfo"]["name"], SERVER_NAME);
        let (s, r) = post(&reg.url, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await;
        assert_eq!((s, r), (202, None));
        let (_, r) = post(&reg.url, json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" })).await;
        let tools = r.unwrap()["result"]["tools"].clone();
        assert_eq!(tools.as_array().unwrap().len(), TOOL_COUNT);
        assert_eq!(tools[0]["name"], TOOL_NAME);
        assert_eq!(tools[0]["inputSchema"]["required"], json!(["question"]));
    }

    #[tokio::test]
    async fn unknown_token_is_404_and_get_is_405() {
        let (ch, _) = capture();
        let reg = register(None, Arc::new(TurnState::new()), ch).await.unwrap();
        let bad = format!("{}x", reg.url);
        let (s, _) = post(&bad, json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).await;
        assert_eq!(s, 404);
        let r = reqwest::get(&reg.url).await.unwrap();
        assert_eq!(r.status().as_u16(), 405);
    }

    #[tokio::test]
    async fn call_shows_card_and_returns_the_answer() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, call("Which color?", json!({ "choices": ["Red", "Green", "Blue"] }))).await });
        let ev = wait_ask(&seen).await;
        assert_eq!(ev["question"], "Which color?");
        assert_eq!(ev["options"], json!(["Red", "Green", "Blue"]));
        assert_eq!(ev["multi"], false);
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), "Blue".into()).unwrap();
        let (s, r) = pending.await.unwrap();
        let r = r.unwrap();
        assert_eq!(s, 200);
        assert_eq!(r["id"], 7);
        assert_eq!(r["result"]["isError"], false);
        assert_eq!(r["result"]["content"][0]["text"], "The user answered: Blue");
        assert!(turn.ask_pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn multi_select_answer_is_a_list() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, call("Toppings?", json!({ "choices": ["Ham", "Olives", "Basil"], "multiple": true }))).await });
        let ev = wait_ask(&seen).await;
        assert_eq!(ev["multi"], true);
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), r#"["Ham","Basil"]"#.into()).unwrap();
        let r = pending.await.unwrap().1.unwrap();
        assert_eq!(r["result"]["content"][0]["text"], "The user selected: Ham; Basil");
    }

    #[tokio::test]
    async fn stop_while_pending_cancels_the_call() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, call("Proceed?", json!({}))).await });
        wait_ask(&seen).await;
        crate::work_agent::agent_stop(&turn);
        let r = pending.await.unwrap().1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("stopped"));
        // a stopped turn shows no new card
        let r = post(&reg.url, call("Again?", json!({}))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert_eq!(seen.lock().unwrap().iter().filter(|e| e["type"] == "askrequest").count(), 1);
    }

    #[tokio::test]
    async fn shared_route_goes_to_the_newest_live_turn() {
        let (ch1, seen1) = capture();
        let (ch2, seen2) = capture();
        let (t1, t2) = (Arc::new(TurnState::new()), Arc::new(TurnState::new()));
        let key = format!("opencode|/ws-{}", random_token());
        let r1 = register(Some(&key), t1.clone(), ch1).await.unwrap();
        let r2 = register(Some(&key), t2.clone(), ch2).await.unwrap();
        assert_eq!(r1.url, r2.url, "one URL per workspace route");
        let url = r1.url.clone();
        let url_again = r1.url.clone();
        drop(r2); // the newest turn ends: the older one answers again
        let pending = tokio::spawn(async move { post(&url, call("Q?", json!({}))).await });
        let ev = wait_ask(&seen1).await;
        assert!(seen2.lock().unwrap().is_empty());
        crate::work_agent::agent_answer(&t1, ev["ask_id"].as_str().unwrap(), "yes".into()).unwrap();
        assert_eq!(pending.await.unwrap().1.unwrap()["result"]["content"][0]["text"], "The user answered: yes");
        drop(r1);
        // route known, nobody listening: a tool error, not a hang
        let r = post(&url_again, call("Q?", json!({}))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], true);
    }

    #[test]
    fn ask_args_accept_engine_habits() {
        let (q, c, m) = parse_ask_args(&json!({ "question": " Pick ", "options": [{ "label": "A" }, "B", "B", ""], "multiSelect": true })).unwrap();
        assert_eq!((q.as_str(), c, m), ("Pick", vec!["A".to_string(), "B".to_string()], true));
        // multiple without choices is meaningless
        assert!(!parse_ask_args(&json!({ "question": "Name?", "multiple": true })).unwrap().2);
        assert!(parse_ask_args(&json!({ "question": "  " })).is_err());
    }

    #[test]
    fn answers_format_for_the_model() {
        assert_eq!(format_answer("Blue", false), "The user answered: Blue");
        assert_eq!(format_answer(r#"["A","B"]"#, true), "The user selected: A; B");
        assert_eq!(format_answer("[]", true), "The user selected none of the choices.");
        assert_eq!(format_answer("something else", true), "The user answered: something else");
        assert!(format_answer("User dismissed the question without answering.", false).starts_with("User dismissed"));
    }

    #[test]
    fn ask_server_goes_first_and_replaces_a_namesake() {
        let user = vec![json!({ "name": "vicuna-mcp", "command": "x" }), json!({ "name": "fs", "command": "y" })];
        let out = with_ask_server(user.clone(), Some("http://127.0.0.1:1/mcp/t"));
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], server_entry("http://127.0.0.1:1/mcp/t"));
        assert_eq!(out[1]["name"], "fs");
        assert_eq!(with_ask_server(user.clone(), None), user);
    }

    #[test]
    fn chunked_bodies_decode() {
        assert_eq!(decode_chunked(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\n\r\n").unwrap(), b"Wikipedia");
        assert!(decode_chunked(b"4\r\nWi").is_none());
    }
}
