// Work mode turn bookkeeping + Tauri commands.
//
// The agent itself is the user's opencode CLI over ACP (acp.rs); this module
// only owns the per-turn state (stop flag, queued steering messages, pending
// ask answers) and the stable IPC names the frontend invokes. There is no
// built-in agent loop and no fallback engine: when opencode is missing the
// turn fails with the install hint (the UI disables Send before that).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tauri::{ipc::Channel, AppHandle, Manager, State};
use tokio::sync::oneshot;

use super::ChatMessage;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum WorkAgentEvent {
    Token { text: String },
    Think { text: String },
    Status { label: Option<String> },
    RoundStart { mode: String },
    /// The opencode model this turn actually runs on (reply label).
    Model { model: String },
    RoundEnd { gen_ms: u64, gen_tps: Option<f64>, gen_cost: Option<f64> },
    DisplayNote { text: String },
    QueueDrained { ids: Vec<String>, messages: Vec<ChatMessage> },
    ToolStart { name: String, args: serde_json::Value },
    ToolEnd { name: String, output: String },
    /// The chat's ask card: a permission request (`perm-*`, Allow/Deny) or
    /// an agent question from vicuna-mcp's ask_user (`ask-*`; `multi` lets
    /// the user pick several options, answered as a JSON array).
    AskRequest {
        ask_id: String,
        question: String,
        options: Vec<String>,
        #[serde(default)]
        multi: bool,
    },
    BgCompleted { job_id: String, command: String, output: String },
    /// vicuna-mcp `run` started a Vicuna-owned job and the tool call is
    /// waiting on it (the chat shows it with a Background button).
    RunStarted { job_id: String, command: String, started_ms: u64 },
    /// that waiting `run` call returned (finished, stopped or backgrounded)
    RunEnded { job_id: String },
    /// the job keeps running detached from the turn; the frontend watches it
    /// and sends its output to the session as a message when it finishes
    RunBackground { job_id: String, command: String, started_ms: u64 },
    ContextCompacted { note: String },
    /// vicuna-mcp `schedule_wakeup`: the chat window resolves the time in the
    /// user's timezone, adds a Scheduled send for this session, and answers
    /// `req_id` (via work_agent_answer) with `{ok, when}` or `{ok:false, error}`.
    #[serde(rename = "schedulewakeup")]
    ScheduleWakeup { req_id: String, message: String, at: Option<String>, in_minutes: Option<f64> },
    /// vicuna-mcp `manage_sessions` (lead sessions only): the window owns the
    /// sessions, so it performs `action` and answers `req_id` with
    /// `{ok, text}` or `{ok:false, error}`.
    #[serde(rename = "sessioncontrol")]
    SessionControl { req_id: String, action: String, args: serde_json::Value },
    Done { stopped: bool },
    Error { message: String },
}

/// Best-effort event send: when the webview is gone (e.g. refresh mid-turn)
/// the Channel breaks — mark the turn stopped so the backend unwinds
/// instead of running headless.
pub(crate) fn emit_ev(ch: &Channel<WorkAgentEvent>, stop: &AtomicBool, ev: WorkAgentEvent) {
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
    /// vicuna-mcp `run` calls waiting on their job, by job id: sending moves
    /// the job to the background and returns the tool call at once
    pub run_pending: Mutex<HashMap<String, oneshot::Sender<()>>>,
    /// the session is the user's lead: vicuna-mcp `manage_sessions` works
    pub lead: AtomicBool,
    /// the backend loop for this turn has returned (turns stay in the map
    /// after finishing, so presence alone says nothing about liveness)
    pub done: AtomicBool,
}

/// Marks a turn finished when dropped: held for the whole of
/// `acp::run_turn`, so early returns and panics also count as finished.
pub struct TurnDoneGuard(pub Arc<TurnState>);

impl Drop for TurnDoneGuard {
    fn drop(&mut self) {
        self.0.done.store(true, Ordering::Relaxed);
    }
}

