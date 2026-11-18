//! Server-owned Chat turns. Subscribing takes a snapshot and registers a
//! private event channel under the same lock, so a reload cannot lose or
//! replay any part of the answer.

mod provider;

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::{BridgeState, actions, persist, turns};
use crate::server_store;

const CHANNEL_RETENTION: Duration = Duration::from_secs(120);

pub(super) type ChatTurns = Mutex<HashMap<String, Arc<Turn>>>;

#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Options {
    model: String,
    think: Option<Value>,
    continue_mode: Option<String>,
    #[serde(default = "default_stop_words")]
    stop: Vec<String>,
    open_router_api_key: Option<String>,
    #[serde(skip)]
    temperature: Option<f64>,
}

fn default_stop_words() -> Vec<String> {
    ["\nUser:", "\nAssistant:", "<end_of_turn>", "<|im_end|>"]
        .map(str::to_owned)
        .into()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Request {
    id: String,
    base_rev: Option<i64>,
    #[serde(flatten)]
    options: Options,
    #[serde(flatten)]
    operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case")]
enum Operation {
    Send {
        content: String,
        images: Option<Vec<String>>,
        #[serde(rename = "titleSource")]
        title_source: Option<String>,
    },
    Regenerate {
        index: usize,
    },
    Continue {
        index: usize,
    },
    Edit {
        index: usize,
        content: String,
        images: Option<Vec<String>>,
    },
    ForkEdit {
        index: usize,
        content: String,
        images: Option<Vec<String>>,
        #[serde(rename = "forkId")]
        fork_id: String,
    },
}

pub(super) struct Turn {
    id: String,
    started_at: i64,
    extending: bool,
    stop: Notify,
    state: Mutex<Live>,
}

struct Live {
    messages: Vec<Value>,
    index: usize,
    original: String,
    generated: String,
    first_token: bool,
    produced: bool,
    dirty: bool,
    channels: HashSet<String>,
    images_stripped: bool,
}

fn get(st: &BridgeState, id: &str) -> Option<Arc<Turn>> {
    st.chat_turns.lock().unwrap().get(id).cloned()
}

pub(super) fn is_running(st: &BridgeState, id: &str) -> bool {
    st.chat_turns.lock().unwrap().contains_key(id)
}

pub(super) fn running_ids(st: &BridgeState) -> Vec<String> {
    st.chat_turns.lock().unwrap().keys().cloned().collect()
}

pub(super) fn live_turns(st: &BridgeState) -> HashMap<String, String> {
    st.chat_turns
        .lock()
        .unwrap()
        .iter()
        .map(|(id, turn)| (id.clone(), turn.id.clone()))
        .collect()
}

fn channel(args: &Value) -> Option<String> {
    args["onEvent"]["__bridge_channel"]
        .as_str()
        .filter(|id| {
            !id.is_empty()
                && id.len() <= 128
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
        .map(str::to_owned)
}

fn snapshot(turn: &Turn, live: &mut Live, mut chat: Value, channel: Option<String>) -> Value {
    live.channels.extend(channel);
    chat["messages"] = json!(live.messages);
    json!({
        "id": chat["id"], "turnId": turn.id, "chat": chat, "startedMs": turn.started_at,
        "index": live.index, "continuing": turn.extending, "imagesStripped": live.images_stripped,
    })
}

pub(super) async fn attach(st: &BridgeState, args: &Value) -> Result<Value, String> {
    let _write = st.chat_lock.lock().await;
    let id = super::arg_str(args, "id")?;
    let Some(turn) = get(st, &id) else {
        return Ok(Value::Null);
    };
    let db = st.db.as_ref().ok_or("Chat turns need a recording server")?;
    let Some(chat) = server_store::load_chat(db, &id).await? else {
        return Ok(Value::Null);
    };
    let response = snapshot(&turn, &mut turn.state.lock().unwrap(), chat, channel(args));
    Ok(response)
}

pub(super) fn stop(st: &BridgeState, id: &str, expected: Option<&str>) -> bool {
    let Some(turn) = get(st, id) else {
        return false;
    };
    if expected.is_some_and(|id| id != turn.id) {
        return false;
    }
    turn.stop.notify_one();
    true
}

pub(super) async fn start(st: &Arc<BridgeState>, args: &Value) -> Result<Value, String> {
    let (response, turn, options) = prepare(st, args).await?;
    let st = st.clone();
    let id = response["id"].as_str().unwrap().to_owned();
    tokio::spawn(async move {
        let history = {
            let live = turn.state.lock().unwrap();
            model_history(&live, turn.extending)
        };
        let completion = provider::generate(&st, &turn, options, history);
        record(&st, &id, &turn, completion).await;
    });
    Ok(response)
}

/// Summarize Work context with the selected Chat provider, without creating a Chat turn.
pub(super) async fn summarize(
    st: &Arc<BridgeState>,
    model: &str,
    api_key: Option<&str>,
    messages: &[Value],
) -> Result<String, String> {
    let transcript = messages.iter().map(|msg| {
        let who = match msg["role"].as_str() {
            Some("user") => "User",
            Some("assistant") => "Assistant",
            Some("tool") => "Tool",
            _ => "System",
        };
        let text = msg["content"].as_str().unwrap_or("");
        let images = msg["images"].as_array().map_or(0, Vec::len);
        if images == 0 {
            format!("{who}: {text}")
        } else {
            format!("{who}: {text} [{images} image(s) attached]")
        }
    }).collect::<Vec<_>>().join("\n\n");
    let turn = Arc::new(Turn {
        id: turns::uuid(),
        started_at: turns::now_ms(),
        extending: false,
        stop: Notify::new(),
        state: Mutex::new(Live {
            messages: vec![persist::assistant_placeholder(model)],
            index: 0,
            original: String::new(),
            generated: String::new(),
            first_token: true,
            produced: false,
            dirty: false,
            channels: HashSet::new(),
            images_stripped: false,
        }),
    });
    let options = Options {
        model: model.to_owned(),
        think: Some(json!(false)),
        temperature: Some(0.2),
        open_router_api_key: api_key.map(str::to_owned),
        ..Options::default()
    };
    let history = vec![
        json!({"role": "system", "content": "Summarize this conversation transcript concisely: files listed/read/written, commands run and their key output, decisions made, and what remains to do. Keep file paths and facts; drop chatter."}),
        json!({"role": "user", "content": transcript}),
    ];
    provider::generate(st, &turn, options, history).await?;
    let live = turn.state.lock().unwrap();
    Ok(live.messages[0]["content"].as_str().unwrap_or("").to_owned())
}

fn model_history(live: &Live, extending: bool) -> Vec<Value> {
    let mut history = persist::compacted_history(&live.messages[..live.index + extending as usize]);
    if extending {
        // Preserve the exact prefill, including trailing whitespace.
        if let Some(reply) = history.last_mut() {
            reply["content"] = json!(live.original);
        }
    }
    history
}

async fn prepare(st: &BridgeState, args: &Value) -> Result<(Value, Arc<Turn>, Options), String> {
    let request: Request =
        serde_json::from_value(args.clone()).map_err(|e| format!("Invalid Chat turn: {e}"))?;
    let Request {
        id,
        base_rev,
        options,
        operation,
    } = request;
    if id.trim().is_empty() || options.model.trim().is_empty() {
        return Err("A session id and model are required".into());
    }
    if let Some(think) = &options.think {
        if !think.is_boolean() && !think.is_string() {
            return Err("think must be a boolean or an effort level".into());
        }
    }
    let db = st.db.as_ref().ok_or("Chat turns need a recording server")?;
    let _write = st.chat_lock.lock().await;
    if turns::live_sessions(st).contains(&id) || turns::live_turn(st, &id).is_some() {
        return Err("This session already has a turn running on the server".into());
    }
    let stored = server_store::load_chat(db, &id).await?;
    if stored.as_ref().is_some_and(|c| c["mode"] == "work") {
        return Err("This action requires a Chat session".into());
    }
    // Appends can safely use the latest revision. Index-based edits must
    // refer to the same transcript the caller saw.
    if !matches!(&operation, Operation::Send { .. })
        && base_rev.is_some_and(|rev| stored.as_ref().and_then(|c| c["rev"].as_i64()) != Some(rev))
    {
        return Err(format!(
            "{}: This session changed on the server. Review the latest transcript and try again.",
            super::SESSION_CHANGED
        ));
    }
    let now = turns::now_ms();
    let mut chat = match stored {
        Some(chat) => chat,
        None if base_rev.unwrap_or(0) == 0 && matches!(&operation, Operation::Send { .. }) => {
            json!({"id": id, "title": "New session", "mode": "chat", "createdAt": now, "updatedAt": now, "messages": []})
        }
        None => return Err(format!("No session {id}")),
    };
    let mut messages = chat["messages"].as_array().cloned().unwrap_or_default();
    let extending = matches!(&operation, Operation::Continue { .. });
    match operation {
        Operation::Send {
            content,
            images,
            title_source,
        } => {
            let images = images.filter(|images| !images.is_empty());
            if content.trim().is_empty() && images.is_none() {
                return Err("A message needs text or an image".into());
            }
            let title = title_source.as_deref().unwrap_or_else(|| {
                if content.trim().is_empty() {
                    "Image"
                } else {
                    &content
                }
            });
            if let Some(title) = actions::retitle(chat["title"].as_str().unwrap_or(""), title) {
                chat["title"] = json!(title);
            }
            chat["updatedAt"] = json!(now);
            messages.push(json!({"role": "user", "content": content, "images": images}));
        }
        Operation::Regenerate { index } | Operation::Continue { index } => {
            if index.checked_add(1) != Some(messages.len())
                || messages[index]["role"] != "assistant"
            {
                return Err("Select the last assistant reply".into());
            }
            if extending {
                let tag = options
                    .model
                    .rsplit_once(':')
                    .map(|(_, tag)| tag)
                    .unwrap_or("");
                if options.model.starts_with("openrouter:")
                    || tag == "cloud"
                    || tag.ends_with("-cloud")
                {
                    return Err("Continue is unavailable for this model".into());
                }
            } else {
                messages.truncate(index);
            }
        }
        Operation::Edit {
            index,
            content,
            images,
        } => {
            edit_user(&mut chat, &mut messages, index, &content, images)?;
        }
        Operation::ForkEdit {
            index,
            content,
            images,
            fork_id,
        } => {
            if fork_id.trim().is_empty() || server_store::load_chat(db, &fork_id).await?.is_some() {
                return Err("The fork needs a new session id".into());
            }
            if messages.get(index).is_none() {
                return Err(format!("No message {index}"));
            }
            chat = actions::fork_row(&chat, &id, index, &fork_id, messages[..=index].to_vec());
            edit_user(&mut chat, &mut messages, index, &content, images)?;
        }
    }
    if !extending {
        messages.push(persist::assistant_placeholder(&options.model));
    }
    let index = messages.len() - 1;
    let original = messages[index]["content"].as_str().unwrap_or("").to_owned();
    chat["baseRev"] = chat["rev"].clone();
    chat["messages"] = json!(messages);
    actions::as_save_row(&mut chat);
    let result =
        server_store::merge_json(db, &json!([chat]), vec![], &turns::live_sessions(st)).await?;
    if !result.conflicts.is_empty() {
        return Err(format!(
            "{}: This session changed on the server",
            super::SESSION_CHANGED
        ));
    }
    let id = chat["id"].as_str().unwrap().to_owned();
    let saved = server_store::load_chat(db, &id)
        .await?
        .ok_or("Session vanished")?;
    let turn = Arc::new(Turn {
        id: turns::uuid(),
        started_at: now,
        extending,
        stop: Notify::new(),
        state: Mutex::new(Live {
            messages,
            index,
            original,
            generated: String::new(),
            first_token: true,
            produced: false,
            dirty: false,
            channels: HashSet::new(),
            images_stripped: false,
        }),
    });
    let response = snapshot(&turn, &mut turn.state.lock().unwrap(), saved, channel(args));
    st.chat_turns.lock().unwrap().insert(id, turn.clone());
    Ok((response, turn, options))
}

fn edit_user(
    chat: &mut Value,
    messages: &mut Vec<Value>,
    index: usize,
    content: &str,
    images: Option<Vec<String>>,
) -> Result<(), String> {
    if messages.get(index).and_then(|m| m["role"].as_str()) != Some("user") {
        return Err("Select a user message to regenerate its reply".into());
    }
    actions::apply_edit(
        messages,
        index,
        content,
        images.map(|images| json!(images)),
        true,
    )?;
    chat["updatedAt"] = json!(turns::now_ms());
    if let Some(title) = actions::retitle(chat["title"].as_str().unwrap_or(""), content) {
        chat["title"] = json!(title);
    }
    Ok(())
}

fn publish(st: &BridgeState, live: &Live, event: Value) {
    if live.channels.is_empty() {
        return;
    }
    let raw = event.to_string();
    for channel in &live.channels {
        super::push_event(&st.events, channel, &raw);
    }
}

async fn record(
    st: &BridgeState,
    id: &str,
    turn: &Turn,
    run: impl Future<Output = Result<provider::Stats, String>>,
) {
    let wall = Instant::now();
    let result = {
        tokio::pin!(run);
        let mut checkpoint = tokio::time::interval(Duration::from_secs(2));
        checkpoint.tick().await;
        loop {
            tokio::select! {
                biased;
                _ = turn.stop.notified() => break Ok(provider::Stats::default()),
                result = &mut run => break result,
                _ = checkpoint.tick() => {
                    let _write = st.chat_lock.lock().await;
                    let messages = {
                        let mut live = turn.state.lock().unwrap();
                        if !live.dirty { continue }
                        live.dirty = false;
                        live.messages.clone()
                    };
                    if let Err(err) = server_store::write_messages(st.db.as_ref().unwrap(), id, &messages, None).await {
                        eprintln!("vicuna: {err}");
                        turn.state.lock().unwrap().dirty = true;
                    }
                }
            }
        }
    };

    let _write = st.chat_lock.lock().await;
    let messages = {
        let mut live = turn.state.lock().unwrap();
        let index = live.index;
        if result.is_err() && !live.produced && !turn.extending {
            live.messages.truncate(index);
        } else {
            let msg = &mut live.messages[index];
            let ms = wall.elapsed().as_millis() as u64;
            msg["genMs"] = json!(msg["genMs"].as_f64().unwrap_or(0.0) + ms as f64);
            if let Ok(stats) = &result {
                let seconds = stats.seconds.unwrap_or(ms as f64 / 1000.0);
                if stats.tokens > 0 && seconds > 0.0 {
                    msg["genTps"] = json!(stats.tokens as f64 / seconds);
                }
                if let Some(cost) = stats.cost {
                    let cost = msg["genCost"].as_f64().unwrap_or(0.0) + cost;
                    if cost > 0.0 {
                        msg["genCost"] = json!(cost);
                    }
                }
            }
        }
        live.messages.clone()
    };
    let write = server_store::write_messages(st.db.as_ref().unwrap(), id, &messages, None).await;
    st.chat_turns.lock().unwrap().remove(id);
    let event = match write.map(|_| ()).and(result.map(|_| ())) {
        Ok(()) => json!({"type": "done"}),
        Err(message) => json!({"type": "error", "message": message}),
    };
    let mut live = turn.state.lock().unwrap();
    publish(st, &live, event);
    let channels = std::mem::take(&mut live.channels);
    if !channels.is_empty() {
        let events = st.events.clone();
        let polls = st.last_poll.clone();
        tokio::spawn(async move {
            tokio::time::sleep(CHANNEL_RETENTION).await;
            let mut polls = polls.lock().unwrap();
            let mut events = events.lock().unwrap();
            for id in channels {
                events.remove(&id);
                polls.remove(&id);
            }
        });
    }
}

fn token(st: &BridgeState, turn: &Turn, mut text: &str, thinking: bool, raw: bool) -> bool {
    if text.is_empty() {
        return true;
    }
    let mut live = turn.state.lock().unwrap();
    live.produced = true;
    live.dirty = true;
    let index = live.index;
    let field = if thinking { "thinking" } else { "content" };
    if !thinking {
        if live.first_token && live.original.ends_with([' ', '\t']) {
            text = text.trim_start_matches([' ', '\t']);
        }
        live.first_token = false;
        if raw {
            live.generated.push_str(text);
        }
        if raw && provider::trim_repetition(&mut live.generated) {
            let content = format!("{}{}", live.original, live.generated);
            live.messages[index][field] = json!(content);
            publish(st, &live, json!({"type": "replace", "text": content}));
            return false;
        }
    }
    if let Value::String(full) = &mut live.messages[index][field] {
        full.push_str(text);
    } else {
        live.messages[index][field] = json!(text);
    }
    let kind = if thinking { "think" } else { "token" };
    publish(st, &live, json!({"type": kind, "text": text}));
    true
}

fn images_stripped(st: &BridgeState, turn: &Turn) {
    let mut live = turn.state.lock().unwrap();
    live.images_stripped = true;
    publish(st, &live, json!({"type": "images_stripped"}));
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn state() -> BridgeState {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        server_store::init_schema(&db).await.unwrap();
        BridgeState {
            db: Some(db),
            ..Default::default()
        }
    }

    async fn stored(st: &BridgeState, id: &str) -> Value {
        server_store::load_chat(st.db.as_ref().unwrap(), id)
            .await
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn reload_gets_snapshot_then_only_new_tokens_and_stop_saves_partial() {
        let st = Arc::new(state().await);
        let args = json!({
            "id": "chat", "model": "gemma4", "operation": "send", "content": "hello",
            "onEvent": {"__bridge_channel": "first"},
        });
        let (start, turn, _) = prepare(&st, &args).await.unwrap();
        assert_eq!(start["chat"]["title"], "hello");
        assert!(prepare(&st, &args).await.is_err());
        assert!(
            actions::dispatch(&st, "message_delete", &json!({"id": "chat", "index": 1}))
                .await
                .unwrap_err()
                .contains("running")
        );
        // A second window sees its snapshot even if the first is no longer polling.
        token(&st, &turn, "one ", false, false);
        let attached = attach(
            &st,
            &json!({
                "id": "chat", "onEvent": {"__bridge_channel": "reload"},
            }),
        )
        .await
        .unwrap();
        assert_eq!(attached["turnId"], start["turnId"]);
        assert_eq!(attached["chat"]["messages"][1]["content"], "one ");
        token(&st, &turn, "two", false, false);
        actions::dispatch(
            &st,
            "session_update",
            &json!({"id": "chat", "title": "renamed"}),
        )
        .await
        .unwrap();
        assert!(!stop(&st, "chat", Some("different-turn")));
        assert!(stop(&st, "chat", start["turnId"].as_str()));
        record(&st, "chat", &turn, std::future::pending()).await;
        let saved = stored(&st, "chat").await;
        assert_eq!(saved["messages"][1]["content"], "one two");
        assert_eq!(saved["title"], "renamed");
        assert!(saved["messages"][1]["genMs"].is_number());
        assert!(attach(&st, &json!({"id": "chat"})).await.unwrap().is_null());
        assert_eq!(
            super::super::drain_events(&st.events, "first"),
            vec![
                json!({"type": "token", "text": "one two"}),
                json!({"type": "done"}),
            ]
        );
        assert_eq!(
            super::super::drain_events(&st.events, "reload"),
            vec![
                json!({"type": "token", "text": "two"}),
                json!({"type": "done"}),
            ]
        );
    }

    #[tokio::test]
    async fn continue_regenerate_and_fork_edit_update_the_expected_transcript() {
        let st = state().await;
        let (_, send, _) = prepare(
            &st,
            &json!({
                "id": "chat", "model": "qwen3", "operation": "send", "content": "question",
            }),
        )
        .await
        .unwrap();
        token(&st, &send, "answer ", false, false);
        send.state.lock().unwrap().messages[1]["genMs"] = json!(123.5);
        record(
            &st,
            "chat",
            &send,
            std::future::ready(Ok(provider::Stats::default())),
        )
        .await;

        let base = stored(&st, "chat").await["rev"].clone();
        let (_, continuation, _) = prepare(&st, &json!({
            "id": "chat", "baseRev": base, "model": "qwen3", "operation": "continue", "index": 1,
        })).await.unwrap();
        assert_eq!(
            model_history(&continuation.state.lock().unwrap(), true)[1]["content"],
            "answer "
        );
        token(&st, &continuation, " more", false, false);
        record(
            &st,
            "chat",
            &continuation,
            std::future::ready(Ok(provider::Stats::default())),
        )
        .await;
        let reply = &stored(&st, "chat").await["messages"][1];
        assert_eq!(reply["content"], "answer more");
        assert!(reply["genMs"].as_f64().unwrap() >= 123.5);
        assert!(prepare(&st, &json!({
            "id": "chat", "baseRev": base, "model": "qwen3", "operation": "regenerate", "index": 1,
        })).await.err().unwrap().contains(super::super::SESSION_CHANGED));

        let (fork, fork_turn, _) = prepare(
            &st,
            &json!({
                "id": "chat", "forkId": "fork", "model": "qwen3", "operation": "fork_edit",
                "index": 0, "content": "another question",
            }),
        )
        .await
        .unwrap();
        assert_eq!(fork["id"], "fork");
        assert_eq!(fork["chat"]["forkedFrom"]["id"], "chat");
        token(&st, &fork_turn, "other answer", false, false);
        let stats = provider::Stats {
            tokens: 6,
            seconds: Some(2.0),
            cost: Some(0.01),
        };
        record(&st, "fork", &fork_turn, std::future::ready(Ok(stats))).await;
        assert_eq!(
            stored(&st, "chat").await["messages"][0]["content"],
            "question"
        );
        let messages = &stored(&st, "fork").await["messages"];
        assert_eq!(messages[0]["content"], "another question");
        assert_eq!(messages[1]["content"], "other answer");
        assert_eq!(messages[1]["genTps"], 3.0);
        assert_eq!(messages[1]["genCost"], 0.01);

        let (regen, regen_turn, _) = prepare(
            &st,
            &json!({
                "id": "fork", "model": "gemma4", "operation": "regenerate", "index": 1,
            }),
        )
        .await
        .unwrap();
        assert_eq!(regen["chat"]["messages"][1]["model"], "gemma4");
        assert_eq!(regen["chat"]["messages"][1]["content"], "");
        record(
            &st,
            "fork",
            &regen_turn,
            std::future::ready(Err("provider failed".into())),
        )
        .await;
        assert_eq!(
            stored(&st, "fork").await["messages"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }
}
