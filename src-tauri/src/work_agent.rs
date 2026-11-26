
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

use crate::ipc::Channel;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, State};
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
    ContextCompacted { note: String },
    /// vicuna-mcp `schedule_wakeup`: the chat window resolves the time in the
    /// user's timezone, adds a Scheduled send for this session, and answers
    /// `req_id` (via work_agent_answer) with `{ok, when}` or `{ok:false, error}`.
    #[serde(rename = "schedulewakeup")]
    ScheduleWakeup { req_id: String, message: String, at: Option<String>, in_minutes: Option<f64> },
    /// vicuna-mcp `manage_sessions` (lead sessions only): the window owns the
    /// sessions, so it performs `action` and answers `req_id` with
    /// `{ok, text}` or `{ok:false, error}`.
    /// vicuna-mcp `manage_schedule`: list/create/update/cancel Scheduled
    /// items. `lead` = the calling turn may touch other sessions' items.
    #[serde(rename = "schedulecontrol")]
    ScheduleControl { req_id: String, action: String, args: serde_json::Value, lead: bool },
    #[serde(rename = "sessioncontrol")]
    SessionControl { req_id: String, action: String, args: serde_json::Value },
    SkillsChanged {
        session_id: String,
        enabled: Option<crate::skills::EnabledMap>,
        removed: Option<String>,
    },
    McpChanged {
        enabled: Option<crate::mcp::EnabledMap>,
        wake: Option<(String, Option<usize>)>,
    },
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
            prune_turns(&mut turns, 20);
        }
        turn_id
    }

    pub fn get_turn(&self, turn_id: &str) -> Option<Arc<TurnState>> {
        self.turns.lock().ok()?.get(turn_id).cloned()
    }
}

/// Keep at most `keep` turns, dropping FINISHED ones oldest first. A live
/// turn is never dropped: `keys().next()` on a HashMap used to evict an
/// arbitrary (often running) turn once a server had seen 20, so its answers
/// failed with "unknown turn" and Stop saw it as idle while it ran on.
fn prune_turns(turns: &mut HashMap<String, Arc<TurnState>>, keep: usize) {
    if turns.len() <= keep {
        return;
    }
    let seq = |k: &str| k.trim_start_matches("turn-").parse::<u64>().unwrap_or(0);
    let mut done: Vec<String> = turns.iter().filter(|(_, t)| t.done.load(Ordering::Relaxed)).map(|(k, _)| k.clone()).collect();
    done.sort_by_key(|k| seq(k));
    for k in done {
        if turns.len() <= keep {
            break;
        }
        turns.remove(&k);
    }
}

pub struct TurnState {
    pub stop: AtomicBool,
    pub queue: Mutex<Vec<(String, ChatMessage)>>,
    pub ask_pending: Mutex<HashMap<String, oneshot::Sender<String>>>,
    pub ask_seq: AtomicU64,
    /// the session is the user's lead: vicuna-mcp `manage_sessions` works
    pub lead: AtomicBool,
    /// the backend loop for this turn has returned (turns stay in the map
    /// after finishing, so presence alone says nothing about liveness)
    pub done: AtomicBool,
    /// agent questions (vicuna-mcp `ask_user`) whose card is still up but no
    /// tool call is waiting on right now: the engine gave up on the call, or
    /// it returned a "still waiting" poll (see `ask_poll_secs`). A repeat call
    /// with the same question re-attaches; an answer meanwhile is queued as a
    /// user message.
    pub ask_parked: Mutex<Vec<ParkedAsk>>,
    /// >0: a waiting `ask_user` call returns a "still waiting, call again"
    /// result after this many seconds, for engines that time MCP calls out
    /// with no way to raise the limit (agy: 3 minutes)
    pub ask_poll_secs: AtomicU64,
    pub(crate) skills_enabled: tokio::sync::Mutex<crate::skills::EnabledMap>,
    skills_prompt: String,
    pub(crate) mcp: std::sync::OnceLock<crate::vicuna_mcp::TurnMcp>,
    pub(crate) mcp_wakes: Mutex<Vec<(String, Option<usize>)>>,
}

pub struct ParkedAsk {
    pub ask_id: String,
    pub question: String,
    pub multi: bool,
    pub rx: oneshot::Receiver<String>,
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
    pub(crate) fn with_skills(mut self, enabled: crate::skills::EnabledMap) -> Self {
        self.skills_prompt = crate::skills::format_skills_prompt(&enabled);
        *self.skills_enabled.get_mut() = enabled;
        self
    }