/// The engine loops' Stop poll. Must be created once outside the
/// `select!` loop: a `sleep` arm rebuilt every iteration is reset by every
/// engine message, so a steady token stream starved the stop check and the
/// turn kept running (and showing as running) after Stop.
pub fn stop_check_interval() -> tokio::time::Interval {
    let mut i = tokio::time::interval(std::time::Duration::from_millis(120));
    i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    i
}

/// Is this turn's backend loop still running? `false` for unknown (pruned)
/// ids too: the window uses it to clear a stale "running" mark after Stop.
pub fn turn_is_live(state: &WorkAgentState, turn_id: &str) -> bool {
    state.get_turn(turn_id).is_some_and(|t| !t.done.load(Ordering::Relaxed))
}

impl TurnState {
    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            queue: Mutex::new(Vec::new()),
            ask_pending: Mutex::new(HashMap::new()),
            ask_seq: AtomicU64::new(1),
            run_pending: Mutex::new(HashMap::new()),
            lead: AtomicBool::new(false),
            done: AtomicBool::new(false),
        }
    }
}

impl Default for TurnState {
    fn default() -> Self {
        Self::new()
    }
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
        .map(|mut q| q.push((id, message)))
        .map_err(|_| "agent state poisoned".to_string())
}

pub fn agent_remove_msg(turn: &Arc<TurnState>, id: &str) -> Result<(), String> {
    turn.queue
        .lock()
        .map(|mut q| q.retain(|(qid, _)| qid != id))
        .map_err(|_| "agent state poisoned".to_string())
}

