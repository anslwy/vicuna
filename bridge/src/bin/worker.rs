// Vicuna remote worker: the agent runtime, on a machine that is NOT the
// user's laptop. The desktop app stays a client; the agent loop, tools, and
// the `lb` browser live here and keep working when the client disconnects.
//
//   vicuna-worker [--bind 127.0.0.1:1421] [--token <t>] [--state-dir <dir>]
//
// First start with no --token mints one, prints it, and persists it (0600) in
// the state dir so restarts don't invalidate a paired client. Every route
// except /health needs `Authorization: Bearer <token>`.
//
// Routes
//   GET  /health                        {"ok":true,"worker":1,"sessions":N}
//   GET  /status                        worker info + per-session state
//   POST /invoke {"cmd":...,"args":...}  same command surface as the desktop
//   GET  /events/<sid>?after=<cursor>   append-only session log, cursor reads
//   POST /sessions/<sid>/stop           interrupt a running turn
//
// Why this is not just the dev bridge: there, events belong to a
// client-generated channel uuid, are drained destructively on poll, and the
// queue dies with the client. Here events belong to a *session*, are appended
// with a monotonic cursor, and stay readable after the turn ends — so a
// client that reconnects with its last cursor sees exactly what it missed.
// Nothing in the running turn depends on a client being connected.
//
// Scope of this first version: the log is in memory (a worker restart loses
// it), and a session runs one turn at a time. Durable session/history state
// across a worker restart is the next step.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vicuna_bridge::{dispatch, drain_events, BridgeState, BODY_CAP, EVENT_CAP};

#[derive(Default)]
struct SessionLog {
    /// Cursor of the next event to append (1-based; 0 means "nothing yet").
    next: u64,
    events: VecDeque<(u64, serde_json::Value)>,
    turn_id: Option<String>,
    running: bool,
}

type Sessions = Arc<Mutex<HashMap<String, SessionLog>>>;

struct Worker {
    state: Arc<BridgeState>,
    sessions: Sessions,
}

fn token_matches(presented: Option<&str>, token: &str) -> bool {
    let Some(rest) = presented.and_then(|v| v.strip_prefix("Bearer ")) else {
        return false;
    };
    let (a, b) = (rest.as_bytes(), token.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn auth_header(req: &tiny_http::Request) -> Option<String> {
    req.headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().to_string())
}

fn respond(req: tiny_http::Request, code: u16, body: String) {
    use tiny_http::Header;
    let h = |n: &[u8], v: &[u8]| Header::from_bytes(n, v).unwrap();
    let res = tiny_http::Response::from_string(body)
        .with_status_code(code)
        .with_header(h(b"Content-Type", b"application/json"))
        .with_header(h(b"Cache-Control", b"no-store"));
    let _ = req.respond(res);
}

fn error(msg: &str) -> String {
    serde_json::json!({"ok": false, "error": msg}).to_string()
}

fn read_body(req: &mut tiny_http::Request) -> Result<Vec<u8>, String> {
    let mut body: Vec<u8> = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match req.as_reader().read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if body.len() + n > BODY_CAP as usize {
                    return Err("body too large".to_string());
                }
                body.extend_from_slice(&buf[..n]);
            }
            Err(e) => return Err(format!("unreadable body: {e}")),
        }
    }
    Ok(body)
}

fn parse_url(url: &str) -> (String, HashMap<String, String>) {
    let mut parts = url.splitn(2, '?');
    let path = parts.next().unwrap_or("/").to_string();
    let mut q = HashMap::new();
    if let Some(rest) = parts.next() {
        for kv in rest.split('&') {
            let mut it = kv.splitn(2, '=');
            q.insert(it.next().unwrap_or("").to_string(), it.next().unwrap_or("").to_string());
        }
    }
    (path, q)
}

/// Append one event to a session log, closing the turn on done/error.
fn append(sessions: &Sessions, sid: &str, ev: serde_json::Value) {
    if let Ok(mut s) = sessions.lock() {
        if let Some(log) = s.get_mut(sid) {
            let c = log.next;
            log.next += 1;
            if let Some(kind) = ev.get("type").and_then(|t| t.as_str()) {
                if kind == "done" || kind == "error" {
                    log.running = false;
                }
            }
            log.events.push_back((c, ev));
            while log.events.len() > EVENT_CAP {
                log.events.pop_front();
            }
        }
    }
}

fn session_running(sessions: &Sessions, sid: &str) -> bool {
    sessions
        .lock()
        .ok()
        .and_then(|s| s.get(sid).map(|l| l.running))
        .unwrap_or(false)
}

