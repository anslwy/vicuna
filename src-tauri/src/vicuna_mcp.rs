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
use sqlx::Row;
use crate::ipc::Channel;
#[cfg(feature = "desktop")]
use tauri::Manager;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

use crate::work_agent::{emit_ev, TurnState, WorkAgentEvent};

/// Server name as the engines see it (Claude: `mcp__vicuna-mcp__ask_user`).
pub const SERVER_NAME: &str = "vicuna-mcp";
pub const TOOL_NAME: &str = "ask_user";
pub const GET_SESSION_TOOL: &str = "get_session_id";
pub const READ_SESSION_TOOL: &str = "read_session";
/// One inline image or uploaded file of a session, on demand.
pub const READ_ATTACHMENT_TOOL: &str = "read_attachment";
pub const LIST_SESSIONS_TOOL: &str = "list_sessions";
pub const MANAGE_MCP_TOOL: &str = "manage_mcp";
/// List/read/add/edit/enable/disable/remove Vicuna skills (~/vicuna/skills/<slug>/SKILL.md).
pub const MANAGE_SKILLS_TOOL: &str = "manage_skills";
/// The agent schedules a message to itself (a wake-up) in its own session.
pub const SCHEDULE_TOOL: &str = "schedule_wakeup";
/// Lead sessions create/start/stop/remove other sessions (refused elsewhere).
pub const MANAGE_SESSIONS_TOOL: &str = "manage_sessions";
/// List/create/update/cancel Scheduled items (own session; leads: any).
pub const MANAGE_SCHEDULE_TOOL: &str = "manage_schedule";
pub const OVERVIEW_TOOL: &str = "session_overview";
pub const SEARCH_TOOL: &str = "search_sessions";
/// Read each Work harness's remaining subscription quota (read-only, cached).
pub const USAGE_TOOL: &str = "usage_status";
/// Show an image file from disk in the assistant's reply (and on paired bridges).
pub const SHOW_IMAGE_TOOL: &str = "show_image";
/// Tools this server adds to the engine's tool budget.
pub const TOOL_COUNT: usize = 14;
/// Claude's tool ids, pre-approved: ask_user is itself the question, and
/// session/mcp tools are explicit.
pub const CLAUDE_TOOL_ID: &str = "mcp__vicuna-mcp__ask_user";
pub const CLAUDE_GET_SESSION_TOOL_ID: &str = "mcp__vicuna-mcp__get_session_id";
pub const CLAUDE_READ_SESSION_TOOL_ID: &str = "mcp__vicuna-mcp__read_session";
pub const CLAUDE_READ_ATTACHMENT_TOOL_ID: &str = "mcp__vicuna-mcp__read_attachment";
pub const CLAUDE_LIST_SESSIONS_TOOL_ID: &str = "mcp__vicuna-mcp__list_sessions";
pub const CLAUDE_MANAGE_MCP_TOOL_ID: &str = "mcp__vicuna-mcp__manage_mcp";
pub const CLAUDE_MANAGE_SKILLS_TOOL_ID: &str = "mcp__vicuna-mcp__manage_skills";
pub const CLAUDE_SCHEDULE_TOOL_ID: &str = "mcp__vicuna-mcp__schedule_wakeup";
pub const CLAUDE_MANAGE_SESSIONS_TOOL_ID: &str = "mcp__vicuna-mcp__manage_sessions";
pub const CLAUDE_MANAGE_SCHEDULE_TOOL_ID: &str = "mcp__vicuna-mcp__manage_schedule";
pub const CLAUDE_OVERVIEW_TOOL_ID: &str = "mcp__vicuna-mcp__session_overview";
pub const CLAUDE_SEARCH_TOOL_ID: &str = "mcp__vicuna-mcp__search_sessions";
pub const CLAUDE_USAGE_TOOL_ID: &str = "mcp__vicuna-mcp__usage_status";
pub const CLAUDE_SHOW_IMAGE_TOOL_ID: &str = "mcp__vicuna-mcp__show_image";

/// The ask card's "Answer later" sentinel: the waiting ask_user call returns
/// at once and the answer reaches the session later as a message.
pub const ASK_LATER: &str = "{\"vicunaAskLater\":true}";

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

/// agy's MCP call timeout is a fixed 3 minutes (measured: agy 1.5.12 fails the
/// call at 3:00 and ends the turn), so its waiting ask_user calls return a
/// "still waiting" poll well before that.
pub const AGY_ASK_POLL_SECS: u64 = 150;

const INSTRUCTIONS: &str = "Vicuna shows questions to the user as a card in the chat. \
When you need a decision, a preference or missing information from the user, call ask_user \
(with short choices when there are a few likely answers) instead of asking in plain text or \
using a built-in question tool; it waits for the answer and returns it. Run commands with your own shell tool \
(use its background mode, where it has one, for servers and other long-running jobs). Use `get_session_id`, `read_session` (paginated; `plain: true` collapses tool cards; messages list image metadata and the session its `uploads`), `read_attachment` (one listed image or uploaded file, on demand), `show_image` (display an image file you generated or edited in the chat; it also reaches the user's Telegram/Discord/Slack), `session_overview` (what the most recently active sessions are doing), `search_sessions` (full-text search), \
and `list_sessions` to inspect the current Vicuna session or earlier conversation history if the workspace/harness \
changed or earlier messages were truncated. Use `manage_mcp` to list, add, edit, enable, disable, or remove \
custom MCP servers in Vicuna (except the fixed `vicuna-mcp` server). Use `manage_skills` to list, search, read, add, edit, \
enable, disable, or remove Vicuna skills (`~/vicuna/skills/<slug>/SKILL.md`). Search takes `query`. Switches are per Work session; the lead can pass `session_id` to target another session. Use `schedule_wakeup` to have Vicuna send you a \
message in this session later (check back on slow work, follow up at a time the user names); `manage_schedule` lists, reschedules or cancels scheduled items \
(your own session's; the lead can manage every session's). In the user's lead session, \
`manage_sessions` creates, messages, stops and removes worker sessions (each on its own harness/model); a worker's reply \
reaches the lead as a message when its turn ends. `usage_status` reads each installed harness's remaining subscription \
quota and reset time (cached ~60 s). Check it before starting an expensive worker session so you pick a harness with \
headroom, and after a harness call fails with a limit/usage error so you can explain it (e.g. \"Claude is at its limit \
until 18:00\") or switch harnesses.";

#[cfg(feature = "desktop")]
static APP_HANDLE: std::sync::OnceLock<tauri::AppHandle> = std::sync::OnceLock::new();

#[cfg(feature = "desktop")]
pub fn set_app_handle(app: tauri::AppHandle) {
    let _ = APP_HANDLE.set(app);
}

/// Headless server (`vicuna serve`): its own vicuna.db, since there is
/// no app handle / sql plugin there. Wins over the plugin pool when set.
static DB_OVERRIDE: std::sync::OnceLock<sqlx::SqlitePool> = std::sync::OnceLock::new();

pub fn set_db_pool_override(pool: sqlx::SqlitePool) {
    let _ = DB_OVERRIDE.set(pool);
}

async fn db_pool() -> Result<sqlx::SqlitePool, String> {
    if let Some(p) = DB_OVERRIDE.get() {
        return Ok(p.clone());
    }
    db_pool_app().await
}

#[cfg(not(feature = "desktop"))]
async fn db_pool_app() -> Result<sqlx::SqlitePool, String> {
    Err("vicuna.db is not available (headless server without a database)".to_string())
}

#[cfg(feature = "desktop")]
async fn db_pool_app() -> Result<sqlx::SqlitePool, String> {
    let app = APP_HANDLE
        .get()
        .ok_or_else(|| "vicuna.db is not available (app handle not initialized)".to_string())?;
    let instances = app
        .try_state::<tauri_plugin_sql::DbInstances>()
        .ok_or_else(|| "vicuna.db is not available (DbInstances not loaded)".to_string())?;
    let guard = instances.0.read().await;
    match guard.get("sqlite:vicuna.db") {
        Some(tauri_plugin_sql::DbPool::Sqlite(pool)) => Ok(pool.clone()),
        None => Err("vicuna.db is not loaded".to_string()),
    }
}

#[derive(Clone)]
struct Target {
    seq: u64,
    /// the turn's workspace
    workspace: String,
    /// Vicuna chat session id (if known)
    session_id: Option<String>,
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
    workspace: &str,
    session_id: Option<&str>,
    turn: Arc<TurnState>,
    on_event: Channel<WorkAgentEvent>,
) -> Result<AskRegistration, String> {
    let srv = server()?;
    Ok(register_on(&srv.registry, srv.port, route, workspace, session_id, turn, on_event))
}

fn register_on(
    registry: &SharedRegistry,
    port: u16,
    route: Option<&str>,
    workspace: &str,
    session_id: Option<&str>,
    turn: Arc<TurnState>,
    on_event: Channel<WorkAgentEvent>,
) -> AskRegistration {
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let mut r = registry.lock().unwrap_or_else(|p| p.into_inner());
    let token = match route {
        Some(k) => r.tokens.entry(k.to_string()).or_insert_with(random_token).clone(),
        None => random_token(),
    };
    r.targets.entry(token.clone()).or_default().push(Target {
        seq,
        workspace: workspace.to_string(),
        session_id: session_id.map(str::to_string),
        turn,
        on_event,
    });
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

/// Running / asking state of every session with a live turn on this process.
fn live_sessions() -> crate::session_insight::LiveMap {
    match SERVER.get() {
        Some(Ok(srv)) => live_from(&srv.registry),
        _ => Default::default(),
    }
}

fn live_from(registry: &SharedRegistry) -> crate::session_insight::LiveMap {
    let mut m = crate::session_insight::LiveMap::new();
    let Ok(r) = registry.lock() else { return m };
    for t in r.targets.values().flatten() {
        let Some(sid) = t.session_id.clone() else { continue };
        if t.turn.done.load(Ordering::Relaxed) {
            continue;
        }
        let asking = t.turn.ask_pending.lock().map(|p| p.keys().any(|k| k.starts_with("ask-") || k.starts_with("perm-"))).unwrap_or(false)
            || t.turn.ask_parked.lock().map(|p| !p.is_empty()).unwrap_or(false);
        let e = m.entry(sid).or_default();
        e.running = true;
        e.asking |= asking;
    }
    m
}

/// Optional argument on every session-scoped vicuna-mcp tool: the Vicuna
/// session the calling agent belongs to (it is in the workspace rules).
pub(crate) const CALLER_ARG: &str = "caller_session_id";

/// The calling session named in a `tools/call`, if the agent passed one.
fn caller_session(msg: &Value) -> Option<&str> {
    (msg.get("method").and_then(Value::as_str) == Some("tools/call"))
        .then(|| msg.pointer(&format!("/params/arguments/{CALLER_ARG}")).and_then(Value::as_str))
        .flatten()
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Which turn a call belongs to. A per-workspace route (opencode/fx/agy) is
/// shared by every session in that folder, so "the newest turn" answers a
/// lead's call with a worker's turn (wrong session, or a turn that has since
/// ended → "did not answer"), and a turn registered under a different spelling
/// of the folder leaves the route empty ("No Vicuna Work turn is running").
/// When the agent names its session, the live turn of that session wins: on
/// this route first, else on any route. Skills calls use a session-bound
/// route, where a named caller must match the session on that connection.
fn target_for_call(registry: &SharedRegistry, token: &str, default: Option<Target>, msg: &Value) -> Option<Target> {
    let Some(sid) = caller_session(msg) else { return default };
    if msg.pointer("/params/name").and_then(Value::as_str) == Some(MANAGE_SKILLS_TOOL) {
        return default.filter(|target| target.session_id.as_deref() == Some(sid));
    }
    let Ok(r) = registry.lock() else { return default };
    let mine = |t: &&Target| t.session_id.as_deref() == Some(sid);
    if let Some(t) = r.targets.get(token).and_then(|v| v.iter().rev().find(mine)) {
        return Some(t.clone());
    }
    // A remote-server call is permission-gated by the target session, so a
    // caller id may never pull in a session from another route: that would
    // let any session borrow a permitted session's identity.
    if msg.pointer("/params/name").and_then(Value::as_str).is_some_and(|n| is_remote_call(n, msg.pointer("/params/arguments").unwrap_or(&Value::Null))) {
        return default;
    }
    let mut any: Vec<&Target> = r.targets.values().flatten().filter(mine).collect();
    any.sort_by_key(|t| t.seq);
    any.last().map(|t| (*t).clone()).or(default)
}

/// Adds the optional `caller_session_id` argument to a tool schema.
fn with_caller_arg(mut schema: Value) -> Value {
    if let Some(props) = schema.pointer_mut("/inputSchema/properties").and_then(Value::as_object_mut) {
        props.insert(
            CALLER_ARG.to_string(),
            json!({ "type": "string", "description": "Your own Vicuna session ID (from your instructions). Pass it on every call so Vicuna routes the call to your session when several sessions share a workspace." }),
        );
    }
    schema
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

fn schedule_schema() -> Value {
    json!({
        "name": SCHEDULE_TOOL,
        "title": "Schedule a wake-up",
        "description": "Schedule a message to yourself in this Vicuna session at a later time: Vicuna sends `message` into this chat then, which starts a new turn (or joins one already running). Use it to check back on something slow (a deploy, a CI run, a long job), to follow up later, or when the user asks you to continue at a certain time. Give exactly one of `in_minutes` (relative) or `at` (a time in the user's local timezone: `HH:MM` for the next occurrence, or `YYYY-MM-DD HH:MM`; ISO 8601 with an offset also works). The result states the exact local time. Up to 30 days ahead. It is delivered only while the user's Vicuna app is open; the user sees it under Scheduled and can cancel it. After scheduling, end your turn unless there is other work to do now.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "What to tell yourself when you wake up: enough context to pick the task back up." },
                "in_minutes": { "type": "number", "description": "Wake up this many minutes from now (e.g. 15, 90)." },
                "at": { "type": "string", "description": "Wake-up time in the user's local timezone: `HH:MM` or `YYYY-MM-DD HH:MM` (or ISO 8601 with an offset)." }
            },
            "required": ["message"]
        }
    })
}

fn manage_schedule_schema() -> Value {
    json!({
        "name": MANAGE_SCHEDULE_TOOL,
        "title": "See, change or cancel scheduled messages",
        "description": "Manage Vicuna's Scheduled items (wake-ups from `schedule_wakeup` and messages the user scheduled). Any session can manage items aimed at itself; only the user's lead session can manage other sessions' items. Actions: `list` (default: this session's pending items; `session_id` for another session, `all: true` for every session — both lead only; `include_past: true` adds missed/failed ones; each row has id, kind wakeup/send, local fire time, target session + title, who created it and a message preview), `cancel` (`id`: it will not be sent), `update` (`id` plus a new time — `in_minutes` or local `at` — and/or a new `message`), `create` (like schedule_wakeup, but `session_id` may name another session; defaults to yours). Ids may be shortened to a unique prefix. Use it when plans change, e.g. cancel a wake-up whose task the user already did.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "create", "update", "cancel"] },
                "id": { "type": "string", "description": "cancel/update: the item id from `list` (or a unique prefix)." },
                "session_id": { "type": "string", "description": "list/create: target session (default: yours; others need the lead)." },
                "all": { "type": "boolean", "description": "list: every session's items (lead only)." },
                "include_past": { "type": "boolean", "description": "list: include missed / failed items." },
                "message": { "type": "string", "description": "create: the message; update: replacement message." },
                "in_minutes": { "type": "number", "description": "create/update: fire this many minutes from now." },
                "at": { "type": "string", "description": "create/update: local time `HH:MM` or `YYYY-MM-DD HH:MM` (or ISO 8601 with an offset)." }
            },
            "required": ["action"]
        }
    })
}

/// Scheduled items live in the server's store: the server answers this
/// (no window needed). The lead flag rides along; ownership is checked there.
async fn manage_schedule(target: &Target, args: &Value, client_gone: impl std::future::Future<Output = ()>) -> Value {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !["list", "create", "update", "cancel"].contains(&action.as_str()) {
        return text_result("`action` must be one of list, create, update, cancel.", true);
    }
    if target.session_id.is_none() {
        return text_result("This turn has no Vicuna session.", true);
    }
    let turn = &target.turn;
    if turn.stop.load(Ordering::Relaxed) {
        return text_result("The user stopped the turn; nothing was done.", true);
    }
    let lead = turn.lead.load(Ordering::Relaxed);
    let req_id = format!("sched-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(req_id.clone(), tx);
    }
    emit_ev(&target.on_event, &turn.stop, WorkAgentEvent::ScheduleControl { req_id: req_id.clone(), action, args: args.clone(), lead });
    let reply = tokio::select! {
        a = rx => a.ok(),
        _ = client_gone => None,
        _ = tokio::time::sleep(Duration::from_secs(30)) => None,
    };
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.remove(&req_id);
    }
    let Some(reply) = reply else {
        return text_result("No answer from the Vicuna server (scheduled items are managed by `vicuna serve` / the desktop's embedded server). Check `list` before retrying.", true);
    };
    let v: Value = serde_json::from_str(&reply).unwrap_or(Value::Null);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        text_result(v.get("text").and_then(Value::as_str).unwrap_or("Done."), false)
    } else {
        text_result(&format!("Not done: {}", v.get("error").and_then(Value::as_str).unwrap_or("refused")), true)
    }
}