/// The user's Background button on a waiting vicuna-mcp `run` call.
pub fn agent_background(turn: &Arc<TurnState>, job_id: &str) -> Result<(), String> {
    let tx = turn
        .run_pending
        .lock()
        .map_err(|_| "agent state poisoned".to_string())?
        .remove(job_id);
    match tx {
        Some(tx) => {
            let _ = tx.send(());
            Ok(())
        }
        None => Err("that command is no longer waiting".to_string()),
    }
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

/// Start a Work turn on `engine` ("opencode" | "claude"; absent = the saved
/// choice). Resolves with the turn id at once; events stream through
/// `on_event` until done/error. `model` is the engine's model id (opencode
/// `provider/model`, Claude e.g. "haiku"; empty = the pick saved in Vicuna,
/// else the engine's default). Unknown extra arguments are ignored.
#[cfg(feature = "desktop")]
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn work_agent_start(
    app: AppHandle,
    model: Option<String>,
    workspace: String,
    messages: Vec<ChatMessage>,
    session_id: Option<String>,
    goal_text: Option<String>,
    fresh_session: Option<bool>,
    engine: Option<String>,
    lead: Option<bool>,
    on_event: Channel<WorkAgentEvent>,
    agent_state: State<'_, WorkAgentState>,
) -> Result<String, String> {
    let engine = crate::work_engine::effective_engine(engine.as_deref());
    if !engine.is_present() {
        return Err(engine.missing_message());
    }
    crate::vicuna_mcp::set_app_handle(app.clone());
    let turn = Arc::new(TurnState::new());
    turn.lead.store(lead.unwrap_or(false), Ordering::Relaxed);
    let turn_id = agent_state.next_turn(turn.clone());
    // the MCP dialog's servers (+ built-in browser) go to opencode
    let mcp_snapshot = crate::acp::McpSnapshot::capture(
        &app.state::<crate::mcp::McpManager>(),
        app.path().resource_dir().ok().as_deref(),
    )
    .await;
    tokio::spawn(async move {
        let acp = app.state::<crate::acp::AcpState>();
        crate::acp::run_turn(
            turn,
            model,
            workspace,
            messages,
            session_id,
            goal_text,
            fresh_session.unwrap_or(false),
            on_event,
            &acp,
            mcp_snapshot,
            engine,
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

/// Whether a turn is still running backend-side (see `turn_is_live`).
#[tauri::command]
pub async fn work_agent_turn_live(turn_id: String, agent_state: State<'_, WorkAgentState>) -> Result<bool, String> {
    Ok(turn_is_live(&agent_state, &turn_id))
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

#[tauri::command]
pub async fn work_agent_background(
    turn_id: String,
    job_id: String,
    agent_state: State<'_, WorkAgentState>,
) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_background(&turn, &job_id)
}

/// Poll a vicuna-mcp `run` job (also after its turn ended: the frontend
/// watches backgrounded jobs to send their output as a message).
#[tauri::command]
pub async fn work_run_status(id: String) -> Result<crate::BgJobStatus, String> {
    crate::bg_snapshot(crate::vicuna_mcp::jobs(), &id)
}

#[tauri::command]
pub async fn work_run_kill(id: String) -> Result<crate::BgJobStatus, String> {
    crate::bg_kill(crate::vicuna_mcp::jobs(), &id)
}

/// Backgrounded `run` jobs not yet reported to their session (a reopened
/// window rebuilds its chips from this).
#[tauri::command]
pub async fn work_run_list() -> Vec<crate::vicuna_mcp::BgRunRecord> {
    crate::vicuna_mcp::bg_runs_pending()
}

/// One window delivers a finished job's message: true = this caller may.
#[tauri::command]
pub async fn work_run_claim(id: String, release: Option<bool>) -> bool {
    crate::vicuna_mcp::bg_run_claim(&id, release.unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(text: &str) -> ChatMessage {
        serde_json::from_value(serde_json::json!({ "role": "user", "content": text })).unwrap()
    }

    #[test]
    fn turn_live_tracks_the_done_guard_and_unknown_ids() {
        let st = WorkAgentState::default();
        let t = Arc::new(TurnState::new());
        let id = st.next_turn(t.clone());
        assert!(turn_is_live(&st, &id));
        drop(TurnDoneGuard(t.clone()));
        assert!(!turn_is_live(&st, &id), "finished turns stay in the map but are not live");
        assert!(!turn_is_live(&st, "no-such-turn"));
    }

    /// The bug behind "Stop does nothing until reload": engine messages
    /// faster than 120ms reset a per-iteration sleep forever.
    #[tokio::test]
    async fn stop_tick_fires_under_a_steady_message_stream() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        tokio::spawn(async move {
            loop {
                if tx.send(()).is_err() { break; }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        });
        let start = std::time::Instant::now();
        // the old shape never reaches the sleep arm
        let mut old_fired = false;
        while start.elapsed() < std::time::Duration::from_millis(400) {
            tokio::select! {
                _ = rx.recv() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(120)) => { old_fired = true; break; }
            }
        }
        assert!(!old_fired, "precondition: a rebuilt sleep is starved by the stream");
        let mut tick = stop_check_interval();
        tick.tick().await; // first tick is immediate
        let start = std::time::Instant::now();
        loop {
            tokio::select! {
                _ = rx.recv() => {}
                _ = tick.tick() => break,
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(2), "stop tick starved");
        }
        assert!(start.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn queue_and_remove() {
        let t = Arc::new(TurnState::new());
        agent_queue_msg(&t, "a".into(), msg("one")).unwrap();
        agent_queue_msg(&t, "b".into(), msg("two")).unwrap();
        agent_remove_msg(&t, "a").unwrap();
        let q = t.queue.lock().unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!(q[0].0, "b");
    }

    #[test]
    fn stop_sets_flag_and_cancels_asks() {
        let t = Arc::new(TurnState::new());
        let (tx, mut rx) = oneshot::channel();
        t.ask_pending.lock().unwrap().insert("q".into(), tx);
        agent_stop(&t);
        assert!(t.stop.load(Ordering::Relaxed));
        assert_eq!(rx.try_recv().unwrap(), "cancelled");
        assert!(agent_answer(&t, "q", "x".into()).is_err());
    }

    #[test]
    fn turn_ids_are_sequential_and_pruned() {
        let s = WorkAgentState::default();
        let first = s.next_turn(Arc::new(TurnState::new()));
        assert_eq!(first, "turn-1");
        for _ in 0..25 {
            s.next_turn(Arc::new(TurnState::new()));
        }
        assert!(s.turns.lock().unwrap().len() <= 20);
    }
}