/// Tap the turn's event queue into the durable session log. `run_loop` is
/// untouched: it still sends on a real `tauri::ipc::Channel`, whose sink is
/// the queue keyed by the session id (the `__bridge_channel` marker).
/// How long a turn may be completely silent before the tap assumes it died.
/// `run_loop` always emits `Done` on its normal exit (incl. Stop), so this
/// only fires on a panic inside a tool — without it the session would stay
/// `running` forever and 409 every later turn.
const IDLE_CEILING: Duration = Duration::from_secs(15 * 60);

fn spawn_tap(sessions: Sessions, queues: vicuna_bridge::EventQueues, sid: String) {
    std::thread::spawn(move || {
        let mut last_event = Instant::now();
        loop {
            let drained = drain_events(&queues, &sid);
            if drained.is_empty() {
                if !session_running(&sessions, &sid) {
                    break;
                }
                if last_event.elapsed() > IDLE_CEILING {
                    append(
                        &sessions,
                        &sid,
                        serde_json::json!({
                            "type": "error",
                            "message": "worker: turn went silent and was abandoned",
                        }),
                    );
                    break;
                }
            } else {
                last_event = Instant::now();
                for ev in drained {
                    append(&sessions, &sid, ev);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });
}

fn handle_session_get(w: &Arc<Worker>, req: tiny_http::Request, path: &str, q: &HashMap<String, String>) {
    let sid = path.trim_start_matches("/events/");
    let after: u64 = q.get("after").and_then(|s| s.parse().ok()).unwrap_or(0);
    let mut events = Vec::new();
    let (mut cursor, mut running, mut turn_id) = (0u64, false, None);
    if let Ok(s) = w.sessions.lock() {
        // An unknown session reads as an empty log rather than an error: a
        // client may legitimately reconnect before the worker has ever seen
        // that session id.
        if let Some(log) = s.get(sid) {
            for (c, ev) in log.events.iter() {
                if *c > after {
                    events.push(serde_json::json!({"cursor": c, "event": ev}));
                }
            }
            cursor = log.next.saturating_sub(1);
            running = log.running;
            turn_id = log.turn_id.clone();
        }
    }
    let body = serde_json::json!({
        "ok": true, "cursor": cursor, "running": running,
        "turnId": turn_id, "events": events,
    });
    respond(req, 200, body.to_string());
}

fn handle_invoke(
    rt: &tokio::runtime::Handle,
    w: &Arc<Worker>,
    mut req: tiny_http::Request,
) {
    let body = match read_body(&mut req) {
        Ok(b) => b,
        Err(e) => return respond(req, 400, error(&e)),
    };
    let v: serde_json::Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return respond(req, 400, error(&format!("bad json: {e}"))),
    };
    let cmd = v.get("cmd").and_then(|c| c.as_str()).unwrap_or("").to_string();
    let mut args = v.get("args").cloned().unwrap_or(serde_json::Value::Null);

    if cmd != "work_agent_start" {
        let out = match rt.block_on(dispatch(&w.state, &cmd, &args)) {
            Ok(data) => serde_json::json!({"ok": true, "data": data}).to_string(),
            Err(e) => error(&e),
        };
        return respond(req, 200, out);
    }

    // A turn is bound to a worker session id, not to a client-generated
    // channel: that is what makes reconnection possible.
    let Some(sid) = args
        .get("sessionId")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
    else {
        return respond(req, 400, error("work_agent_start needs sessionId"));
    };
    {
        let mut s = match w.sessions.lock() {
            Ok(s) => s,
            Err(_) => return respond(req, 500, error("session lock poisoned")),
        };
        let log = s.entry(sid.clone()).or_default();
        if log.running {
            return respond(req, 409, error("a turn is already running in this session"));
        }
        log.events.clear();
        log.next = 1;
        log.running = true;
        log.turn_id = None;
    }
    args.as_object_mut().map(|o| {
        o.insert(
            "onEvent".to_string(),
            serde_json::json!({"__bridge_channel": sid.clone()}),
        )
    });
    spawn_tap(w.sessions.clone(), w.state.events.clone(), sid.clone());

    let out = match rt.block_on(dispatch(&w.state, &cmd, &args)) {
        Ok(data) => {
            if let Ok(mut s) = w.sessions.lock() {
                if let Some(log) = s.get_mut(&sid) {
                    log.turn_id = data.as_str().map(|x| x.to_string());
                }
            }
            serde_json::json!({"ok": true, "data": data, "sessionId": sid}).to_string()
        }
        Err(e) => {
            if let Ok(mut s) = w.sessions.lock() {
                if let Some(log) = s.get_mut(&sid) {
                    log.running = false;
                }
            }
            error(&e)
        }
    };
    respond(req, 200, out);
}

fn handle(rt: &tokio::runtime::Handle, w: &Arc<Worker>, req: tiny_http::Request, token: &str) {
    use tiny_http::Method::{Get, Options, Post};
    let (path, query) = parse_url(req.url());
    let method = req.method().clone();

    if method == Options {
        return respond(req, 200, String::new());
    }
    if method == Get && path == "/health" {
        let n = w.sessions.lock().map(|s| s.len()).unwrap_or(0);
        return respond(
            req,
            200,
            serde_json::json!({"ok": true, "worker": 1, "sessions": n}).to_string(),
        );
    }
    if !token_matches(auth_header(&req).as_deref(), token) {
        return respond(req, 401, error("unauthorized"));
    }

    if method == Get && path == "/status" {
        let mut list = Vec::new();
        if let Ok(s) = w.sessions.lock() {
            for (sid, log) in s.iter() {
                list.push(serde_json::json!({
                    "sessionId": sid,
                    "turnId": log.turn_id,
                    "running": log.running,
                    "events": log.events.len(),
                    "cursor": log.next.saturating_sub(1),
                }));
            }
        }
        let body = serde_json::json!({
            "ok": true,
            "worker": 1,
            "cwd": std::env::current_dir().ok().map(|p| p.display().to_string()),
            "lb": vicuna_core::browser::browser_mcp_command(None).ok(),
            "lbServer": vicuna_core::browser::lb_server_url(),
            "sessions": list,
        });
        return respond(req, 200, body.to_string());
    }
    if method == Get && path.starts_with("/events/") {
        return handle_session_get(w, req, &path, &query);
    }
    if method == Post && path.starts_with("/sessions/") && path.ends_with("/stop") {
        let sid = path.trim_start_matches("/sessions/").trim_end_matches("/stop");
        let turn = w
            .sessions
            .lock()
            .ok()
            .and_then(|s| s.get(sid).and_then(|l| l.turn_id.clone()));
        if let Some(t) = turn.and_then(|tid| w.state.agent.get_turn(&tid)) {
            vicuna_core::work_agent::agent_stop(&t);
        }
        return respond(req, 200, r#"{"ok":true}"#.to_string());
    }
    if method == Post && path == "/invoke" {
        return handle_invoke(rt, w, req);
    }
    respond(req, 404, error("not found"));
}

fn arg(name: &str) -> Option<String> {
    let args: Vec<String> = std::env::args().collect();
    args.windows(2).find(|w| w[0] == name).map(|w| w[1].clone())
}

fn random_token() -> String {
    use sha2::{Digest, Sha256};
    let mut raw = [0u8; 32];
    getrandom::fill(&mut raw).expect("entropy");
    let mut h = Sha256::new();
    h.update(raw);
    h.finalize()
        .iter()
        .take(24)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn load_or_create_token(state_dir: &std::path::Path) -> String {
    let path = state_dir.join("worker-token");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if s.len() >= 16 {
            return s;
        }
    }
    let t = random_token();
    let _ = std::fs::create_dir_all(state_dir);
    let _ = std::fs::write(&path, &t);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    t
}

fn main() {
    let bind = arg("--bind").unwrap_or_else(|| "127.0.0.1:1421".to_string());
    let state_dir =
        std::path::PathBuf::from(arg("--state-dir").unwrap_or_else(|| "./.vicuna-worker".to_string()));
    let token = std::env::var("VICUNA_WORKER_TOKEN")
        .ok()
        .or_else(|| arg("--token"))
        .unwrap_or_else(|| load_or_create_token(&state_dir));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("worker runtime");
    let worker = Arc::new(Worker {
        state: Arc::new(BridgeState::default()),
        sessions: Arc::new(Mutex::new(HashMap::new())),
    });

    println!("vicuna-worker on http://{bind}");
    println!("  token: {token}");
    println!("  state: {}", state_dir.display());
    println!(
        "  cwd:   {}",
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    );
    match vicuna_core::browser::browser_mcp_command(None) {
        Ok(argv) => println!("  lb:    {}", argv.join(" ")),
        Err(e) => println!("  lb:    NOT FOUND ({e}) — browser tools unavailable"),
    }
    if !bind.starts_with("127.0.0.1") && !bind.starts_with("localhost") {
        eprintln!(
            "  WARNING: bound to {bind}; the agent runs shell commands here. \
             Use a private network/SSH tunnel, never a public port."
        );
    }

    let server = tiny_http::Server::http(&bind).unwrap_or_else(|e| panic!("bind {bind}: {e}"));
    for req in server.incoming_requests() {
        let w = worker.clone();
        let h = rt.handle().clone();
        let t = token.clone();
        std::thread::spawn(move || handle(&h, &w, req, &t));
    }
}