fn manage_sessions_schema() -> Value {
    json!({
        "name": MANAGE_SESSIONS_TOOL,
        "title": "Manage worker sessions (lead controls; permissioned send & remote read/send)",
        "description": "Manage Vicuna Work sessions, each running its own harness (engine) and model. The user's lead session can delegate work; a non-lead session with the Message other sessions permission may use `send` (locally, or on another server with Use sessions on other servers). `report` on `send` defaults to true for the lead and to false for permissioned non-lead sessions, which may opt into `report: true` (locally and on other servers); a non-lead turn that was itself woken by a report callback cannot nest `report: true` (single-hop loop-breaker: it is downgraded to false). With `report: false` the target's reply is NOT pushed back automatically; use `schedule_wakeup` to follow up later. Other management actions remain lead-only, except `set_workspace` on your own session (works anywhere) and remote `servers`/`list`/`status` (see Other servers below). Actions: `list` (sessions with status, engine/model, workspace, last activity, lead/worker relation and pending questions; archived sessions are hidden unless `include_archived: true`, then tagged `[archived]`; same filters as `list_sessions`), `create` (new worker session: `workspace` defaults to yours, optional `title`, `engine`, `model`, and `message` to start it right away), `send` (`session_id` + `message`: starts a turn, or is queued into the running one; `report` sends the worker's reply back to you when its turn ends, default true for lead calls and false for permissioned non-lead calls, which may pass true), `stop` (`session_id`: stop its running turn), `delete` (`session_id`: remove the session; the user must confirm it in the app, a refusal is final), `archive` / `unarchive` (`session_id`: hide the session from the sidebar or bring it back, no confirm; nothing is deleted or stopped, and `list` hides them unless `include_archived: true`), `status` (`session_id`: running state and the tail of its last reply; use `read_session` for full history), `set_workspace` (`workspace`: absolute folder; omit `session_id` to move your own session, which works in any session and applies from your next turn; another session must be idle), and `permissions` (lead only: inspect or set another session's `read` and `allow_message` booleans). Other servers: with this session's \"Use sessions on other servers\" permission, any session can list saved servers (`servers`), list or inspect remote sessions (`list`/`status` with Read other sessions), and message remote sessions (`send` with Message other sessions; non-lead `send` defaults to `report: false` and may opt into `report: true`). The lead can also pass `server` to `create` (needs `workspace`, an absolute folder on that machine), `stop`, `archive` and `unarchive` there; replies of reported remote workers reach the lead the same way. Deleting remote sessions is not allowed. When a worker you created or messaged finishes a turn with reporting enabled, its reply reaches you as a message automatically, so after delegating, end your turn instead of polling.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["list", "create", "send", "stop", "delete", "archive", "unarchive", "status", "set_workspace", "permissions", "servers"] },
                "server": { "type": "string", "description": "Run the action on another Vicuna server the user saved (its name; see `servers`). Needs this session's \"Use sessions on other servers\" permission (plus Read/Message permissions for non-lead `list`/`status`/`send`, or the lead role for `create`/`stop`/`archive`/`unarchive`)." },
                "read": { "type": "boolean", "description": "permissions: allow reading other sessions' histories/state." },
                "allow_message": { "type": "boolean", "description": "permissions: allow sending messages to other sessions." },
                "include_archived": { "type": "boolean", "description": "list: include archived sessions, tagged [archived] (default false)." },
                "status": { "type": "string", "enum": ["running", "idle", "any"], "description": "list: only running or idle sessions." },
                "sort": { "type": "string", "enum": ["recent", "created", "title"], "description": "list: order (default recent = last activity, newest first)." },
                "since": { "type": "string", "description": "list: only sessions active since then: `2h`, `3d`, `2026-11-19 14:30` (local) or epoch ms." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "list: max rows (default 20)." },
                "offset": { "type": "integer", "minimum": 0, "description": "list: rows to skip." },
                "session_id": { "type": "string", "description": "Target session (send/stop/delete/archive/unarchive/status/set_workspace/permissions; set_workspace defaults to your own)." },
                "title": { "type": "string", "description": "create: sidebar title for the worker." },
                "workspace": { "type": "string", "description": "create: absolute workspace folder (default: the lead's). set_workspace: the new folder (required)." },
                "engine": { "type": "string", "description": "create: harness, e.g. claude, codex, opencode, fx, agy, pi (default: the user's current pick). See `list` for installed ones." },
                "model": { "type": "string", "description": "create: that engine's model id (default: the engine's default)." },
                "message": { "type": "string", "description": "create/send: the prompt for the worker." },
                "report": { "type": "boolean", "description": "create/send: send the target's reply back to you when its turn ends. Default true for lead calls; false for non-lead send, which may pass true (local or remote) unless this turn was woken by a report callback (single-hop loop-breaker). With false, nothing is pushed back: use `schedule_wakeup` to follow up." }
            },
            "required": ["action"]
        }
    })
}

/// Hand a session action to the window that runs this (lead) turn.
async fn manage_sessions(target: &Target, args: &Value, client_gone: impl std::future::Future<Output = ()>) -> Value {
    let action = args.get("action").and_then(Value::as_str).unwrap_or("").trim().to_string();
    if !["list", "create", "send", "stop", "delete", "archive", "unarchive", "status", "set_workspace", "permissions", "servers"].contains(&action.as_str()) {
        return text_result("`action` must be one of list, create, send, stop, delete, archive, unarchive, status, set_workspace, permissions, servers.", true);
    }
    let turn = &target.turn;
    if action == "permissions" && !can_manage_permissions(turn.lead.load(Ordering::Relaxed)) {
        return text_result("Only the lead session can view or change other sessions' permissions.", true);
    }
    let remote = action == "servers" || remote_server(args).is_some();
    if remote {
        let lead = turn.lead.load(Ordering::Relaxed);
        let has_session = target.session_id.is_some();
        let remote_perm = has_session && may_remote(&target.session_id).await;
        let read_perm = if remote_perm && matches!(action.as_str(), "list" | "status") {
            may_read_other(Some(target), "__all__").await
        } else {
            false
        };
        let message_perm = if remote_perm && action == "send" {
            may_message(&target.session_id).await
        } else {
            false
        };
        if let Err(e) = remote_session_decision(has_session, remote_perm, lead, read_perm, message_perm, &action) {
            return text_result(e, true);
        }
        if turn.stop.load(Ordering::Relaxed) {
            return text_result("The user stopped the turn; nothing was done.", true);
        }
    }
    // moving your own session's folder needs no lead role
    let sid = args.get("session_id").and_then(Value::as_str).map(str::trim).unwrap_or("");
    let own = action == "set_workspace" && (sid.is_empty() || Some(sid) == target.session_id.as_deref());
    if own && target.session_id.is_none() {
        return text_result("This turn has no Vicuna session, so there is nothing to move.", true);
    }
    let messaging = action == "send" && may_message(&target.session_id).await;
    if !remote && !can_manage_sessions_action(turn.lead.load(Ordering::Relaxed), own, &action, messaging) {
        return text_result("manage_sessions only works in the user's lead session. The user can make a session the lead from its sidebar menu (Make lead).", true);
    }
    let read_target = if action == "list" { "__all__" } else { sid };
    let reads_other = action == "list" || action == "status";
    if !remote && reads_other && !may_read_other(Some(target), read_target).await {
        return text_result("This session is not allowed to read other sessions (Permissions… in the session menu).", true);
    }
    if turn.stop.load(Ordering::Relaxed) {
        return text_result("The user stopped the turn; nothing was done.", true);
    }
    if action == "list" && !remote {
        // read-only: straight from the database, no window needed
        let pool = match db_pool().await {
            Ok(p) => p,
            Err(e) => return text_result(&e, true),
        };
        return match crate::session_insight::list(&pool, args, &live_sessions()).await {
            Ok(v) => {
                let engines: Vec<&str> = crate::work_engine::Engine::all().into_iter().filter(|e| e.is_present()).map(|e| e.id()).collect();
                text_result(&format!("{}\nInstalled harnesses: {}", crate::session_insight::list_text(&v), engines.join(", ")), false)
            }
            Err(e) => text_result(&e, true),
        };
    }
    let req_id = format!("sess-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(req_id.clone(), tx);
    }
    // Cheap reads should fail promptly; engine startup can legitimately be slow.
    // Delete waits for the user's confirmation card.
    let wait = match action.as_str() {
        "delete" => ASK_TIMEOUT_SECS,
        "create" | "send" => 120,
        _ => 60,
    };
    let (forwarded_args, report_note) = forwarded_session_args(
        args, &action, turn.lead.load(Ordering::Relaxed), turn.report_woken.load(Ordering::Relaxed),
    );
    if target.on_event.send(WorkAgentEvent::SessionControl { req_id: req_id.clone(), action, args: forwarded_args }).is_err() {
        turn.stop.store(true, Ordering::Relaxed);
        turn.ask_pending.lock().ok().map(|mut m| m.remove(&req_id));
        return text_result("The app event channel is unavailable; the action was not dispatched.", true);
    }
    let reply = tokio::select! {
        a = rx => a.map_err(|_| "The reply channel closed"),
        _ = client_gone => Err("The MCP client disconnected"),
        _ = tokio::time::sleep(Duration::from_secs(wait)) => Err("Timed out waiting for the app reply"),
    };
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.remove(&req_id);
    }
    let reply = match reply {
        Ok(reply) => reply,
        Err(reason) => return text_result(&format!("{reason}. The action may have run; check session state before retrying."), true),
    };
    let v: Value = serde_json::from_str(&reply).unwrap_or(Value::Null);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        let text = v.get("text").and_then(Value::as_str).unwrap_or("Done.");
        match report_note {
            Some(note) => text_result(&format!("{text}\n{note}"), false),
            None => text_result(text, false),
        }
    } else {
        text_result(v.get("error").and_then(Value::as_str).unwrap_or("the app refused the action"), true)
    }
}

/// Calls that reach (or list) other saved servers. Every one of them must pass
/// the "Use sessions on other servers" gate before any server is resolved.
fn is_remote_call(tool: &str, args: &Value) -> bool {
    match tool {
        MANAGE_SESSIONS_TOOL => args.get("action").and_then(Value::as_str).map(str::trim) == Some("servers") || remote_server(args).is_some(),
        READ_SESSION_TOOL | READ_ATTACHMENT_TOOL | SEARCH_TOOL => remote_server(args).is_some(),
        _ => false,
    }
}

const REMOTE_READ_DENIED: &str = "Reading sessions on other servers needs this session's \"Use sessions on other servers\" permission (Permissions… in the session menu).";
const REMOTE_SEND_DENIED: &str = "Messaging sessions on other servers needs this session's \"Use sessions on other servers\" and \"Message other sessions\" permissions (Permissions… in the session menu).";
const REMOTE_MANAGE_DENIED: &str = "Managing sessions on other servers needs the lead session with the \"Use sessions on other servers\" permission (Permissions… in the session menu).";
const READ_OTHER_DENIED: &str = "This session is not allowed to read other sessions (Permissions… in the session menu).";

/// The remote gate's decision, kept pure. Reading (read/search) needs only the
/// `remote` permission; managing also needs the lead role. Error texts are
/// fixed strings: they never name saved servers.
fn remote_gate_decision(has_session: bool, permitted: bool, lead: bool, manage: bool) -> Result<(), &'static str> {
    if manage {
        if has_session && permitted && lead { Ok(()) } else { Err(REMOTE_MANAGE_DENIED) }
    } else if has_session && permitted { Ok(()) } else { Err(REMOTE_READ_DENIED) }
}

/// Pure permission decision for remote `manage_sessions` actions (`servers` or
/// `server: ...`). Every refusal returns a fixed string that never names saved
/// servers, and runs before any server list or target is resolved.
fn remote_session_decision(
    has_session: bool,
    remote_perm: bool,
    lead: bool,
    read_perm: bool,
    message_perm: bool,
    action: &str,
) -> Result<(), &'static str> {
    match action {
        "servers" => {
            if has_session && remote_perm { Ok(()) } else { Err(REMOTE_READ_DENIED) }
        }
        "list" | "status" => {
            if !has_session || !remote_perm {
                Err(REMOTE_READ_DENIED)
            } else if !read_perm {
                Err(READ_OTHER_DENIED)
            } else {
                Ok(())
            }
        }
        "send" => {
            if has_session && remote_perm && (lead || message_perm) {
                Ok(())
            } else {
                Err(REMOTE_SEND_DENIED)
            }
        }
        _ => {
            if has_session && remote_perm && lead {
                Ok(())
            } else {
                Err(REMOTE_MANAGE_DENIED)
            }
        }
    }
}

fn remote_server(args: &Value) -> Option<String> {
    args.get("server").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Reading another server needs only the `remote` permission (not the lead):
/// "continue session X from my computer" is usually asked of a worker.
async fn remote_gate(target: Option<&Target>) -> Result<(), String> {
    let sid = target.and_then(|t| t.session_id.clone());
    let permitted = sid.is_some() && may_remote(&sid).await;
    remote_gate_decision(sid.is_some(), permitted, false, false).map_err(str::to_string)
}

async fn session_permission_in(pool: &sqlx::SqlitePool, id: &str, name: &str) -> bool {
    let Ok(Some(row)) = sqlx::query("SELECT extra FROM chats WHERE id = ?").bind(id).fetch_optional(pool).await else {
        return false;
    };
    let extra = row
        .try_get::<Option<String>, _>("extra")
        .ok()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .unwrap_or(Value::Null);
    effective_permission(&extra, name)
}

async fn may_remote(session_id: &Option<String>) -> bool {
    let Some(id) = session_id.as_deref() else { return false };
    let Ok(pool) = db_pool().await else { return false };
    may_remote_in(&pool, id).await
}

async fn may_remote_in(pool: &sqlx::SqlitePool, id: &str) -> bool {
    session_permission_in(pool, id, "remote").await
}

async fn may_message(session_id: &Option<String>) -> bool {
    let Some(id) = session_id.as_deref() else { return false };
    let Ok(pool) = db_pool().await else { return false };
    session_permission_in(&pool, id, "message").await
}

fn can_manage_permissions(is_lead: bool) -> bool { is_lead }

/// `report: false` sends: the reply is never pushed back on its own.
const REPORT_OFF_HINT: &str = "Reporting is off (`report: false`): the target's reply will not be sent back to you automatically. To follow up later, call `schedule_wakeup` and check it with `status`/`read_session`.";
const REPORT_LOOP_BREAKER: &str = "Loop-breaker: this turn was woken by a report callback, so nested `report: true` is disabled for non-lead sessions to prevent reply loops; the send went out with `report: false`.";

/// The `report` a `send` actually runs with. Lead: default true. Non-lead
/// (already permission-gated): default false, may opt into true unless this
/// turn was itself woken by a report callback (single-hop loop-breaker).
/// Returns (report, loop_breaker_applied).
fn effective_report(args: &Value, is_lead: bool, report_woken: bool) -> (bool, bool) {
    let asked = args.get("report").and_then(Value::as_bool);
    if is_lead {
        return (asked != Some(false), false);
    }
    match asked {
        Some(true) if report_woken => (false, true),
        Some(true) => (true, false),
        _ => (false, false),
    }
}

/// Args handed to the session controller (`report` made explicit on `send`)
/// plus the note to append to a successful result.
fn forwarded_session_args(args: &Value, action: &str, is_lead: bool, report_woken: bool) -> (Value, Option<String>) {
    let mut forwarded = args.clone();
    if action != "send" {
        return (forwarded, None);
    }
    let (report, broke) = effective_report(args, is_lead, report_woken);
    forwarded["report"] = json!(report);
    let note = match (report, broke) {
        (true, _) => None,
        (false, true) => Some(format!("{REPORT_LOOP_BREAKER} {REPORT_OFF_HINT}")),
        (false, false) => Some(REPORT_OFF_HINT.to_string()),
    };
    (forwarded, note)
}

fn can_manage_sessions_action(is_lead: bool, own: bool, action: &str, message_permission: bool) -> bool {
    own || is_lead || (action == "send" && message_permission)
}

fn can_read_other(caller: Option<&str>, requested: &str, permission: Option<bool>) -> bool {
    caller.is_none() || caller == Some(requested) || permission.unwrap_or(false)
}

fn effective_permission(extra: &Value, name: &str) -> bool {
    extra.pointer(&format!("/permissions/{name}")).and_then(Value::as_bool).unwrap_or_else(|| {
        name == "read" || (name == "message" && extra.get("role").and_then(Value::as_str) == Some("lead"))
    })
}

fn get_session_schema() -> Value {
    json!({
        "name": GET_SESSION_TOOL,
        "title": "Get current Vicuna session ID",
        "description": "Return the current Vicuna chat session_id, workspace, and title (if available). Use this when you need to pass session_id to `read_session` or check which Vicuna session you are in.",
        "inputSchema": {
            "type": "object",
            "properties": {}
        }
    })
}

fn read_session_schema() -> Value {
    json!({
        "name": READ_SESSION_TOOL,
        "title": "Read Vicuna session history (paginated)",
        "description": "Read messages from a Vicuna session stored in vicuna.db. Useful when the workspace or harness/engine changed mid-conversation, when earlier messages were truncated from the prompt, or when inspecting another session. Paginated with `offset` and `limit` (or `tail: true` for the latest page; with `tail`, long messages keep their END, which is cheap even for 150 KB replies). Each message reports its full `chars`. `plain: true` returns readable text without tool-card HTML. Messages with pasted images carry `images: [{index, mime, bytes}]` and the session lists staged files as `uploads: [{name, bytes}]`; read one with `read_attachment`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "Read from another Vicuna server the user saved (its name; `manage_sessions` action `servers` lists them), e.g. a session on the user's own computer. Needs this session's \"Use sessions on other servers\" permission." },
                "session_id": { "type": "string", "description": "Vicuna session ID to read. Defaults to the current session if omitted." },
                "offset": { "type": "integer", "minimum": 0, "description": "0-based message index to start from (default 0)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 100, "description": "Maximum number of messages to return (default 20, max 100)." },
                "tail": { "type": "boolean", "description": "When true and `offset` is omitted, return the last `limit` messages (default false)." },
                "max_chars_per_message": { "type": "integer", "minimum": 0, "maximum": 50000, "description": "Truncate each message's content to this many characters (default 8000; 0 = no truncation)." },
                "include_thinking": { "type": "boolean", "description": "Include model thinking/reasoning text for assistant messages (default false)." },
                "plain": { "type": "boolean", "description": "Plain text: tool-result cards collapse to one `▸ tool: args` line each, no HTML (default false)." }
            }
        }
    })
}

fn show_image_schema() -> Value {
    json!({
        "name": SHOW_IMAGE_TOOL,
        "title": "Show an image to the user",
        "description": "Display an image file (png, jpeg, webp or gif, up to 10 MB) in your reply in Vicuna's chat; it is also sent to the user's paired Telegram/Discord/Slack. Use it after generating, editing, rendering or screenshotting an image the user should see, instead of only naming the path.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Absolute path, or a path relative to the workspace, of the image file." },
                "caption": { "type": "string", "description": "Optional short caption." }
            },
            "required": ["path"]
        }
    })
}

