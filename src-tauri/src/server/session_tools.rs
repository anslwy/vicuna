use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Value, json};

use super::{BridgeState, actions, chat, persist, turns};
use crate::server_store;

struct RemovedTurn {
    index: usize,
    messages: Vec<Value>,
}

#[derive(Default)]
struct RedoStack {
    revision: i64,
    turns: Vec<RemovedTurn>,
}

#[derive(Default)]
pub(super) struct RedoHistory(Mutex<HashMap<String, RedoStack>>);

impl RedoHistory {
    pub(super) fn clear(&self, id: &str) {
        self.0.lock().unwrap().remove(id);
    }

    pub(super) fn decorate(&self, chat: &mut Value) {
        if let (Some(id), Some(revision)) = (chat["id"].as_str(), chat["rev"].as_i64()) {
            let history = self.0.lock().unwrap();
            chat["canRedo"] =
                json!(history.get(id).is_some_and(|stack| {
                    stack.revision == revision && !stack.turns.is_empty()
                }));
        }
    }
}

async fn work_chat(st: &BridgeState, id: &str, args: &Value) -> Result<Value, String> {
    actions::idle(st, id)?;
    let chat = actions::load(st, id).await?;
    if chat["mode"] != "work" {
        return Err("This action requires a Work session.".into());
    }
    if args["baseRev"].is_i64() && args["baseRev"] != chat["rev"] {
        return Err(format!(
            "{}: Session changed — try again.",
            super::SESSION_CHANGED
        ));
    }
    Ok(chat)
}

fn transcript(chat: &Value) -> Vec<Value> {
    chat["messages"].as_array().cloned().unwrap_or_default()
}

pub(super) async fn compact(st: &Arc<BridgeState>, args: &Value) -> Result<Value, String> {
    let id = super::arg_str(args, "id")?;
    let model = super::arg_str(args, "model")?;
    let (original, visible) = {
        let _write = st.chat_lock.lock().await;
        let original = transcript(&work_chat(st, &id, args).await?);
        if original.is_empty() {
            return Ok(json!({"notice": "Nothing to compact — the session has no messages yet."}));
        }
        let visible = persist::compacted_history(&original);
        if visible.len() <= 1 && original.iter().any(|msg| msg["summary"] == true) {
            return Ok(json!({"notice": "Already compacted — send more messages first."}));
        }
        (original, visible)
    };
    let summary = tokio::time::timeout(
        Duration::from_secs(110),
        chat::summarize(st, &model, args["openRouterApiKey"].as_str(), &visible),
    )
    .await
    .map_err(|_| "Compaction timed out.")??;
    if summary.trim().is_empty() {
        return Ok(json!({"notice": "The model returned no summary."}));
    }

    let _write = st.chat_lock.lock().await;
    let chat = work_chat(st, &id, &Value::Null).await?;
    let mut messages = transcript(&chat);
    if messages != original {
        return Ok(json!({"notice": "Session changed during compaction — try again."}));
    }
    for msg in &mut messages {
        if let Some(obj) = msg.as_object_mut() {
            obj.remove("summary");
        }
    }
    messages.push(
        json!({"role": "assistant", "content": summary.trim(), "model": model, "summary": true}),
    );
    let db = st.db.as_ref().ok_or("no server db")?;
    server_store::write_messages(db, &id, &messages, None).await?;
    st.redo_history.clear(&id);
    result(st, &id, None).await
}

pub(super) async fn change_turn(
    st: &BridgeState,
    args: &Value,
    redo: bool,
) -> Result<Value, String> {
    let id = super::arg_str(args, "id")?;
    let _write = st.chat_lock.lock().await;
    let chat = work_chat(st, &id, args).await?;
    let revision = chat["rev"].as_i64().unwrap_or_default();
    let mut messages = transcript(&chat);
    let removed = if redo {
        let history = st.redo_history.0.lock().unwrap();
        let Some(turn) = history
            .get(&id)
            .filter(|s| s.revision == revision)
            .and_then(|s| s.turns.last())
        else {
            return Ok(
                json!({"notice": "Transcript changed since the undo — redo is no longer possible."}),
            );
        };
        messages.splice(turn.index..turn.index, turn.messages.clone());
        None
    } else {
        let Some(index) = messages.iter().rposition(|msg| msg["role"] == "user") else {
            return Ok(json!({"notice": "Nothing to undo — no user message yet."}));
        };
        if messages
            .get(index + 1)
            .is_none_or(|msg| msg["role"] != "assistant" || msg["summary"] == true)
        {
            return Ok(json!({"notice": "Nothing to undo — last user message has no reply yet."}));
        }
        Some(RemovedTurn {
            index,
            messages: messages.drain(index..index + 2).collect(),
        })
    };
    let db = st.db.as_ref().ok_or("no server db")?;
    let (notice, moved_files) = match apply_files(st, &id, redo).await {
        Ok(msg) => (msg, true),
        Err(err)
            if err.starts_with(if redo {
                "Nothing to redo"
            } else {
                "Nothing to undo"
            }) =>
        {
            let msg = if redo {
                "Redone last turn — no file changes to replay."
            } else {
                "Undone last turn — no file changes to revert."
            };
            (msg.to_owned(), false)
        }
        Err(err) => {
            return Err(format!(
                "{} failed: {err}",
                if redo { "Redo" } else { "Undo" }
            ));
        }
    };
    let next_revision =
        match server_store::write_messages(db, &id, &messages, Some(turns::now_ms())).await {
            Ok(revision) => revision,
            Err(err) => {
                if moved_files {
                    apply_files(st, &id, !redo).await.map_err(|rollback| {
                        format!("{err}; restoring the file changes failed: {rollback}")
                    })?;
                }
                return Err(err);
            }
        };

    {
        let mut history = st.redo_history.0.lock().unwrap();
        let stack = history.entry(id.clone()).or_default();
        if stack.revision != revision {
            stack.turns.clear();
        }
        if let Some(turn) = removed {
            if stack.turns.len() == 20 {
                stack.turns.remove(0);
            }
            stack.turns.push(turn);
        } else {
            stack.turns.pop();
        }
        stack.revision = next_revision;
    }
    result(st, &id, Some(notice)).await
}

async fn apply_files(st: &BridgeState, id: &str, redo: bool) -> Result<String, String> {
    if redo {
        crate::work_redo_logic(&st.undo, id.to_owned()).await
    } else {
        crate::work_undo_logic(&st.undo, id.to_owned()).await
    }
}

async fn result(st: &BridgeState, id: &str, notice: Option<String>) -> Result<Value, String> {
    let mut chat = actions::load(st, id).await?;
    st.redo_history.decorate(&mut chat);
    Ok(json!({"chat": chat, "notice": notice}))
}