    pub(crate) fn skills_prompt(&self) -> &str {
        &self.skills_prompt
    }

    pub fn new() -> Self {
        Self {
            stop: AtomicBool::new(false),
            queue: Mutex::new(Vec::new()),
            ask_pending: Mutex::new(HashMap::new()),
            ask_seq: AtomicU64::new(1),
            lead: AtomicBool::new(false),
            done: AtomicBool::new(false),
            ask_parked: Mutex::new(Vec::new()),
            ask_poll_secs: AtomicU64::new(0),
            skills_enabled: Default::default(),
            skills_prompt: Default::default(),
            mcp: Default::default(),
            mcp_wakes: Mutex::new(Vec::new()),
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
    if let Ok(mut p) = turn.ask_parked.lock() {
        p.clear();
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


pub fn agent_answer(turn: &Arc<TurnState>, ask_id: &str, answer: String) -> Result<(), String> {
    let tx = turn
        .ask_pending
        .lock()
        .map_err(|_| "agent state poisoned".to_string())?
        .remove(ask_id);
    let parked = turn.ask_parked.lock().ok().and_then(|mut p| {
        let i = p.iter().position(|a| a.ask_id == ask_id)?;
        Some(p.remove(i))
    });
    match (tx, parked) {
        // no tool call is waiting on this card right now
        (Some(_), Some(p)) => answer_parked(turn, &p, answer),
        (Some(tx), None) => {
            let _ = tx.send(answer);
            Ok(())
        }
        (None, _) => Err("unknown question".to_string()),
    }
}

/// The user answered a question no `ask_user` call is waiting on (the engine
/// gave up on the call, or is between "still waiting" polls): hand the answer
/// to the agent as a queued user message (it joins the running turn at its
/// next step), quoting the question.
fn answer_parked(turn: &Arc<TurnState>, ask: &ParkedAsk, answer: String) -> Result<(), String> {
    if turn.done.load(Ordering::Relaxed) || turn.stop.load(Ordering::Relaxed) {
        return Err("the agent stopped waiting for this answer and its turn has ended; send it as a message".to_string());
    }
    let a = crate::vicuna_mcp::parse_ask_answer(&answer);
    let q = ask.question.split_whitespace().collect::<Vec<_>>().join(" ");
    let body = if a.text.trim() == crate::vicuna_mcp::ASK_LATER {
        "The user will answer this later; the answer will reach you as a message. Do not ask again.".to_string()
    } else if a.text.trim().is_empty() {
        "(see the attached image)".to_string()
    } else {
        crate::vicuna_mcp::format_answer(&a.text, ask.multi)
    };
    let images: Vec<String> = a.images.into_iter().map(|(_, data)| data).collect();
    let mut msg = serde_json::json!({ "role": "user", "content": format!("Answer to your earlier question \"{q}\":\n\n{body}") });
    if !images.is_empty() {
        msg["images"] = serde_json::json!(images);
    }
    let msg: ChatMessage = serde_json::from_value(msg).map_err(|e| e.to_string())?;
    agent_queue_msg(turn, format!("late-{}", ask.ask_id), msg)
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
    mcp_enabled: Option<crate::mcp::EnabledMap>,
    skills_enabled: Option<crate::skills::EnabledMap>,
    mcp_only: Option<Vec<String>>,
    on_event: Channel<WorkAgentEvent>,
    agent_state: State<'_, WorkAgentState>,
) -> Result<String, String> {
    let engine = crate::work_engine::requested_engine(engine.as_deref())?;
    if !engine.is_present() {
        return Err(engine.missing_message());
    }
    crate::vicuna_mcp::set_app_handle(app.clone());
    let turn = Arc::new(TurnState::new().with_skills(skills_enabled.unwrap_or_else(crate::skills::defaults)));
    turn.lead.store(lead.unwrap_or(false), Ordering::Relaxed);
    let turn_id = agent_state.next_turn(turn.clone());
    let mgr = app.state::<crate::mcp::McpManager>().inner().clone();
    let resources = app.path().resource_dir().ok();
    let selection = crate::mcp::SessionSelection { enabled: mcp_enabled, only: mcp_only };
    let mut mcp_snapshot = crate::acp::McpSnapshot::capture(&mgr, resources.as_deref(), &selection).await;
    let _ = turn.mcp.set(crate::vicuna_mcp::TurnMcp::new(mgr, resources.clone(), selection, None));
    tokio::spawn(async move {
        if mcp_snapshot.uses_builtin_browser() {
            let browser = app.state::<std::sync::Arc<crate::browser::BrowserState>>();
            let _ = crate::browser::browser_start_logic(resources, &browser).await;
            mcp_snapshot.update_browser_url();
        }
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

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_agent_stop(turn_id: String, agent_state: State<'_, WorkAgentState>) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_stop(&turn);
    Ok(())
}

/// Whether a turn is still running backend-side (see `turn_is_live`).
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_agent_turn_live(turn_id: String, agent_state: State<'_, WorkAgentState>) -> Result<bool, String> {
    Ok(turn_is_live(&agent_state, &turn_id))
}

#[cfg(feature = "desktop")]
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
#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_agent_remove(
    turn_id: String,
    id: String,
    agent_state: State<'_, WorkAgentState>,
) -> Result<(), String> {
    let turn = get_turn(&agent_state, &turn_id)?;
    agent_remove_msg(&turn, &id)
}

#[cfg(feature = "desktop")]
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
mod prune_tests {
    use super::*;

    #[test]
    fn pruning_keeps_live_turns_and_drops_the_oldest_finished() {
        let mut m: HashMap<String, Arc<TurnState>> = HashMap::new();
        for i in 1..=25u64 {
            let t = Arc::new(TurnState::new());
            // odd turns finished, even ones still running
            t.done.store(i % 2 == 1, Ordering::Relaxed);
            m.insert(format!("turn-{i}"), t);
        }
        prune_turns(&mut m, 20);
        assert_eq!(m.len(), 20);
        for i in (2..=24).step_by(2) {
            assert!(m.contains_key(&format!("turn-{i}")), "live turn-{i} kept");
        }
        for i in [1, 3, 5, 7, 9] {
            assert!(!m.contains_key(&format!("turn-{i}")), "oldest finished turn-{i} dropped");
        }
        assert!(m.contains_key("turn-25"));
    }
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

    fn park(t: &Arc<TurnState>, id: &str, q: &str) {
        let (tx, rx) = oneshot::channel();
        t.ask_pending.lock().unwrap().insert(id.into(), tx);
        t.ask_parked.lock().unwrap().push(ParkedAsk { ask_id: id.into(), question: q.into(), multi: false, rx });
    }

    #[test]
    fn answer_to_a_parked_question_is_queued() {
        let t = Arc::new(TurnState::new());
        assert_eq!(agent_answer(&t, "ask-1", "x".into()).unwrap_err(), "unknown question");
        park(&t, "ask-1", "Which  fruit?");
        agent_answer(&t, "ask-1", "Kiwi".into()).unwrap();
        {
            let q = t.queue.lock().unwrap();
            assert_eq!(q.len(), 1);
            assert_eq!(q[0].0, "late-ask-1");
            assert_eq!(q[0].1.content, "Answer to your earlier question \"Which fruit?\":\n\nThe user answered: Kiwi");
        }
        assert!(t.ask_parked.lock().unwrap().is_empty());
        assert!(agent_answer(&t, "ask-1", "x".into()).is_err());
        // an ended turn says so instead of "unknown question"
        park(&t, "ask-3", "Q");
        t.done.store(true, Ordering::Relaxed);
        assert!(agent_answer(&t, "ask-3", "y".into()).unwrap_err().contains("turn has ended"));
        // Stop clears parked cards
        agent_stop(&t);
        assert!(t.ask_parked.lock().unwrap().is_empty());
    }

    #[test]
    fn turn_ids_are_sequential_and_pruned() {
        let s = WorkAgentState::default();
        let first = s.next_turn(Arc::new(TurnState::new()));
        assert_eq!(first, "turn-1");
        for _ in 0..25 {
            let t = Arc::new(TurnState::new());
            t.done.store(true, Ordering::Relaxed); // finished turns are prunable
            s.next_turn(t);
        }
        assert!(s.turns.lock().unwrap().len() <= 20);
        // a live turn is never pruned, however many follow it
        let live = s.next_turn(Arc::new(TurnState::new()));
        for _ in 0..30 {
            let t = Arc::new(TurnState::new());
            t.done.store(true, Ordering::Relaxed);
            s.next_turn(t);
        }
        assert!(s.get_turn(&live).is_some());
    }
}