/// vicuna-mcp `show_image`: load + validate the file and put it on the turn's
/// event channel; the recorder/windows append it to the live reply's images.
fn show_image(target: &Target, args: &Value) -> Value {
    let path = args.get("path").and_then(Value::as_str).unwrap_or("");
    let caption = args.get("caption").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(|s| s.chars().take(1000).collect::<String>());
    let img = match crate::attachments::load_show_image(path, &target.workspace) {
        Ok(i) => i,
        Err(e) => return text_result(&e, true),
    };
    let filename = img.filename.clone();
    emit_ev(
        &target.on_event,
        &target.turn.stop,
        WorkAgentEvent::AssistantImage { data_url: img.data_url, caption, filename: Some(img.filename) },
    );
    text_result(&format!("Shown {filename} to the user in the chat."), false)
}

fn read_attachment_schema() -> Value {
    json!({
        "name": READ_ATTACHMENT_TOOL,
        "title": "Read one session attachment",
        "description": "Read ONE attachment that `read_session` listed: an inline image of a message (`message_idx` = the message's `idx`, `image_index` from its `images`) or an uploaded file (`filename` from `uploads`). Supported images (png/jpeg/webp/gif) come back as an image you can see; other files, or any file with `save_to_file: true`, are saved under ~/vicuna/cache/remote-attachments/ and the local path is returned. Limit 10 MB.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "Read from another Vicuna server the user saved (its name). Needs this session's \"Use sessions on other servers\" and \"Read other sessions\" permissions." },
                "session_id": { "type": "string", "description": "Session to read from. Defaults to the current session (required with `server`)." },
                "message_idx": { "type": "integer", "minimum": 0, "description": "0-based message index (`idx` in read_session) holding the image." },
                "image_index": { "type": "integer", "minimum": 0, "description": "Which image of that message (default 0)." },
                "filename": { "type": "string", "description": "Uploaded file name from read_session's `uploads` (instead of `message_idx`)." },
                "save_to_file": { "type": "boolean", "description": "Save to ~/vicuna/cache/remote-attachments/ and return the path instead of an inline image (default false)." }
            }
        }
    })
}

/// Permission decision for `read_attachment`, kept pure. Own session: always.
/// Another local session: `read` or lead. Another server: `remote` AND
/// (`read` or lead).
fn attachment_gate(has_session: bool, own: bool, remote: bool, remote_perm: bool, read_perm: bool, lead: bool) -> Result<(), &'static str> {
    if remote {
        if !has_session || !remote_perm {
            Err(REMOTE_READ_DENIED)
        } else if !(read_perm || lead) {
            Err(READ_OTHER_DENIED)
        } else {
            Ok(())
        }
    } else if !has_session || own || read_perm || lead {
        Ok(())
    } else {
        Err(READ_OTHER_DENIED)
    }
}

/// (remote, read, lead) permissions of the calling session.
async fn caller_perms(target: Option<&Target>) -> (bool, bool, bool) {
    let Some(t) = target else { return (false, false, false) };
    let turn_lead = t.turn.lead.load(Ordering::Relaxed);
    let Some(id) = t.session_id.as_deref() else { return (false, false, turn_lead) };
    let Ok(pool) = db_pool().await else { return (false, false, turn_lead) };
    caller_perms_in(&pool, id, turn_lead).await
}

async fn caller_perms_in(pool: &sqlx::SqlitePool, id: &str, turn_lead: bool) -> (bool, bool, bool) {
    let extra = match sqlx::query("SELECT extra FROM chats WHERE id = ?").bind(id).fetch_optional(pool).await {
        Ok(Some(row)) => row.try_get::<Option<String>, _>("extra").ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null),
        _ => return (false, false, turn_lead),
    };
    let lead = turn_lead || extra.get("role").and_then(Value::as_str) == Some("lead");
    (effective_permission(&extra, "remote"), effective_permission(&extra, "read"), lead)
}

async fn read_attachment(target: Option<&Target>, args: &Value) -> Value {
    let server = remote_server(args);
    let caller = target.and_then(|t| t.session_id.clone());
    let requested = args.get("session_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let own = server.is_none() && (requested.is_none() || requested == caller);
    // with no turn at all (plain MCP client) local reads are unrestricted, like read_session
    if target.is_some() || server.is_some() {
        let (remote_perm, read_perm, lead) = if own && server.is_none() { (false, true, false) } else { caller_perms(target).await };
        if let Err(e) = attachment_gate(caller.is_some(), own, server.is_some(), remote_perm, read_perm, lead) {
            return text_result(e, true);
        }
    }
    let which = match crate::attachments::parse_which(args) {
        Ok(w) => w,
        Err(e) => return text_result(&e, true),
    };
    let save = args.get("save_to_file").and_then(Value::as_bool).unwrap_or(false);
    let (att, source) = if let Some(server) = server {
        let Some(id) = requested else {
            return text_result("`session_id` is required with `server` (find it with search_sessions or manage_sessions list on that server).", true);
        };
        match crate::server::remote_lead::read_attachment(&server, &id, &which).await {
            Ok(a) => (a, format!("Session {id} on server {server}")),
            Err(e) => return text_result(&e, true),
        }
    } else {
        let Some(id) = requested.or(caller) else {
            return text_result("No `session_id` provided and current turn has no session_id.", true);
        };
        let pool = db_pool().await.ok();
        match crate::attachments::read_local(pool.as_ref(), &id, &which).await {
            Ok(a) => (a, format!("Session {id}")),
            Err(e) => return text_result(&e, true),
        }
    };
    match crate::attachments::mcp_result(&att, &source, save) {
        Ok(v) => v,
        Err(e) => text_result(&e, true),
    }
}

fn list_sessions_schema() -> Value {
    json!({
        "name": LIST_SESSIONS_TOOL,
        "title": "List Vicuna sessions",
        "description": "List Vicuna sessions straight from vicuna.db (works with no app window open). Each row: full session id, title, status (running/idle), waiting_on_question (an ask_user/permission card is up), mode, engine/model, workspace, last_activity (server local time), archived, lead, lead_id (the lead that runs this worker), message_count. Archived sessions are hidden unless `include_archived: true` (`hidden_archived` counts them). Use `session_overview` for what sessions are doing and `search_sessions` to find one by content.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Case-insensitive substring of title or workspace, or an id prefix." },
                "status": { "type": "string", "enum": ["running", "idle", "any"], "description": "Only running or idle sessions (default any)." },
                "mode": { "type": "string", "enum": ["work", "chat"], "description": "Only Work or Chat sessions (default both)." },
                "include_archived": { "type": "boolean", "description": "Include archived sessions (default false)." },
                "sort": { "type": "string", "enum": ["recent", "created", "title"], "description": "recent = last activity, newest first (default)." },
                "since": { "type": "string", "description": "Only sessions active since: `30m`, `2h`, `3d`, `1w`, a local `2026-11-19 14:30`, ISO 8601, or epoch ms." },
                "offset": { "type": "integer", "minimum": 0, "description": "0-based offset (default 0)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum rows (default 20)." }
            }
        }
    })
}

fn overview_schema() -> Value {
    json!({
        "name": OVERVIEW_TOOL,
        "title": "What are the Vicuna sessions doing?",
        "description": "One-call overview of the most recently active Work sessions (read from vicuna.db, no app window needed): for each, the list_sessions row (status, waiting_on_question, engine/model, workspace, last activity, lead_id) plus the first user request, the latest user message, and the tail of the latest assistant reply as plain text (tool calls collapsed to `▸ tool: args` lines). Use it to answer \"what is everyone doing?\"; then `read_session` for details.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "limit": { "type": "integer", "minimum": 1, "maximum": 20, "description": "How many sessions (default 5)." },
                "status": { "type": "string", "enum": ["running", "idle", "any"], "description": "Only running or idle sessions." },
                "mode": { "type": "string", "enum": ["work", "chat", "any"], "description": "Default work." },
                "include_archived": { "type": "boolean", "description": "Include archived sessions (default false)." },
                "since": { "type": "string", "description": "Only sessions active since (`2h`, `1d`, a local date/time)." },
                "reply_chars": { "type": "integer", "minimum": 100, "maximum": 8000, "description": "Characters of each reply tail (default 800)." }
            }
        }
    })
}

fn search_schema() -> Value {
    json!({
        "name": SEARCH_TOOL,
        "title": "Search Vicuna sessions",
        "description": "Full-text search over session titles and message text in vicuna.db (no app window needed). Returns session id, title, a plain-text snippet around the match, last activity (local time), status and workspace, newest first. Archived sessions are skipped unless `include_archived: true`.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "Search another Vicuna server the user saved (its name; `manage_sessions` action `servers` lists them), e.g. a session on the user's own computer. Needs this session's \"Use sessions on other servers\" permission." },
                "query": { "type": "string", "description": "Text to find (case-insensitive substring)." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Max results (default 10)." },
                "include_archived": { "type": "boolean", "description": "Include archived sessions (default false)." }
            },
            "required": ["query"]
        }
    })
}

fn usage_schema() -> Value {
    json!({
        "name": USAGE_TOOL,
        "title": "Harness usage and quota",
        "description": "Read the subscription usage/quota of each installed Work harness (Claude Code, Codex, Antigravity, and enabled plugin harnesses that report usage) — the same numbers Vicuna's Usage view shows — with no app window needed. Per harness: plan name, every meter with used/left %, its reset time in local time, and a verdict: `ok` (≥20% left), `low` (<20% left), `exhausted` (none left) or `unknown` with the reason (not installed, not signed in, or the CLI printed nothing). Pass `harness` for one engine (a plugin harness by its `plugin:<plugin>/<harness>` id). Results are cached ~60 s. Use it before starting an expensive worker session (pick a harness with headroom) and when a harness call fails with a limit/usage error, so you can explain it or switch harnesses. OpenCode, pi and fx do not report subscription quota.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "harness": { "type": "string", "description": "Only this harness: claude, codex, agy, opencode, pi, fx, or a plugin harness id like plugin:<plugin>/<harness> (default: every installed quota harness)." }
            }
        }
    })
}

fn manage_mcp_schema() -> Value {
    json!({
        "name": MANAGE_MCP_TOOL,
        "title": "Manage Vicuna MCP servers",
        "description": "List MCP servers for the calling session, switch servers on or off for this session, or add, edit, and remove shared custom server definitions. The fixed `vicuna-mcp` server is always available.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "add", "edit", "enable", "disable", "remove"],
                    "description": "Operation to perform."
                },
                "name": {
                    "type": "string",
                    "description": "Server name / ID (required for add, edit, enable, disable, remove)."
                },
                "new_name": {
                    "type": "string",
                    "description": "Optional new server name when renaming via action='edit'."
                },
                "transport": {
                    "type": "string",
                    "enum": ["http", "stdio"],
                    "description": "Server transport: 'http' (remote streamable-HTTP endpoint) or 'stdio' (local command). Inferred from url/command on 'add' if omitted."
                },
                "url": {
                    "type": "string",
                    "description": "HTTP/HTTPS endpoint URL for 'http' servers (required on 'add' for http)."
                },
                "headers": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Optional HTTP headers map (e.g. {\"Authorization\": \"Bearer ...\"})."
                },
                "command": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Command and arguments for 'stdio' servers (e.g. [\"npx\", \"-y\", \"@org/mcp\"])."
                },
                "env": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Optional environment variables map for 'stdio' servers."
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Whether the server is enabled in this session (defaults to true on 'add'; preserves existing value on 'edit' if omitted)."
                },
                "wake": {
                    "type": "boolean",
                    "description": "When true (default for 'add' and 'enable' once the server connects), Vicuna automatically starts a follow-up turn after this turn finishes so you can immediately call the newly active MCP server without waiting for the user. Set false if no follow-up turn is needed."
                }
            },
            "required": ["action"]
        }
    })
}

fn manage_skills_schema() -> Value {
    json!({
        "name": MANAGE_SKILLS_TOOL,
        "title": "Manage Vicuna skills",
        "description": "Manage shared Vicuna skill definitions (~/vicuna/skills/<slug>/SKILL.md) and per-session switches. Enabled skills are available in the target Work session's harness.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "search", "read", "add", "edit", "enable", "disable", "remove"],
                    "description": "Operation to perform."
                },
                "session_id": {
                    "type": "string",
                    "description": "Work session whose enabled states to use. Defaults to the caller. Only the lead can target another session."
                },
                "query": {
                    "type": "string",
                    "description": "Required for search. Case-insensitive substring of slug, name, description, or SKILL.md body."
                },
                "slug": {
                    "type": "string",
                    "description": "Skill slug: lowercase [a-z0-9][a-z0-9_-]{0,63} (required for read, add, edit, enable, disable, remove)."
                },
                "content": {
                    "type": "string",
                    "description": "Full SKILL.md Markdown content with YAML frontmatter `name:` and `description:` (required for 'add'; optional for 'edit')."
                },
                "enabled": {
                    "type": "boolean",
                    "description": "Set the skill's switch in the target session. Omit to leave its switch unchanged. New definitions are enabled by default."
                }
            },
            "required": ["action"]
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
    // a repeat of a question whose card is still up (the engine gave up on
    // the earlier call, or it was a "still waiting" poll) re-attaches to it
    let key = question.split_whitespace().collect::<Vec<_>>().join(" ");
    let parked = turn.ask_parked.lock().ok().and_then(|mut p| {
        let i = p.iter().position(|a| a.question.split_whitespace().collect::<Vec<_>>().join(" ") == key)?;
        Some(p.remove(i))
    });
    let (ask_id, mut rx) = match parked {
        Some(p) => (p.ask_id, p.rx),
        None => {
            let ask_id = format!("ask-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
            let (tx, rx) = oneshot::channel::<String>();
            if let Ok(mut m) = turn.ask_pending.lock() {
                m.insert(ask_id.clone(), tx);
            }
            emit_ev(
                &target.on_event,
                &turn.stop,
                WorkAgentEvent::AskRequest { ask_id: ask_id.clone(), question: question.clone(), options: choices, multi: multiple },
            );
            (ask_id, rx)
        }
    };
    let poll = turn.ask_poll_secs.load(Ordering::Relaxed);
    let poll_wait = async move {
        if poll == 0 {
            std::future::pending::<()>().await
        } else {
            tokio::time::sleep(Duration::from_secs(poll)).await
        }
    };
    // None = no answer yet (poll elapsed or the client left)
    let (answer, polled) = tokio::select! {
        a = &mut rx => (Some(a.ok()), false),
        _ = client_gone => (None, false),
        _ = poll_wait => (None, true),
    };
    let answer = match answer {
        Some(a) => a,
        None => {
            if turn.stop.load(Ordering::Relaxed) {
                turn.ask_pending.lock().ok().map(|mut m| m.remove(&ask_id));
                return text_result("The user stopped the turn before answering.", true);
            }
            // keep the card up: a repeat call re-attaches, an answer meanwhile
            // is queued as a message (work_agent::agent_answer)
            if let Ok(mut p) = turn.ask_parked.lock() {
                p.push(crate::work_agent::ParkedAsk { ask_id, question, multi: multiple, rx });
            }
            return text_result(
                if polled {
                    "The user has not answered yet; the question is still shown to them. Call ask_user again right away with exactly the same question to keep waiting for the answer. Do not ask in plain text and do not end your turn."
                } else {
                    "The question is still shown to the user; their answer will reach you as a message."
                },
                false,
            );
        }
    };
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.remove(&ask_id);
    }
    match answer {
        Some(a) if a.trim() == ASK_LATER => text_result(
            "The user will answer this later. Continue with work that does not depend on the answer, or end your turn if everything depends on it. Do not ask the same question again: the answer will reach you as a message.",
            false,
        ),
        Some(a) if !turn.stop.load(Ordering::Relaxed) => answer_result(&parse_ask_answer(&a), multiple),
        _ => text_result("The user stopped the turn before answering.", true),
    }
}

/// Parse + check `schedule_wakeup` arguments: (message, at, in_minutes).
fn parse_schedule_args(args: &Value) -> Result<(String, Option<String>, Option<f64>), String> {
    let message = args.get("message").and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();
    if message.is_empty() {
        return Err("`message` is required: what to tell yourself when you wake up.".into());
    }
    let at = args.get("at").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let mins = args.get("in_minutes").and_then(Value::as_f64);
    match (&at, mins) {
        (Some(_), Some(_)) => Err("Give either `in_minutes` or `at`, not both.".into()),
        (None, None) => Err("Give `in_minutes` (relative) or `at` (local time) for the wake-up.".into()),
        (None, Some(m)) if !(m.is_finite() && m >= 1.0 && m <= 30.0 * 24.0 * 60.0) => {
            Err("`in_minutes` must be between 1 and 43200 (30 days).".into())
        }
        _ => Ok((message, at, mins)),
    }
}

/// Hand the wake-up to the chat window that runs this turn: it resolves the
/// time in the user's timezone, adds it to Scheduled (the same delivery as a
/// user-scheduled send) and answers with the exact time or a reason.
async fn schedule_wakeup(target: &Target, args: &Value, client_gone: impl std::future::Future<Output = ()>) -> Value {
    let (message, at, in_minutes) = match parse_schedule_args(args) {
        Ok(v) => v,
        Err(e) => return text_result(&e, true),
    };
    if target.session_id.is_none() {
        return text_result("This turn has no Vicuna session to schedule into.", true);
    }
    let turn = &target.turn;
    if turn.stop.load(Ordering::Relaxed) {
        return text_result("The user stopped the turn; nothing was scheduled.", true);
    }
    let req_id = format!("wake-{}", turn.ask_seq.fetch_add(1, Ordering::Relaxed));
    let (tx, rx) = oneshot::channel::<String>();
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.insert(req_id.clone(), tx);
    }
    emit_ev(
        &target.on_event,
        &turn.stop,
        WorkAgentEvent::ScheduleWakeup { req_id: req_id.clone(), message, at, in_minutes },
    );
    let reply = tokio::select! {
        a = rx => a.ok(),
        _ = client_gone => None,
        _ = tokio::time::sleep(Duration::from_secs(30)) => None,
    };
    if let Ok(mut m) = turn.ask_pending.lock() {
        m.remove(&req_id);
    }
    let Some(reply) = reply else {
        return text_result("The app did not confirm the wake-up. It may have been scheduled; check Scheduled sends before retrying.", true);
    };
    let v: Value = serde_json::from_str(&reply).unwrap_or(Value::Null);
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        let when = v.get("when").and_then(Value::as_str).unwrap_or("the requested time");
        let idnote = v.get("id").and_then(Value::as_str).map(|i| format!(" (id {i}; see/change/cancel it with manage_schedule)")).unwrap_or_default();
        // a headless server stores and delivers it itself (no window needed)
        let delivery = if v.get("server").and_then(Value::as_bool) == Some(true) {
            "The Vicuna server delivers it even when no app window is open"
        } else {
            "It is delivered while the user's Vicuna app is open"
        };
        text_result(
            &format!("Scheduled: Vicuna will send your message into this session at {when}{idnote}. {delivery}, and the user can see or cancel it under Scheduled. End your turn now unless there is other work to do."),
            false,
        )
    } else {
        let e = v.get("error").and_then(Value::as_str).unwrap_or("the app refused the wake-up");
        text_result(&format!("Not scheduled: {e}"), true)
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

fn truncate_chars(s: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (s.to_string(), false);
    }
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        (format!("{head}…[truncated]"), true)
    } else {
        (head, false)
    }
}

