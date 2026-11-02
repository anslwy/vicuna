// Event sink abstraction.
//
// The agent loop used to hold a concrete `tauri::ipc::Channel<WorkAgentEvent>`,
// which was its only reason to depend on Tauri. It only ever calls `.send()`
// and checks `is_err()` (a failed send means "the client is gone"). This
// module gives that one operation a name so the same loop runs behind Tauri
// IPC, the dev bridge's poll queue, or the worker's per-session log.
//
// Serialization lives here, not in the callers: every sink receives the same
// JSON string, so all three runtimes observe a byte-identical event stream.

use std::sync::Arc;

use crate::work_agent::WorkAgentEvent;

/// A place agent events go. `send` returning Err means the consumer is gone;
/// the loop treats that as a stop signal (same as the old broken Channel).
pub trait EventSink: Send + Sync {
    fn send_json(&self, json: &str) -> Result<(), ()>;
}

/// Sends every serialized event to a closure. Used by the bridge/worker to
/// append into their own queues, and by tests to collect into a Vec.
pub struct CallbackSink<F>(pub F);

impl<F> EventSink for CallbackSink<F>
where
    F: Fn(&str) + Send + Sync,
{
    fn send_json(&self, json: &str) -> Result<(), ()> {
        (self.0)(json);
        Ok(())
    }
}

/// Accepts and discards everything. Never signals a broken consumer.
pub struct NoopSink;

impl EventSink for NoopSink {
    fn send_json(&self, _json: &str) -> Result<(), ()> {
        Ok(())
    }
}

/// The sink handed to `run_loop`. Cloning is cheap (an Arc), so the loop can
/// pass it down to helpers by value.
pub type Sink = Arc<dyn EventSink>;

/// Convenience: build a sink from any closure.
pub fn sink<F>(f: F) -> Sink
where
    F: Fn(&str) + Send + Sync + 'static,
{
    Arc::new(CallbackSink(f))
}

/// Serialize one event to the wire form all runtimes share.
pub fn encode(ev: &WorkAgentEvent) -> String {
    serde_json::to_string(ev).unwrap_or_else(|_| "{}".to_string())
}
