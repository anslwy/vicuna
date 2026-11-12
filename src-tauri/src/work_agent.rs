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
    ContextCompacted { note: String },
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
    on_event: Channel<WorkAgentEvent>,
    agent_state: State<'_, WorkAgentState>,
) -> Result<String, String> {
    let engine = crate::work_engine::effective_engine(engine.as_deref());
    if !engine.is_present() {
        return Err(engine.missing_message());
    }
    let turn = Arc::new(TurnState::new());
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
mod tests {
    use super::*;

    fn msg(text: &str) -> ChatMessage {
        serde_json::from_value(serde_json::json!({ "role": "user", "content": text })).unwrap()
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