async fn get_session_id(target: &Target) -> Value {
    let mut out = json!({
        "session_id": target.session_id,
        "workspace": target.workspace,
    });
    if let (Some(sid), Ok(pool)) = (target.session_id.as_deref(), db_pool().await) {
        if let Ok(Some(row)) = sqlx::query("SELECT title, mode FROM chats WHERE id = $1")
            .bind(sid)
            .fetch_optional(&pool)
            .await
        {
            out["title"] = json!(row.try_get::<String, _>("title").unwrap_or_default());
            out["mode"] = json!(row.try_get::<Option<String>, _>("mode").ok().flatten());
        }
    }
    text_result(&out.to_string(), false)
}

async fn read_session_from_pool(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    args: &Value,
) -> Result<Value, String> {
    let uploads_dir = crate::updates_uploads::upload_session_dir(session_id).ok();
    read_session_page(pool, session_id, args, uploads_dir.as_deref()).await
}

async fn read_session_page(
    pool: &sqlx::SqlitePool,
    session_id: &str,
    args: &Value,
    uploads_dir: Option<&std::path::Path>,
) -> Result<Value, String> {
    let chat_row = sqlx::query("SELECT id, title, mode, workspace, goal FROM chats WHERE id = $1")
        .bind(session_id)
        .fetch_optional(pool)
        .await
        .map_err(|e| format!("read_session failed: {e}"))?;
    let Some(chat_row) = chat_row else {
        return Err(format!("Session not found: {session_id}"));
    };

    let plain = args.get("plain").and_then(Value::as_bool).unwrap_or(false);
    let filter = if plain { " AND context_cleared IS NOT 1" } else { "" };
    let total: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM messages WHERE chat_id = $1{filter}"))
        .bind(session_id)
        .fetch_one(pool)
        .await
        .map_err(|e| format!("read_session count failed: {e}"))?;

    let limit = args
        .get("limit")
        .and_then(Value::as_i64)
        .unwrap_or(20)
        .clamp(1, 100);
    let tail = args.get("tail").and_then(Value::as_bool).unwrap_or(false);
    let offset = match args.get("offset").and_then(Value::as_i64) {
        Some(o) => o.max(0),
        None if tail => (total - limit).max(0),
        None => 0,
    };
    let max_chars = args
        .get("max_chars_per_message")
        .and_then(Value::as_i64)
        .unwrap_or(8000)
        .clamp(0, 50_000) as usize;
    let include_thinking = args
        .get("include_thinking")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    // Only a window of each message leaves sqlite: plain text shrinks cards,
    // so it reads more than it returns. 0 = no truncation = whole message.
    let window: i64 = if max_chars == 0 { -1 } else if plain { (max_chars as i64 * 8).max(20_000) } else { max_chars as i64 + 1 };
    let content_sql = match (window < 0, tail) {
        (true, _) => "content",
        (false, true) => "substr(content, -$4)",
        (false, false) => "substr(content, 1, $4)",
    };
    let rows = sqlx::query(&format!(
        "SELECT idx, role, {content_sql} AS content, length(content) AS chars, thinking, model, context_cleared, images FROM messages WHERE chat_id = $1{filter} ORDER BY idx ASC LIMIT $2 OFFSET $3",
    ))
    .bind(session_id)
    .bind(limit)
    .bind(offset)
    .bind(window)
    .fetch_all(pool)
    .await
    .map_err(|e| format!("read_session query failed: {e}"))?;

    let mut messages = Vec::with_capacity(rows.len());
    for r in rows {
        let idx: i64 = r.try_get("idx").unwrap_or(0);
        let role: String = r.try_get("role").unwrap_or_default();
        let raw_content: String = r.try_get("content").unwrap_or_default();
        let model: Option<String> = r.try_get("model").ok().flatten();
        let chars: i64 = r.try_get("chars").unwrap_or(0);
        let windowed = window >= 0 && chars > window;
        let text = if plain && role == "assistant" { crate::session_insight::plain_text(&raw_content) } else { raw_content };
        let (content, truncated) = if tail && max_chars > 0 {
            let n = text.chars().count();
            if n > max_chars { (format!("…{}", text.chars().skip(n - max_chars).collect::<String>()), true) } else { (text, windowed) }
        } else {
            let (c, t) = truncate_chars(&text, max_chars);
            (c, t || windowed)
        };
        let mut msg = json!({
            "idx": idx,
            "role": role,
            "model": model,
            "chars": chars,
            "content": content,
            "truncated": truncated,
        });
        if r.try_get::<Option<i64>, _>("context_cleared").ok().flatten() == Some(1) {
            msg["contextCleared"] = json!(true);
        }
        if let Some(raw) = r.try_get::<Option<String>, _>("images").ok().flatten() {
            let meta = crate::attachments::images_meta(&Value::String(raw));
            if !meta.is_empty() {
                msg["images"] = json!(meta);
            }
        }
        if include_thinking {
            let raw_thinking: String = r.try_get("thinking").unwrap_or_default();
            if !raw_thinking.is_empty() {
                let (thinking, _) = truncate_chars(&raw_thinking, max_chars);
                msg["thinking"] = json!(thinking);
            }
        }
        messages.push(msg);
    }

    let returned = messages.len() as i64;
    let next_offset = offset + returned;
    let has_more = next_offset < total;

    let mut out = json!({
        "session": {
            "id": chat_row.try_get::<String, _>("id").unwrap_or_default(),
            "title": chat_row.try_get::<String, _>("title").unwrap_or_default(),
            "mode": chat_row.try_get::<Option<String>, _>("mode").ok().flatten(),
            "workspace": chat_row.try_get::<Option<String>, _>("workspace").ok().flatten(),
            "goal": chat_row.try_get::<Option<String>, _>("goal").ok().flatten(),
        },
        "total_messages": total,
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "has_more": has_more,
        "next_offset": if has_more { json!(next_offset) } else { Value::Null },
        "messages": messages,
    });
    let uploads = uploads_dir.map(crate::attachments::uploads_meta_in).unwrap_or_default();
    if !uploads.is_empty() {
        out["uploads"] = json!(uploads);
    }
    Ok(out)
}

async fn read_session(target: Option<&Target>, args: &Value) -> Value {
    if let Some(server) = remote_server(args) {
        if let Err(e) = remote_gate(target).await { return text_result(&e, true); }
        let Some(id) = args.get("session_id").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) else {
            return text_result("`session_id` is required with `server` (find it with search_sessions or manage_sessions list on that server).", true);
        };
        return match crate::server::remote_lead::read(&server, id, args).await {
            Ok(t) => text_result(&t, false),
            Err(e) => text_result(&e, true),
        };
    }
    let requested = args
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| target.and_then(|t| t.session_id.clone()));
    let Some(session_id) = requested else {
        return text_result("No `session_id` provided and current turn has no session_id.", true);
    };
    if !may_read_other(target, &session_id).await {
        return text_result("This session is not allowed to read other sessions (Permissions… in the session menu).", true);
    }
    let pool = match db_pool().await {
        Ok(p) => p,
        Err(e) => return text_result(&e, true),
    };
    match read_session_from_pool(&pool, &session_id, args).await {
        Ok(v) => text_result(&v.to_string(), false),
        Err(e) => text_result(&e, true),
    }
}

async fn may_read_other(target: Option<&Target>, requested: &str) -> bool {
    let Some(t) = target else { return true };
    if t.session_id.as_deref() == Some(requested) { return true; }
    let Some(caller) = t.session_id.as_deref() else { return true };
    let Ok(pool) = db_pool().await else { return false };
    let Ok(row) = sqlx::query("SELECT extra FROM chats WHERE id = ?").bind(caller).fetch_optional(&pool).await else { return false };
    let Some(row) = row else { return false };
    let extra = row.try_get::<Option<String>, _>("extra").ok().flatten().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or(Value::Null);
    can_read_other(Some(caller), requested, Some(effective_permission(&extra, "read")))
}

async fn list_sessions_from_pool(pool: &sqlx::SqlitePool, args: &Value) -> Result<Value, String> {
    crate::session_insight::list(pool, args, &live_sessions()).await
}

async fn list_sessions(args: &Value) -> Value {
    let pool = match db_pool().await {
        Ok(p) => p,
        Err(e) => return text_result(&e, true),
    };
    match list_sessions_from_pool(&pool, args).await {
        Ok(v) => text_result(&v.to_string(), false),
        Err(e) => text_result(&e, true),
    }
}

async fn session_overview(args: &Value) -> Value {
    let pool = match db_pool().await {
        Ok(p) => p,
        Err(e) => return text_result(&e, true),
    };
    match crate::session_insight::overview(&pool, args, &live_sessions()).await {
        Ok(v) => text_result(&serde_json::to_string_pretty(&v).unwrap_or_default(), false),
        Err(e) => text_result(&e, true),
    }
}

async fn search_sessions(target: Option<&Target>, args: &Value) -> Value {
    if let Some(server) = remote_server(args) {
        if let Err(e) = remote_gate(target).await { return text_result(&e, true); }
        let q = args.get("query").and_then(Value::as_str).unwrap_or("").trim();
        if q.is_empty() { return text_result("`query` is required.", true); }
        let limit = args.get("limit").and_then(Value::as_i64).unwrap_or(10);
        return match crate::server::remote_lead::search(&server, q, limit).await {
            Ok(t) => text_result(&t, false),
            Err(e) => text_result(&e, true),
        };
    }
    let pool = match db_pool().await {
        Ok(p) => p,
        Err(e) => return text_result(&e, true),
    };
    match crate::session_insight::search(&pool, args, &live_sessions()).await {
        Ok(v) => text_result(&v.to_string(), false),
        Err(e) => text_result(&e, true),
    }
}

/// The local zone's offset from UTC in minutes, via sqlite (no tz crate):
/// `strftime('%s')` of a localtime string read as UTC is exactly the offset.
/// `None` (no database) leaves relative reset text as the CLI wrote it.
async fn local_offset_minutes() -> Option<i64> {
    let pool = db_pool().await.ok()?;
    let secs: Option<i64> = sqlx::query_scalar(
        "SELECT CAST(strftime('%s','now','localtime') AS INTEGER) - CAST(strftime('%s','now') AS INTEGER)",
    )
    .fetch_one(&pool)
    .await
    .ok();
    secs.map(|s| s / 60)
}

/// `usage_status`: the Usage view's report as plain text. Runs here (the
/// backend) rather than through the window, so a headless `vicuna serve` and
/// a lead session can read quota with no GUI open. `caller_session_id` is
/// accepted for consistency but not needed (the read is not turn-scoped).
async fn usage_status(args: &Value) -> Value {
    let harness = args.get("harness").and_then(Value::as_str);
    let offset = local_offset_minutes().await;
    match crate::work_usage::usage_status_text(harness, offset).await {
        Ok(text) => text_result(&text, false),
        Err(e) => text_result(&e, true),
    }
}

fn is_fixed_vicuna_mcp(name: &str) -> bool {
    crate::mcp::is_fixed(name)
}

fn parse_string_map_arg(v: Option<&Value>) -> Result<Option<HashMap<String, String>>, String> {
    let Some(val) = v else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let Some(obj) = val.as_object() else {
        return Err("expected a JSON object of string key/value pairs".to_string());
    };
    if obj.is_empty() {
        return Ok(None);
    }
    let mut out = HashMap::with_capacity(obj.len());
    for (k, v) in obj {
        let s = v
            .as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| v.to_string());
        out.insert(k.clone(), s);
    }
    Ok(Some(out))
}

fn parse_command_arg(v: Option<&Value>) -> Result<Option<Vec<String>>, String> {
    let Some(val) = v else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    if let Some(arr) = val.as_array() {
        let parts: Vec<String> = arr
            .iter()
            .filter_map(|x| x.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string))
            .collect();
        return Ok((!parts.is_empty()).then_some(parts));
    }
    if let Some(s) = val.as_str() {
        let parts: Vec<String> = s.split_whitespace().map(str::to_string).collect();
        return Ok((!parts.is_empty()).then_some(parts));
    }
    Err("`command` must be an array of strings".to_string())
}

fn normalize_transport_arg(raw: &str) -> Result<String, String> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "http" | "remote" => Ok("http".to_string()),
        "stdio" | "local" => Ok("stdio".to_string()),
        other => Err(format!("invalid transport `{other}`; expected `http` or `stdio`")),
    }
}

fn validate_http_url(url: &str) -> Result<String, String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return Err("`url` is required for http MCP servers".to_string());
    }
    if !trimmed.starts_with("http://") && !trimmed.starts_with("https://") {
        return Err("`url` must start with http:// or https://".to_string());
    }
    Ok(trimmed.to_string())
}

fn validate_server_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 100 {
        return Err("server `name` must be 1..100 characters".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err("server `name` cannot contain slashes".to_string());
    }
    if is_fixed_vicuna_mcp(trimmed) {
        return Err("`vicuna-mcp` is a fixed built-in server and cannot be added, modified, disabled, or removed.".to_string());
    }
    Ok(trimmed.to_string())
}

fn find_server<'a>(servers: &'a [crate::mcp::McpServerConfig], name: &str) -> Option<&'a crate::mcp::McpServerConfig> {
    let q = name.trim();
    servers
        .iter()
        .find(|s| s.name == q || s.id == q)
        .or_else(|| servers.iter().find(|s| s.name.eq_ignore_ascii_case(q) || s.id.eq_ignore_ascii_case(q)))
}

fn list_servers_in(path: Option<&std::path::Path>) -> Result<Vec<crate::mcp::McpServerConfig>, String> {
    match path {
        Some(p) => crate::mcp::list_file_servers_in(p),
        None => crate::mcp::list_file_servers(),
    }
}

fn save_server_in(
    path: Option<&std::path::Path>,
    cfg: &crate::mcp::McpServerConfig,
) -> Result<Vec<crate::mcp::McpServerConfig>, String> {
    crate::mcp::rename_file_server(path, &cfg.id, cfg)
}

fn delete_server_in(
    path: Option<&std::path::Path>,
    id: &str,
) -> Result<Vec<crate::mcp::McpServerConfig>, String> {
    match path {
        Some(p) => crate::mcp::delete_file_server_in(p, id),
        None => crate::mcp::delete_file_server(id),
    }
}

/// The MCP management tool uses the same selection and backend as the calling turn.
pub(crate) struct TurnMcp {
    manager: Option<crate::mcp::McpManager>,
    resources: Option<std::path::PathBuf>,
    local: tokio::sync::Mutex<crate::mcp::SessionSelection>,
    server: Option<std::sync::Weak<crate::server::BridgeState>>,
}

impl TurnMcp {
    pub fn new(
        manager: crate::mcp::McpManager,
        resources: Option<std::path::PathBuf>,
        selection: crate::mcp::SessionSelection,
        server: Option<std::sync::Weak<crate::server::BridgeState>>,
    ) -> Self {
        Self { manager: Some(manager), resources, local: tokio::sync::Mutex::new(selection), server }
    }

    async fn start_browser(&self) {
        if let Some(st) = self.server.as_ref().and_then(|st| st.upgrade()) {
            let _ = crate::browser::browser_start_logic(self.resources.clone(), &st.browser).await;
            return;
        }
        #[cfg(feature = "desktop")]
        if let Some(browser) = APP_HANDLE.get().and_then(|app| app.try_state::<std::sync::Arc<crate::browser::BrowserState>>()) {
            let _ = crate::browser::browser_start_logic(self.resources.clone(), &browser).await;
        }
    }

    async fn selection(&self, sid: &str) -> Result<crate::mcp::SessionSelection, String> {
        if let Some(st) = self.server.as_ref().and_then(|st| st.upgrade()) {
            if let Some(db) = &st.db {
                return crate::server_store::session_mcp(db, sid).await?
                    .ok_or_else(|| "Work session no longer exists".into());
            }
        }
        Ok(self.local.lock().await.clone())
    }

    async fn set_enabled(&self, sid: &str, path: Option<&std::path::Path>, id: &str, enabled: bool) -> Result<crate::mcp::EnabledMap, String> {
        if let Some(st) = self.server.as_ref().and_then(|st| st.upgrade()) {
            if let Some(db) = &st.db {
                let _write = st.chat_lock.lock().await;
                return Ok(crate::server_store::set_session_mcp(db, sid, id, enabled).await?.enabled.unwrap_or_default());
            }
        }
        let defaults = if path.is_some() { crate::mcp::defaults_in(path) } else { crate::mcp::defaults() };
        Ok(self.local.lock().await.set_enabled(&defaults, id, enabled).clone())
    }
}

struct ManageMcpOutcome {
    value: Value,
    changed: bool,
    wake_server: Option<(String, Option<usize>)>,
    enabled: Option<crate::mcp::EnabledMap>,
}

