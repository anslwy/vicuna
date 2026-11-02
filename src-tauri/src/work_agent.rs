// Tauri command surface for the Work agent.
//
// The loop itself lives in `vicuna_core::work_agent` (no Tauri dependency) and
// streams events through `vicuna_core::events::Sink`. This module is only the
// IPC edge: it adapts a `tauri::ipc::Channel` into that sink and maps the
// command arguments onto the core functions. The bridge/worker call the same
// core functions over HTTP, so `run_loop` is byte-identical across runtimes.

pub use vicuna_core::work_agent::*;

use std::collections::HashMap;
use std::sync::Arc;

use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, State};

use vicuna_core::events::{encode, sink as make_sink};
use vicuna_core::{
    BgExecState, ChatMessage, UndoState,
};
use vicuna_core::browser::BrowserState;
use vicuna_core::mcp::McpManager;

use vicuna_core::work_agent::{
    agent_answer, agent_queue_msg, agent_remove_msg, agent_stop, run_loop, ChatSnapshot, McpRoute,
    TurnState, WorkAgentEvent,
};

fn get_turn(agent_state: &WorkAgentState, turn_id: &str) -> Result<Arc<TurnState>, String> {
    agent_state.get_turn(turn_id).ok_or_else(|| "unknown turn".to_string())
}

#[tauri::command]
pub async fn work_agent_start(
    app: AppHandle,
    model: String,
    provider: Option<String>,
    api_key: Option<String>,
    context_length: Option<u64>,
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
    // resource dir for the bundled `lb` (same lookup the browser_* commands
    // use — the agent's browser enable/list needs the sidecar too)
    let browser_resource_dir = app.path().resource_dir().ok();
    // Adapt the webview Channel to the core sink. A failed send (webview
    // gone, e.g. refresh mid-turn) surfaces as Err so the loop unwinds,
    // which is the behaviour the old concrete-Channel code had.
    let sink = make_sink(move |json| {
        if let Ok(ev) = serde_json::from_str::<WorkAgentEvent>(json) {
            let _ = on_event.send(ev);
        }
    });
    tokio::spawn(async move {
        let bg = app.state::<BgExecState>();
        let mcp = app.state::<McpManager>();
        let browser_state = app.state::<BrowserState>();
        let undo_state = app.state::<UndoState>();
        // Hold the guards for the whole turn.
        let (bg_ref, mcp_ref, browser_ref, undo_ref) = (&*bg, &*mcp, &*browser_state, &*undo_state);
        run_loop(
            turn,
            model,
            provider.unwrap_or_else(|| "ollama".to_string()),
            api_key,
            context_length,
            workspace,
            messages,
            think,
            tools,
            session_id,
            goal_text,
            goal_paused.unwrap_or(true),
            chats.unwrap_or_default(),
            mcp_routes.unwrap_or_default(),
            sink,
            bg_ref,
            mcp_ref,
            &tid,
            browser_resource_dir,
            browser_ref,
            undo_ref,
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

// `encode` is re-exported for callers that need the wire form; silence the
// unused-import warning when only the sink helper is used above.
#[allow(unused_imports)]
use encode as _encode;