async fn connect_summary(
    mgr: Option<&crate::mcp::McpManager>,
    cfg: &crate::mcp::McpServerConfig,
) -> (Value, bool, Option<usize>) {
    let Some(m) = mgr else {
        return (Value::Null, false, None);
    };
    match m.connect(cfg).await {
        Ok(info) => {
            let n = info.tools.len();
            (json!({ "connected": true, "tools": n }), true, Some(n))
        }
        Err(e) => (json!({ "connected": false, "error": e }), false, None),
    }
}

async fn manage_mcp_in(
    path: Option<&std::path::Path>,
    args: &Value,
    context: &TurnMcp,
    sid: &str,
) -> Result<ManageMcpOutcome, String> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("")
        .to_ascii_lowercase();

    let mgr = context.manager.as_ref();
    let configured = || list_servers_in(path);
    let available = || match path {
        Some(_) => configured(),
        None => crate::mcp::session_servers(context.resources.as_deref()),
    };
    match action.as_str() {
        "list" => {
            let servers = available()?;
            let selection = context.selection(sid).await?;
            let (connected_ids, tool_counts) = match mgr {
                Some(m) => (m.connected_ids().await, m.tool_counts().await),
                None => (Vec::new(), HashMap::new()),
            };
            let items: Vec<Value> = servers
                .iter()
                .map(|s| {
                    let enabled = selection.is_enabled(&s.id, s.enabled);
                    let is_conn = enabled && connected_ids.iter().any(|id| id == &s.id);
                    let mut header_keys: Vec<&String> = s.headers.as_ref().map(|h| h.keys().collect()).unwrap_or_default();
                    header_keys.sort();
                    let mut env_keys: Vec<&String> = s.env.as_ref().map(|e| e.keys().collect()).unwrap_or_default();
                    env_keys.sort();
                    json!({
                        "name": s.name,
                        "transport": s.normalized_transport(),
                        "url": s.url,
                        "command": s.command,
                        "header_keys": header_keys,
                        "env_keys": env_keys,
                        "enabled": enabled,
                        "connected": is_conn,
                        "tools": tool_counts.get(&s.id).copied(),
                    })
                })
                .collect();
            Ok(ManageMcpOutcome {
                value: json!({
                    "fixed_server": {
                        "name": SERVER_NAME,
                        "enabled": true,
                        "fixed": true,
                        "note": "Built-in Vicuna MCP server; cannot be edited, disabled, or removed."
                    },
                    "servers": items,
                }),
                changed: false,
                wake_server: None,
                enabled: None,
            })
        }
        "add" => {
            let raw_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let name = validate_server_name(raw_name)?;
            let existing = available()?;
            if find_server(&existing, &name).is_some() {
                return Err(format!("MCP server `{name}` already exists. Use action='edit' to update it."));
            }
            let url = args.get("url").and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
            let command = parse_command_arg(args.get("command"))?;
            let transport = match args.get("transport").and_then(|v| v.as_str()) {
                Some(t) => normalize_transport_arg(t)?,
                None if url.is_some() => "http".to_string(),
                None if command.is_some() => "stdio".to_string(),
                None => return Err("provide `url` (for http) or `command` (for stdio)".to_string()),
            };
            let (final_url, final_headers, final_cmd, final_env) = if transport == "http" {
                let u = validate_http_url(url.unwrap_or(""))?;
                let h = parse_string_map_arg(args.get("headers"))?;
                (Some(u), h, None, None)
            } else {
                let cmd = command.ok_or_else(|| "`command` is required for stdio MCP servers".to_string())?;
                let env = parse_string_map_arg(args.get("env"))?;
                (None, None, Some(cmd), env)
            };
            let enabled = args.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
            let cfg = crate::mcp::McpServerConfig {
                id: name.clone(),
                name: name.clone(),
                transport,
                command: final_cmd,
                env: final_env,
                url: final_url,
                headers: final_headers,
                oauth: None,
                enabled,
            };
            save_server_in(path, &cfg)?;
            let selection = context.set_enabled(sid, path, &cfg.id, enabled).await?;
            let (conn, connected_ok, tools_n) = if enabled {
                connect_summary(mgr, &cfg).await
            } else {
                (Value::Null, false, None)
            };
            let wake_arg = args.get("wake").and_then(|v| v.as_bool());
            let waking = enabled && connected_ok && wake_arg != Some(false);
            let note = if waking {
                "Saved to vicuna.json and connected. Vicuna will automatically start a follow-up turn when this turn finishes so you can use this server right away."
            } else {
                "Saved to vicuna.json. Work engines pick up MCP config changes on the next turn."
            };
            Ok(ManageMcpOutcome {
                value: json!({
                    "ok": true,
                    "action": "add",
                    "name": cfg.name,
                    "transport": cfg.transport,
                    "enabled": cfg.enabled,
                    "connection": conn,
                    "waking_next_turn": waking,
                    "note": note
                }),
                changed: true,
                wake_server: waking.then(|| (cfg.name.clone(), tools_n)),
                enabled: Some(selection),
            })
        }
        "edit" => {
            let raw_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if is_fixed_vicuna_mcp(raw_name) {
                return Err("`vicuna-mcp` is a fixed built-in server and cannot be modified.".to_string());
            }
            let existing_list = list_servers_in(path)?;
            let prev = find_server(&existing_list, raw_name)
                .ok_or_else(|| format!("MCP server `{}` not found", raw_name.trim()))?
                .clone();
            let target_name = match args.get("new_name").and_then(|v| v.as_str()) {
                Some(nn) => {
                    let validated = validate_server_name(nn)?;
                    if !validated.eq_ignore_ascii_case(&prev.name) && find_server(&existing_list, &validated).is_some() {
                        return Err(format!("Cannot rename to `{validated}`: a server with that name already exists."));
                    }
                    validated
                }
                None => prev.name.clone(),
            };
            let transport = match args.get("transport").and_then(|v| v.as_str()) {
                Some(t) => normalize_transport_arg(t)?,
                None => prev.normalized_transport().to_string(),
            };
            let url = match args.get("url") {
                Some(v) if v.is_null() => None,
                Some(v) => v.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
                None => prev.url.clone(),
            };
            let headers = match args.get("headers") {
                Some(v) => parse_string_map_arg(Some(v))?,
                None => prev.headers.clone(),
            };
            let command = match args.get("command") {
                Some(v) => parse_command_arg(Some(v))?,
                None => prev.command.clone(),
            };
            let env = match args.get("env") {
                Some(v) => parse_string_map_arg(Some(v))?,
                None => prev.env.clone(),
            };
            let enabled = match args.get("enabled").and_then(Value::as_bool) {
                Some(on) => on,
                None => context.selection(sid).await?.is_enabled(&prev.id, prev.enabled),
            };

            let (final_url, final_headers, final_cmd, final_env) = if transport == "http" {
                let u = validate_http_url(url.as_deref().unwrap_or(""))?;
                (Some(u), headers, None, None)
            } else {
                let cmd = command.filter(|c| !c.is_empty()).ok_or_else(|| "`command` is required for stdio MCP servers".to_string())?;
                (None, None, Some(cmd), env)
            };

            let updated = crate::mcp::McpServerConfig {
                id: target_name.clone(),
                name: target_name.clone(),
                transport,
                command: final_cmd,
                env: final_env,
                url: final_url,
                headers: final_headers,
                oauth: prev.oauth.clone(),
                enabled,
            };

            if let Some(m) = mgr {
                let _ = m.disconnect(&prev.id).await;
            }
            crate::mcp::rename_file_server(path, &prev.id, &updated)?;
            let selection = if target_name != prev.id || args.get("enabled").and_then(Value::as_bool).is_some() {
                Some(context.set_enabled(sid, path, &updated.id, enabled).await?)
            } else {
                None
            };
            let (conn, connected_ok, tools_n) = if updated.enabled {
                connect_summary(mgr, &updated).await
            } else {
                (Value::Null, false, None)
            };
            let wake_arg = args.get("wake").and_then(|v| v.as_bool());
            let waking = updated.enabled && connected_ok && wake_arg == Some(true);
            let note = if waking {
                "Updated in vicuna.json and connected. Vicuna will automatically start a follow-up turn when this turn finishes so you can use this server right away."
            } else {
                "Updated in vicuna.json. Work engines pick up MCP config changes on the next turn."
            };
            Ok(ManageMcpOutcome {
                value: json!({
                    "ok": true,
                    "action": "edit",
                    "name": updated.name,
                    "transport": updated.transport,
                    "enabled": updated.enabled,
                    "connection": conn,
                    "waking_next_turn": waking,
                    "note": note
                }),
                changed: true,
                wake_server: waking.then(|| (updated.name.clone(), tools_n)),
                enabled: selection,
            })
        }
        "enable" | "disable" => {
            let raw_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if is_fixed_vicuna_mcp(raw_name) {
                return Err("`vicuna-mcp` is a fixed built-in server and cannot be enabled or disabled.".to_string());
            }
            let existing_list = available()?;
            let mut cfg = find_server(&existing_list, raw_name)
                .ok_or_else(|| format!("MCP server `{}` not found", raw_name.trim()))?
                .clone();
            let enable = action == "enable";
            cfg.enabled = enable;
            let selection = context.set_enabled(sid, path, &cfg.id, enable).await?;
            if enable && cfg.id == crate::mcp::BROWSER_ID &&
                !configured()?.iter().any(|s| s.id == cfg.id) {
                context.start_browser().await;
                if let Some(env) = cfg.env.as_mut() {
                    env.insert("LB_SERVER".into(), crate::browser::lb_server_url());
                }
            }
            let (conn, connected_ok, tools_n) = if enable {
                connect_summary(mgr, &cfg).await
            } else {
                (Value::Null, false, None)
            };
            let wake_arg = args.get("wake").and_then(|v| v.as_bool());
            let waking = enable && connected_ok && wake_arg != Some(false);
            let note = if waking {
                "Enabled for this session and connected. Vicuna will start a follow-up turn when this turn finishes."
            } else {
                "Updated for this session. The change applies on the next turn."
            };
            Ok(ManageMcpOutcome {
                value: json!({
                    "ok": true,
                    "action": action,
                    "name": cfg.name,
                    "enabled": cfg.enabled,
                    "connection": conn,
                    "waking_next_turn": waking,
                    "note": note
                }),
                changed: true,
                wake_server: waking.then(|| (cfg.name.clone(), tools_n)),
                enabled: Some(selection),
            })
        }
        "remove" => {
            let raw_name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if is_fixed_vicuna_mcp(raw_name) {
                return Err("`vicuna-mcp` is a fixed built-in server and cannot be removed.".to_string());
            }
            let existing_list = list_servers_in(path)?;
            let prev = find_server(&existing_list, raw_name)
                .ok_or_else(|| format!("MCP server `{}` not found", raw_name.trim()))?
                .clone();
            if let Some(m) = mgr {
                let _ = m.disconnect(&prev.id).await;
            }
            delete_server_in(path, &prev.id)?;
            Ok(ManageMcpOutcome {
                value: json!({
                    "ok": true,
                    "action": "remove",
                    "name": prev.name,
                    "note": "Removed from vicuna.json. Work engines pick up MCP config changes on the next turn."
                }),
                changed: true,
                wake_server: None,
                enabled: None,
            })
        }
        _ => Err("`action` must be one of: list, add, edit, enable, disable, remove".to_string()),
    }
}

async fn manage_mcp(target: Option<&Target>, args: &Value) -> Value {
    let Some((target, context, sid)) = target.and_then(|t| {
        Some((t, t.turn.mcp.get()?, t.session_id.as_deref()?))
    }) else {
        return text_result("manage_mcp needs a Work session", true);
    };
    match manage_mcp_in(None, args, context, sid).await {
        Ok(out) => {
            if let Some(wake) = &out.wake_server {
                if let Ok(mut wakes) = target.turn.mcp_wakes.lock() {
                    if !wakes.iter().any(|(name, _)| name == &wake.0) {
                        wakes.push(wake.clone());
                    }
                }
            }
            if out.changed {
                let _ = target.on_event.send(WorkAgentEvent::McpChanged { enabled: out.enabled, wake: out.wake_server });
            }
            text_result(&out.value.to_string(), false)
        }
        Err(e) => text_result(&e, true),
    }
}

fn skills_target<'a>(target: &'a Target, args: &'a Value) -> Result<&'a str, String> {
    let caller = target.session_id.as_deref().ok_or("manage_skills needs a Work session")?;
    if target.turn.stop.load(Ordering::Relaxed) {
        return Err("The user stopped the turn; nothing was done.".into());
    }
    let session = args["session_id"].as_str().map(str::trim).filter(|id| !id.is_empty()).unwrap_or(caller);
    if session != caller && !target.turn.lead.load(Ordering::Relaxed) {
        return Err("Only the lead session can manage skills for another Work session.".into());
    }
    Ok(session)
}

async fn manage_skills_in(
    root: &std::path::Path,
    config: &std::path::Path,
    db: Option<&sqlx::SqlitePool>,
    target: &Target,
    args: &Value,
) -> Result<Value, String> {
    use crate::skills;
    let sid = skills_target(target, args)?;
    let defaults = skills::defaults_in(config);
    let mut local = None;
    let selection = if let Some(db) = db {
        crate::server_store::session_skills(db, sid).await?.ok_or("Work session no longer exists")?
    } else if target.session_id.as_deref() == Some(sid) {
        local = Some(target.turn.skills_enabled.lock().await);
        local.as_deref().unwrap().clone()
    } else {
        return Err("Targeting another Work session requires a session store".into());
    };
    let action = args["action"].as_str().unwrap_or("").trim();
    if matches!(action, "list" | "search") {
        let items = if action == "search" {
            skills::search_skills_in(root, &selection, args["query"].as_str().unwrap_or(""))?
        } else {
            skills::list_skills_in(root, &selection)?
        };
        return Ok(json!({ "skills": items, "skills_dir": root.to_string_lossy(), "session_id": sid }));
    }
    if !matches!(action, "read" | "add" | "edit" | "enable" | "disable" | "remove") {
        return Err("`action` must be one of list, search, read, add, edit, enable, disable, remove".into());
    }
    let slug = args["slug"].as_str().map(str::trim).filter(|s| !s.is_empty())
        .ok_or_else(|| format!("manage_skills action '{action}' requires `slug`"))?;
    skills::validate_slug(slug)?;
    let existing = skills::read_skill_in(root, &selection, slug);
    if action == "read" {
        return Ok(json!(existing?));
    }
    if action == "remove" {
        skills::remove_skill_in(root, config, slug)?;
        if let Some(db) = db {
            crate::server_store::remove_session_skill(db, slug).await?;
        } else if let Some(local) = local.as_mut() {
            local.remove(slug);
        }
        let _ = target.on_event.send(WorkAgentEvent::SkillsChanged {
            session_id: sid.into(), enabled: None, removed: Some(slug.into()),
        });
        return Ok(json!({ "ok": true, "action": action, "slug": slug }));
    }
    let content = args["content"].as_str();
    let enabled = match action {
        "enable" => Some(true),
        "disable" => Some(false),
        _ => args["enabled"].as_bool(),
    };
    let mut skill = match action {
        "add" => {
            if existing.is_ok() {
                return Err(format!("skill '{slug}' already exists; use action='edit' to update it"));
            }
            let content = content.ok_or("manage_skills action 'add' requires `content`")?;
            json!(skills::save_skill_in(root, &selection, slug, content)?)
        }
        _ => {
            let detail = existing?;
            if let Some(content) = content.filter(|_| action == "edit") {
                json!(skills::save_skill_in(root, &selection, slug, content)?)
            } else if enabled.is_some() {
                json!({
                    "slug": detail.slug, "name": detail.name, "description": detail.description,
                    "path": detail.path, "enabled": detail.enabled,
                })
            } else {
                return Err("manage_skills action 'edit' requires `content` and/or `enabled`".into());
            }
        }
    };
    let mut changed = None;
    if let Some(enabled) = enabled {
        let updated = if let Some(db) = db {
            crate::server_store::set_session_skill(db, sid, slug, enabled, &defaults).await?
        } else {
            let local = local.as_deref_mut().unwrap();
            local.insert(slug.into(), enabled);
            local.clone()
        };
        skill["enabled"] = json!(enabled);
        changed = Some(updated);
    }
    let _ = target.on_event.send(WorkAgentEvent::SkillsChanged {
        session_id: sid.into(), enabled: changed, removed: None,
    });
    Ok(json!({ "ok": true, "action": action, "skill": skill }))
}

async fn manage_skills(target: Option<&Target>, args: &Value) -> Value {
    let result = async {
        let target = target.ok_or("manage_skills needs a Work session")?;
        let root = crate::skills::skills_dir()?;
        let config = crate::skills::skills_config_path()?;
        let server = target.turn.mcp.get().and_then(|mcp| mcp.server.as_ref()?.upgrade());
        let desktop_db = if server.is_none() { db_pool_app().await.ok() } else { None };
        let db = server.as_ref().and_then(|st| st.db.as_ref()).or(desktop_db.as_ref());
        let _write = if let Some(st) = &server { Some(st.chat_lock.lock().await) } else { None };
        manage_skills_in(&root, &config, db, target, args).await
    }.await;
    match result {
        Ok(value) => text_result(&value.to_string(), false),
        Err(error) => text_result(&error, true),
    }
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
        "tools/list" => json!({ "tools": [
            with_caller_arg(tool_schema()),
            with_caller_arg(get_session_schema()),
            with_caller_arg(read_session_schema()),
            with_caller_arg(read_attachment_schema()),
            with_caller_arg(show_image_schema()),
            with_caller_arg(list_sessions_schema()),
            with_caller_arg(overview_schema()),
            with_caller_arg(search_schema()),
            with_caller_arg(usage_schema()),
            manage_mcp_schema(),
            with_caller_arg(manage_skills_schema()),
            with_caller_arg(schedule_schema()),
            with_caller_arg(manage_sessions_schema()),
            with_caller_arg(manage_schedule_schema()),
        ] }),
        "tools/call" => {
            let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match (target, name) {
                (Some(t), TOOL_NAME) => ask(t, &args, client_gone).await,
                (Some(t), GET_SESSION_TOOL) => get_session_id(t).await,
                (t, READ_SESSION_TOOL) => read_session(t, &args).await,
                (t, READ_ATTACHMENT_TOOL) => read_attachment(t, &args).await,
                (Some(t), SHOW_IMAGE_TOOL) => show_image(t, &args),
                (Some(t), SCHEDULE_TOOL) => schedule_wakeup(t, &args, client_gone).await,
                (Some(t), MANAGE_SESSIONS_TOOL) => manage_sessions(t, &args, client_gone).await,
                (Some(t), MANAGE_SCHEDULE_TOOL) => manage_schedule(t, &args, client_gone).await,
                (t, LIST_SESSIONS_TOOL) => if may_read_other(t, "__all__").await { list_sessions(&args).await } else { text_result("This session is not allowed to read other sessions (Permissions… in the session menu).", true) },
                (t, OVERVIEW_TOOL) => if may_read_other(t, "__all__").await { session_overview(&args).await } else { text_result("This session is not allowed to read other sessions (Permissions… in the session menu).", true) },
                (t, SEARCH_TOOL) => if remote_server(&args).is_some() || may_read_other(t, "__all__").await { search_sessions(t, &args).await } else { text_result("This session is not allowed to read other sessions (Permissions… in the session menu).", true) },
                (_, USAGE_TOOL) => usage_status(&args).await,
                (_, MANAGE_MCP_TOOL) => manage_mcp(target, &args).await,
                (_, MANAGE_SKILLS_TOOL) => manage_skills(target, &args).await,
                (None, TOOL_NAME) => text_result("No Vicuna Work turn is running for this chat, so the question cannot be shown.", true),
                (None, GET_SESSION_TOOL | SHOW_IMAGE_TOOL | SCHEDULE_TOOL | MANAGE_SESSIONS_TOOL | MANAGE_SCHEDULE_TOOL) => text_result("No Vicuna Work turn is running for this chat.", true),
                _ => return Some(rpc_error(id, -32602, &format!("unknown tool: {name}"))),
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
        let target = target_for_call(&registry, &token, target, &msg);
        return stream_call(&msg, target.as_ref(), gone, &mut write_half).await;
    }
    let reply = match &msg {
        Value::Array(batch) => {
            let gone = futures_util::future::FutureExt::shared(Box::pin(gone));
            let mut out = Vec::new();
            for m in batch {
                let target = target_for_call(&registry, &token, target.clone(), m);
                if let Some(r) = handle_rpc(m, target.as_ref(), gone.clone()).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => {
            let target = target_for_call(&registry, &token, target, m);
            handle_rpc(m, target.as_ref(), gone).await
        }
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

    #[test]
    fn nonlead_send_report_defaults_off_with_wakeup_hint() {
        let (fwd, note) = forwarded_session_args(&json!({"action":"send","session_id":"s","message":"m"}), "send", false, false);
        assert_eq!(fwd["report"], false);
        let note = note.unwrap();
        assert!(note.contains("schedule_wakeup") && note.contains("will not be sent back"), "{note}");
        assert!(!note.contains("Loop-breaker"));
    }

    #[test]
    fn nonlead_send_may_opt_into_report_local_and_remote() {
        for args in [
            json!({"action":"send","session_id":"s","message":"m","report":true}),
            json!({"action":"send","server":"VPS","session_id":"s","message":"m","report":true}),
        ] {
            let (fwd, note) = forwarded_session_args(&args, "send", false, false);
            assert_eq!(fwd["report"], true, "{args}");
            assert_eq!(note, None, "{args}");
        }
    }

    #[test]
    fn report_woken_nonlead_turn_cannot_nest_report_true() {
        for args in [
            json!({"action":"send","session_id":"s","message":"m","report":true}),
            json!({"action":"send","server":"VPS","session_id":"s","message":"m","report":true}),
        ] {
            let (fwd, note) = forwarded_session_args(&args, "send", false, true);
            assert_eq!(fwd["report"], false, "downgraded: {args}");
            let note = note.unwrap();
            assert!(note.contains("Loop-breaker") && note.contains("woken by a report callback"), "{note}");
            assert!(note.contains("schedule_wakeup"), "{note}");
        }
        // a woken non-lead turn without report: true just gets the plain hint
        let (_, note) = forwarded_session_args(&json!({"action":"send"}), "send", false, true);
        assert!(!note.unwrap().contains("Loop-breaker"));
    }

    #[test]
    fn report_woken_lead_still_delegates_with_report() {
        assert_eq!(effective_report(&json!({}), true, true), (true, false));
        assert_eq!(effective_report(&json!({"report":true}), true, true), (true, false));
        let (fwd, note) = forwarded_session_args(&json!({"action":"send","server":"VPS","report":true}), "send", true, true);
        assert_eq!((fwd["report"].clone(), note), (json!(true), None));
    }

    #[test]
    fn explicit_report_false_gets_wakeup_hint() {
        for lead in [true, false] {
            let (fwd, note) = forwarded_session_args(&json!({"action":"send","report":false}), "send", lead, false);
            assert_eq!(fwd["report"], false);
            assert!(note.unwrap().contains("schedule_wakeup"), "lead={lead}");
        }
    }

    #[test]
    fn session_permission_defaults_and_explicit_overrides() {
        assert!(effective_permission(&json!({}), "read"));
        assert!(!effective_permission(&json!({}), "message"));
        assert!(effective_permission(&json!({"role":"lead"}), "message"));
        assert!(!effective_permission(&json!({"role":"lead", "permissions":{"message":false}}), "message"));
        assert!(!effective_permission(&json!({"permissions":{"read":false}}), "read"));
        assert!(!effective_permission(&json!({"role":"lead"}), "remote"), "remote is opt-in, even for the lead");
        assert!(effective_permission(&json!({"role":"lead", "permissions":{"remote":true}}), "remote"));
        assert!(can_read_other(Some("same"), "same", None), "own session reads are unconditional");
        assert!(can_read_other(None, "other", None), "turns without a session id retain access");
        assert!(!can_read_other(Some("worker"), "other", None), "a missing permission row denies");
        assert!(!can_read_other(Some("worker"), "other", Some(false)));
        assert!(can_read_other(Some("worker"), "other", Some(true)));
        assert!(can_manage_permissions(true));
        assert!(!can_manage_permissions(false));
        assert_eq!(forwarded_session_args(&json!({"action":"send", "report":true}), "send", false, false).0["report"], true);
        assert_eq!(forwarded_session_args(&json!({"action":"send"}), "send", true, false).0["report"], true);
        assert_eq!(forwarded_session_args(&json!({"action":"stop", "report":true}), "stop", false, false), (json!({"action":"stop", "report":true}), None));
        assert!(can_manage_sessions_action(false, false, "send", true));
        assert!(!can_manage_sessions_action(false, false, "send", false));
        for action in ["create", "stop", "delete", "archive", "unarchive", "status", "list"] {
            assert!(!can_manage_sessions_action(false, false, action, true), "{action} stays lead-only");
            assert!(can_manage_sessions_action(true, false, action, false), "lead keeps {action}");
        }
        assert!(can_manage_sessions_action(false, true, "set_workspace", false));
    }
    use crate::ipc::InvokeResponseBody;

    #[test]
    fn calls_route_to_the_named_caller_session() {
        let reg: SharedRegistry = Arc::default();
        let (ch, _) = capture();
        // lead then worker on the same per-workspace route: the worker is newest
        let lead = register_on(&reg, 1, Some("opencode acp|/w"), "/w", Some("lead"), Arc::new(TurnState::new()), ch.clone());
        let _worker = register_on(&reg, 1, Some("opencode acp|/w"), "/w", Some("worker"), Arc::new(TurnState::new()), ch.clone());
        // another spelling of the folder: a route with no live turn
        let other = register_on(&reg, 1, Some("opencode acp|/w/"), "/w/", Some("x"), Arc::new(TurnState::new()), ch.clone());
        let other_token = other.token.clone();
        drop(other);
        let call = |sid: Option<&str>| {
            let mut args = json!({});
            if let Some(s) = sid { args[CALLER_ARG] = json!(s); }
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": SCHEDULE_TOOL, "arguments": args } })
        };
        let pick = |token: &str, sid| {
            let def = target_for(&reg, token).flatten();
            target_for_call(&reg, token, def, &call(sid)).and_then(|t| t.session_id)
        };
        assert_eq!(pick(&lead.token, None).as_deref(), Some("worker"), "old behaviour without the arg");
        assert_eq!(pick(&lead.token, Some("lead")).as_deref(), Some("lead"));
        assert_eq!(pick(&other_token, Some("lead")).as_deref(), Some("lead"), "found on another route");
        assert_eq!(pick(&other_token, None), None);
        assert_eq!(pick(&lead.token, Some("gone")).as_deref(), Some("worker"));
    }

    const SAVED_NAMES: [&str; 3] = ["Deck Remote Server", "Oracle VPS", "Deck Family"];

    fn no_saved_names(text: &str) {
        for n in SAVED_NAMES { assert!(!text.contains(n), "leaked {n}: {text}"); }
        assert!(!text.contains("Saved servers"), "{text}");
    }

    fn gate_text(v: &Value) -> (String, bool) {
        (v.pointer("/content/0/text").and_then(Value::as_str).unwrap_or("").to_string(), v["isError"] == true)
    }

    #[test]
    fn remote_gate_decision_matrix() {
        // read/search: the permission alone is enough (lead not needed)
        assert!(remote_gate_decision(true, true, false, false).is_ok());
        assert!(remote_gate_decision(true, true, true, false).is_ok());
        assert_eq!(remote_gate_decision(true, false, true, false), Err(REMOTE_READ_DENIED));
        assert_eq!(remote_gate_decision(false, true, false, false), Err(REMOTE_READ_DENIED));
        // manage: lead AND permission
        assert!(remote_gate_decision(true, true, true, true).is_ok());
        assert_eq!(remote_gate_decision(true, true, false, true), Err(REMOTE_MANAGE_DENIED));
        assert_eq!(remote_gate_decision(true, false, true, true), Err(REMOTE_MANAGE_DENIED));
        assert_eq!(remote_gate_decision(false, true, true, true), Err(REMOTE_MANAGE_DENIED));
        for e in [REMOTE_READ_DENIED, REMOTE_SEND_DENIED, REMOTE_MANAGE_DENIED] {
            assert!(e.contains("\"Use sessions on other servers\""));
            no_saved_names(e);
        }
        no_saved_names(READ_OTHER_DENIED);
    }

    #[test]
    fn remote_session_decision_matrix() {
        let lead_only = ["create", "stop", "archive", "unarchive", "delete", "set_workspace", "permissions"];

        // Non-lead with remote + message + read: servers/list/status/send allowed; lead-only blocked
        assert!(remote_session_decision(true, true, false, true, true, "servers").is_ok());
        assert!(remote_session_decision(true, true, false, true, true, "list").is_ok());
        assert!(remote_session_decision(true, true, false, true, true, "status").is_ok());
        assert!(remote_session_decision(true, true, false, true, true, "send").is_ok());
        for action in lead_only {
            assert_eq!(remote_session_decision(true, true, false, true, true, action), Err(REMOTE_MANAGE_DENIED), "{action}");
        }

        // Non-lead with remote + read only (no message): servers/list/status allowed; send & lead-only blocked
        assert!(remote_session_decision(true, true, false, true, false, "servers").is_ok());
        assert!(remote_session_decision(true, true, false, true, false, "list").is_ok());
        assert!(remote_session_decision(true, true, false, true, false, "status").is_ok());
        assert_eq!(remote_session_decision(true, true, false, true, false, "send"), Err(REMOTE_SEND_DENIED));
        for action in lead_only {
            assert_eq!(remote_session_decision(true, true, false, true, false, action), Err(REMOTE_MANAGE_DENIED), "{action}");
        }

        // Non-lead with remote + message but read=false: servers & send allowed; list/status blocked
        assert!(remote_session_decision(true, true, false, false, true, "servers").is_ok());
        assert!(remote_session_decision(true, true, false, false, true, "send").is_ok());
        assert_eq!(remote_session_decision(true, true, false, false, true, "list"), Err(READ_OTHER_DENIED));
        assert_eq!(remote_session_decision(true, true, false, false, true, "status"), Err(READ_OTHER_DENIED));

        // Non-lead with message + read only (no remote): all remote actions blocked
        for action in ["servers", "list", "status"] {
            assert_eq!(remote_session_decision(true, false, false, true, true, action), Err(REMOTE_READ_DENIED), "{action}");
        }
        assert_eq!(remote_session_decision(true, false, false, true, true, "send"), Err(REMOTE_SEND_DENIED));
        for action in lead_only {
            assert_eq!(remote_session_decision(true, false, false, true, true, action), Err(REMOTE_MANAGE_DENIED), "{action}");
        }

        // Lead without remote: all remote actions still blocked
        for action in ["servers", "list", "status"] {
            assert_eq!(remote_session_decision(true, false, true, true, true, action), Err(REMOTE_READ_DENIED), "{action}");
        }
        assert_eq!(remote_session_decision(true, false, true, true, true, "send"), Err(REMOTE_SEND_DENIED));
        for action in lead_only {
            assert_eq!(remote_session_decision(true, false, true, true, true, action), Err(REMOTE_MANAGE_DENIED), "{action}");
        }

        // Lead with remote: all actions pass the remote gate (read=false still gates list/status)
        for action in ["servers", "list", "status", "send", "create", "stop", "archive", "unarchive", "delete"] {
            assert!(remote_session_decision(true, true, true, true, false, action).is_ok(), "lead + remote: {action}");
        }
        assert_eq!(remote_session_decision(true, true, true, false, true, "list"), Err(READ_OTHER_DENIED));

        // Missing session id: always refused
        for action in ["servers", "list", "status", "send", "create"] {
            let err = remote_session_decision(false, true, true, true, true, action).unwrap_err();
            no_saved_names(err);
        }

        // Non-lead remote send may opt into report: true; server is preserved
        let remote = json!({"action": "send", "server": "Oracle VPS", "session_id": "s1", "message": "hi", "report": true});
        let (fwd, note) = forwarded_session_args(&remote, "send", false, false);
        assert_eq!(fwd["report"], true);
        assert_eq!(fwd["server"], "Oracle VPS");
        assert_eq!(note, None);
        let (fwd_lead, _) = forwarded_session_args(&remote, "send", true, false);
        assert_eq!(fwd_lead["report"], true);
    }

    #[test]
    fn remote_calls_are_recognized() {
        assert!(is_remote_call(SEARCH_TOOL, &json!({"server": "Oracle VPS"})));
        assert!(is_remote_call(READ_SESSION_TOOL, &json!({"server": "nope"})));
        assert!(is_remote_call(MANAGE_SESSIONS_TOOL, &json!({"action": "servers"})));
        assert!(is_remote_call(MANAGE_SESSIONS_TOOL, &json!({"action": "list", "server": "vps"})));
        assert!(is_remote_call(MANAGE_SESSIONS_TOOL, &json!({"action": "send", "server": "vps", "session_id": "s1", "message": "hi"})));
        assert!(!is_remote_call(SEARCH_TOOL, &json!({"server": "  "})));
        assert!(!is_remote_call(READ_SESSION_TOOL, &json!({})));
        assert!(!is_remote_call(LIST_SESSIONS_TOOL, &json!({"server": "vps"})));
    }

    #[tokio::test]
    async fn unauthorized_remote_read_and_search_are_refused_without_names() {
        let (ch, _) = capture();
        let worker = Target { seq: 0, workspace: ws(), session_id: Some("worker-no-perm".into()), turn: Arc::new(TurnState::new()), on_event: ch };
        let none: Option<&Target> = None;
        // valid saved names and unknown ones alike: the gate answers first
        for server in ["Oracle VPS", "Deck Family", "vps", "no-such-server", "https://evil.example"] {
            for t in [Some(&worker), none] {
                let (text, err) = gate_text(&search_sessions(t, &json!({"server": server, "query": "test"})).await);
                assert!(err, "{text}");
                assert_eq!(text, REMOTE_READ_DENIED);
                no_saved_names(&text);
                let (text, err) = gate_text(&read_session(t, &json!({"server": server, "session_id": "abc"})).await);
                assert!(err, "{text}");
                assert_eq!(text, REMOTE_READ_DENIED);
                no_saved_names(&text);
            }
        }
    }

    #[tokio::test]
    async fn unauthorized_remote_manage_is_refused_without_names() {
        for lead in [false, true] {
            let (ch, seen) = capture();
            let turn = Arc::new(TurnState::new());
            turn.lead.store(lead, Ordering::Relaxed);
            let t = Target { seq: 0, workspace: ws(), session_id: Some("lead-no-perm".into()), turn, on_event: ch };
            for (args, expected) in [
                (json!({"action": "servers"}), REMOTE_READ_DENIED),
                (json!({"action": "list", "server": "Oracle VPS"}), REMOTE_READ_DENIED),
                (json!({"action": "status", "server": "unknown", "session_id": "x"}), REMOTE_READ_DENIED),
                (json!({"action": "send", "server": "Oracle VPS", "session_id": "x", "message": "hi"}), REMOTE_SEND_DENIED),
                (json!({"action": "create", "server": "Oracle VPS", "workspace": "/tmp"}), REMOTE_MANAGE_DENIED),
                (json!({"action": "stop", "server": "Oracle VPS", "session_id": "x"}), REMOTE_MANAGE_DENIED),
                (json!({"action": "archive", "server": "Oracle VPS", "session_id": "x"}), REMOTE_MANAGE_DENIED),
                (json!({"action": "unarchive", "server": "Oracle VPS", "session_id": "x"}), REMOTE_MANAGE_DENIED),
            ] {
                let (text, err) = gate_text(&manage_sessions(&t, &args, std::future::pending::<()>()).await);
                assert!(err, "{text}");
                assert_eq!(text, expected, "lead={lead} args={args}");
                no_saved_names(&text);
            }
            assert!(seen.lock().unwrap().is_empty(), "nothing may reach the window");
        }
    }

    #[tokio::test]
    async fn remote_permission_reads_the_session_row() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE chats (id TEXT PRIMARY KEY, extra TEXT)").execute(&pool).await.unwrap();
        for (id, extra) in [
            ("ok", r#"{"permissions":{"remote":true}}"#),
            ("worker-remote-msg", r#"{"permissions":{"remote":true,"message":true}}"#),
            ("worker-remote-noread", r#"{"permissions":{"remote":true,"read":false}}"#),
            ("worker-msg-only", r#"{"permissions":{"message":true}}"#),
            ("lead-ok", r#"{"role":"lead","permissions":{"remote":true}}"#),
            ("lead-plain", r#"{"role":"lead"}"#),
            ("off", r#"{"permissions":{"remote":false,"read":true}}"#),
        ] {
            sqlx::query("INSERT INTO chats (id, extra) VALUES (?, ?)").bind(id).bind(extra).execute(&pool).await.unwrap();
        }
        assert!(may_remote_in(&pool, "ok").await);
        assert!(may_remote_in(&pool, "worker-remote-msg").await);
        assert!(session_permission_in(&pool, "worker-remote-msg", "message").await);
        assert!(session_permission_in(&pool, "worker-remote-msg", "read").await);
        assert!(!session_permission_in(&pool, "worker-remote-noread", "read").await);
        assert!(!may_remote_in(&pool, "worker-msg-only").await);
        assert!(session_permission_in(&pool, "worker-msg-only", "message").await);
        assert!(may_remote_in(&pool, "lead-ok").await);
        assert!(!may_remote_in(&pool, "lead-plain").await, "opt-in even for the lead");
        assert!(!may_remote_in(&pool, "off").await);
        assert!(!may_remote_in(&pool, "missing").await);
        assert!(!session_permission_in(&pool, "missing", "read").await, "missing row must not default read to true");
        // and the decision lets the permitted ones through
        assert!(remote_gate_decision(true, may_remote_in(&pool, "ok").await, false, false).is_ok());
        assert!(remote_gate_decision(true, may_remote_in(&pool, "lead-ok").await, true, true).is_ok());

        let decide = |id: &'static str, lead: bool, action: &'static str| {
            let pool = pool.clone();
            async move {
                let r = may_remote_in(&pool, id).await;
                let rd = session_permission_in(&pool, id, "read").await;
                let msg = session_permission_in(&pool, id, "message").await;
                remote_session_decision(true, r, lead, rd, msg, action)
            }
        };
        assert!(decide("worker-remote-msg", false, "servers").await.is_ok());
        assert!(decide("worker-remote-msg", false, "list").await.is_ok());
        assert!(decide("worker-remote-msg", false, "status").await.is_ok());
        assert!(decide("worker-remote-msg", false, "send").await.is_ok());
        assert_eq!(decide("worker-remote-msg", false, "create").await, Err(REMOTE_MANAGE_DENIED));
        assert!(decide("ok", false, "servers").await.is_ok());
        assert!(decide("ok", false, "list").await.is_ok());
        assert_eq!(decide("ok", false, "send").await, Err(REMOTE_SEND_DENIED));
        assert_eq!(decide("worker-remote-noread", false, "list").await, Err(READ_OTHER_DENIED));
        assert_eq!(decide("worker-msg-only", false, "send").await, Err(REMOTE_SEND_DENIED));
        assert_eq!(decide("lead-plain", true, "servers").await, Err(REMOTE_READ_DENIED));
        assert!(decide("lead-ok", true, "create").await.is_ok());
    }

    #[test]
    fn caller_id_cannot_borrow_another_routes_session_for_remote_calls() {
        let reg: SharedRegistry = Arc::default();
        let (ch, _) = capture();
        let lead = register_on(&reg, 1, Some("claude|a"), "/a", Some("lead"), Arc::new(TurnState::new()), ch.clone());
        let worker = register_on(&reg, 1, Some("claude|b"), "/b", Some("worker"), Arc::new(TurnState::new()), ch.clone());
        let call = |tool: &str, mut args: Value| {
            args[CALLER_ARG] = json!("lead");
            json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": tool, "arguments": args } })
        };
        let pick = |msg: &Value| target_for_call(&reg, &worker.token, target_for(&reg, &worker.token).flatten(), msg).and_then(|t| t.session_id);
        for msg in [
            call(SEARCH_TOOL, json!({"server": "Oracle VPS", "query": "q"})),
            call(READ_SESSION_TOOL, json!({"server": "Oracle VPS", "session_id": "s"})),
            call(MANAGE_SESSIONS_TOOL, json!({"action": "servers"})),
            call(MANAGE_SESSIONS_TOOL, json!({"action": "list", "server": "Oracle VPS"})),
            call(MANAGE_SESSIONS_TOOL, json!({"action": "send", "server": "Oracle VPS", "session_id": "s", "message": "hi"})),
        ] {
            assert_eq!(pick(&msg).as_deref(), Some("worker"), "{msg}");
        }
        // ordinary calls keep the cross-route lookup (#76)
        assert_eq!(pick(&call(SCHEDULE_TOOL, json!({}))).as_deref(), Some("lead"));
        // on the lead's own route the lead still resolves for remote calls
        let own = target_for_call(&reg, &lead.token, None, &call(SEARCH_TOOL, json!({"server": "x", "query": "q"}))).and_then(|t| t.session_id);
        assert_eq!(own.as_deref(), Some("lead"));
    }

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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        wait_event(seen, "askrequest").await
    }

    async fn wait_event(seen: &Arc<Mutex<Vec<Value>>>, kind: &str) -> Value {
        for _ in 0..500 {
            if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == kind).cloned() {
                return e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no {kind} event");
    }

    /// a real directory to run commands in
    fn ws() -> String {
        std::env::temp_dir().to_string_lossy().to_string()
    }

    fn result_text(r: &Value) -> String {
        r["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    fn wake_call(args: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 11, "method": "tools/call", "params": { "name": "schedule_wakeup", "arguments": args } })
    }

    #[test]
    fn schedule_args_need_a_message_and_exactly_one_time() {
        assert!(parse_schedule_args(&json!({ "in_minutes": 5 })).is_err());
        assert!(parse_schedule_args(&json!({ "message": "x" })).is_err());
        assert!(parse_schedule_args(&json!({ "message": "x", "in_minutes": 5, "at": "09:00" })).is_err());
        assert!(parse_schedule_args(&json!({ "message": "x", "in_minutes": 0.2 })).is_err());
        assert!(parse_schedule_args(&json!({ "message": "x", "in_minutes": 50000 })).is_err());
        assert_eq!(parse_schedule_args(&json!({ "message": " check CI ", "in_minutes": 15 })).unwrap(), ("check CI".to_string(), None, Some(15.0)));
        assert_eq!(parse_schedule_args(&json!({ "message": "x", "at": "09:30" })).unwrap().1.as_deref(), Some("09:30"));
    }

    #[tokio::test]
    async fn schedule_wakeup_asks_the_window_and_reports_its_answer() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, &ws(), Some("sess-1"), turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, wake_call(json!({ "message": "check the deploy", "in_minutes": 20 }))).await });
        // the window gets the request with the raw spec, resolves it, answers
        let ev = loop {
            if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == "schedulewakeup").cloned() {
                break e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(ev["message"], "check the deploy");
        assert_eq!(ev["in_minutes"], 20.0);
        assert!(ev["at"].is_null());
        crate::work_agent::agent_answer(&turn, ev["req_id"].as_str().unwrap(), json!({ "ok": true, "when": "Sun 16 Nov, 09:30" }).to_string()).unwrap();
        let text = result_text(&pending.await.unwrap().1.unwrap());
        assert!(text.contains("at Sun 16 Nov, 09:30"), "{text}");

        // a refusal from the window comes back as a tool error
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, wake_call(json!({ "message": "x", "at": "yesterday" }))).await });
        let ev = loop {
            let found = seen.lock().unwrap().iter().filter(|e| e["type"] == "schedulewakeup").nth(1).cloned();
            if let Some(e) = found {
                break e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        crate::work_agent::agent_answer(&turn, ev["req_id"].as_str().unwrap(), json!({ "ok": false, "error": "that time is in the past" }).to_string()).unwrap();
        let r = pending.await.unwrap().1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(result_text(&r).contains("that time is in the past"));
    }

    #[tokio::test]
    async fn schedule_wakeup_needs_a_session() {
        let (ch, _seen) = capture();
        let reg = register(None, &ws(), None, Arc::new(TurnState::new()), ch).await.unwrap();
        let r = post(&reg.url, wake_call(json!({ "message": "x", "in_minutes": 5 }))).await.1.unwrap();
        assert!(result_text(&r).contains("no Vicuna session"), "{}", result_text(&r));
    }

    fn sess_call(args: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": 12, "method": "tools/call", "params": { "name": "manage_sessions", "arguments": args } })
    }

    #[tokio::test]
    async fn manage_sessions_is_lead_only_and_asks_the_window() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, &ws(), Some("lead-1"), turn.clone(), ch).await.unwrap();
        // not the lead: refused without bothering the window
        let r = post(&reg.url, sess_call(json!({ "action": "list" }))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(result_text(&r).contains("lead session"), "{}", result_text(&r));
        assert!(!seen.lock().unwrap().iter().any(|e| e["type"] == "sessioncontrol"));
        // bad action
        turn.lead.store(true, Ordering::Relaxed);
        let r = post(&reg.url, sess_call(json!({ "action": "explode" }))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        // lead: the window performs it and its text is the result
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, sess_call(json!({ "action": "create", "engine": "codex", "message": "hi" }))).await });
        let ev = loop {
            if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == "sessioncontrol").cloned() {
                break e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(ev["action"], "create");
        assert_eq!(ev["args"]["engine"], "codex");
        crate::work_agent::agent_answer(&turn, ev["req_id"].as_str().unwrap(), json!({ "ok": true, "text": "Created w-1" }).to_string()).unwrap();
        let r = pending.await.unwrap().1.unwrap();
        assert_eq!(result_text(&r), "Created w-1");
    }

    #[tokio::test]
    async fn manage_sessions_dead_channel_stops_the_turn_without_dispatch() {
        let ch = Channel::<WorkAgentEvent>::new(|_| Err(crate::ipc::io_error(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "closed"))));
        let turn = Arc::new(TurnState::new());
        turn.lead.store(true, Ordering::Relaxed);
        let target = Target { seq: 0, workspace: ws(), session_id: Some("lead".into()), turn: turn.clone(), on_event: ch };
        let result = manage_sessions(&target, &json!({ "action": "create", "engine": "codex", "message": "go" }), std::future::pending()).await;
        assert!(turn.stop.load(Ordering::Relaxed));
        assert!(turn.ask_pending.lock().unwrap().is_empty());
        assert!(result["content"][0]["text"].as_str().unwrap().contains("not dispatched"));
    }

    #[tokio::test]
    async fn manage_sessions_disconnect_warns_about_side_effects_and_cleans_pending() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        turn.lead.store(true, Ordering::Relaxed);
        let target = Target { seq: 0, workspace: ws(), session_id: Some("lead".into()), turn: turn.clone(), on_event: ch };
        let result = manage_sessions(&target, &json!({ "action": "create" }), async {}).await;
        assert_eq!(result["isError"], true);
        let text = result["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("MCP client disconnected"));
        assert!(text.contains("action may have run"));
        assert!(seen.lock().unwrap().iter().any(|e| e["type"] == "sessioncontrol"));
        assert!(turn.ask_pending.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn manage_sessions_forwards_archive_without_a_long_wait() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        turn.lead.store(true, Ordering::Relaxed);
        let reg = register(None, &ws(), Some("lead-1"), turn.clone(), ch).await.unwrap();
        for action in ["archive", "unarchive"] {
            let url = reg.url.clone();
            let pending = tokio::spawn(async move { post(&url, sess_call(json!({ "action": action, "session_id": "w-1" }))).await });
            let ev = loop {
                if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == "sessioncontrol" && e["action"] == action).cloned() {
                    break e;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            };
            assert_eq!(ev["args"]["session_id"], "w-1");
            crate::work_agent::agent_answer(&turn, ev["req_id"].as_str().unwrap(), json!({ "ok": true, "text": format!("{action}d") }).to_string()).unwrap();
            let r = pending.await.unwrap().1.unwrap();
            assert_eq!(r["result"]["isError"], false);
        }
    }

    #[tokio::test]
    async fn set_workspace_on_own_session_needs_no_lead() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, &ws(), Some("s-1"), turn.clone(), ch).await.unwrap();
        // another session still needs the lead role
        let r = post(&reg.url, sess_call(json!({ "action": "set_workspace", "session_id": "other", "workspace": "/tmp" }))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(result_text(&r).contains("lead session"), "{}", result_text(&r));
        // own session: forwarded to the window
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, sess_call(json!({ "action": "set_workspace", "workspace": "/tmp" }))).await });
        let ev = loop {
            if let Some(e) = seen.lock().unwrap().iter().find(|e| e["type"] == "sessioncontrol").cloned() {
                break e;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(ev["action"], "set_workspace");
        crate::work_agent::agent_answer(&turn, ev["req_id"].as_str().unwrap(), json!({ "ok": true, "text": "Moved" }).to_string()).unwrap();
        assert_eq!(result_text(&pending.await.unwrap().1.unwrap()), "Moved");
    }

    #[tokio::test]
    async fn answer_later_returns_without_an_answer() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
        let url = reg.url.clone();
        let pending = tokio::spawn(async move { post(&url, call("Which db?", json!({}))).await });
        let ev = wait_ask(&seen).await;
        crate::work_agent::agent_answer(&turn, ev["ask_id"].as_str().unwrap(), ASK_LATER.into()).unwrap();
        let r = pending.await.unwrap().1.unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(result_text(&r).starts_with("The user will answer this later"), "{r}");
    }

    /// agy times MCP calls out at 3 min: the call polls, the repeat call
    /// re-attaches to the same card (no second card), and the answer lands.
    #[tokio::test]
    async fn polled_ask_reattaches_to_the_same_card() {
        let (ch, seen) = capture();
        let turn = Arc::new(TurnState::new());
        turn.ask_poll_secs.store(1, Ordering::Relaxed);
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
        let r = post(&reg.url, call("Which  db?", json!({}))).await.1.unwrap();
        assert_eq!(r["result"]["isError"], false);
        assert!(result_text(&r).contains("Call ask_user again"), "{r}");
        let ev = wait_ask(&seen).await;
        let id = ev["ask_id"].as_str().unwrap().to_string();
        let url = reg.url.clone();
        let again = tokio::spawn(async move { post(&url, call("Which db?", json!({}))).await });
        // wait until the repeat call has re-attached (parked list empties)
        for _ in 0..100 {
            if turn.ask_parked.lock().unwrap().is_empty() { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        crate::work_agent::agent_answer(&turn, &id, "Postgres".into()).unwrap();
        let r = again.await.unwrap().1.unwrap();
        assert_eq!(result_text(&r), "The user answered: Postgres");
        let cards = seen.lock().unwrap().iter().filter(|e| e["type"] == "askrequest").count();
        assert_eq!(cards, 1, "the repeat call must not raise a second card");
    }

    #[tokio::test]
    async fn handshake_lists_ask_user() {
        let (ch, _) = capture();
        let reg = register(None, &ws(), None, Arc::new(TurnState::new()), ch).await.unwrap();
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
        let names: Vec<&str> = tools.as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, vec![TOOL_NAME, GET_SESSION_TOOL, READ_SESSION_TOOL, READ_ATTACHMENT_TOOL, SHOW_IMAGE_TOOL, LIST_SESSIONS_TOOL, OVERVIEW_TOOL, SEARCH_TOOL, USAGE_TOOL, MANAGE_MCP_TOOL, MANAGE_SKILLS_TOOL, SCHEDULE_TOOL, MANAGE_SESSIONS_TOOL, MANAGE_SCHEDULE_TOOL]);
        let properties = &tools.as_array().unwrap().iter()
            .find(|tool| tool["name"] == MANAGE_SKILLS_TOOL).unwrap()["inputSchema"]["properties"];
        for arg in [CALLER_ARG, "session_id", "query"] {
            assert_eq!(properties[arg]["type"], "string");
        }
        assert!(properties["action"]["enum"].as_array().unwrap().contains(&json!("search")));
        assert!(!names.contains(&"run"), "the run tool was removed");
        // a stale client calling it gets a protocol error, not a crash
        let (_, r) = post(&reg.url, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": { "name": "run", "arguments": { "command": "true" } } })).await;
        assert!(r.unwrap()["error"]["message"].as_str().unwrap().contains("unknown tool: run"));
    }

    #[tokio::test]
    async fn unknown_token_is_404_and_get_is_405() {
        let (ch, _) = capture();
        let reg = register(None, &ws(), None, Arc::new(TurnState::new()), ch).await.unwrap();
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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        let reg = register(None, &ws(), None, turn.clone(), ch).await.unwrap();
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
        let r1 = register(Some(&key), &ws(), None, t1.clone(), ch1).await.unwrap();
        let r2 = register(Some(&key), &ws(), None, t2.clone(), ch2).await.unwrap();
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

    #[tokio::test]
    async fn get_session_id_returns_registered_session() {
        let (ch, _) = capture();
        let turn = Arc::new(TurnState::new());
        let reg = register(None, "/tmp/ws-test", Some("sess-123"), turn, ch).await.unwrap();
        let resp = post(&reg.url, json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": GET_SESSION_TOOL, "arguments": {} }
        }))
        .await
        .1
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let txt = resp["result"]["content"][0]["text"].as_str().unwrap();
        let v: Value = serde_json::from_str(txt).unwrap();
        assert_eq!(v["session_id"], "sess-123");
        assert_eq!(v["workspace"], "/tmp/ws-test");
    }

    async fn attachment_pool() -> sqlx::SqlitePool {
        use base64::Engine as _;
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE chats (id TEXT PRIMARY KEY, title TEXT NOT NULL, mode TEXT, workspace TEXT, goal TEXT, extra TEXT)").execute(&pool).await.unwrap();
        sqlx::query("CREATE TABLE messages (chat_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, thinking TEXT NOT NULL, model TEXT, idx INTEGER NOT NULL, images TEXT, context_cleared INTEGER)").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO chats (id, title, extra) VALUES ('s1', 'T', NULL), ('noread', 'N', '{\"permissions\":{\"read\":false}}'), ('remote-only', 'R', '{\"permissions\":{\"remote\":true,\"read\":false}}'), ('remote-read', 'RR', '{\"permissions\":{\"remote\":true}}'), ('lead', 'L', '{\"role\":\"lead\",\"permissions\":{\"read\":false}}')").execute(&pool).await.unwrap();
        let png = base64::engine::general_purpose::STANDARD.encode([0x89u8, b'P', b'N', b'G']);
        let imgs = json!([format!("data:image/png;base64,{png}"), "data:text/html;base64,PGI+"]).to_string();
        sqlx::query("INSERT INTO messages (chat_id, role, content, thinking, idx, images) VALUES ('s1', 'user', 'look', '', 0, $1), ('s1', 'assistant', 'ok', '', 1, NULL), ('s1', 'user', 'empty', '', 2, '[]')")
            .bind(imgs).execute(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn read_session_lists_attachment_metadata_without_base64() {
        use base64::Engine as _;
        let pool = attachment_pool().await;
        let dir = crate::attachments::test_dir("rs-up");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("notes.txt"), b"hello").unwrap();
        let v = read_session_page(&pool, "s1", &json!({}), Some(&dir)).await.unwrap();
        let text = v.to_string();
        assert!(!text.contains("base64") && !text.contains(&base64::engine::general_purpose::STANDARD.encode([0x89u8, b'P', b'N', b'G'])), "{text}");
        assert_eq!(v["messages"][0]["images"], json!([{"index":0,"mime":"image/png","bytes":4},{"index":1,"mime":"text/html","bytes":3}]));
        assert!(v["messages"][1].get("images").is_none());
        assert!(v["messages"][2].get("images").is_none(), "empty arrays are omitted");
        assert_eq!(v["uploads"], json!([{"name":"notes.txt","bytes":5}]));
        let none = read_session_page(&pool, "s1", &json!({}), Some(&dir.join("missing"))).await.unwrap();
        assert!(none.get("uploads").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn read_attachment_local_inline_and_validation() {
        use crate::attachments::{mcp_result_in, read_local, Which};
        let pool = attachment_pool().await;
        let cache = crate::attachments::test_dir("rs-cache");
        let a = read_local(Some(&pool), "s1", &Which::Inline { message_idx: 0, image_index: 0 }).await.unwrap();
        assert_eq!((a.mime.as_str(), a.data.len()), ("image/png", 4));
        let r = mcp_result_in(&a, "Session s1", false, &cache).unwrap();
        assert_eq!(r["content"][0]["type"], "text");
        assert_eq!(r["content"][1], json!({"type":"image","data":"iVBORw==","mimeType":"image/png"}));
        // a non-whitelisted mime is saved to the cache instead of an image block
        let html = read_local(Some(&pool), "s1", &Which::Inline { message_idx: 0, image_index: 1 }).await.unwrap();
        let r = mcp_result_in(&html, "Session s1", false, &cache).unwrap();
        assert_eq!(r["content"].as_array().unwrap().len(), 1);
        assert!(r["content"][0]["text"].as_str().unwrap().contains(&cache.display().to_string()));
        let r = mcp_result_in(&a, "Session s1", true, &cache).unwrap();
        assert!(r["content"][0]["text"].as_str().unwrap().contains("saved to"), "save_to_file forces a file");
        assert!(read_local(Some(&pool), "s1", &Which::Inline { message_idx: 1, image_index: 0 }).await.is_err());
        assert!(read_local(Some(&pool), "s1", &Which::Inline { message_idx: 9, image_index: 0 }).await.unwrap_err().contains("not found"));
        assert!(read_local(Some(&pool), "s1", &Which::Upload { filename: "../x".into() }).await.is_err());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[tokio::test]
    async fn read_attachment_permission_gates() {
        // own session always; others need read or lead; remote needs remote + read
        assert!(attachment_gate(true, true, false, false, false, false).is_ok());
        assert_eq!(attachment_gate(true, false, false, false, false, false), Err(READ_OTHER_DENIED));
        assert!(attachment_gate(true, false, false, false, true, false).is_ok());
        assert!(attachment_gate(true, false, false, false, false, true).is_ok());
        assert_eq!(attachment_gate(true, false, true, false, true, true), Err(REMOTE_READ_DENIED));
        assert_eq!(attachment_gate(true, false, true, true, false, false), Err(READ_OTHER_DENIED));
        assert!(attachment_gate(true, false, true, true, true, false).is_ok());
        assert_eq!(attachment_gate(false, false, true, true, true, true), Err(REMOTE_READ_DENIED));
        let pool = attachment_pool().await;
        assert_eq!(caller_perms_in(&pool, "s1", false).await, (false, true, false));
        assert_eq!(caller_perms_in(&pool, "noread", false).await, (false, false, false));
        assert_eq!(caller_perms_in(&pool, "remote-only", false).await, (true, false, false));
        assert_eq!(caller_perms_in(&pool, "remote-read", false).await, (true, true, false));
        assert_eq!(caller_perms_in(&pool, "lead", false).await, (false, false, true));
        assert_eq!(caller_perms_in(&pool, "missing", false).await, (false, false, false));
        // through the tool: a remote read without a permitted session is refused before any server lookup
        let (ch, _) = capture();
        let t = Target { seq: 0, workspace: ws(), session_id: Some("no-such-session".into()), turn: Arc::new(TurnState::new()), on_event: ch };
        let r = read_attachment(Some(&t), &json!({"server": "vps", "session_id": "x", "message_idx": 0})).await;
        assert_eq!(gate_text(&r), (REMOTE_READ_DENIED.to_string(), true));
        assert!(is_remote_call(READ_ATTACHMENT_TOOL, &json!({"server": "vps"})));
    }

    #[tokio::test]
    async fn read_and_list_sessions_paginate_cleanly() {
        let pool = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        sqlx::query("CREATE TABLE chats (id TEXT PRIMARY KEY, title TEXT NOT NULL, created_at INTEGER NOT NULL, forked_from TEXT, fork_from_index INTEGER, mode TEXT, workspace TEXT, goal TEXT, updated_at INTEGER, extra TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE messages (chat_id TEXT NOT NULL, role TEXT NOT NULL, content TEXT NOT NULL, thinking TEXT NOT NULL, model TEXT, idx INTEGER NOT NULL, images TEXT, gen_ms REAL, gen_tps REAL, summary INTEGER, gen_cost REAL, context_cleared INTEGER)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO chats (id, title, created_at, mode, workspace, updated_at) VALUES ('s1', 'First chat', 100, 'work', '/repo/a', 200), ('s2', 'Second chat', 150, 'chat', '/repo/b', 150)")
            .execute(&pool)
            .await
            .unwrap();
        for i in 0..5 {
            sqlx::query("INSERT INTO messages (chat_id, role, content, thinking, model, idx) VALUES ('s1', $1, $2, 'thought', 'm1', $3)")
                .bind(if i % 2 == 0 { "user" } else { "assistant" })
                .bind(format!("msg-{i}-abcdefghij"))
                .bind(i)
                .execute(&pool)
                .await
                .unwrap();
        }

        let page1 = read_session_from_pool(&pool, "s1", &json!({ "offset": 0, "limit": 2, "max_chars_per_message": 6 })).await.unwrap();
        assert_eq!(page1["total_messages"], 5);
        assert_eq!(page1["returned"], 2);
        assert_eq!(page1["has_more"], true);
        assert_eq!(page1["next_offset"], 2);
        assert_eq!(page1["messages"][0]["content"], "msg-0-…[truncated]");
        assert_eq!(page1["messages"][0]["truncated"], true);

        let tail = read_session_from_pool(&pool, "s1", &json!({ "tail": true, "limit": 2, "include_thinking": true })).await.unwrap();
        assert_eq!(tail["offset"], 3);
        assert_eq!(tail["has_more"], false);
        assert_eq!(tail["next_offset"], Value::Null);
        assert_eq!(tail["messages"][0]["idx"], 3);
        assert_eq!(tail["messages"][1]["idx"], 4);
        assert_eq!(tail["messages"][0]["thinking"], "thought");

        let listed = list_sessions_from_pool(&pool, &json!({ "query": "first", "limit": 10 })).await.unwrap();
        assert_eq!(listed["total"], 1);
        assert_eq!(listed["sessions"][0]["id"], "s1");
        assert_eq!(listed["sessions"][0]["message_count"], 5);

        // a huge reply: tail keeps the end and reads only a window; plain drops card HTML
        let huge = format!("{}<details class=\"tool-result\"><summary>Bash — ls</summary><pre><code>x</code></pre></details>\n\nFINAL", "a".repeat(150_000));
        sqlx::query("INSERT INTO messages (chat_id, role, content, thinking, model, idx) VALUES ('s1', 'assistant', $1, '', 'm1', 5)")
            .bind(&huge)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO messages (chat_id, role, content, thinking, idx, context_cleared) VALUES ('s1', 'assistant', '', '', 6, 1)")
            .execute(&pool)
            .await
            .unwrap();
        let raw = read_session_from_pool(&pool, "s1", &json!({ "tail": true, "limit": 1 })).await.unwrap();
        assert_eq!(raw["messages"][0]["contextCleared"], true);
        let t = read_session_from_pool(&pool, "s1", &json!({ "tail": true, "limit": 1, "max_chars_per_message": 40, "plain": true })).await.unwrap();
        assert_eq!(t["total_messages"], 6);
        let c = t["messages"][0]["content"].as_str().unwrap();
        assert!(c.ends_with("▸ Bash: ls\n\nFINAL") && c.starts_with('…') && !c.contains('<'), "{c}");
        assert_eq!(t["messages"][0]["chars"], huge.chars().count() as i64);
        assert_eq!(t["messages"][0]["truncated"], true);
    }

    #[tokio::test]
    async fn manage_mcp_crud_and_vicuna_mcp_protection() {
        let dir = std::env::temp_dir().join(format!("vicuna-mcp-manage-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let cfg_path = dir.join("vicuna.json");
        let context = TurnMcp {
            manager: None,
            resources: None,
            local: Default::default(),
            server: None,
        };

        // 1. list on empty config still shows fixed vicuna-mcp
        let out = manage_mcp_in(Some(&cfg_path), &json!({ "action": "list" }), &context, "test").await.unwrap();
        let (listed, changed) = (out.value, out.changed);
        assert!(!changed);
        assert!(out.wake_server.is_none());
        assert_eq!(listed["fixed_server"]["name"], SERVER_NAME);
        assert_eq!(listed["servers"].as_array().unwrap().len(), 0);

        // 2. vicuna-mcp cannot be added, edited, enabled, disabled, or removed
        for act in ["add", "edit", "enable", "disable", "remove"] {
            let err = manage_mcp_in(
                Some(&cfg_path),
                &json!({ "action": act, "name": "vicuna-mcp", "url": "http://127.0.0.1:9999/mcp" }),
                &context,
            "test",
            )
            .await
            .err()
            .unwrap();
            assert!(err.contains("fixed"), "expected fixed error for {act}, got {err}");
        }

        // 3. add http server (transport inferred from url)
        let out = manage_mcp_in(
            Some(&cfg_path),
            &json!({
                "action": "add",
                "name": "kurisu",
                "url": "http://100.118.224.35:8793/mcp/5471aba1",
                "headers": { "Authorization": "Bearer secret" }
            }),
            &context,
            "test",
        )
        .await
        .unwrap();
        let (added, changed) = (out.value, out.changed);
        assert!(changed);
        assert_eq!(added["name"], "kurisu");
        assert_eq!(added["transport"], "http");
        assert_eq!(added["enabled"], true);

        // duplicate add fails
        let dup_err = manage_mcp_in(
            Some(&cfg_path),
            &json!({ "action": "add", "name": "Kurisu", "url": "http://127.0.0.1:1/mcp" }),
            &context,
            "test",
        )
        .await
        .err()
        .unwrap();
        assert!(dup_err.contains("already exists"));

        // 4. disable / enable (case-insensitive)
        let out = manage_mcp_in(Some(&cfg_path), &json!({ "action": "disable", "name": "KURISU" }), &context, "test")
            .await
            .unwrap();
        let (dis, changed) = (out.value, out.changed);
        assert!(changed);
        assert_eq!(dis["enabled"], false);

        let out = manage_mcp_in(Some(&cfg_path), &json!({ "action": "enable", "name": "kurisu" }), &context, "test")
            .await
            .unwrap();
        let (en, changed) = (out.value, out.changed);
        assert!(changed);
        assert_eq!(en["enabled"], true);
        assert!(!crate::mcp::list_file_servers_in(&cfg_path).unwrap()[0].enabled);
        assert!(!crate::mcp::SessionSelection::default().is_enabled("kurisu", false));

        // 5. edit url + rename
        let out = manage_mcp_in(
            Some(&cfg_path),
            &json!({
                "action": "edit",
                "name": "kurisu",
                "new_name": "kurisu-prod",
                "url": "https://example.com/mcp"
            }),
            &context,
            "test",
        )
        .await
        .unwrap();
        let (ed, changed) = (out.value, out.changed);
        assert!(changed);
        assert_eq!(ed["name"], "kurisu-prod");

        // list shows renamed server with header_keys (not raw secret)
        let listed2 = manage_mcp_in(Some(&cfg_path), &json!({ "action": "list" }), &context, "test").await.unwrap().value;
        let servers = listed2["servers"].as_array().unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0]["name"], "kurisu-prod");
        assert_eq!(servers[0]["url"], "https://example.com/mcp");
        assert_eq!(servers[0]["header_keys"], json!(["Authorization"]));

        // 6. remove
        let out = manage_mcp_in(Some(&cfg_path), &json!({ "action": "remove", "name": "kurisu-prod" }), &context, "test")
            .await
            .unwrap();
        let (rem, changed) = (out.value, out.changed);
        assert!(changed);
        assert_eq!(rem["name"], "kurisu-prod");
        let listed3 = manage_mcp_in(Some(&cfg_path), &json!({ "action": "list" }), &context, "test").await.unwrap().value;
        assert_eq!(listed3["servers"].as_array().unwrap().len(), 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn manage_skills_crud_search_and_session_permissions() {
        let dir = std::env::temp_dir().join(format!("vicuna-mcp-skills-test-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
        let root = dir.join("skills");
        let cfg = dir.join("config").join("skills.json");
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        crate::server_store::init_schema(&db).await.unwrap();
        for (id, mode) in [("lead", "work"), ("worker", "work"), ("chat", "chat")] {
            crate::server_store::create_chat(&db, &json!({
                "id": id, "title": id, "mode": mode, "createdAt": 1, "messages": [],
                "extra": r#"{"skillsEnabled":{},"mcpEnabled":{}}"#,
            })).await.unwrap();
        }
        let make_target = |id: &str, lead| {
            let turn = Arc::new(TurnState::new());
            turn.lead.store(lead, Ordering::Relaxed);
            Target { seq: 0, workspace: ws(), session_id: Some(id.into()), turn, on_event: capture().0 }
        };
        let lead = make_target("lead", true);
        let worker = make_target("worker", false);
        let call = async |target: &Target, args: &Value| manage_skills_in(&root, &cfg, Some(&db), target, args).await;

        let md = "---\nname: Release Checklist\ndescription: Ship safely\n---\n\n# Verify the changelog\n";
        let add = json!({"action": "add", "slug": "deploy", "content": md, "enabled": false});
        assert_eq!(call(&worker, &add).await.unwrap()["skill"]["enabled"], false);
        assert!(call(&worker, &add).await.is_err());
        assert!(call(&worker, &json!({"action": "add", "slug": "../bad", "content": md})).await.is_err());
        assert_eq!(call(&worker, &json!({"action": "read", "slug": "deploy"})).await.unwrap()["content"], md);
        for query in ["DePlOy", "CHECKLIST", "SHIP SAFELY", "CHANGELOG"] {
            let search = json!({"action": "search", "query": query, "session_id": "worker"});
            assert_eq!(call(&lead, &search).await.unwrap()["skills"][0]["enabled"], false);
            assert_eq!(call(&worker, &search).await.unwrap()["skills"][0]["enabled"], false);
        }
        assert_eq!(call(&lead, &json!({"action": "list"})).await.unwrap()["skills"][0]["enabled"], true);
        for action in ["list", "search", "enable", "disable"] {
            let denied = json!({"action": action, "session_id": "lead", "slug": "deploy", "query": "release"});
            assert!(call(&worker, &denied).await.unwrap_err().contains("Only the lead"));
        }
        assert!(call(&lead, &json!({"action": "list", "session_id": "chat"})).await.is_err());
        assert!(call(&lead, &json!({"action": "search", "query": " "})).await.is_err());
        assert!(call(&lead, &json!({"action": "search", "query": "unrelated"})).await.unwrap()["skills"].as_array().unwrap().is_empty());

        let enable = json!({"action": "enable", "slug": "deploy", "session_id": "worker"});
        assert_eq!(call(&lead, &enable).await.unwrap()["skill"]["enabled"], true);
        let edit = json!({"action": "edit", "slug": "deploy", "content": "---\nname: Deploy v2\n---", "enabled": false});
        assert_eq!(call(&worker, &edit).await.unwrap()["skill"]["name"], "Deploy v2");
        assert!(crate::skills::defaults_in(&cfg).is_empty());

        call(&lead, &json!({"action": "remove", "slug": "deploy"})).await.unwrap();
        assert!(call(&worker, &json!({"action": "list"})).await.unwrap()["skills"].as_array().unwrap().is_empty());
        assert_eq!(crate::server_store::session_skills(&db, "worker").await.unwrap(), Some(Default::default()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn manage_skills_caller_is_bound_to_its_route() {
        let registry = SharedRegistry::default();
        let worker = register_on(&registry, 0, None, "/tmp", Some("worker"), Arc::new(TurnState::new()), capture().0);
        let _lead = register_on(&registry, 0, None, "/tmp", Some("lead"), Arc::new(TurnState::new()), capture().0);
        let default = target_for(&registry, &worker.token).unwrap();
        let call = |caller| json!({"method": "tools/call", "params": {"name": MANAGE_SKILLS_TOOL, "arguments": caller}});
        assert!(target_for_call(&registry, &worker.token, default.clone(), &call(json!({"caller_session_id": "lead"}))).is_none());
        assert_eq!(
            target_for_call(&registry, &worker.token, default.clone(), &call(json!({"caller_session_id": "worker"}))).unwrap().session_id.as_deref(),
            Some("worker")
        );
        assert_eq!(
            target_for_call(&registry, &worker.token, default, &call(json!({}))).unwrap().session_id.as_deref(),
            Some("worker")
        );
    }

}
