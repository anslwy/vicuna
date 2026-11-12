// Vicuna backend: all Ollama inference traffic goes through here.
// The frontend (JS) is display-only: it invokes these commands and renders
// the streamed events. Direct `fetch("http://localhost:11434/...")` from JS
// is only a web-dev fallback (see src/lib/ollama.ts).

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tauri::ipc::Channel;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, State, WindowEvent};
use tauri_plugin_sql::{DbInstances, DbPool};
pub mod acp;
pub mod browser;
pub mod mcp;
pub mod opencode;
pub mod exe_resolve;
pub mod claude;
pub mod claude_cli;
pub mod codex;
pub mod codex_cli;
pub mod work_engine;
pub mod work_agent;
pub mod vicuna_mcp;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

pub const OLLAMA_BASE: &str = "http://localhost:11434";

/// Windows: keep background children from flashing a console window on the
/// user's screen (agent `exec`, its timeout killer, self-started
/// `ollama serve`, pid kills). No-op on other platforms.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// ---- chat storage: atomic full rewrite (see store.ts saveChats) ----
// The JS rewrite used to run DELETE + per-message INSERTs as separate plugin
// statements. A page unload (right-click → Refresh on Windows) kills the
// awaiting JS loop right after the DELETEs land, leaving the tables empty —
// history was lost exactly this way (live, 2026-11-11). The plugin pool runs
// each statement on a possibly different connection, so BEGIN/COMMIT from JS
// is unsafe; one sqlx transaction on that pool makes the rewrite
// all-or-nothing: a save killed mid-flight rolls back instead of truncating.

const CHATS_DB_KEY: &str = "sqlite:vicuna.db";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct MsgSaveRow {
    role: String,
    content: String,
    thinking: String,
    model: Option<String>,
    images: Option<Vec<String>>,
    gen_ms: Option<f64>,
    gen_tps: Option<f64>,
    gen_cost: Option<f64>,
    summary: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ChatSaveRow {
    id: String,
    title: String,
    created_at: i64,
    updated_at: Option<i64>,
    forked_from: Option<String>,
    fork_from_index: Option<i64>,
    mode: Option<String>,
    workspace: Option<String>,
    goal: Option<String>,
    messages: Vec<MsgSaveRow>,
}

#[tauri::command]
async fn chats_save(
    instances: State<'_, DbInstances>,
    chats: Vec<ChatSaveRow>,
) -> Result<(), String> {
    let instances = instances.0.read().await;
    let pool = match instances.get(CHATS_DB_KEY) {
        Some(DbPool::Sqlite(pool)) => pool,
        Some(_) => return Err("chats_save: vicuna.db pool is not sqlite".into()),
        None => return Err("chats_save: vicuna.db is not loaded".into()),
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("chats_save begin failed: {e}"))?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
    sqlx::query("DELETE FROM chats")
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
    for chat in chats {
        sqlx::query(
            "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(chat.id.clone())
        .bind(chat.title.clone())
        .bind(chat.created_at)
        .bind(chat.forked_from)
        .bind(chat.fork_from_index)
        .bind(chat.mode.unwrap_or_else(|| "chat".into()))
        .bind(chat.workspace)
        .bind(chat.goal)
        .bind(chat.updated_at.unwrap_or(chat.created_at))
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
        for (i, m) in chat.messages.iter().enumerate() {
            let images = m
                .images
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default());
            sqlx::query(
                "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            )
            .bind(chat.id.clone())
            .bind(m.role.clone())
            .bind(m.content.clone())
            .bind(m.thinking.clone())
            .bind(m.model.clone())
            .bind(i as i64)
            .bind(images)
            .bind(m.gen_ms)
            .bind(m.gen_tps)
            .bind(m.summary.map(|b| if b { 1 } else { 0 }))
            .bind(m.gen_cost)
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("chats_save: {e}"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| format!("chats_save commit failed: {e}"))?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ChatMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ChatEvent {
    Token { text: String },
    Think { text: String },
    Done {
        done_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        eval_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        eval_duration: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tool_calls: Option<Vec<serde_json::Value>>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum GenerateEvent {
    Token {
        text: String,
    },
    Done {
        done_reason: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        eval_count: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        eval_duration: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum PullEvent {
    Progress {
        status: String,
        digest: Option<String>,
        total: Option<u64>,
        completed: Option<u64>,
    },
}

fn ollama_error(context: &str, status: reqwest::StatusCode, body: &str) -> String {
    format!("ollama {context} failed: {status} {body}")
}

#[tauri::command]
async fn ollama_list_models() -> Result<serde_json::Value, String> {
    let res = reqwest::get(format!("{OLLAMA_BASE}/api/tags"))
        .await
        .map_err(|e| format!("ollama tags failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("tags", status, &body));
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("ollama tags failed: {e}"))?;
    Ok(data)
}

#[tauri::command]
async fn ollama_list_running() -> Result<serde_json::Value, String> {
    let res = reqwest::get(format!("{OLLAMA_BASE}/api/ps"))
        .await
        .map_err(|e| format!("ollama ps failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("ps", status, &body));
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("ollama ps failed: {e}"))?;
    Ok(data)
}

/// Model metadata for context management: reports the architecture's
/// `*.context_length` from /api/show `model_info` (works for `:cloud`
/// models too — the local daemon proxies). `None` when the model isn't
/// available or reports no window; callers fall back to 32768.
pub fn parse_show_context_length(model_info: &serde_json::Value) -> Option<u64> {
    let map = model_info.as_object()?;
    let mut best: Option<u64> = None;
    for (k, v) in map {
        if k.ends_with(".context_length") {
            if let Some(n) = v.as_u64().filter(|&n| n > 0) {
                if best.is_none_or(|b| n > b) {
                    best = Some(n);
                }
            }
        }
    }
    best
}

#[derive(Debug, Clone, Serialize)]
struct ThinkingInfo {
    values: Vec<serde_json::Value>,
    default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
struct ModelShowInfo {
    context_length: Option<u64>,
    thinking: Option<ThinkingInfo>,
    family: Option<String>,
}

#[tauri::command]
async fn ollama_model_show(model: String) -> Result<ModelShowInfo, String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OLLAMA_BASE}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| format!("ollama show failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("show", status, &body));
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("ollama show failed: {e}"))?;
    Ok(ModelShowInfo {
        context_length: data.get("model_info").and_then(parse_show_context_length),
        thinking: data.get("thinking").and_then(|t| {
            let values = t
                .get("values")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if values.is_empty() {
                return None;
            }
            Some(ThinkingInfo {
                values,
                default: t.get("default").cloned(),
            })
        }),
        family: data
            .get("details")
            .and_then(|d| d.get("family"))
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string()),
    })
}

/// Load/unload a model by generating one throwaway token with the given
/// keep-alive. Same 32k ctx as chat so follow-ups don't trigger a reload.
#[tauri::command]
async fn ollama_warm_model(model: String, keep_alive: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OLLAMA_BASE}/api/generate"))
        .json(&serde_json::json!({
            "model": model,
            "prompt": "hi",            "stream": false,
            "think": false,
            "keep_alive": keep_alive,
            "options": { "num_predict": 1, "num_ctx": 32768 }
        }))
        .send()
        .await
        .map_err(|e| format!("ollama warm failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        let action = if keep_alive == "0" { "unload" } else { "load" };
        return Err(format!("model {action} failed: {status} {body}"));
    }
    Ok(())
}

/// Streaming chat completion. Emits Token/Think/Done events over the channel
/// and returns the full concatenated content. `stop` words are forwarded as
/// `options.stop` so the model ends the turn instead of roleplaying new ones.
#[tauri::command]
async fn ollama_chat_stream(
    model: String,
    messages: Vec<ChatMessage>,
    think: Option<serde_json::Value>,
    temperature: Option<f64>,
    stop: Option<Vec<String>>,
    tools: Option<serde_json::Value>,
    on_event: Channel<ChatEvent>,
) -> Result<String, String> {
    let mut options = serde_json::Map::new();
    options.insert("num_ctx".to_string(), serde_json::json!(32768));
    if let Some(t) = temperature {
        options.insert("temperature".to_string(), serde_json::json!(t));
    }
    if let Some(s) = stop {
        options.insert("stop".to_string(), serde_json::json!(s));
    }
    let client = reqwest::Client::new();
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": true,
        "think": think.unwrap_or(serde_json::Value::Bool(true)),
        "options": options,
    });
    if let Some(t) = tools {
        if !t.is_null() {
            body["tools"] = t;
        }
    }
    let res = client
        .post(format!("{OLLAMA_BASE}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ollama chat failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("chat", status, &body));
    }
    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();
    let mut done_reason: Option<String> = None;
    let mut eval_count: Option<u64> = None;
    let mut eval_duration: Option<u64> = None;
    let mut tool_calls: Vec<serde_json::Value> = Vec::new();
    let mut client_gone = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("ollama chat failed: {e}"))?;
        buf.extend_from_slice(&chunk);
        // split off complete lines; keep the trailing partial line buffered
        let mut start = 0;
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                let line = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
                start = i + 1;
                if line.is_empty() {
                    continue;
                }
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(r) = obj.get("done_reason").and_then(|v| v.as_str()) {
                        done_reason = Some(r.to_string());
                    }
                    if eval_count.is_none() {
                        eval_count = obj.get("eval_count").and_then(|v| v.as_u64());
                    }
                    if eval_duration.is_none() {
                        eval_duration = obj.get("eval_duration").and_then(|v| v.as_u64());
                    }
                    if let Some(tc) = obj.get("message").and_then(|m| m.get("tool_calls")).and_then(|v| v.as_array()) {
                        tool_calls.extend(tc.clone());
                    }
                    if let Some(t) = obj
                        .get("message")
                        .and_then(|m| m.get("thinking"))
                        .and_then(|v| v.as_str())
                    {
                        if !t.is_empty() {
                            if on_event
                                .send(ChatEvent::Think { text: t.to_string() })
                                .is_err()
                            {
                                // frontend went away (e.g. refresh): stop
                                // draining Ollama instead of streaming nowhere
                                client_gone = true;
                                break;
                            }
                        }
                    }
                    if let Some(t) = obj
                        .get("message")
                        .and_then(|m| m.get("content"))
                        .and_then(|v| v.as_str())
                    {
                        if !t.is_empty() {
                            full.push_str(t);
                            if on_event
                                .send(ChatEvent::Token { text: t.to_string() })
                                .is_err()
                            {
                                client_gone = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
        buf.drain(..start);
        if client_gone {
            buf.clear();
            break;
        }
    }
    // flush remainder
    let rest = String::from_utf8_lossy(&buf).trim().to_string();
    if !rest.is_empty() {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&rest) {
            if let Some(r) = obj.get("done_reason").and_then(|v| v.as_str()) {
                done_reason = Some(r.to_string());
            }
            if eval_count.is_none() {
                eval_count = obj.get("eval_count").and_then(|v| v.as_u64());
            }
            if eval_duration.is_none() {
                eval_duration = obj.get("eval_duration").and_then(|v| v.as_u64());
            }
            if let Some(tc) = obj.get("message").and_then(|m| m.get("tool_calls")).and_then(|v| v.as_array()) {
                tool_calls.extend(tc.clone());
            }
            if let Some(t) = obj
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|v| v.as_str())
            {
                if !t.is_empty() {
                    full.push_str(t);
                    let _ = on_event.send(ChatEvent::Token { text: t.to_string() });
                }
            }
        }
    }
    let tool_calls_opt = if tool_calls.is_empty() { None } else { Some(tool_calls) };
    let _ = on_event.send(ChatEvent::Done {
        done_reason,
        eval_count,
        eval_duration,
        tool_calls: tool_calls_opt,
    });
    Ok(full)
}

/// Raw text completion (no chat template). Continue on gemma-family models
/// uses this with the family's own prompt prefill (src/lib/continue.ts) —
/// their chat template closes the assistant turn, so /api/chat starts a new
/// turn; every other Continue goes through /api/chat.
#[tauri::command]
async fn ollama_generate_stream(
    model: String,
    prompt: String,
    temperature: Option<f64>,
    stop: Option<Vec<String>>,
    on_event: Channel<GenerateEvent>,
) -> Result<String, String> {
    let mut options = serde_json::Map::new();
    options.insert("num_ctx".to_string(), serde_json::json!(32768));
    if let Some(t) = temperature {
        options.insert("temperature".to_string(), serde_json::json!(t));
    }
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OLLAMA_BASE}/api/generate"))
        .json(&serde_json::json!({
            "model": model,
            "prompt": prompt,
            "stream": true,
            "raw": true,
            "think": false,
            "stop": stop.unwrap_or_else(|| vec![
                "\nUser:".to_string(),
                "\nAssistant:".to_string(),
                "<end_of_turn>".to_string(),
                "<|im_end|>".to_string(),
            ]),
            "options": options,
        }))
        .send()
        .await
        .map_err(|e| format!("ollama generate failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("generate", status, &body));
    }
    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();
    let mut done_reason: Option<String> = None;
    let mut eval_count: Option<u64> = None;
    let mut eval_duration: Option<u64> = None;
    let mut client_gone = false;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("ollama generate failed: {e}"))?;
        buf.extend_from_slice(&chunk);
        let mut start = 0;
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                let line = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
                start = i + 1;
                if line.is_empty() {
                    continue;
                }
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(r) = obj.get("done_reason").and_then(|v| v.as_str()) {
                        done_reason = Some(r.to_string());
                    }
                    if eval_count.is_none() {
                        eval_count = obj.get("eval_count").and_then(|v| v.as_u64());
                    }
                    if eval_duration.is_none() {
                        eval_duration = obj.get("eval_duration").and_then(|v| v.as_u64());
                    }
                    if let Some(t) = obj.get("response").and_then(|v| v.as_str()) {
                        if !t.is_empty() {
                            full.push_str(t);
                            if on_event
                                .send(GenerateEvent::Token { text: t.to_string() })
                                .is_err()
                            {
                                // frontend went away: stop draining Ollama
                                client_gone = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
        buf.drain(..start);
        if client_gone {
            buf.clear();
            break;
        }
    }
    let rest = String::from_utf8_lossy(&buf).trim().to_string();
    if !rest.is_empty() {
        if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&rest) {
            if let Some(r) = obj.get("done_reason").and_then(|v| v.as_str()) {
                done_reason = Some(r.to_string());
            }
            if eval_count.is_none() {
                eval_count = obj.get("eval_count").and_then(|v| v.as_u64());
            }
            if eval_duration.is_none() {
                eval_duration = obj.get("eval_duration").and_then(|v| v.as_u64());
            }
            if let Some(t) = obj.get("response").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    full.push_str(t);
                    let _ = on_event.send(GenerateEvent::Token { text: t.to_string() });
                }
            }
        }
    }
    let _ = on_event.send(GenerateEvent::Done { done_reason, eval_count, eval_duration });
    Ok(full)
}

/// Streaming model pull. Forwards each /api/pull status line as a Progress
/// event; a final line with {"status":"success"} ends the stream. An
/// {"error": ...} line fails the command.
#[tauri::command]
async fn ollama_pull_model(model: String, on_event: Channel<PullEvent>) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OLLAMA_BASE}/api/pull"))
        .json(&serde_json::json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|e| format!("ollama pull failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("pull", status, &body));
    }
    let mut stream = res.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("ollama pull failed: {e}"))?;
        buf.extend_from_slice(&chunk);
        let mut start = 0;
        for i in 0..buf.len() {
            if buf[i] == b'\n' {
                let line = String::from_utf8_lossy(&buf[start..i]).trim().to_string();
                start = i + 1;
                if line.is_empty() {
                    continue;
                }
                if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
                    if let Some(err) = obj.get("error").and_then(|v| v.as_str()) {
                        if !err.is_empty() {
                            return Err(format!("ollama pull failed: {err}"));
                        }
                    }
                    if let Some(status) = obj.get("status").and_then(|v| v.as_str()) {
                        let ev = PullEvent::Progress {
                            status: status.to_string(),
                            digest: obj
                                .get("digest")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            total: obj.get("total").and_then(|v| v.as_u64()),
                            completed: obj.get("completed").and_then(|v| v.as_u64()),
                        };
                        if on_event.send(ev).is_err() {
                            // frontend went away (e.g. refresh): stop draining
                            return Ok(());
                        }
                    }
                }
            }
        }
        buf.drain(..start);
    }
    Ok(())
}

/// Delete a model from local storage. The frontend unloads it first so the
/// delete is never rejected as in-use.
#[tauri::command]
async fn ollama_delete_model(model: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .delete(format!("{OLLAMA_BASE}/api/delete"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
        .map_err(|e| format!("ollama delete failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("delete", status, &body));
    }
    Ok(())
}

/// Fetch a registry page as text (powers the Discover "Fetch" button).
/// The browser cannot do this itself — ollama.com and registry.ollama.ai
/// send no CORS headers — but the Rust backend has no such restriction.
/// The URL is allowlisted to the public Ollama registry so the command
/// cannot be abused as a generic proxy. Bounded by a timeout: without one,
/// a single stalled manifest pinned the Fetch progress at N-1/N indefinitely.
pub async fn ollama_fetch_text_logic(url: String) -> Result<String, String> {
    const ALLOWED: [&str; 2] = [
        "https://ollama.com/library",
        "https://registry.ollama.ai/v2/library/",
    ];
    if !ALLOWED.iter().any(|p| url.starts_with(p)) {
        return Err("refusing to fetch non-registry URL".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("registry fetch failed: {e}"))?
        .get(&url)
        .header("User-Agent", "vicuna-catalog-fetch/1.0")
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .send()
        .await
        .map_err(|e| format!("registry fetch failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(ollama_error("fetch", status, &body));
    }
    res.text()
        .await
        .map_err(|e| format!("registry fetch failed: {e}"))
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkFetchContent {
    pub url: String,
    pub content: String,
    pub truncated: bool,
    pub size: u64,
}

/// Fetch a web page as plain text (the agent's `fetch_url` tool: read the
/// article behind a search-result link instead of guessing from snippets).
/// Unlike `ollama_fetch_text` — allowlisted to the Ollama registry for
/// catalog Fetch — this takes any http(s) URL: the agent needs the open web.
/// http(s) scheme only, 30s timeout, 1MB raw cap, HTML stripped to text,
/// ~64KB text cap like `read`.
pub async fn work_fetch_url_logic(url: String) -> Result<WorkFetchContent, String> {
    let url = url.trim().to_string();
    if url.is_empty() || url.len() > 2048 {
        return Err("fetch_url needs a full http(s) URL".to_string());
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("fetch_url needs a full http(s) URL (not a bare host or other scheme)".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("fetch failed: {e}"))?
        .get(&url)
        .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0 Safari/537.36")
        .header("Accept", "text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8")
        .send()
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("fetch failed: HTTP {}", res.status()));
    }
    let ctype = res
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_lowercase();
    if !ctype.is_empty()
        && !ctype.contains("html")
        && !ctype.starts_with("text/")
        && !ctype.contains("json")
        && !ctype.contains("xml")
        && !ctype.contains("javascript")
    {
        let short = ctype.split(';').next().unwrap_or(&ctype);
        return Err(format!(
            "unsupported content type ({short}) — fetch_url reads pages and text, not files"
        ));
    }
    let bytes = res.bytes().await.map_err(|e| format!("fetch failed: {e}"))?;
    const RAW_CAP: usize = 1024 * 1024;
    let raw = &bytes[..bytes.len().min(RAW_CAP)];
    if raw.iter().take(4096).any(|&b| b == 0) {
        return Err("binary content — fetch_url reads text pages only".to_string());
    }
    let raw_str = String::from_utf8_lossy(raw);
    let sniff_html = raw_str.trim_start().starts_with('<');
    let text = if ctype.contains("html") || (ctype.is_empty() && sniff_html) {
        html_to_text(&raw_str)
    } else {
        raw_str.trim().to_string()
    };
    if text.is_empty() {
        return Err("page contained no readable text".to_string());
    }
    const TEXT_CAP: usize = 65536;
    let full_len = text.chars().count() as u64;
    let truncated = full_len > TEXT_CAP as u64;
    let content = if truncated {
        text.chars().take(TEXT_CAP).collect()
    } else {
        text
    };
    Ok(WorkFetchContent {
        url,
        content,
        truncated,
        size: full_len,
    })
}

/// Block-level tags that break a line in the extracted text.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "ul"
            | "ol"
            | "br"
            | "hr"
            | "tr"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "nav"
            | "aside"
            | "main"
            | "blockquote"
            | "pre"
            | "figure"
            | "figcaption"
            | "dd"
            | "dt"
            | "dl"
    )
}

/// Byte offset of `</name` (ASCII case-insensitive) in `hay`, if any.
/// A whole-string `to_lowercase` would shift byte indices on some Unicode,
/// so the comparison stays byte-wise.
fn find_close_tag(hay: &[u8], name: &str) -> Option<usize> {
    let needle = format!("</{name}");
    let nb = needle.as_bytes();
    if hay.len() < nb.len() {
        return None;
    }
    hay.windows(nb.len()).position(|w| w.eq_ignore_ascii_case(nb))
}

/// Contents of the first `<title>` tag, entity-decoded and flattened to one
/// line. News pages put the headline there, so it leads the fetched text.
fn extract_html_title(html: &str) -> Option<String> {
    let bytes = html.as_bytes();
    if bytes.len() < 6 {
        return None;
    }
    let open = bytes.windows(6).position(|w| w.eq_ignore_ascii_case(b"<title"))?;
    let after_open = html[open..].find('>')? + open + 1;
    let tail = &bytes[after_open..];
    if tail.len() < 8 {
        return None;
    }
    let rel = tail.windows(8).position(|w| w.eq_ignore_ascii_case(b"</title>"))?;
    let inner = &html[after_open..after_open + rel];
    // titles carry no markup, but strip tags defensively
    let mut text = String::with_capacity(inner.len());
    let mut skipping = false;
    for c in inner.chars() {
        if c == '<' {
            skipping = true;
            continue;
        }
        if c == '>' {
            skipping = false;
            continue;
        }
        if !skipping {
            text.push(c);
        }
    }
    let flat = decode_html_entities(&text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.is_empty() { None } else { Some(flat) }
}

/// One entity body (between `&` and `;`): the five XML predefines,
/// `&nbsp;` (as a plain space), and numeric `&#123;` / `&#x1F;` references.
/// Unknown entities return None and pass through untouched.
fn decode_entity(ent: &str) -> Option<String> {
    match ent {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" | "#39" => Some("'".to_string()),
        "nbsp" => Some(" ".to_string()),
        _ if ent.starts_with('#') => {
            let num = &ent[1..];
            let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                u32::from_str_radix(hex, 16).ok()
            } else {
                num.parse::<u32>().ok()
            };
            code.and_then(char::from_u32).map(|c| {
                if c == '\u{a0}' {
                    " ".to_string()
                } else {
                    c.to_string()
                }
            })
        }
        _ => None,
    }
}

/// Decode HTML entities in text. Unknown entities pass through untouched.
fn decode_html_entities(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&'
            && let Some(semi) = bytes[i..]
                .iter()
                .position(|&b| b == b';')
                .filter(|&p| p > 0 && p <= 12)
                .map(|p| i + p)
            && let Some(d) = decode_entity(&s[i + 1..semi])
        {
            out.push_str(&d);
            i = semi + 1;
            continue;
        }
        // `&` without a valid entity (or any other byte) copies through;
        // multibyte chars copy whole so indices stay on char boundaries.
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch_len = s[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&s[i..end]);
            i = end;
        }
    }
    out
}

/// Strip HTML down to readable text: drop script/style blocks (Next.js pages
/// bury 100KB+ of JSON in `__NEXT_DATA__`), turn block tags into newlines,
/// strip the rest, decode entities, collapse whitespace. Single pass, no
/// dependencies; byte-wise with char-boundary-safe advances.
fn html_to_text(html: &str) -> String {
    let title = extract_html_title(html);
    let bytes = html.as_bytes();
    let mut out = String::with_capacity(html.len() / 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if html[i..].starts_with("<!--") {
                match html[i..].find("-->") {
                    Some(end) => i += end + 3,
                    None => break,
                }
                continue;
            }
            let mut j = i + 1;
            let closing = j < bytes.len() && bytes[j] == b'/';
            if closing {
                j += 1;
            }
            let name_start = j;
            while j < bytes.len() && bytes[j].is_ascii_alphanumeric() {
                j += 1;
            }
            let name = html[name_start..j].to_lowercase();
            if name.is_empty() && !closing {
                // not a tag: doctype/processing-instruction skips to '>',
                // a bare `<` in prose stays literal
                if j < bytes.len() && (bytes[j] == b'!' || bytes[j] == b'?') {
                    i = html[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
                } else {
                    out.push('<');
                    i += 1;
                }
                continue;
            }
            if (name == "script" || name == "style") && !closing {
                match find_close_tag(&bytes[i..], &name) {
                    Some(p) => {
                        let rest = &html[i + p..];
                        i += p + rest.find('>').map(|q| q + 1).unwrap_or(rest.len());
                    }
                    None => break,
                }
                continue;
            }
            if is_block_tag(&name) {
                out.push('\n');
            }
            if name == "td" || name == "th" {
                out.push(' ');
            }
            i = html[i..].find('>').map(|p| i + p + 1).unwrap_or(bytes.len());
            continue;
        }
        if bytes[i] < 0x80 {
            out.push(bytes[i] as char);
            i += 1;
        } else {
            let ch_len = html[i..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
            let end = (i + ch_len).min(bytes.len());
            out.push_str(&html[i..end]);
            i = end;
        }
    }
    let decoded = decode_html_entities(&out);
    let body = decoded
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match title {
        Some(t) if !t.is_empty() && !body.starts_with(&t) => format!("{t}\n\n{body}"),
        _ => body,
    }
}

/// Where the `ollama` binary may live. GUI apps launched from Finder get a
/// sparse PATH (`/usr/bin:/bin/...`), so a Homebrew install is invisible to
/// a bare `"ollama"` lookup — probe the usual locations in order.
fn ollama_candidates() -> [&'static str; 3] {
    [
        "ollama",
        "/opt/homebrew/bin/ollama",
        "/usr/local/bin/ollama",
    ]
}

/// The `ollama serve` child this app launched (if any). Only a server Vicuna
/// itself started can be stopped from here — a foreign `ollama serve` (user
/// terminal, another app) is left alone.
#[derive(Default)]
struct OllamaServerState {
    child: Mutex<Option<Child>>,
}

async fn ollama_version_inner(client: &reqwest::Client) -> Result<String, String> {
    let res = client
        .get(format!("{OLLAMA_BASE}/api/version"))
        .send()
        .await
        .map_err(|_| "ollama server not reachable".to_string())?;
    if !res.status().is_success() {
        return Err("ollama server not reachable".to_string());
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|_| "ollama server not reachable".to_string())?;
    Ok(data
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string())
}

/// Report the local Ollama server version, or an error when it is down.
#[tauri::command]
async fn ollama_server_version() -> Result<String, String> {
    let client = reqwest::Client::new();
    ollama_version_inner(&client).await
}

/// Start a local `ollama serve` when none is reachable. If a server already
/// answers (user's terminal, another app), it is adopted as-is instead of
/// spawning a second one on the busy port.
#[tauri::command]
async fn ollama_start_server(state: State<'_, OllamaServerState>) -> Result<String, String> {
    let client = reqwest::Client::new();
    if let Ok(v) = ollama_version_inner(&client).await {
        return Ok(v);
    }
    let mut child: Option<Child> = None;
    let mut spawn_err = String::from("no `ollama` binary tried");
    for bin in ollama_candidates() {
        let mut serve = Command::new(bin);
        serve
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        serve.creation_flags(CREATE_NO_WINDOW);
        match serve.spawn()
        {
            Ok(c) => {
                child = Some(c);
                break;
            }
            Err(e) => spawn_err = format!("{bin}: {e}"),
        }
    }
    let mut child = child.ok_or_else(|| {
        format!("could not start `ollama serve` ({spawn_err}). Is Ollama installed? (brew install ollama)")
    })?;
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok(v) = ollama_version_inner(&client).await {
            state
                .child
                .lock()
                .map_err(|_| "server state poisoned".to_string())?
                .replace(child);
            return Ok(v);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "`ollama serve` exited ({status}): port in use or another error — run `ollama serve` in a terminal for details"
                ))
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(format!("could not watch `ollama serve`: {e}"));
            }
        }
    }
    let _ = child.kill();
    Err("`ollama serve` did not become ready in time".to_string())
}

/// Stop the `ollama serve` this app launched. Refuses when Vicuna didn't
/// start one — kill foreign servers in their own terminal.
#[tauri::command]
async fn ollama_stop_server(state: State<'_, OllamaServerState>) -> Result<(), String> {
    let mut guard = state
        .child
        .lock()
        .map_err(|_| "server state poisoned".to_string())?;
    match guard.take() {
        Some(mut child) => child
            .kill()
            .map_err(|e| format!("could not stop the server: {e}")),
        None => Err(
            "no server was started by Vicuna — stop your own `ollama serve` in its terminal"
                .to_string(),
        ),
    }
}

/// ---------- Work mode: workspace-scoped file + shell tools ----------
///
/// Minimal agent backend (Tauri desktop only — plain browsers have no FS).
/// Every path is sandboxed to the session's workspace root: the root is
/// canonicalized once, joined paths must stay under it, and `..` escapes are
/// rejected. Shell commands run with cwd=root, a timeout, and capped output.

pub fn work_canonical_root(root: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::Path::new(root);
    if !p.is_absolute() {
        return Err("workspace must be an absolute path".to_string());
    }
    let canon = p
        .canonicalize()
        .map_err(|_| "workspace folder not found".to_string())?;
    if !canon.is_dir() {
        return Err("workspace is not a folder".to_string());
    }
    // The filesystem root ("/", "C:\") would make every tool crawl the
    // whole machine — including macOS TCC-protected spots (Documents,
    // /Volumes network mounts) that re-prompt on every access. Refuse it.
    if canon.parent().is_none() {
        return Err("workspace cannot be the filesystem root — pick a project folder".to_string());
    }
    Ok(canon)
}

/// Join a session-relative path onto a canonical root, rejecting escapes.
/// Empty/`"."` resolves to the root itself.
pub fn work_join(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    work_join_inner(root, rel, false)
}

/// File tools (read/list/write/view_image) also accept absolute paths under
/// the system temp dir, so agents have a sanctioned place for scratch files
/// that is not the repository. Everything else outside the workspace stays
/// refused; exec is unaffected.
pub fn work_join_allow_temp(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    work_join_inner(root, rel, true)
}

fn canonicalize_lenient(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut current = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match std::fs::canonicalize(&current) {
            Ok(mut base) => {
                for part in tail.iter().rev() {
                    base.push(part);
                }
                return Some(base);
            }
            Err(_) => {
                let name = current.file_name()?.to_os_string();
                tail.push(name);
                if !current.pop() {
                    return None;
                }
            }
        }
    }
}

fn temp_roots() -> Vec<std::path::PathBuf> {
    let mut roots: Vec<std::path::PathBuf> = Vec::new();
    let mut push = |p: std::path::PathBuf| {
        let c = std::fs::canonicalize(&p).unwrap_or(p);
        if !roots.contains(&c) {
            roots.push(c);
        }
    };
    push(std::env::temp_dir());
    // Agents commonly reach for /tmp even when TMPDIR is unset (Rust's
    // temp_dir() uses the per-user Darwin temp dir on macOS); accept both.
    #[cfg(unix)]
    {
        push(std::path::PathBuf::from("/tmp"));
        push(std::path::PathBuf::from("/private/tmp"));
    }
    roots
}

fn path_is_under_temp(path: &std::path::Path) -> bool {
    let resolved = canonicalize_lenient(path).unwrap_or_else(|| path.to_path_buf());
    temp_roots()
        .iter()
        .any(|root| resolved.starts_with(root) || path.starts_with(root))
}

fn resolve_temp_absolute(abs: &std::path::Path) -> Result<std::path::PathBuf, String> {
    use std::path::Component;
    if abs.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err("path escapes the temp dir".to_string());
    }
    let resolved = canonicalize_lenient(abs).ok_or_else(|| "invalid path".to_string())?;
    if !temp_roots().iter().any(|root| resolved.starts_with(root)) {
        return Err(
            "absolute paths are only allowed under the system temp dir — use workspace-relative paths elsewhere"
                .to_string(),
        );
    }
    Ok(resolved)
}

fn work_join_inner(
    root: &std::path::Path,
    rel: &str,
    allow_temp: bool,
) -> Result<std::path::PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() || rel == "." || rel == "./" {
        return Ok(root.to_path_buf());
    }
    if std::path::Path::new(rel).is_absolute() {
        if allow_temp {
            return resolve_temp_absolute(std::path::Path::new(rel));
        }
        return Err("absolute paths are not allowed — use workspace-relative paths".to_string());
    }
    let mut cur = root.to_path_buf();
    for comp in std::path::Path::new(rel).components() {
        use std::path::Component::*;
        match comp {
            Normal(s) => cur.push(s),
            CurDir => {}
            ParentDir => {
                if !cur.pop() || !cur.starts_with(root) {
                    return Err("path escapes the workspace".to_string());
                }
            }
            _ => return Err("invalid path".to_string()),
        }
    }
    if !cur.starts_with(root) {
        return Err("path escapes the workspace".to_string());
    }
    Ok(cur)
}

/// Strip Rust's verbatim-path prefix for display + session storage.
/// `canonicalize()` on Windows returns extended-length paths (`\\?\C:\...`
/// or `\\?\UNC\server\...`); users should see `C:\...` / `\\server\...`.
/// Non-Windows paths pass through untouched.
fn work_display_string(p: &std::path::Path) -> String {
    let s = p.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Validate a workspace path: must exist and be a directory. Returns the
/// canonical absolute path for display + session storage (verbatim prefix
/// stripped on Windows so the UI never shows `\\?\C:\...`).
pub async fn work_validate_root_logic(path: String) -> Result<String, String> {
    Ok(work_display_string(&work_canonical_root(path.trim())?))
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkDirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
}

/// List a workspace-relative directory (default: root). Deny hidden dirs
/// are still listed — the model decides what matters.
pub async fn work_list_dir_logic(root: String, path: Option<String>) -> Result<Vec<WorkDirEntry>, String> {
    let canon = work_canonical_root(root.trim())?;
    let dir = work_join_allow_temp(&canon, path.as_deref().unwrap_or(""))?;
    if !dir.is_dir() {
        return Err("not a folder".to_string());
    }
    let mut out: Vec<WorkDirEntry> = Vec::new();
    let rd = std::fs::read_dir(&dir).map_err(|e| format!("cannot list folder: {e}"))?;
    for ent in rd.take(500) {
        let ent = ent.map_err(|e| format!("cannot list folder: {e}"))?;
        let ft = ent.file_type().map_err(|e| format!("cannot list folder: {e}"))?;
        let name = ent.file_name().to_string_lossy().to_string();
        let full_path = ent.path();
        let rel = full_path
            .strip_prefix(&canon)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| full_path.to_string_lossy().to_string());
        out.push(WorkDirEntry {
            name,
            path: rel,
            is_dir: ft.is_dir(),
            size: if ft.is_file() {
                ent.metadata().ok().map(|m| m.len())
            } else {
                None
            },
        });
    }
    out.sort_by(|a, b| {
        (b.is_dir as u8)
            .cmp(&(a.is_dir as u8))
            .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkTreeEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: Option<u64>,
    pub depth: usize,
}

/// Recursive workspace listing for the file explorer panel (flat, sorted
/// by path). Skips dependency/build dirs; hidden files ARE included.
/// Caps: depth 1-8 (default 4), 3000 entries.
pub async fn work_tree_logic(root: String, max_depth: Option<usize>) -> Result<Vec<WorkTreeEntry>, String> {
    let canon = work_canonical_root(root.trim())?;
    let max_depth = max_depth.unwrap_or(4).clamp(1, 8);
    const SKIP: [&str; 8] = [
        "node_modules",
        "target",
        "build",
        ".git",
        "dist",
        "__pycache__",
        ".venv",
        "venv",
    ];
    // Synthetic / system mounts: never legit project content, and on macOS
    // touching /Volumes re-triggers the network-volume consent prompt.
    const SKIP_SYSTEM: [&str; 6] = ["Volumes", "System", "private", "dev", "proc", "sys"];
    let mut out: Vec<WorkTreeEntry> = Vec::new();
    let mut stack = vec![(canon.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if out.len() >= 3000 {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd {
            if out.len() >= 3000 {
                break;
            }
            let ent = match ent {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = ent.file_name().to_string_lossy().to_string();
            let p = ent.path();
            let is_dir = ent.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir && (SKIP.contains(&name.as_str()) || SKIP_SYSTEM.contains(&name.as_str())) {
                continue;
            }
            let rel = p
                .strip_prefix(&canon)
                .map(|x| x.to_string_lossy().to_string())
                .unwrap_or_else(|_| name.clone());
            out.push(WorkTreeEntry {
                name,
                path: rel,
                is_dir,
                size: if is_dir {
                    None
                } else {
                    ent.metadata().ok().map(|m| m.len())
                },
                depth,
            });
            if is_dir && depth + 1 < max_depth {
                stack.push((p, depth + 1));
            }
        }
    }
    out.sort_by(|a, b| {
        a.path
            .to_lowercase()
            .cmp(&b.path.to_lowercase())
    });
    Ok(out)
}

/// Import a file dropped from the OS into the workspace (base64 bytes).
/// Sandbox-checked like every other work_ entry point. 16MB cap.
pub async fn work_import_file_logic(root: String, path: String, base64: String) -> Result<String, String> {
    const CAP: usize = 16 * 1024 * 1024;
    let bytes = decode_base64(&base64).map_err(|_| "invalid file data".to_string())?;
    if bytes.len() > CAP {
        return Err("file too large (max 16MB)".to_string());
    }
    let canon = work_canonical_root(root.trim())?;
    let full = work_join(&canon, &path)?;
    if full.is_dir() {
        return Err("cannot overwrite a folder".to_string());
    }
    if let Some(parent) = full.parent() {
        if !parent.starts_with(&canon) {
            return Err("path escapes the workspace".to_string());
        }
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create folders: {e}"))?;
    }
    std::fs::write(&full, &bytes).map_err(|e| format!("cannot write file: {e}"))?;
    Ok(path.trim().to_string())
}

/// Minimal standard-alphabet base64 decoder (avoids a new dependency for
/// the single import-file path). Rejects whitespace-tolerant input with
/// bad characters or misplaced padding.
fn decode_base64(s: &str) -> Result<Vec<u8>, ()> {
    fn val(c: u8) -> Result<u8, ()> {
        match c {
            b'A'..=b'Z' => Ok(c - b'A'),
            b'a'..=b'z' => Ok(c - b'a' + 26),
            b'0'..=b'9' => Ok(c - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err(()),
        }
    }
    let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return Err(());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut sextets = [0u8; 4];
        let mut pad = 0;
        for (i, &b) in chunk.iter().enumerate() {
            if b == b'=' {
                pad += 1;
            } else {
                if pad > 0 {
                    return Err(());
                }
                sextets[i] = val(b)?;
            }
        }
        if pad > 2 {
            return Err(());
        }
        let n = ((sextets[0] as u32) << 18)
            | ((sextets[1] as u32) << 12)
            | ((sextets[2] as u32) << 6)
            | (sextets[3] as u32);
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkFileContent {
    pub path: String,
    pub content: String,
    pub truncated: bool,
    pub size: u64,
}

/// Read a workspace-relative text file (cap ~64KB; binary smells rejected).
/// `offset` is a 1-based starting line and `limit` a line count: together they
/// make files larger than one read possible to inspect without writing blind.
pub async fn work_read_file_logic(
    root: String,
    path: String,
    max_bytes: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<WorkFileContent, String> {
    let canon = work_canonical_root(root.trim())?;
    let full = work_join_allow_temp(&canon, &path)?;
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    let cap = max_bytes.unwrap_or(65536).clamp(1024, 262144) as usize;
    let bytes = std::fs::read(&full).map_err(|e| format!("cannot read file: {e}"))?;
    // reject obvious binaries (NUL byte in the head sample)
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err("binary file — refusing to dump as text (use view_image for png/jpg/webp/gif)".to_string());
    }
    let text = String::from_utf8_lossy(&bytes);
    let ranged = offset.is_some() || limit.is_some();
    let (selected, more_lines) = if ranged {
        let lines: Vec<&str> = text.split_inclusive('\n').collect();
        let start = offset.unwrap_or(1).max(1) as usize - 1;
        let start = start.min(lines.len());
        let end = match limit {
            Some(n) if n > 0 => start.saturating_add(n as usize).min(lines.len()),
            Some(_) => start,
            None => lines.len(),
        };
        (lines[start..end].concat(), end < lines.len())
    } else {
        (text.to_string(), false)
    };
    let truncated = more_lines || selected.len() > cap;
    let content = if selected.len() > cap {
        String::from_utf8_lossy(&selected.as_bytes()[..cap]).to_string()
    } else {
        selected
    };
    Ok(WorkFileContent {
        path: path.trim().to_string(),
        content,
        truncated,
        size,
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkImageContent {
    pub path: String,
    pub base64: String,
    pub mime: String,
    pub size: u64,
}

/// Read a workspace image file for vision models (png/jpg/webp/gif, 10MB cap).
/// Returns raw base64 — the frontend feeds it as Ollama `images[]` so the
/// agent actually SEES the picture instead of getting a binary-dump error.
pub async fn work_read_image_logic(root: String, path: String) -> Result<WorkImageContent, String> {
    let canon = work_canonical_root(root.trim())?;
    let full = work_join_allow_temp(&canon, &path)?;
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    if size == 0 {
        return Err("empty file".to_string());
    }
    if size > 10 * 1024 * 1024 {
        return Err("image too large (max 10MB)".to_string());
    }
    let ext = full
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        _ => return Err("unsupported image type (png/jpg/webp/gif only) — use read for text files".to_string()),
    };
    let bytes = std::fs::read(&full).map_err(|e| format!("cannot read file: {e}"))?;
    {
        use base64::Engine as _;
        let base64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        Ok(WorkImageContent {
            path: path.trim().to_string(),
            base64,
            mime: mime.to_string(),
            size,
        })
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkGrepHit {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// Substring search (case-insensitive) across workspace text files.
/// Caps: 200 files scanned, 100 hits, 512KB per file, skips hidden dirs.
pub async fn work_grep_logic(root: String, pattern: String, include: Option<String>) -> Result<Vec<WorkGrepHit>, String> {
    let needle = pattern.to_lowercase();
    if needle.is_empty() || needle.len() > 200 {
        return Err("empty or oversized search pattern".to_string());
    }
    let canon = work_canonical_root(root.trim())?;
    let include = include.unwrap_or_default().to_lowercase();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut stack = vec![canon.clone()];
    while let Some(dir) = stack.pop() {
        if files.len() >= 200 {
            break;
        }
        let rd = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for ent in rd {
            let ent = match ent {
                Ok(e) => e,
                Err(_) => continue,
            };
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let p = ent.path();
            if p.is_dir() {
                if matches!(name.as_str(), "node_modules" | "target" | "build" | ".git" | "dist" | "Volumes" | "System" | "private" | "dev" | "proc" | "sys") {
                    continue;
                }
                stack.push(p);
            } else if p.is_file() {
                if !include.is_empty() && !name.to_lowercase().contains(&include) {
                    continue;
                }
                if ent.metadata().map(|m| m.len()).unwrap_or(0) > 524288 {
                    continue;
                }
                files.push(p);
                if files.len() >= 200 {
                    break;
                }
            }
        }
    }
    let mut hits: Vec<WorkGrepHit> = Vec::new();
    for f in files {
        if hits.len() >= 100 {
            break;
        }
        let bytes = match std::fs::read(&f) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.iter().take(1024).any(|&b| b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let rel = f
            .strip_prefix(&canon)
            .unwrap_or(&f)
            .to_string_lossy()
            .to_string();
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                hits.push(WorkGrepHit {
                    path: rel.clone(),
                    line: i + 1,
                    text: line.chars().take(240).collect(),
                });
                if hits.len() >= 100 {
                    break;
                }
            }
        }
    }
    Ok(hits)
}

/// Write (or overwrite) a workspace-relative text file, creating parents.
/// Refuses to overwrite files larger than 1MB without an explicit read first
/// — the agent should read before clobbering.
pub async fn work_write_file_logic(root: String, path: String, content: String) -> Result<String, String> {
    if content.len() > 524288 {
        return Err("content too large (max ~512KB per write)".to_string());
    }
    let canon = work_canonical_root(root.trim())?;
    let full = work_join_allow_temp(&canon, &path)?;
    if full.is_dir() {
        return Err("cannot overwrite a folder".to_string());
    }
    if let Some(parent) = full.parent() {
        if !parent.starts_with(&canon) && !path_is_under_temp(parent) {
            return Err("path escapes the workspace".to_string());
        }
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create folders: {e}"))?;
    }
    std::fs::write(&full, content.as_bytes()).map_err(|e| format!("cannot write file: {e}"))?;
    Ok(path.trim().to_string())
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
    pub timed_out: bool,
    pub truncated: bool,
}

fn trunc_out(s: &str) -> (String, bool) {
    const CAP: usize = 32768;
    if s.len() > CAP {
        (format!("{}\n…[truncated]", &s[..CAP]), true)
    } else {
        (s.to_string(), false)
    }
}

/// Refuse agent `exec` commands that crawl the whole disk (or a whole home
/// folder) with a recursive walker: `find / …`, `find ~ …`, `grep -r … /`,
/// …. On macOS such a scan descends into every TCC-protected zone
/// (Documents, Downloads, Photos/Music libraries, …) and macOS raises one
/// consent dialog per zone for the full scan — a prompt storm the user
/// cannot click through (verified live: one `find / -name …` produced the
/// Documents/Downloads/Photos/Music dialogs back to back until the 30s
/// timeout killed it). It also violates the workspace boundary outright.
/// Narrow scans (`find /opt/homebrew …`, `grep -r … src`) still pass: only
/// storm-scale roots are refused. Returns the offending fragment for the
/// coaching error, or None when the command is fine.
fn refuse_full_disk_scan(cmd: &str) -> Option<String> {
    /// Storm-scale scan roots: the filesystem root, any home folder, and
    /// macOS's physical-root alias. Anything narrower (a specific bin dir,
    /// a workspace-relative path, `$HOME/sub`) stays allowed.
    fn is_storm_root(tok: &str, home: &str) -> bool {
        let norm = tok.trim_end_matches('/');
        let norm = if norm.is_empty() { "/" } else { norm };
        if matches!(
            norm,
            "/" | "/." | "~" | "$HOME" | "${HOME}" | "/Users" | "/System/Volumes/Data"
        ) {
            return true;
        }
        if !home.is_empty() && norm == home.trim_end_matches('/') {
            return true;
        }
        // /Users/<name> is a whole home folder; deeper /Users/<name>/…
        // subpaths are bounded single-zone scans, left to the boundary rule.
        if let Some(rest) = norm.strip_prefix("/Users/") {
            if !rest.is_empty() && !rest.contains('/') {
                return true;
            }
        }
        // ~user (no slash) is a whole home folder too; ~/… is bounded.
        if norm.starts_with('~') && !norm.contains('/') {
            return true;
        }
        false
    }
    fn basename(argv0: &str) -> &str {
        argv0.rsplit('/').next().unwrap_or(argv0)
    }
    let home = std::env::var("HOME").unwrap_or_default();
    // Split on shell command separators so `a; find / …` and `a && find / …`
    // are caught too (covers $(…) via the parens). Quote-aware enough for
    // the check: quote chars are stripped from tokens (`"/"` still matches).
    // NB: `{`/`}` are NOT separators — splitting on them would shred
    // `${HOME}`; brace groups are skipped as wrapper tokens below instead.
    for seg in cmd.split([';', '|', '&', '\n', '(', ')', '`']) {
        let toks: Vec<String> = seg
            .split_whitespace()
            .map(|t| t.replace(['\'', '"'], ""))
            .filter(|t| !t.is_empty())
            .collect();
        if toks.is_empty() {
            continue;
        }
        // skip wrappers (sudo/command/env), brace-group markers, and VAR=
        // prefixes to the real argv0
        let mut i = 0;
        while i < toks.len()
            && (matches!(toks[i].as_str(), "sudo" | "command" | "env" | "{" | "}")
                || (toks[i].contains('=') && !toks[i].starts_with('=')))
        {
            i += 1;
        }
        if i >= toks.len() {
            continue;
        }
        let argv0 = basename(&toks[i]);
        let rest = &toks[i + 1..];
        let recursive_walker = match argv0 {
            "find" | "rg" | "fd" | "du" | "tree" => true,
            "grep" => rest.iter().any(|t| {
                t == "--recursive"
                    || (t.starts_with('-')
                        && !t.starts_with("--")
                        && t.len() > 1
                        && t[1..].chars().any(|c| c == 'r' || c == 'R'))
            }),
            // ls -r is reverse-sort, not recursive: only -R/--recursive walks
            "ls" => rest.iter().any(|t| {
                t == "--recursive"
                    || (t.starts_with('-')
                        && !t.starts_with("--")
                        && t.len() > 1
                        && t[1..].contains('R'))
            }),
            _ => false,
        };
        if !recursive_walker {
            continue;
        }
        let mut skip_next = false;
        let mut find_options = argv0 == "find";
        for t in rest {
            if skip_next {
                skip_next = false;
                continue;
            }
            if t.starts_with('-') {
                // A pattern-taking flag consumes the next token — except in
                // --opt=value form, where the value is already attached.
                // (Exact `--file`, not the prefix: rg's `--files` takes no
                // value and must not swallow the scan root.)
                if matches!(t.as_str(), "-e" | "-f" | "--regexp" | "--file") {
                    skip_next = true;
                }
                if argv0 == "find" {
                    // find's global options (-H/-L/-P, -O0-3, -D…) precede
                    // the paths; any other flag ends the path operands
                    // (expression-first means default `.` = workspace: fine).
                    if find_options
                        && (matches!(t.as_str(), "-H" | "-L" | "-P")
                            || t.starts_with("-O")
                            || t.starts_with("-D"))
                    {
                        continue;
                    }
                    break;
                }
                continue;
            }
            find_options = false;
            // Skip -e/-f values above so a pattern of `/` can't misfire.
            if is_storm_root(t, &home) {
                return Some(format!("{argv0} {t}"));
            }
        }
    }
    None
}

/// Coaching error for refused full-disk scans: tells the model what to do
/// instead so the turn continues productively.
fn full_disk_scan_error(frag: &str) -> String {
    format!(
        "refused: `{frag} …` crawls the whole disk (or a whole home folder), including macOS privacy-protected folders — macOS raises one consent dialog per folder for the entire scan. Locate tools with `command -v <name>` or `ls` on specific bin dirs (/opt/homebrew/bin, /usr/local/bin, /usr/bin), search inside the workspace with the grep tool, or ask the user for the path."
    )
}

/// PATH used by Work shell commands. GUI apps launched from Finder may have
/// a sparse process PATH; prefer the user's login-shell PATH and append any
/// app-only entries. Probe once so command execution does not pay the shell
/// startup cost on every invocation. If probing fails, inherit the app PATH.
#[cfg(not(windows))]
static WORK_EXEC_PATH: tokio::sync::OnceCell<Option<std::ffi::OsString>> =
    tokio::sync::OnceCell::const_new();

#[cfg(not(windows))]
fn work_path_dirs(
    shell_dirs: &[std::path::PathBuf],
    process_dirs: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut out = shell_dirs.to_vec();
    for dir in process_dirs {
        if !out.iter().any(|existing| existing == dir) {
            out.push(dir.clone());
        }
    }
    out
}

#[cfg(not(windows))]
async fn work_exec_path() -> Option<std::ffi::OsString> {
    WORK_EXEC_PATH
        .get_or_init(|| async {
            let shell_dirs = cli_shell_path_dirs().await;
            if shell_dirs.is_empty() {
                return None;
            }
            std::env::join_paths(work_path_dirs(&shell_dirs, &cli_path_dirs())).ok()
        })
        .await
        .clone()
}

/// Run a shell command with cwd=workspace root. Timeout default 30s (max
/// 120s). Output capped at 32KB per stream. Destructive commands are the
/// user's responsibility — Work mode is explicit opt-in per session.
pub async fn work_exec_logic(root: String, command: String, timeout_ms: Option<u64>) -> Result<WorkExecResult, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    if cmd.len() > 8000 {
        return Err("command too long".to_string());
    }
    if let Some(frag) = refuse_full_disk_scan(cmd) {
        return Err(full_disk_scan_error(&frag));
    }
    let canon = work_canonical_root(root.trim())?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30000).clamp(1000, 120000));
    #[cfg(windows)]
    let child = tokio::process::Command::new("powershell")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", cmd])
        .envs(AGENT_NONINTERACTIVE_ENV.iter().copied())
        .current_dir(&canon)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("cannot run command: {e}"))?;
    #[cfg(not(windows))]
    let child = {
        let mut builder = tokio::process::Command::new("sh");
        builder
            .arg("-c")
            .arg(cmd)
            .current_dir(&canon)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for (k, v) in AGENT_NONINTERACTIVE_ENV {
            builder.env(k, v);
        }
        builder.env("TMPDIR", std::env::temp_dir());
        if let Some(path) = work_exec_path().await {
            builder.env("PATH", path);
        }
        builder.spawn().map_err(|e| format!("cannot run command: {e}"))?
    };
    // wait_with_output takes ownership, so keep the pid for the timeout kill
    let pid = child.id();
    let status = tokio::time::timeout(timeout, child.wait_with_output()).await;
    match status {
        Err(_) => {
            // timed out: the wait future (owning the child handle) is dropped,
            // so kill by pid to avoid leaving the process running
            if let Some(pid) = pid {
                #[cfg(not(windows))]
                let _ = tokio::process::Command::new("kill")
                    .arg("-9")
                    .arg(pid.to_string())
                    .output()
                    .await;
                #[cfg(windows)]
                let _ = tokio::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                    .await;
            }
            Ok(WorkExecResult {
                stdout: String::new(),
                stderr: format!("timed out after {}s — process killed", timeout.as_secs()),
                exit_code: None,
                timed_out: true,
                truncated: false,
            })
        }
        Ok(Err(e)) => Err(format!("command failed: {e}")),
        Ok(Ok(out)) => {
            let (stdout, t1) = trunc_out(&String::from_utf8_lossy(&out.stdout));
            let (stderr, t2) = trunc_out(&String::from_utf8_lossy(&out.stderr));
            Ok(WorkExecResult {
                stdout,
                stderr,
                exit_code: out.status.code(),
                timed_out: false,
                truncated: t1 || t2,
            })
        }
    }
}

/// ---------- Tauri command wrappers (stable IPC names) ----------
///
/// The `*_logic` functions above hold the real implementation so the backend
/// agent loop (work_agent.rs) can call them directly. These thin wrappers
/// preserve the exact command names the frontend invokes — the IPC contract
/// is unchanged.
#[tauri::command]
async fn work_list_dir(root: String, path: Option<String>) -> Result<Vec<WorkDirEntry>, String> {
    work_list_dir_logic(root, path).await
}

#[tauri::command]
async fn work_read_file(
    root: String,
    path: String,
    max_bytes: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<WorkFileContent, String> {
    work_read_file_logic(root, path, max_bytes, offset, limit).await
}

#[tauri::command]
async fn work_read_image(root: String, path: String) -> Result<WorkImageContent, String> {
    work_read_image_logic(root, path).await
}

#[tauri::command]
async fn work_grep(root: String, pattern: String, include: Option<String>) -> Result<Vec<WorkGrepHit>, String> {
    work_grep_logic(root, pattern, include).await
}

#[tauri::command]
async fn work_write_file(root: String, path: String, content: String) -> Result<String, String> {
    work_write_file_logic(root, path, content).await
}

#[tauri::command]
async fn work_exec(root: String, command: String, timeout_ms: Option<u64>) -> Result<WorkExecResult, String> {
    work_exec_logic(root, command, timeout_ms).await
}

#[tauri::command]
async fn upload_read_file(session_id: String, name: String) -> Result<WorkFileContent, String> {
    upload_read_file_logic(session_id, name).await
}

/// ---------- In-app update wrappers (same _logic pattern) ----------
#[tauri::command]
async fn work_engine_info() -> Result<opencode::WorkEngineInfo, String> {
    // re-detects on every call: the UI's Refresh / window focus picks up an
    // engine installed while Vicuna is running. Returns the current engine
    // (opencode or Claude Code) plus a summary of every engine.
    Ok(work_engine::engines_info().await)
}

#[tauri::command]
async fn app_build_info() -> Result<serde_json::Value, String> {
    app_build_info_logic().await
}

#[tauri::command]
async fn github_latest_release() -> Result<serde_json::Value, String> {
    github_latest_release_logic().await
}

#[tauri::command]
async fn github_download_asset(url: String, file_name: String) -> Result<String, String> {
    github_download_asset_logic(url, file_name).await
}

// ---------- More command wrappers (same pattern: logic is pub + callable
// headlessly, wrappers preserve the exact IPC names). These cover the
// remaining backend surface the headless bridge drives.
#[tauri::command]
async fn work_validate_root(path: String) -> Result<String, String> {
    work_validate_root_logic(path).await
}

#[tauri::command]
async fn work_tree(root: String, max_depth: Option<usize>) -> Result<Vec<WorkTreeEntry>, String> {
    work_tree_logic(root, max_depth).await
}

#[tauri::command]
async fn work_rename(root: String, path: String, new_name: String) -> Result<String, String> {
    work_rename_logic(root, path, new_name).await
}

#[tauri::command]
async fn work_delete(root: String, path: String) -> Result<(), String> {
    work_delete_logic(root, path).await
}

#[tauri::command]
async fn work_import_file(root: String, path: String, base64: String) -> Result<String, String> {
    work_import_file_logic(root, path, base64).await
}

#[tauri::command]
async fn upload_stage_file(session_id: String, file_name: String, base64: String) -> Result<String, String> {
    upload_stage_file_logic(session_id, file_name, base64).await
}

#[tauri::command]
async fn ollama_fetch_text(url: String) -> Result<String, String> {
    ollama_fetch_text_logic(url).await
}

#[tauri::command]
async fn work_fetch_url(url: String) -> Result<WorkFetchContent, String> {
    work_fetch_url_logic(url).await
}

/// ---------- Background exec jobs (work mode, desktop only) ----------
///
/// Long commands (test suites, builds) run detached: `work_exec_start`
/// returns immediately with a job id (`bg-N`), the model keeps working on
/// other tools, and `work_exec_status` / `work_exec_list` / `work_exec_kill`
/// report back. Output streams are captured incrementally (32KB cap per
/// stream, like foreground exec) so polling mid-run shows progress.

#[derive(Debug, Clone, Serialize)]
pub struct BgJobStatus {
    pub id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgJobSummary {
    pub id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
}

struct BgJobRecord {
    command: String,
    pid: Option<u32>,
    running: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    truncated: bool,
    started_at: u64,
}

#[derive(Default)]
pub struct BgExecState {
    jobs: Mutex<HashMap<String, Arc<Mutex<BgJobRecord>>>>,
    seq: AtomicU64,
}

fn bg_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cap for a captured background stream (32KB like foreground exec).
const BG_STREAM_CAP: usize = 32768;

/// Body of a detached job: spawn `sh -c` / `cmd /C` rooted at `canon`,
/// stream both pipes into the shared record, then mark completion.
/// Spawn failures are recorded as an immediately-finished job so polling
/// the id still returns something explainable.
async fn bg_run_job(id: String, canon: std::path::PathBuf, cmd: String, rec: Arc<Mutex<BgJobRecord>>) {
    #[cfg(windows)]
    let mut builder = {
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &cmd]);
        c
    };
    #[cfg(not(windows))]
    let mut builder = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(&cmd);
        c
    };
    builder
        .current_dir(&canon)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in AGENT_NONINTERACTIVE_ENV {
        builder.env(k, v);
    }
    builder.env("TMPDIR", std::env::temp_dir());
    #[cfg(not(windows))]
    if let Some(path) = work_exec_path().await {
        builder.env("PATH", path);
    }
    #[cfg(windows)]
    builder.creation_flags(CREATE_NO_WINDOW);
    let mut child = match builder.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Ok(mut g) = rec.lock() {
                g.running = false;
                g.stderr = format!("cannot run command: {e}");
            }
            return;
        }
    };
    if let Ok(mut g) = rec.lock() {
        g.pid = child.id();
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let rec_out = rec.clone();
    let out_h = tokio::spawn(async move {
        if let Some(mut pipe) = out_pipe.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if let Ok(mut g) = rec_out.lock() {
                            // sequential field touches only: the guard's
                            // DerefMut can't prove two &mut field borrows
                            // disjoint, so never hold both at once
                            if !g.truncated {
                                let room = BG_STREAM_CAP.saturating_sub(g.stdout.len());
                                if chunk.len() > room {
                                    g.stdout.push_str(&chunk[..chunk.len().min(room)]);
                                    g.stdout.push_str("\n…[truncated]");
                                    g.truncated = true;
                                } else {
                                    g.stdout.push_str(&chunk);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let rec_err = rec.clone();
    let err_h = tokio::spawn(async move {
        if let Some(mut pipe) = err_pipe.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if let Ok(mut g) = rec_err.lock() {
                            if !g.truncated {
                                let room = BG_STREAM_CAP.saturating_sub(g.stderr.len());
                                if chunk.len() > room {
                                    g.stderr.push_str(&chunk[..chunk.len().min(room)]);
                                    g.stderr.push_str("\n…[truncated]");
                                    g.truncated = true;
                                } else {
                                    g.stderr.push_str(&chunk);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let status = child.wait().await.ok();
    let _ = tokio::join!(out_h, err_h);
    if let Ok(mut g) = rec.lock() {
        g.running = false;
        g.pid = None;
        g.exit_code = status.and_then(|s| s.code());
    }
    let _ = id;
}

/// Plain (non-Tauri-State) start so unit tests can drive it directly.
pub async fn bg_start(state: &BgExecState, root: String, command: String) -> Result<String, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    if cmd.len() > 8000 {
        return Err("command too long".to_string());
    }
    if let Some(frag) = refuse_full_disk_scan(cmd) {
        return Err(full_disk_scan_error(&frag));
    }
    let canon = work_canonical_root(root.trim())?;
    let n = state.seq.fetch_add(1, Ordering::Relaxed);
    let id = format!("bg-{}", n + 1);
    let rec = Arc::new(Mutex::new(BgJobRecord {
        command: cmd.to_string(),
        pid: None,
        running: true,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        started_at: bg_now_secs(),
    }));
    {
        let mut jobs = state.jobs.lock().map_err(|_| "exec state poisoned".to_string())?;
        jobs.insert(id.clone(), rec.clone());
        // cap memory: keep running jobs plus the newest ~100 finished ones
        if jobs.len() > 120 {
            let mut done: Vec<(String, u64)> = jobs
                .iter()
                .filter_map(|(k, r)| {
                    r.lock()
                        .ok()
                        .filter(|g| !g.running)
                        .map(|g| (k.clone(), g.started_at))
                })
                .collect();
            done.sort_by_key(|(_, t)| *t);
            let drop_n = jobs.len().saturating_sub(120);
            for (k, _) in done.into_iter().take(drop_n) {
                jobs.remove(&k);
            }
        }
    }
    tokio::spawn(bg_run_job(id.clone(), canon, cmd.to_string(), rec));
    Ok(id)
}

pub fn bg_snapshot(state: &BgExecState, id: &str) -> Result<BgJobStatus, String> {
    let rec = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "exec state poisoned".to_string())?;
        jobs
            .get(id.trim())
            .cloned()
            .ok_or_else(|| "unknown background job".to_string())?
    };
    let g = rec.lock().map_err(|_| "exec state poisoned".to_string())?;
    Ok(BgJobStatus {
        id: id.trim().to_string(),
        command: g.command.clone(),
        running: g.running,
        exit_code: g.exit_code,
        stdout: g.stdout.clone(),
        stderr: g.stderr.clone(),
        truncated: g.truncated,
    })
}

pub fn bg_list(state: &BgExecState) -> Result<Vec<BgJobSummary>, String> {
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| "exec state poisoned".to_string())?;
    let mut out: Vec<BgJobSummary> = Vec::new();
    for (id, rec) in jobs.iter() {
        if let Ok(g) = rec.lock() {
            out.push(BgJobSummary {
                id: id.clone(),
                command: g.command.clone(),
                running: g.running,
                exit_code: g.exit_code,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

pub fn bg_kill(state: &BgExecState, id: &str) -> Result<BgJobStatus, String> {
    let pid = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "exec state poisoned".to_string())?;
        let rec = jobs
            .get(id.trim())
            .cloned()
            .ok_or_else(|| "unknown background job".to_string())?;
        rec.lock()
            .map_err(|_| "exec state poisoned".to_string())?
            .pid
    };
    match pid {
        Some(p) => kill_pid(p),
        None => {
            // already finished (or spawn failed): nothing to kill, just report
        }
    }
    bg_snapshot(state, id)
}

/// Start a detached shell command; returns the job id immediately.
#[tauri::command]
async fn work_exec_start(root: String, command: String, state: State<'_, BgExecState>) -> Result<String, String> {
    bg_start(&state, root, command).await
}

/// Poll one background job (full captured output so far + running flag).
#[tauri::command]
async fn work_exec_status(id: String, state: State<'_, BgExecState>) -> Result<BgJobStatus, String> {
    bg_snapshot(&state, &id)
}

/// List background jobs (newest last, no output payload).
#[tauri::command]
async fn work_exec_list(state: State<'_, BgExecState>) -> Result<Vec<BgJobSummary>, String> {
    bg_list(&state)
}

/// Kill a background job (best-effort); returns the latest snapshot.
#[tauri::command]
async fn work_exec_kill(id: String, state: State<'_, BgExecState>) -> Result<BgJobStatus, String> {
    bg_kill(&state, &id)
}

/// ---------- Persistent interactive agent shell ----------
///
/// Work `exec` no longer spawns a fresh `sh -c` per call. One long-lived
/// shell per workspace keeps cwd, exported vars, `source venv/bin/activate`,
/// `nvm use`, shell functions and aliases alive between calls (the same
/// stateful model Claude Code and Codex use). The shell runs on a PTY so
/// interactive programs work: stdin can be fed with `exec_input`, and
/// `exec_kill` sends Ctrl-C. Windows runs PowerShell — the shell the user
/// actually sees — never `cmd`, which is what made Windows tasks fail when
/// the model wrote PowerShell syntax.
///
/// Completion is detected with a unique prompt marker (PS1 / PowerShell's
/// `prompt` function), so no sentinel command has to be injected into stdin
/// (a sentinel line would be eaten by any program reading stdin).

#[derive(Debug, Clone, Serialize)]
pub struct ShellRun {
    pub output: String,
    pub exit_code: Option<i32>,
    pub running: bool,
    pub truncated: bool,
    /// Working directory after the command (best effort, from the exit probe).
    pub cwd: Option<String>,
    /// Wall-clock duration of the exec call in milliseconds.
    pub duration_ms: Option<u64>,
}

struct AgentShell {
    pid: Option<u32>,
    /// Kept alive so the pty (and therefore the shell) stays open. Wrapped in
    /// a Mutex because `MasterPty` is Send but not Sync.
    _master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    buf: Arc<Mutex<String>>,
    marker: String,
    /// Exit-probe marker, also built from an env var so a re-enabled terminal
    /// echo of the probe line can never be mistaken for its output.
    exit_marker: String,
    alive: Arc<AtomicBool>,
    busy: AtomicBool,
    /// Epoch seconds when the current command started (0 while idle). Used to
    /// tell the model how long a blocked shell has been stuck.
    busy_since: AtomicU64,
    last_used: AtomicU64,
}

impl AgentShell {
    fn touch(&self) {
        self.last_used.store(bg_now_secs(), Ordering::Relaxed);
    }

    fn mark_busy(&self) {
        self.busy.store(true, Ordering::Relaxed);
        self.busy_since.store(bg_now_secs(), Ordering::Relaxed);
    }

    fn mark_idle(&self) {
        self.busy.store(false, Ordering::Relaxed);
        self.busy_since.store(0, Ordering::Relaxed);
    }

    fn busy_secs(&self) -> u64 {
        match self.busy_since.load(Ordering::Relaxed) {
            0 => 0,
            start => bg_now_secs().saturating_sub(start),
        }
    }

    fn write_raw(&self, data: &str) -> Result<(), String> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| "agent shell state poisoned".to_string())?;
        w.write_all(data.as_bytes())
            .map_err(|e| format!("agent shell write failed: {e}"))?;
        w.flush()
            .map_err(|e| format!("agent shell write failed: {e}"))
    }

    fn write_line(&self, line: &str) -> Result<(), String> {
        self.write_raw(&format!("{line}\n"))
    }

    fn take_buf(&self) -> String {
        self.buf
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    fn drain(&self) {
        let _ = self.take_buf();
    }
}

/// Take buffered output and, when the prompt marker is present, clear the
/// busy flag (the command finished). Returns (text before the marker, done).
fn take_output(shell: &AgentShell) -> (String, bool) {
    let raw = shell.take_buf();
    if let Some(idx) = raw.find(&shell.marker) {
        shell.mark_idle();
        (raw[..idx].to_string(), true)
    } else {
        (raw, false)
    }
}

#[derive(Default)]
pub struct AgentShellState {
    shells: Mutex<HashMap<String, Arc<AgentShell>>>,
}

impl AgentShellState {
    fn all(&self) -> Vec<Arc<AgentShell>> {
        self.shells
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// Kill every agent shell (Quit path).
pub fn kill_agent_shells(state: &AgentShellState) {
    let shells = state.all();
    if let Ok(mut m) = state.shells.lock() {
        m.clear();
    }
    for s in shells {
        if let Some(pid) = s.pid {
            kill_pid(pid);
        }
    }
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Strip ANSI escape sequences (colours, cursor moves) and carriage returns
/// from PTY output so the model sees plain text.
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if i < bytes.len() {
                i += 1;
            }
        } else if bytes[i] == b'\r' {
            i += 1;
        } else {
            let end = (i + utf8_char_len(bytes[i])).min(bytes.len());
            out.push_str(&input[i..end]);
            i = end;
        }
    }
    out
}

/// The PTY echoes what we type (Windows always; Unix is silenced with
/// `stty -echo`, but be safe). Drop a leading copy of the command.
fn strip_leading_echo(out: &str, command: &str) -> String {
    let trimmed = out.trim_start_matches([' ', '\t', '\n']);
    let cmd = command.trim_end();
    if !cmd.is_empty() && trimmed.starts_with(cmd) {
        return trimmed[cmd.len()..].trim_start_matches('\n').to_string();
    }
    out.to_string()
}

fn clean_shell_output(raw: &str, command: Option<&str>) -> (String, bool) {
    let plain = strip_ansi(raw);
    let stripped = match command {
        Some(c) => strip_leading_echo(&plain, c),
        None => plain,
    };
    trunc_out(stripped.trim_end_matches('\n'))
}

/// Environment that keeps agent-run commands non-interactive. The persistent
/// shell runs on a real PTY, so `git diff`, `git log`, `man`, etc. would
/// otherwise launch a pager, take over the TTY and swallow every subsequent
/// command as keystrokes. Disabling pagers/editors up front is the only
/// reliable fix; a stuck pager cannot be trusted to exit on Ctrl-C.
const AGENT_NONINTERACTIVE_ENV: &[(&str, &str)] = &[
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
    ("SYSTEMD_PAGER", "cat"),
    ("LESS", "FRX"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_EDITOR", "true"),
    ("GIT_SEQUENCE_EDITOR", "true"),
    ("GIT_MERGE_AUTOEDIT", "no"),
];

async fn spawn_agent_shell(root: &str) -> Result<AgentShell, String> {
    let canon = work_canonical_root(root.trim())?;
    let cwd = work_display_string(&canon);
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(portable_pty::PtySize {
            // Wide, tall pty: less line wrapping/scrollback churn in output.
            rows: 40,
            cols: 240,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("cannot open agent shell pty: {e}"))?;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = portable_pty::CommandBuilder::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-NoExit"]);
        c
    };
    #[cfg(not(windows))]
    let is_fish = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        shell
            .rsplit('/')
            .next()
            .map(|name| name.starts_with("fish"))
            .unwrap_or(false)
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut c = portable_pty::CommandBuilder::new(shell);
        c.args(["-i"]);
        if let Some(path) = work_exec_path().await {
            c.env("PATH", path);
        }
        c
    };
    cmd.cwd(&cwd);
    // The marker is built from this env var, never written literally: the
    // PTY echoes our init line, and a literal marker in that echo would be
    // mistaken for the shell becoming ready.
    let token = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    cmd.env("VICUNA_READY_TOKEN", &token);
    // Same anti-echo trick for the exit probe: the typed line only ever
    // contains the env-var name, while the real output expands to the marker.
    let exit_token = format!("{token}x");
    cmd.env("VICUNA_EXIT_TOKEN", &exit_token);
    // Point the agent shell at the same temp root the file tools accept, so
    // `mktemp -d "$TMPDIR/..."` always yields a writable scratch path.
    cmd.env("TMPDIR", std::env::temp_dir());
    cmd.env("TERM", "dumb");
    cmd.env("NO_COLOR", "1");
    cmd.env("CLICOLOR", "0");
    for (k, v) in AGENT_NONINTERACTIVE_ENV {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("cannot spawn agent shell: {e}"))?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("agent shell pty failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("agent shell pty failed: {e}"))?;
    let buf = Arc::new(Mutex::new(String::new()));
    let alive = Arc::new(AtomicBool::new(true));
    let b = buf.clone();
    let a = alive.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut g) = b.lock() {
                        g.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    }
                }
                Err(_) => break,
            }
        }
        a.store(false, Ordering::Relaxed);
    });
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let marker = format!("__VICUNA_READY_{token}__");
    let exit_marker = format!("__VICUNA_EXIT_{exit_token}__");
    let shell = AgentShell {
        pid,
        _master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        buf,
        marker: marker.clone(),
        exit_marker,
        alive,
        busy: AtomicBool::new(false),
        busy_since: AtomicU64::new(0),
        last_used: AtomicU64::new(bg_now_secs()),
    };
    #[cfg(windows)]
    let init = "function prompt { \"__VICUNA_READY_$($env:VICUNA_READY_TOKEN)__\" }\n$OutputEncoding = [Console]::OutputEncoding = [Text.Encoding]::UTF8\n"
        .to_string();
    #[cfg(not(windows))]
    let init = if is_fish {
        "stty -echo 2>/dev/null; function fish_prompt; printf '%s' \"__VICUNA_READY_${VICUNA_READY_TOKEN}__\"; end; function fish_right_prompt; end\n"
            .to_string()
    } else {
        "stty -echo 2>/dev/null; PS1=\"__VICUNA_READY_${VICUNA_READY_TOKEN}__\"; PS2=''; PROMPT=$PS1; RPROMPT=''; PROMPT_COMMAND=''; export PS1 PROMPT\n"
            .to_string()
    };
    shell.write_raw(&init)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let got = shell.take_buf();
        if got.contains(&marker) {
            break;
        }
        if !shell.alive.load(Ordering::Relaxed) {
            return Err("agent shell exited during startup".to_string());
        }
        if std::time::Instant::now() >= deadline {
            let _ = shell.write_raw("\u{3}");
            return Err("agent shell did not become ready (timed out)".to_string());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    shell.drain();
    Ok(shell)
}

/// Get the workspace's shell, respawning it if it died.
async fn shell_for(state: &AgentShellState, root: &str, key: &str) -> Result<Arc<AgentShell>, String> {
    let key = key.to_string();
    if let Some(existing) = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
    {
        if existing.alive.load(Ordering::Relaxed) {
            existing.touch();
            return Ok(existing);
        }
    }
    let fresh = Arc::new(spawn_agent_shell(root).await?);
    let mut map = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?;
    // another call may have created one while we were spawning; keep theirs
    if let Some(existing) = map.get(&key) {
        if existing.alive.load(Ordering::Relaxed) {
            if let Some(pid) = fresh.pid {
                kill_pid(pid);
            }
            existing.touch();
            return Ok(existing.clone());
        }
    }
    // cap live shells (one per workspace): evict the least recently used
    if map.len() >= 8 {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, s)| s.last_used.load(Ordering::Relaxed))
            .map(|(k, _)| k.clone())
        {
            if let Some(victim) = map.remove(&oldest) {
                if let Some(pid) = victim.pid {
                    kill_pid(pid);
                }
            }
        }
    }
    map.insert(key, fresh.clone());
    Ok(fresh)
}

enum ShellWait {
    Done(String),
    TimedOut(String),
    Dead(String),
}

async fn wait_for_marker(shell: &AgentShell, deadline: std::time::Instant) -> ShellWait {
    let mut acc = String::new();
    loop {
        acc.push_str(&shell.take_buf());
        if let Some(idx) = acc.find(&shell.marker) {
            shell.mark_idle();
            return ShellWait::Done(acc[..idx].to_string());
        }
        if !shell.alive.load(Ordering::Relaxed) {
            shell.mark_idle();
            return ShellWait::Dead(acc);
        }
        if std::time::Instant::now() >= deadline {
            return ShellWait::TimedOut(acc);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Read `$?` and the cwd right after a command finished (before any other
/// input). Returns (exit code, cwd).
async fn probe_exit_status(shell: &AgentShell) -> (Option<i32>, Option<String>) {
    if !shell.alive.load(Ordering::Relaxed) {
        return (None, None);
    }
    shell.drain();
    #[cfg(windows)]
    let probe = "Write-Output ('__VICUNA_EXIT_' + $env:VICUNA_EXIT_TOKEN + '__' + $(if ($?) {0} else {1}) + '|' + (Get-Location).Path)";
    #[cfg(not(windows))]
    let probe = "printf '__VICUNA_EXIT_%s__%s|%s\\n' \"$VICUNA_EXIT_TOKEN\" \"$?\" \"$PWD\"";
    if shell.write_line(probe).is_err() {
        return (None, None);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    match wait_for_marker(shell, deadline).await {
        ShellWait::Done(raw) => {
            let plain = strip_ansi(&raw);
            // Last occurrence: a re-enabled echo of the probe line contains the
            // literal prefix but not the expanded token.
            if !plain.contains(&shell.exit_marker) {
                return (None, None);
            }
            let tail = plain.rsplit(&shell.exit_marker).next().unwrap_or("").trim();
            let mut parts = tail.splitn(2, '|');
            let code = parts.next().and_then(|s| s.trim().parse::<i32>().ok());
            let cwd = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            (code, cwd)
        }
        _ => (None, None),
    }
}

/// Commands are typed into an interactive PTY. A multi-line payload (heredoc,
/// pasted script) is re-parsed line by line and mangled, so route it through
/// the `write` tool instead of pretending it ran.
fn reject_multiline_command(cmd: &str) -> Option<String> {
    if cmd.contains('\n') || cmd.contains('\r') {
        Some(
            "exec takes one command line — this one contains a newline or heredoc, which the \
             interactive shell corrupts. Put the content in a file with the write tool (or \
             write a script with write), then run it, e.g. exec {\"command\": \"sh script.sh\"}."
                .to_string(),
        )
    } else {
        None
    }
}

fn format_elapsed_secs(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Best-effort name of the pty's foreground process (Unix). macOS/Linux `ps`
/// exposes the terminal foreground pgid via `tpgid`.
#[cfg(not(windows))]
fn foreground_process_name(shell_pid: Option<u32>) -> Option<String> {
    let pid = shell_pid?;
    let out = std::process::Command::new("ps")
        .args(["-o", "tpgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let pgid = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()?;
    if pgid == 0 {
        return None;
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pgid.to_string()])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(windows)]
fn foreground_process_name(_shell_pid: Option<u32>) -> Option<String> {
    None
}

fn shell_busy_error(shell: &AgentShell) -> String {
    let mut msg = String::from("the shell is still running a command");
    let secs = shell.busy_secs();
    if secs > 0 {
        msg.push_str(&format!(" (busy {})", format_elapsed_secs(secs)));
    }
    if let Some(fg) = foreground_process_name(shell.pid) {
        let base = fg.rsplit(['/', '\\']).next().unwrap_or(&fg).to_lowercase();
        let hint = match base.as_str() {
            "less" | "more" | "most" | "man" | "vi" | "vim" | "nvim" | "top" | "htop" => {
                " — a pager/TUI owns the terminal: quit it with exec_input {\"text\": \"q\"}"
            }
            _ => "",
        };
        msg.push_str(&format!("; foreground={fg}{hint}"));
    }
    msg.push_str(
        ". Poll with exec_status, answer a prompt with exec_input, or interrupt with exec_kill first.",
    );
    msg
}

/// Run a command in the workspace's persistent shell.
pub async fn agent_shell_exec(
    state: &AgentShellState,
    root: String,
    key: String,
    command: String,
    timeout_ms: Option<u64>,
) -> Result<ShellRun, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    if cmd.len() > 8000 {
        return Err("command too long".to_string());
    }
    if let Some(err) = reject_multiline_command(cmd) {
        return Err(err);
    }
    if let Some(frag) = refuse_full_disk_scan(cmd) {
        return Err(full_disk_scan_error(&frag));
    }
    let shell = shell_for(state, &root, &key).await?;
    let (_pending, finished) = take_output(&shell);
    if shell.busy.load(Ordering::Relaxed) && !finished {
        return Err(shell_busy_error(&shell));
    }
    shell.mark_busy();
    let started = std::time::Instant::now();
    shell.write_line(cmd)?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30000).clamp(1000, 120000));
    match wait_for_marker(&shell, std::time::Instant::now() + timeout).await {
        ShellWait::Done(raw) => {
            shell.mark_idle();
            shell.touch();
            let (exit_code, cwd) = probe_exit_status(&shell).await;
            let (output, truncated) = clean_shell_output(&raw, Some(cmd));
            Ok(ShellRun {
                output,
                exit_code,
                running: false,
                truncated,
                cwd,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            })
        }
        ShellWait::TimedOut(raw) => {
            // Leave the command running: the model may answer a prompt with
            // exec_input, poll with exec_status, or interrupt with exec_kill.
            let (output, truncated) = clean_shell_output(&raw, Some(cmd));
            Ok(ShellRun {
                output,
                exit_code: None,
                running: true,
                truncated,
                cwd: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            })
        }
        ShellWait::Dead(raw) => {
            shell.mark_idle();
            let (output, _) = clean_shell_output(&raw, Some(cmd));
            Err(format!("agent shell exited unexpectedly.\n{output}"))
        }
    }
}

/// Poll the workspace shell (or report it idle when no command is running).
pub async fn agent_shell_status(
    state: &AgentShellState,
    _root: String,
    key: String,
) -> Result<ShellRun, String> {
    let shell = match state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
    {
        Some(s) if s.alive.load(Ordering::Relaxed) => s,
        _ => {
            return Ok(ShellRun {
                output: "Shell idle (no command running).".to_string(),
                exit_code: None,
                running: false,
                truncated: false,
                cwd: None,
                duration_ms: None,
            })
        }
    };
    shell.touch();
    if !shell.busy.load(Ordering::Relaxed) {
        let (out, truncated) = clean_shell_output(&shell.take_buf(), None);
        return Ok(ShellRun {
            output: out,
            exit_code: None,
            running: false,
            truncated,
            cwd: None,
            duration_ms: None,
        });
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    match wait_for_marker(&shell, deadline).await {
        ShellWait::Done(raw) => {
            shell.mark_idle();
            let (exit_code, cwd) = probe_exit_status(&shell).await;
            let (output, truncated) = clean_shell_output(&raw, None);
            Ok(ShellRun {
                output,
                exit_code,
                running: false,
                truncated,
                cwd,
                duration_ms: None,
            })
        }
        ShellWait::TimedOut(raw) | ShellWait::Dead(raw) => {
            let (output, truncated) = clean_shell_output(&raw, None);
            Ok(ShellRun {
                output,
                exit_code: None,
                running: true,
                truncated,
                cwd: None,
                duration_ms: None,
            })
        }
    }
}

/// Feed raw input to the running command (answers prompts, REPLs, …).
pub async fn agent_shell_input(
    state: &AgentShellState,
    _root: String,
    key: String,
    text: String,
) -> Result<String, String> {
    let shell = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
        .ok_or_else(|| "no shell is running for this workspace yet — run exec first".to_string())?;
    shell.touch();
    shell.write_line(&text)?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (raw, finished) = take_output(&shell);
    let (out, _) = clean_shell_output(&raw, None);
    let mut parts = Vec::new();
    if !out.trim().is_empty() {
        parts.push(out);
    }
    if finished {
        parts.push("[prompt ready]".to_string());
    }
    Ok(if parts.is_empty() { "(input sent)".to_string() } else { parts.join("\n") })
}

/// Ctrl-C the running command and wait for the prompt to come back.
pub async fn agent_shell_interrupt(
    state: &AgentShellState,
    _root: String,
    key: String,
) -> Result<ShellRun, String> {
    let shell = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
        .ok_or_else(|| "no shell is running for this workspace".to_string())?;
    shell.touch();
    if !shell.busy.load(Ordering::Relaxed) {
        return Ok(ShellRun {
            output: "Nothing to interrupt — the shell is idle.".to_string(),
            exit_code: None,
            running: false,
            truncated: false,
            cwd: None,
            duration_ms: None,
        });
    }
    // Normal foreground jobs die on Ctrl-C. Full-screen pagers do not: less
    // treats it as "interrupt" and may even open its help screen, so after a
    // failed interrupt try quitting a pager explicitly before giving up.
    shell.write_raw("\u{3}")?;
    let mut acc = String::new();
    match wait_for_marker(&shell, std::time::Instant::now() + Duration::from_millis(1500)).await {
        ShellWait::Done(raw) | ShellWait::Dead(raw) => {
            acc.push_str(&raw);
            let (output, truncated) = clean_shell_output(&acc, None);
            return Ok(ShellRun {
                output,
                exit_code: None,
                running: false,
                truncated,
                cwd: None,
                duration_ms: None,
            });
        }
        ShellWait::TimedOut(raw) => acc.push_str(&raw),
    }
    for _ in 0..3 {
        shell.write_raw("q")?;
        match wait_for_marker(&shell, std::time::Instant::now() + Duration::from_millis(800)).await {
            ShellWait::Done(raw) | ShellWait::Dead(raw) => {
                acc.push_str(&raw);
                let (output, truncated) = clean_shell_output(&acc, None);
                return Ok(ShellRun {
                    output,
                    exit_code: None,
                    running: false,
                    truncated,
                    cwd: None,
                    duration_ms: None,
                });
            }
            ShellWait::TimedOut(raw) => acc.push_str(&raw),
        }
    }
    // Still owned by a TUI: report the truth so the next exec refuses instead
    // of typing a command into it.
    let (mut output, truncated) = clean_shell_output(&acc, None);
    output.push_str("\n[interrupt did not return the prompt — a pager/TUI may still be open; try exec_input {\"text\": \"q\"}]");
    Ok(ShellRun {
        output,
        exit_code: None,
        running: true,
        truncated,
        cwd: None,
        duration_ms: None,
    })
}

/// Persistent-shell command wrappers (same IPC naming pattern as the rest).
#[tauri::command]
async fn work_shell_exec(
    root: String,
    key: Option<String>,
    command: String,
    timeout_ms: Option<u64>,
    state: State<'_, AgentShellState>,
) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_exec(&state, root, key, command, timeout_ms).await
}

#[tauri::command]
async fn work_shell_status(root: String, key: Option<String>, state: State<'_, AgentShellState>) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_status(&state, root, key).await
}

#[tauri::command]
async fn work_shell_input(
    root: String,
    key: Option<String>,
    text: String,
    state: State<'_, AgentShellState>,
) -> Result<String, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_input(&state, root, key, text).await
}

#[tauri::command]
async fn work_shell_interrupt(root: String, key: Option<String>, state: State<'_, AgentShellState>) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_interrupt(&state, root, key).await
}

/// ---------- Work undo/redo (/undo, /redo: revert last turn's file changes) ----------
///
/// The agent mutates the workspace through `write` (direct) and `exec`
/// (arbitrary shell). Before each `write`, dispatch snapshots the previous
/// bytes and records the new ones, so the change is invertible in both
/// directions; `exec` commands are recorded as notes since shell side
/// effects can't be reliably inverted. A turn pushes one frame at start
/// (`undo_begin_turn`); `/undo` pops the last frame and inverts it (delete
/// created files, restore modified ones), pushing it onto a redo stack;
/// `/redo` pops that and re-applies it.
#[derive(Debug, Clone)]
pub enum UndoOp {
    /// File did not exist before the turn: undo deletes it, redo rewrites it.
    Created { path: String, content: Vec<u8> },
    /// File existed: undo restores `prev`, redo restores `next`.
    Modified { path: String, prev: Vec<u8>, next: Vec<u8> },
    ExecNote { command: String },
}

#[derive(Debug, Clone)]
pub struct UndoTurn {
    pub workspace: String,
    pub ops: Vec<UndoOp>,
}

#[derive(Default)]
pub struct UndoState {
    turns: Mutex<HashMap<String, Vec<UndoTurn>>>,
    /// Frames popped by /undo, newest last; cleared by a new agent turn.
    redo: Mutex<HashMap<String, Vec<UndoTurn>>>,
    /// `git status --porcelain` captured at the previous turn start, per session.
    /// Used to tell the agent what was already dirty before it touched anything
    /// (and what changed since its last turn) so it cannot honestly blame
    /// itself for the user's edits — or claim it changed nothing.
    git_baseline: Mutex<HashMap<String, String>>,
}

/// Session key for the undo stack: the session id when present, else the
/// workspace (agent turns always carry a session, the fallback keeps
/// headless callers grouped sensibly).
pub fn undo_key(session_id: Option<&str>, workspace: &str) -> String {
    match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => workspace.trim().to_string(),
    }
}

const GIT_NOTE_MAX_LINES: usize = 40;

fn git_note_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn git_note_block(lines: &[String]) -> String {
    let shown = lines.len().min(GIT_NOTE_MAX_LINES);
    let mut out = lines[..shown].join("\n");
    if lines.len() > shown {
        out.push_str(&format!("\n…(+{} more)", lines.len() - shown));
    }
    out
}

/// Summarize `git status --porcelain` at the start of a Work turn. The agent
/// is told what was already dirty before it acted, and (after the first turn)
/// what changed since its previous turn, so it can distinguish the user's
/// edits from its own instead of making false attribution claims.
/// Returns `None` outside a git work tree (or when git cannot run).
pub async fn workspace_git_note(state: &UndoState, key: &str, workspace: &str) -> Option<String> {
    let res = work_exec_logic(
        workspace.to_string(),
        "git status --porcelain".to_string(),
        Some(5000),
    )
    .await
    .ok()?;
    if res.timed_out || res.exit_code != Some(0) {
        return None;
    }
    let current = git_note_lines(&res.stdout);
    let previous = match state.git_baseline.lock() {
        Ok(mut m) => m.insert(key.to_string(), current.join("\n")),
        Err(_) => None,
    };

    if current.is_empty() {
        return Some(match previous {
            None => "Working tree is clean at turn start (git status --porcelain).".to_string(),
            Some(prev) if prev.trim().is_empty() => {
                "Working tree is clean at turn start (unchanged since your previous turn).".to_string()
            }
            Some(_) => "Working tree is clean at turn start — changes from your previous turn are gone (committed, reverted, or undone).".to_string(),
        });
    }

    let Some(prev) = previous else {
        return Some(format!(
            "Working tree BEFORE this turn (git status --porcelain). These changes already existed when the turn started — they may be the user's or from an earlier turn, not necessarily yours:\n{}",
            git_note_block(&current)
        ));
    };
    let prev_lines = git_note_lines(&prev);
    let prev_set: std::collections::HashSet<&str> =
        prev_lines.iter().map(String::as_str).collect();
    let current_set: std::collections::HashSet<&str> =
        current.iter().map(String::as_str).collect();
    let added: Vec<String> = current
        .iter()
        .filter(|line| !prev_set.contains(line.as_str()))
        .map(|line| {
            if line.starts_with("?? ") {
                format!(
                    "+ {line}  (appeared during your previous turn — if you created this, move scratch to the system temp dir)"
                )
            } else {
                format!("+ {line}")
            }
        })
        .collect();
    let removed: Vec<String> = prev_lines
        .iter()
        .filter(|line| !current_set.contains(line.as_str()))
        .map(|line| format!("- {line}"))
        .collect();
    if added.is_empty() && removed.is_empty() {
        return Some(format!(
            "Working tree unchanged since your previous turn (still dirty):\n{}",
            git_note_block(&current)
        ));
    }
    let mut delta = added;
    delta.extend(removed);
    Some(format!(
        "Working tree CHANGED since your previous turn (sampled before this turn; includes your own last-turn writes and the user's edits):\n{}",
        git_note_block(&delta)
    ))
}

/// Snapshot a file before the agent overwrites it. `Some((existed, bytes))`
/// means tracked; `None` means untrackable (directory, >5MB, unreadable) and
/// the write is then left out of the undo history.
pub fn undo_capture_prev(root: &str, path: &str) -> Option<(bool, Vec<u8>)> {
    let canon = work_canonical_root(root).ok()?;
    let full = work_join(&canon, path).ok()?;
    if !full.exists() {
        return Some((false, Vec::new()));
    }
    if !full.is_file() {
        return None;
    }
    const CAP: u64 = 5 * 1024 * 1024;
    if std::fs::metadata(&full).map(|m| m.len() > CAP).unwrap_or(true) {
        return None;
    }
    std::fs::read(&full).ok().map(|b| (true, b))
}

/// Push a fresh frame for a starting turn (cap 20 per session). A new turn
/// also invalidates the redo history, like an editor after a new edit.
pub fn undo_begin_turn(state: &UndoState, key: &str, workspace: &str) {
    if key.trim().is_empty() {
        return;
    }
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key.to_string()).or_default();
        stack.push(UndoTurn { workspace: workspace.to_string(), ops: Vec::new() });
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if let Ok(mut r) = state.redo.lock() {
        r.remove(key);
    }
}

/// Record one op onto the session's live frame (creating one if the turn
/// started before this backend rolled, e.g. older app versions).
pub fn undo_record(state: &UndoState, key: &str, op: UndoOp) {
    if key.trim().is_empty() {
        return;
    }
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key.to_string()).or_default();
        if stack.is_empty() {
            stack.push(UndoTurn { workspace: String::new(), ops: Vec::new() });
        }
        if let Some(top) = stack.last_mut() {
            top.ops.push(op);
        }
    }
}

/// Record a completed `write` (prev captured before it ran) so both undo
/// and redo can restore the exact bytes.
pub fn undo_record_write(state: &UndoState, key: &str, path: &str, existed: bool, prev: Vec<u8>, next: &[u8]) {
    let op = if existed {
        UndoOp::Modified { path: path.to_string(), prev, next: next.to_vec() }
    } else {
        UndoOp::Created { path: path.to_string(), content: next.to_vec() }
    };
    undo_record(state, key, op);
}

/// Write `bytes` to a workspace-relative file, creating parent folders.
fn undo_write_file(canon: &std::path::Path, path: &str, bytes: &[u8]) -> Result<(), ()> {
    let full = work_join(canon, path).map_err(|_| ())?;
    if full.is_dir() {
        return Err(());
    }
    if let Some(parent) = full.parent() {
        if !parent.starts_with(canon) {
            return Err(());
        }
        std::fs::create_dir_all(parent).map_err(|_| ())?;
    }
    std::fs::write(&full, bytes).map_err(|_| ())
}

/// Invert one frame: created files are deleted, modified ones restored.
/// `exec` notes are reported, never auto-reverted (shell side effects are
/// not reliably invertible).
fn undo_apply_reverse(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
    let mut restored = 0usize;
    let mut removed = 0usize;
    let mut execs: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for op in ops.iter().rev() {
        match op {
            UndoOp::Created { path, .. } => match work_join(canon, path) {
                Ok(full) => {
                    if full.is_file() && !full.is_symlink() {
                        match std::fs::remove_file(&full) {
                            Ok(_) => removed += 1,
                            Err(_) => skipped.push(path.clone()),
                        }
                    } else if !full.exists() {
                        removed += 1; // already gone counts as reverted
                    } else {
                        skipped.push(path.clone());
                    }
                }
                Err(_) => skipped.push(path.clone()),
            },
            UndoOp::Modified { path, prev, .. } => {
                if undo_write_file(canon, path, prev).is_ok() {
                    restored += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::ExecNote { command } => {
                execs.push(command.chars().take(80).collect());
            }
        }
    }
    (restored, removed, execs, skipped)
}

/// Re-apply one frame: created files are rewritten, modified ones get the
/// turn's bytes back. Exec notes still can't be replayed.
fn undo_apply_forward(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
    let mut restored = 0usize;
    let mut recreated = 0usize;
    let mut execs: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for op in ops.iter() {
        match op {
            UndoOp::Created { path, content } => {
                if undo_write_file(canon, path, content).is_ok() {
                    recreated += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::Modified { path, next, .. } => {
                if undo_write_file(canon, path, next).is_ok() {
                    restored += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::ExecNote { command } => {
                execs.push(command.chars().take(80).collect());
            }
        }
    }
    (restored, recreated, execs, skipped)
}

/// Shared tail for the undo/redo notices ("restored 1 file, removed …").
fn undo_message(verb: &str, restored: usize, other: usize, other_verb: &str, other_noun: &str, execs: &[String], skipped: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if restored > 0 {
        parts.push(format!("restored {} file{}", restored, if restored == 1 { "" } else { "s" }));
    }
    if other > 0 {
        parts.push(format!(
            "{other_verb} {other} {other_noun}{}",
            if other == 1 { "" } else { "s" }
        ));
    }
    let mut msg = if parts.is_empty() {
        format!("{verb}: no file writes to change.")
    } else {
        format!("{verb}: {}.", parts.join(", "))
    };
    if !execs.is_empty() {
        msg.push_str(&format!(
            " ({} shell command{} ran — not auto-replayed, check manually).",
            execs.len(),
            if execs.len() == 1 { "" } else { "s" }
        ));
    }
    if !skipped.is_empty() {
        let names: Vec<String> = skipped.iter().take(3).cloned().collect();
        msg.push_str(&format!(" Skipped: {}.", names.join(", ")));
    }
    msg
}

/// Pop the last turn for a session, invert its file ops, and park it on the
/// redo stack. Returns a human summary for the chat notice.
pub async fn work_undo_logic(state: &UndoState, session_id: String) -> Result<String, String> {
    let key = session_id.trim().to_string();
    if key.is_empty() {
        return Err("no session for undo".to_string());
    }
    let turn = {
        let mut m = state.turns.lock().map_err(|_| "undo state poisoned".to_string())?;
        let stack = m.get_mut(&key).ok_or_else(|| "Nothing to undo — no assistant turn was recorded for this session.".to_string())?;
        stack.pop().ok_or_else(|| "Nothing to undo — no assistant turn was recorded for this session.".to_string())?
    };
    // every popped frame is redoable, even an empty one (frontend still
    // restores the message pair)
    if let Ok(mut r) = state.redo.lock() {
        let stack = r.entry(key).or_default();
        stack.push(turn.clone());
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if turn.ops.is_empty() || turn.workspace.trim().is_empty() {
        let execs = turn.ops.iter().filter(|o| matches!(o, UndoOp::ExecNote { .. })).count();
        if execs > 0 {
            return Ok(format!("Undid last turn: no file changes to revert ({} shell command{} ran — check manually).", execs, if execs == 1 { "" } else { "s" }));
        }
        return Ok("Undid last turn: no file changes to revert.".to_string());
    }
    let canon = work_canonical_root(turn.workspace.trim())
        .map_err(|e| format!("cannot undo into workspace: {e}"))?;
    let (restored, removed, execs, skipped) = undo_apply_reverse(&canon, &turn.ops);
    if restored == 0 && removed == 0 && execs.is_empty() && skipped.is_empty() {
        return Ok("Undid last turn: no file changes to revert.".to_string());
    }
    Ok(undo_message("Undid last turn", restored, removed, "removed", "created file", &execs, &skipped))
}

/// Re-apply the last undo frame for a session (the bubbles come back from
/// the frontend's stash). Pushes the frame back onto the undo stack so the
/// pair can alternate.
pub async fn work_redo_logic(state: &UndoState, session_id: String) -> Result<String, String> {
    let key = session_id.trim().to_string();
    if key.is_empty() {
        return Err("no session for redo".to_string());
    }
    let turn = {
        let mut r = state.redo.lock().map_err(|_| "undo state poisoned".to_string())?;
        let stack = r.get_mut(&key).ok_or_else(|| "Nothing to redo — /undo first.".to_string())?;
        stack.pop().ok_or_else(|| "Nothing to redo — /undo first.".to_string())?
    };
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key).or_default();
        stack.push(turn.clone());
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if turn.ops.is_empty() || turn.workspace.trim().is_empty() {
        let execs = turn.ops.iter().filter(|o| matches!(o, UndoOp::ExecNote { .. })).count();
        if execs > 0 {
            return Ok(format!("Redid last turn: no file changes to replay ({} shell command{} — run manually).", execs, if execs == 1 { "" } else { "s" }));
        }
        return Ok("Redid last turn: no file changes to replay.".to_string());
    }
    let canon = work_canonical_root(turn.workspace.trim())
        .map_err(|e| format!("cannot redo into workspace: {e}"))?;
    let (restored, recreated, execs, skipped) = undo_apply_forward(&canon, &turn.ops);
    if restored == 0 && recreated == 0 && execs.is_empty() && skipped.is_empty() {
        return Ok("Redid last turn: no file changes to replay.".to_string());
    }
    Ok(undo_message("Redid last turn", restored, recreated, "recreated", "file", &execs, &skipped))
}

/// Revert the last recorded Work turn's file changes for a session.
/// Pops one undo frame; chat bubbles are removed frontend-side.
#[tauri::command]
async fn work_undo(session_id: String, state: State<'_, UndoState>) -> Result<String, String> {
    work_undo_logic(&state, session_id).await
}

/// Re-apply the turn most recently reverted by /undo. Chat bubbles are
/// restored frontend-side.
#[tauri::command]
async fn work_redo(session_id: String, state: State<'_, UndoState>) -> Result<String, String> {
    work_redo_logic(&state, session_id).await
}

/// ---------- Vicuna CLI launcher (Settings → General, desktop only) ----------
///
/// `vicuna [path]` opens a folder in the app: a tiny launcher script that
/// opens a `vicuna://open?path=…` deep link (same scheme as Copy Link, so no
/// config change is needed) via the OS opener. The frontend validates the
/// path and raises a work session rooted there; no argument means the
/// caller's current directory (the script defaults to `$PWD`/`%CD%`).
/// Install location is a user-writable dir: first non-system PATH entry on
/// unix (falling back to `~/.local/bin`), `%LOCALAPPDATA%\Vicuna\bin` on
/// Windows (user PATH updated via the registry, no admin needed).

/// Marker comment identifying a launcher this app installed (so status never
/// claims a foreign `vicuna` binary as its own).
const CLI_MARKER: &str = "Installed by Vicuna";

/// POSIX launcher (macOS + Linux): resolve the arg (default `$PWD`), fully
/// percent-encode it (python3 when present, portable `od` fallback — slashes
/// included, the app decodes fully), open the link. `&` so the terminal
/// doesn't hang on `xdg-open`.
const CLI_UNIX_SCRIPT: &str = r#"#!/bin/sh
# Installed by Vicuna (Settings > General > Vicuna CLI). Opens a folder in the Vicuna desktop app: `vicuna [path]` (default: current directory).
p="${1:-$PWD}"
case "$p" in
  /*) abs="$p" ;;
  \~/*) abs="$HOME/${p#\~/}" ;;
  *) abs="$PWD/$p" ;;
esac
enc=$(python3 -c 'import sys,urllib.parse; print(urllib.parse.quote(sys.argv[1], safe=""))' "$abs" 2>/dev/null)
if [ -z "$enc" ]; then
  enc=$(printf '%s' "$abs" | od -An -tx1 | tr -d ' \n' | sed 's/\(..\)/%\1/g')
fi
url="vicuna://open?path=$enc"
if command -v xdg-open >/dev/null 2>&1; then
  xdg-open "$url" >/dev/null 2>&1 &
else
  open "$url" >/dev/null 2>&1 &
fi
"#;

/// Windows launcher: `%~f1` resolves the arg to a full path (default `%CD%`),
/// PowerShell percent-encodes it (batch can't), `start` opens the link.
const CLI_WINDOWS_SCRIPT: &str = "@echo off\r\nrem Installed by Vicuna (Settings > General > Vicuna CLI). Opens a folder in the Vicuna desktop app: `vicuna [path]` (default: current directory).\r\nsetlocal\r\nset \"P=%~f1\"\r\nif \"%P%\"==\"\" set \"P=%CD%\"\r\nfor /f \"delims=\" %%E in ('powershell -NoProfile -NonInteractive -Command \"[uri]::EscapeDataString($env:P)\"') do set \"ENC=%%E\"\r\nstart \"\" \"vicuna://open?path=%ENC%\"\r\n";

fn cli_exe_name() -> &'static str {
    if cfg!(windows) { "vicuna.cmd" } else { "vicuna" }
}

fn cli_script_body() -> &'static str {
    if cfg!(windows) { CLI_WINDOWS_SCRIPT } else { CLI_UNIX_SCRIPT }
}

/// PATH entries of this process. GUI apps launched from Finder get a sparse
/// system PATH, so this alone misses user bins like `~/.local/bin` — every
/// consumer below works off `cli_all_path_dirs` (process + login shell).
fn cli_path_dirs() -> Vec<std::path::PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    std::env::var("PATH")
        .unwrap_or_default()
        .split(sep)
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// PATH as the user's terminal sees it: probe `$SHELL -l` (profiles
/// sourced). Best-effort with a short timeout — a slow/broken profile must
/// never hang Settings. Windows doesn't need this (the registry check in
/// `cli_on_path_in` covers it).
#[cfg(not(windows))]
async fn cli_shell_path_dirs() -> Vec<std::path::PathBuf> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let out = tokio::time::timeout(
        Duration::from_secs(3),
        tokio::process::Command::new(&shell)
            .args(["-l", "-c", "echo $PATH"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output(),
    )
    .await;
    let Ok(Ok(out)) = out else { return Vec::new() };
    if !out.status.success() {
        return Vec::new();
    }
    // profiles may print noise above: the PATH is the last non-empty line
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .last()
        .unwrap_or("");
    line
        .split(':')
        .filter(|s| !s.trim().is_empty())
        .map(std::path::PathBuf::from)
        .collect()
}

/// Effective search path: process dirs first, then login-shell extras.
fn cli_all_path_dirs(shell_dirs: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out = cli_path_dirs();
    for d in shell_dirs {
        if !out.iter().any(|e| e == d) {
            out.push(d.clone());
        }
    }
    out
}

#[cfg(not(windows))]
fn cli_is_system_dir(d: &std::path::Path) -> bool {
    matches!(
        d.to_string_lossy().as_ref(),
        "/bin" | "/sbin" | "/usr/bin" | "/usr/sbin" | "/System/Volumes/Data/bin"
    )
}

fn cli_home_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    let home = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").ok()?;
    Some(std::path::PathBuf::from(home))
}

/// Where a launcher would be installed. Unix: first non-system entry of the
/// effective PATH (with `~/.local/bin` preferred when on it), else
/// `~/.local/bin`. Windows: `%LOCALAPPDATA%\Vicuna\bin`. Pure (no fs
/// access) so tests can drive it without touching HOME/PATH.
fn cli_install_dir_in(path_dirs: &[std::path::PathBuf], home: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        let _ = path_dirs;
        home.join("Vicuna").join("bin")
    }
    #[cfg(not(windows))]
    {
        let local = home.join(".local").join("bin");
        if path_dirs.iter().any(|d| d == &local) {
            return local;
        }
        for d in path_dirs {
            if d.is_absolute() && !cli_is_system_dir(d) {
                return d.clone();
            }
        }
        local
    }
}

fn cli_unix_fallback_dir_in(home: &std::path::Path) -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        let _ = home;
        None
    }
    #[cfg(not(windows))]
    {
        Some(home.join(".local").join("bin"))
    }
}

/// First dir holding the launcher name, if any.
fn cli_scan_in(dirs: &[std::path::PathBuf], exe: &str) -> Option<std::path::PathBuf> {
    for d in dirs {
        let cand = d.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

fn cli_scan(exe: &str, all_dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    cli_scan_in(all_dirs, exe)
}

fn cli_is_managed(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(CLI_MARKER))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize)]
struct CliStatus {
    exe_name: String,
    /// resolved `vicuna` on PATH, if any (managed or foreign)
    found: Option<String>,
    /// whether `found` is a launcher this app installed
    managed: bool,
    /// where Install would write
    install_dir: String,
    /// whether `install_dir` is on the effective PATH (process + login
    /// shell; Windows also checks the user PATH registry, which the current
    /// process may predate — a fresh terminal is authoritative)
    on_path: bool,
}

fn cli_on_path_in(dir: &std::path::Path, all_dirs: &[std::path::PathBuf]) -> bool {
    if all_dirs.iter().any(|d| d == dir) {
        return true;
    }
    #[cfg(windows)]
    {
        // the installer updates HKCU PATH, which this process predates:
        // ask the registry instead of trusting our own environment
        if let Ok(out) = Command::new("reg")
            .args(["query", r"HKCU\Environment", "/v", "Path"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
                let needle = dir.to_string_lossy().to_lowercase().to_string();
                return text.contains(&needle);
            }
        }
        false
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn cli_status_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliStatus, String> {
    let exe = cli_exe_name().to_string();
    let found = cli_scan(&exe, all_dirs);
    let managed = found.as_deref().map(cli_is_managed).unwrap_or(false);
    let install_dir = cli_install_dir_in(all_dirs, home);
    let on_path = cli_on_path_in(&install_dir, all_dirs);
    Ok(CliStatus {
        exe_name: exe,
        found: found.map(|p| p.to_string_lossy().to_string()),
        managed,
        install_dir: install_dir.to_string_lossy().to_string(),
        on_path,
    })
}

#[derive(Debug, Clone, Serialize)]
struct CliInstallResult {
    location: String,
    on_path: bool,
    note: String,
}

/// Write the launcher file into `dir` (creating it). Unix sets 0o755.
fn cli_write_launcher(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.to_string_lossy()))?;
    let dest = dir.join(cli_exe_name());
    std::fs::write(&dest, cli_script_body())
        .map_err(|e| format!("cannot write {}: {e}", dest.to_string_lossy()))?;
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("cannot chmod {}: {e}", dest.to_string_lossy()))?;
    }
    Ok(dest)
}

/// Ensure `dir` is in the current user's PATH (Windows only — HKCU, no
/// admin). Best-effort: returns whether the registry now contains it.
#[cfg(windows)]
fn cli_ensure_user_path(dir: &std::path::Path, all_dirs: &[std::path::PathBuf]) -> bool {
    if cli_on_path_in(dir, all_dirs) {
        return true;
    }
    let dir_s = dir.to_string_lossy().to_string();
    // .NET updates HKCU\Environment\Path without setx's truncation issues
    // and broadcasts the change for new processes.
    let ps = format!(
        "$p=[Environment]::GetEnvironmentVariable('Path','User'); if($p -split ';' -notcontains '{}'){{ [Environment]::SetEnvironmentVariable('Path', ($p+';{}'), 'User') }}",
        dir_s.replace('\'', "''"),
        dir_s.replace('\'', "''")
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    cli_on_path_in(dir, all_dirs)
}

fn cli_install_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliInstallResult, String> {
    let primary = cli_install_dir_in(all_dirs, home);
    let dest = match cli_write_launcher(&primary) {
        Ok(d) => (d, None),
        Err(e) => {
            // permission-denied on a PATH dir (e.g. /usr/local/bin owned by
            // root): fall back to the user-local bin instead of failing
            match cli_unix_fallback_dir_in(home) {
                Some(fb) if fb != primary => {
                    let d = cli_write_launcher(&fb).map_err(|e2| {
                        format!("{e}. Fallback also failed ({}): {e2}", fb.to_string_lossy())
                    })?;
                    (d, Some(format!("{} is not writable, installed to {} instead.", primary.to_string_lossy(), fb.to_string_lossy())))
                }
                _ => return Err(e),
            }
        }
    };
    #[cfg(windows)]
    let path_note = if cli_ensure_user_path(dest.0.parent().unwrap_or(&primary), all_dirs) {
        "Added to your user PATH — open a NEW terminal to use it."
    } else {
        "Could not update your user PATH automatically."
    };
    let on_path = cli_on_path_in(dest.0.parent().unwrap_or(&primary), all_dirs);
    let mut note = dest.1.unwrap_or_default();
    #[cfg(windows)]
    {
        if !note.is_empty() {
            note.push(' ');
        }
        note.push_str(path_note);
    }
    #[cfg(not(windows))]
    {
        if !on_path {
            note = format!(
                "{} is not on your terminal PATH — add `export PATH=\"{}:$PATH\"` to your shell profile, then open a new terminal.",
                dest.0.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default(),
                dest.0.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            );
        } else if note.is_empty() {
            note = "Run `vicuna [path]` in a terminal (default: current directory).".to_string();
        }
    }
    Ok(CliInstallResult {
        location: dest.0.to_string_lossy().to_string(),
        on_path,
        note,
    })
}

/// Remove a launcher this app installed (the managed PATH hit, else our own
/// install dir). Refuses foreign `vicuna` binaries.
fn cli_uninstall_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<String, String> {
    let exe = cli_exe_name();
    if let Some(found) = cli_scan(exe, all_dirs) {
        if cli_is_managed(&found) {
            std::fs::remove_file(&found).map_err(|e| format!("cannot remove {}: {e}", found.to_string_lossy()))?;
            return Ok(found.to_string_lossy().to_string());
        }
    }
    let owned = cli_install_dir_in(all_dirs, home).join(exe);
    if owned.is_file() {
        if !cli_is_managed(&owned) {
            return Err(format!("{} was not installed by Vicuna — refusing to remove it", owned.to_string_lossy()));
        }
        std::fs::remove_file(&owned).map_err(|e| format!("cannot remove {}: {e}", owned.to_string_lossy()))?;
        return Ok(owned.to_string_lossy().to_string());
    }
    Err("no Vicuna-installed launcher found".to_string())
}

/// Effective CLI search path for this machine: process PATH plus whatever
/// the user's login shell adds (Finder-launched apps miss user bins).
async fn cli_env() -> Result<(Vec<std::path::PathBuf>, std::path::PathBuf), String> {
    #[cfg(windows)]
    let shell_dirs = Vec::new();
    #[cfg(not(windows))]
    let shell_dirs = cli_shell_path_dirs().await;
    let all_dirs = cli_all_path_dirs(&shell_dirs);
    let home = cli_home_dir().ok_or_else(|| "cannot locate home folder".to_string())?;
    Ok((all_dirs, home))
}

/// Report the `vicuna` launcher status (install location + PATH state).
#[tauri::command]
async fn cli_status() -> Result<CliStatus, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_status_inner(&all_dirs, &home)
}

/// Install (or reinstall) the `vicuna` launcher script.
#[tauri::command]
async fn cli_install() -> Result<CliInstallResult, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_install_inner(&all_dirs, &home)
}

/// Remove the launcher this app installed. Refuses foreign binaries.
#[tauri::command]
async fn cli_uninstall() -> Result<String, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_uninstall_inner(&all_dirs, &home)
}

/// Bring the main window forward (CLI deep links arrive while hidden in the
/// tray; the OS activates the app but doesn't un-hide the window).
#[tauri::command]
async fn show_main_window(app: AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "no main window".to_string())?;
    w.show().map_err(|e| format!("cannot show window: {e}"))?;
    w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
    Ok(())
}

/// ---------- Interactive terminals (side-panel tabs, desktop only) ----------
///
/// Each terminal tab owns a portable-pty pair rooted at the workspace: a
/// reader thread streams output over the Channel, a waiter thread reports
/// the exit code. Master/writer stay in the map for write/resize; kills go
/// by pid so a stuck child is always reaped (best-effort, never fatal).

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum TermEvent {
    Output { text: String },
    Exit { code: Option<i32> },
}

struct TermHandle {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    pid: Option<u32>,
}

#[derive(Default)]
struct TermState {
    terms: Mutex<HashMap<String, TermHandle>>,
    seq: AtomicU64,
}

static TERM_SEQ: AtomicU64 = AtomicU64::new(1);

fn term_size(cols: Option<u16>, rows: Option<u16>) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: rows.unwrap_or(24).clamp(5, 200),
        cols: cols.unwrap_or(80).clamp(20, 400),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Kill by pid (unix kill -9 / windows taskkill). Best-effort.
fn kill_pid(pid: u32) {
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

/// Spawn a login shell on a fresh pty rooted at `root`. The reader thread
/// streams output until EOF; the waiter thread reports the exit code.
/// Returns the handle (master + writer + pid for write/resize/kill).
fn spawn_pty<F, G>(
    root: &str,
    cols: Option<u16>,
    rows: Option<u16>,
    on_output: F,
    on_exit: G,
) -> Result<TermHandle, String>
where
    F: Fn(String) + Send + 'static,
    G: FnOnce(Option<i32>) + Send + 'static,
{
    let canon = work_canonical_root(root.trim())?;
    // `canonicalize()` returns extended-length paths on Windows
    // (`\\?\C:\...`); spawning the shell there makes PowerShell render its
    // prompt as `Microsoft.PowerShell.Core\FileSystem::\\?\C:\...`. The
    // stripped path points at the same folder and prompts cleanly.
    let cwd = work_display_string(&canon);
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(term_size(cols, rows))
        .map_err(|e| format!("cannot open pty: {e}"))?;
    let mut cmd = {
        #[cfg(windows)]
        {
            // PowerShell from the start: nesting it under cmd garbles
            // PSReadLine styling. The Work agent has its own persistent
            // PowerShell pty (AgentShellState), separate from these tabs.
            let mut c = portable_pty::CommandBuilder::new("powershell.exe");
            c.args(["-NoLogo"]);
            c
        }
        #[cfg(not(windows))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let mut c = portable_pty::CommandBuilder::new(shell);
            c.args(["-l"]);
            c
        }
    };
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("cannot spawn shell: {e}"))?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("pty failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("pty failed: {e}"))?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    on_output(String::from_utf8_lossy(&buf[..n]).to_string());
                }
                Err(_) => break,
            }
        }
    });
    std::thread::spawn(move || {
        // signaled deaths (e.g. our kill -9) carry no status code
        let code = child
            .wait()
            .ok()
            .and_then(|s| {
                if s.signal().is_some() {
                    None
                } else {
                    Some(s.exit_code() as i32)
                }
            });
        on_exit(code);
    });
    Ok(TermHandle {
        master: pair.master,
        writer: Mutex::new(writer),
        pid,
    })
}

/// Spawn a login shell rooted at the workspace. Output/exit stream back
/// over the channel; returns the terminal id for write/resize/kill.
#[tauri::command]
async fn term_spawn(
    root: String,
    cols: Option<u16>,
    rows: Option<u16>,
    state: State<'_, TermState>,
    on_event: Channel<TermEvent>,
) -> Result<String, String> {
    let canon = work_canonical_root(root.trim())?;
    let id = format!("term-{}", TERM_SEQ.fetch_add(1, Ordering::Relaxed));
    let ev = on_event.clone();
    let ev2 = on_event;
    let handle = spawn_pty(
        &canon.to_string_lossy(),
        cols,
        rows,
        move |text| {
            if ev.send(TermEvent::Output { text }).is_err() {
                // frontend went away; the reader loop keeps draining the
                // pty until the shell exits (killed with the tab)
            }
        },
        move |code| {
            let _ = ev2.send(TermEvent::Exit { code });
        },
    )?;
    state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?
        .insert(id.clone(), handle);
    Ok(id)
}

/// Send keystrokes to a terminal.
#[tauri::command]
async fn term_write(id: String, data: String, state: State<'_, TermState>) -> Result<(), String> {
    let terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .get(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    let mut w = h
        .writer
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    w.write_all(data.as_bytes())
        .map_err(|e| format!("terminal write failed: {e}"))?;
    w.flush()
        .map_err(|e| format!("terminal write failed: {e}"))?;
    Ok(())
}

/// Resize a terminal (columns × rows from the xterm fit addon).
#[tauri::command]
async fn term_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TermState>,
) -> Result<(), String> {
    let terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .get(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    h.master
        .resize(term_size(Some(cols), Some(rows)))
        .map_err(|e| format!("terminal resize failed: {e}"))?;
    Ok(())
}

/// Kill a terminal's shell (by pid, best-effort) and drop the entry.
#[tauri::command]
async fn term_kill(id: String, state: State<'_, TermState>) -> Result<(), String> {
    let mut terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .remove(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    drop(terms);
    if let Some(pid) = h.pid {
        kill_pid(pid);
    }
    Ok(())
}

/// Rename a file or directory within its own folder (same-directory rename).
/// `new_name` is a bare name — slashes, `..`, and empties are rejected —
/// so the result can never escape the workspace.
pub async fn work_rename_logic(root: String, path: String, new_name: String) -> Result<String, String> {
    let canon = work_canonical_root(root.trim())?;
    let full = work_join(&canon, &path)?;
    if !full.exists() {
        return Err("file not found".to_string());
    }
    let name = new_name.trim();
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
    {
        return Err("invalid name".to_string());
    }
    let parent = full.parent().ok_or_else(|| "invalid path".to_string())?;
    let dest = parent.join(name);
    if !dest.starts_with(&canon) {
        return Err("path escapes the workspace".to_string());
    }
    if dest.exists() {
        return Err("a file with that name already exists".to_string());
    }
    std::fs::rename(&full, &dest).map_err(|e| format!("cannot rename: {e}"))?;
    Ok(dest
        .strip_prefix(&canon)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| dest.to_string_lossy().to_string()))
}

/// Delete a workspace file or directory (recursive for dirs). Refuses the
/// workspace root itself. Symlinks are removed, never followed.
pub async fn work_delete_logic(root: String, path: String) -> Result<(), String> {
    let canon = work_canonical_root(root.trim())?;
    let full = work_join(&canon, &path)?;
    if full == canon {
        return Err("cannot delete the workspace root".to_string());
    }
    let meta = std::fs::symlink_metadata(&full).map_err(|_| "file not found".to_string())?;
    if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
        std::fs::remove_dir_all(&full).map_err(|e| format!("cannot delete folder: {e}"))?;
    } else {
        std::fs::remove_file(&full).map_err(|e| format!("cannot delete file: {e}"))?;
    }
    Ok(())
}

/// ---------- Session file uploads (composer attachments, desktop only) ----------
///
/// Dropped files are *copied* (never moved) into a per-session staging area
/// under the user's home: `~/vicuna/uploads/<session-id>/<file name>`.
/// This keeps project workspaces untouched (no git noise, no build or grep
/// pollution) and works for chat sessions that have no workspace at all.
/// The agent reaches staged files through `upload_read_file`, which is
/// scoped to the calling session — never arbitrary absolute paths.

fn uploads_home_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").map_err(|_| "cannot locate home folder".to_string())?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").map_err(|_| "cannot locate home folder".to_string())?;
    Ok(std::path::PathBuf::from(home))
}

/// The user's Downloads folder (installer landing zone for in-app updates).
pub fn downloads_dir() -> Result<std::path::PathBuf, String> {
    Ok(uploads_home_dir()?.join("Downloads"))
}

/// Open a downloaded installer with the system handler. Bridge-only path
/// (desktop uses the opener plugin). Restricted to installer payloads
/// inside ~/Downloads so the bridge can't be aimed at arbitrary files.
pub fn open_installer(path: &str) -> Result<(), String> {
    let full = std::path::PathBuf::from(path.trim());
    let canon = full.canonicalize().map_err(|_| "file not found".to_string())?;
    let dir = downloads_dir()?;
    let canon_dir = dir.canonicalize().unwrap_or(dir);
    if !canon.starts_with(&canon_dir) {
        return Err("refusing path outside Downloads".to_string());
    }
    let ext_ok = canon
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "dmg" | "exe" | "appimage" | "deb"))
        .unwrap_or(false);
    if !ext_ok {
        return Err("not an installer file".to_string());
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&canon)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot open installer: {e}"))?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&canon)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot open installer: {e}"))?;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let arg = canon.to_string_lossy().to_string();
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(arg)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot open installer: {e}"))?;
    }
    Ok(())
}

/// ---------- In-app updates (Settings → App updates) ----------
///
/// The CI release workflow attaches installers to the rolling `latest`
/// prerelease on every main push with `gh release upload --clobber`, so the
/// release *tag* never changes and — crucially — neither does its
/// `published_at` (verified: the release keeps its first-publish date while
/// the assets are replaced). Staleness is therefore decided against the
/// *asset* timestamp (`updated_at`, created_at fallback), which GitHub
/// refreshes on every clobber upload, compared to this build's baked
/// timestamp (`VICUNA_BUILD_UNIX`, see build.rs). Downloads are allowlisted
/// to this repo's release attachments and land in ~/Downloads; the UI then
/// opens the installer (the user runs it — unsigned side-loaded builds
/// can't self-replace).

#[derive(Debug, Clone, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

/// This build's version + baked timestamp (seconds since epoch, 0 when the
/// build script couldn't read the clock).
pub async fn app_build_info_logic() -> Result<serde_json::Value, String> {
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "builtAtUnix": option_env!("VICUNA_BUILD_UNIX").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
    }))
}

/// Pick the release-attachment fields the frontend needs out of the GitHub
/// `/releases/tags/latest` payload. Pure (unit-tested) so the mapping stays
/// honest when GitHub reshapes the response.
pub fn parse_release_info(v: &serde_json::Value) -> Result<serde_json::Value, String> {
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "release has no tag_name".to_string())?;
    let published_at = v
        .get("published_at")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "release has no published_at".to_string())?;
    let html_url = v
        .get("html_url")
        .and_then(|t| t.as_str())
        .unwrap_or("https://github.com/anslwy/vicuna/releases/latest");
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for a in assets {
        let (Some(name), Some(url)) = (
            a.get("name").and_then(|n| n.as_str()),
            a.get("browser_download_url").and_then(|u| u.as_str()),
        ) else {
            continue;
        };
        // clobber uploads refresh updated_at (created_at on the very first
        // upload); the release-level published_at is frozen, so this is the
        // only reliable "when was this installer built" signal.
        let uploaded = a
            .get("updated_at")
            .and_then(|t| t.as_str())
            .or_else(|| a.get("created_at").and_then(|t| t.as_str()));
        out.push(serde_json::json!({
            "name": name,
            "url": url,
            "size": a.get("size").and_then(|s| s.as_u64()),
            "uploadedAt": uploaded,
        }));
    }
    let updated_at = v.get("updated_at").and_then(|t| t.as_str());
    Ok(serde_json::json!({
        "tag": tag,
        "publishedAt": published_at,
        "updatedAt": updated_at,
        "htmlUrl": html_url,
        "assets": out,
    }))
}

/// Latest published info for the rolling `latest` prerelease. GitHub's
/// `/releases/latest` endpoint skips prereleases, so address the tag
/// directly (GitHub API needs an explicit User-Agent).
pub async fn github_latest_release_logic() -> Result<serde_json::Value, String> {
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("update check failed: {e}"))?
        .get("https://api.github.com/repos/anslwy/vicuna/releases/tags/latest")
        .header("User-Agent", "vicuna-update-check/1.0")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        return Err(format!("update check failed: GitHub answered {status}"));
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    parse_release_info(&data)
}

/// Download a release attachment into ~/Downloads. The URL is allowlisted
/// to this repo's release attachments; the file name is flattened to its
/// final component. Returns the absolute path for the UI to open.
pub async fn github_download_asset_logic(url: String, file_name: String) -> Result<String, String> {
    const PREFIX: &str = "https://github.com/anslwy/vicuna/releases/download/";
    if !url.starts_with(PREFIX) {
        return Err("refusing non-release URL".to_string());
    }
    let name = std::path::Path::new(file_name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != ".." && n.len() <= 255)
        .ok_or_else(|| "invalid file name".to_string())?;
    let dir = downloads_dir()?;
    let dest = dir.join(name);
    if !dest.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("download failed: {e}"))?
        .get(&url)
        .header("User-Agent", "vicuna-update-check/1.0")
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("download failed: GitHub answered {}", res.status()));
    }
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt as _;
    let mut stream = res.bytes_stream();
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    // installers are tens of MB; 1GB cap aborts runaway responses
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
        total += chunk.len() as u64;
        if total > 1024 * 1024 * 1024 {
            return Err("download failed: file too large".to_string());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("download failed: {e}"))?;
    }
    file.flush().await.map_err(|e| format!("download failed: {e}"))?;
    drop(file);
    // AppImages must be executable to run — browsers/KDE etc. set +x on
    // download, a plain HTTPS stream does not.
    #[cfg(unix)]
    if dest
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(dest.to_string_lossy().to_string())
}

fn upload_session_dir(session_id: &str) -> Result<std::path::PathBuf, String> {
    let id = session_id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid session".to_string());
    }
    Ok(uploads_home_dir()?
        .join("vicuna")
        .join("uploads")
        .join(id))
}

/// Copy an uploaded file into the session staging area. The name is flattened
/// to its final component (no subdirectories, no escapes); existing files
/// with the same name are overwritten, like `cp`. Returns the absolute path
/// for the prompt note sent with the message.
pub async fn upload_stage_file_logic(session_id: String, file_name: String, base64: String) -> Result<String, String> {
    const CAP: usize = 16 * 1024 * 1024;
    let bytes = decode_base64(&base64).map_err(|_| "invalid file data".to_string())?;
    if bytes.len() > CAP {
        return Err("file too large (max 16MB)".to_string());
    }
    let name = std::path::Path::new(file_name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != ".." && n.len() <= 255)
        .ok_or_else(|| "invalid file name".to_string())?;
    let dir = upload_session_dir(&session_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot stage upload: {e}"))?;
    let dest = dir.join(name);
    if !dest.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    std::fs::write(&dest, &bytes).map_err(|e| format!("cannot stage upload: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Read a staged session file as text (cap ~64KB; binaries rejected).
/// Scoped to the calling session's upload dir — no absolute paths.
pub async fn upload_read_file_logic(session_id: String, name: String) -> Result<WorkFileContent, String> {
    let dir = upload_session_dir(&session_id)?;
    let clean = std::path::Path::new(name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or_else(|| "invalid file name".to_string())?;
    let full = dir.join(clean);
    if !full.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    let bytes = std::fs::read(&full).map_err(|e| format!("cannot read file: {e}"))?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err("binary file — refusing to dump as text".to_string());
    }
    const CAP: usize = 65536;
    let truncated = bytes.len() > CAP;
    let slice = &bytes[..bytes.len().min(CAP)];
    Ok(WorkFileContent {
        path: clean.to_string(),
        content: String::from_utf8_lossy(slice).to_string(),
        truncated,
        size,
    })
}

/// Window zoom level for Cmd/Ctrl +/−/0. Desktop shells don't zoom webviews
/// on those keys the way browsers do natively, so the frontend catches the
/// chord (Tauri only) and drives the webview zoom from here.
struct ZoomState {    factor: Mutex<f64>,
}

fn clamp_zoom(f: f64) -> f64 {
    f.clamp(0.5, 3.0)
}

#[tauri::command]
async fn zoom_apply(
    window: tauri::WebviewWindow,
    state: State<'_, ZoomState>,
    factor: f64,
) -> Result<f64, String> {
    let factor = clamp_zoom(factor);
    window
        .set_zoom(factor)
        .map_err(|e| format!("zoom failed: {e}"))?;
    state
        .factor
        .lock()
        .map(|mut f| {
            *f = factor;
            *f
        })
        .map_err(|_| "zoom state poisoned".to_string())
}

#[tauri::command]
async fn zoom_step(
    window: tauri::WebviewWindow,
    state: State<'_, ZoomState>,
    direction: String,
) -> Result<f64, String> {
    let current = *state
        .factor
        .lock()
        .map_err(|_| "zoom state poisoned".to_string())?;
    let base = if current <= 0.0 { 1.0 } else { current };
    let next = match direction.as_str() {
        "in" => base * 1.15,
        "out" => base / 1.15,
        _ => 1.0,
    };
    zoom_apply(window, state, next).await
}

/// Whether a window close is a real quit (tray / Settings) or just a hide.
/// Closing the main window hides to the tray so streams keep running;
/// only an explicit Quit sets this and lets the app (and everything it
/// started) exit.
#[derive(Default)]
struct AppQuitState {
    allow: Mutex<bool>,
}

/// Kill everything Vicuna started: a self-launched `ollama serve` plus all
/// terminal ptys. Best-effort — never fails the quit path.
fn shutdown_children(app: &AppHandle) {
    if let Some(s) = app.try_state::<browser::BrowserState>() {
        browser::kill_owned(&s);
    }
    if let Some(s) = app.try_state::<OllamaServerState>() {
        if let Ok(mut g) = s.child.lock() {
            if let Some(mut child) = g.take() {
                let _ = child.kill();
            }
        }
    }
    if let Some(s) = app.try_state::<TermState>() {
        if let Ok(mut terms) = s.terms.lock() {
            for (_, h) in terms.drain() {
                if let Some(pid) = h.pid {
                    kill_pid(pid);
                }
            }
        }
    }
    if let Some(s) = app.try_state::<BgExecState>() {
        if let Ok(jobs) = s.jobs.lock() {
            for (_, rec) in jobs.iter() {
                if let Ok(g) = rec.lock() {
                    if g.running {
                        if let Some(pid) = g.pid {
                            kill_pid(pid);
                        }
                    }
                }
            }
        }
    }
    if let Some(s) = app.try_state::<AgentShellState>() {
        kill_agent_shells(&s);
    }
}

fn allow_quit(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppQuitState>() {
        if let Ok(mut g) = s.allow.lock() {
            *g = true;
        }
    }
}

/// Explicit quit (tray menu / Settings button): kill everything Vicuna
/// started, then exit. Window-close hides to the tray instead (see
/// `on_window_event` below) so background streams survive.
#[tauri::command]
async fn quit_app(
    app: AppHandle,
    _ollama: State<'_, OllamaServerState>,
    _terms: State<'_, TermState>,
) -> Result<(), String> {
    allow_quit(&app);
    shutdown_children(&app);
    app.exit(0);
    Ok(())
}

/// Build the tray menu: a disabled live-session status line ("No running
/// sessions" / "N running work session(s)"), a separator, Show / Quit —
/// plus, while sessions run, a second separator and a disabled elapsed line
/// ("32m 25s") under Quit. Rebuilt on every frontend report.
fn tray_menu<R: Runtime>(
    manager: &impl Manager<R>,
    status: &str,
    elapsed: Option<&str>,
) -> tauri::Result<Menu<R>> {
    let status_item = MenuItem::with_id(manager, "status", status, false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(manager)?;
    let show = MenuItem::with_id(manager, "show", "Show Vicuna", true, None::<&str>)?;
    let quit = MenuItem::with_id(manager, "quit", "Quit Vicuna", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(manager)?;
    let elapsed_item = match elapsed {
        Some(e) => Some(MenuItem::with_id(manager, "elapsed", e, false, None::<&str>)?),
        None => None,
    };
    let mut items: Vec<&dyn IsMenuItem<R>> = vec![
        &status_item as &dyn IsMenuItem<R>,
        &sep as &dyn IsMenuItem<R>,
        &show as &dyn IsMenuItem<R>,
        &quit as &dyn IsMenuItem<R>,
    ];
    if let Some(ref e) = elapsed_item {
        items.push(&sep2 as &dyn IsMenuItem<R>);
        items.push(e as &dyn IsMenuItem<R>);
    }
    Menu::with_items(manager, &items)
}

/// Refresh the tray menu (no-op when the tray is unavailable).
fn refresh_tray_menu(app: &AppHandle, status: &str, elapsed: Option<&str>) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id("main") {
        let menu =
            tray_menu(app, status, elapsed).map_err(|e| format!("tray menu failed: {e}"))?;
        tray.set_menu(Some(menu))
            .map_err(|e| format!("tray menu failed: {e}"))?;
    }
    Ok(())
}

/// Update the tray menu: status line plus, while sessions run, an elapsed
/// line under Quit. The frontend owns streaming state, so it reports both
/// whenever relevant. Best-effort: never fails a turn.
#[tauri::command]
async fn tray_set_status(
    app: AppHandle,
    status: String,
    elapsed: Option<String>,
) -> Result<(), String> {
    let short: String = status.trim().chars().take(80).collect();
    let elapsed: Option<String> = elapsed
        .map(|e| e.trim().chars().take(20).collect::<String>())
        .filter(|e| !e.is_empty());
    refresh_tray_menu(
        &app,
        if short.is_empty() {
            "No running sessions"
        } else {
            &short
        },
        elapsed.as_deref(),
    )
}

/// ---------- MCP servers (Work mode only) ----------
///
/// Configs live in `~/vicuna/config/vicuna.json` (`mcp` map, opencode.json
/// shape) — the file is the source of truth, the UI edits it. Connections
/// are runtime-only in the McpManager (stdio children die on disconnect).

#[tauri::command]
async fn mcp_list_servers() -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::list_file_servers()
}

#[tauri::command]
async fn mcp_save_server(config: mcp::McpServerConfig) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::save_file_server(&config)
}

#[tauri::command]
async fn mcp_delete_server(id: String) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::delete_file_server(&id)
}

#[tauri::command]
async fn mcp_connect(
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    state.connect(&config).await
}

#[tauri::command]
async fn mcp_disconnect(id: String, state: State<'_, mcp::McpManager>) -> Result<(), String> {
    state.disconnect(&id).await
}

#[tauri::command]
async fn mcp_list_tools(
    id: String,
    state: State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::McpToolInfo>, String> {
    state.list_tools(&id).await
}

#[tauri::command]
async fn mcp_call_tool(
    server_id: String,
    tool: String,
    args: serde_json::Value,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpCallResult, String> {
    state.call_tool(&server_id, &tool, args).await
}

/// Browser sign-in for an OAuth-protected remote server: runs the PKCE
/// authorize flow (system browser + loopback callback), persists tokens to
/// auth.json, then connects. Errors when the server needs no auth.
#[tauri::command]
async fn mcp_signin(
    app: AppHandle,
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    use tauri_plugin_opener::OpenerExt;
    mcp::oauth_login(&config, |url| {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| format!("mcp: cannot open browser for sign-in: {e}"))
    })
    .await?;
    state.connect(&config).await
}

/// Forget the stored OAuth login and drop the connection, if any.
#[tauri::command]
async fn mcp_signout(id: String, state: State<'_, mcp::McpManager>) -> Result<(), String> {
    // server names key the token store; resolve the id first
    let name = mcp::list_file_servers()?
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.name)
        .unwrap_or(id.clone());
    mcp::delete_auth(&name);
    let _ = state.disconnect(&id).await;
    Ok(())
}

/// Whether an OAuth login is stored for the server (drives the UI's
/// "Signed in / Sign out" affordance).
#[tauri::command]
async fn mcp_login_present(id: String) -> Result<bool, String> {
    let name = mcp::list_file_servers()?
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.name)
        .unwrap_or(id);
    Ok(mcp::auth_present(&name))
}

/// ---------- Browser tab: `lb` (live-browser) sidecar daemon ----------
///
/// The daemon (Chrome via CDP + JSON API + Svelte UI) is the shared engine:
/// the side-panel tab iframes its web UI while the Work agent drives the
/// same tabs through `lb mcp`. Adopt-or-spawn + owned-only stop, exactly
/// like `ollama serve` above. The headless bridge reuses the `*_logic`
/// fns in `browser.rs` with no resource dir (PATH resolution only).

fn browser_resource_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().resource_dir().ok()
}

#[tauri::command]
async fn browser_status(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_status_logic(browser_resource_dir(&app), &state).await
}

#[tauri::command]
async fn browser_start(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_start_logic(browser_resource_dir(&app), &state).await
}

#[tauri::command]
async fn browser_stop(state: State<'_, browser::BrowserState>) -> Result<browser::BrowserStatus, String> {
    browser::browser_stop_logic(&state).await
}

#[tauri::command]
async fn browser_restart(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_restart_logic(browser_resource_dir(&app), &state).await
}

#[tauri::command]
async fn browser_ensure_engine(app: AppHandle) -> Result<browser::EngineInfo, String> {
    browser::browser_ensure_engine_logic(browser_resource_dir(&app)).await
}

#[tauri::command]
async fn browser_takeover(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_takeover_logic(browser_resource_dir(&app), &state).await
}

/// Absolute `lb mcp` argv for the default `browser` MCP server entry.
#[tauri::command]
async fn browser_mcp_command(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = browser_resource_dir(&app);
    browser::browser_mcp_command(dir.as_deref())
}

/// Window/tray title of the running shell. `tauri dev` builds with
/// `debug_assertions`, and so does a local `cargo build`, so debug builds are
/// labelled "Vicuna (Dev)": on macOS a dev window is otherwise
/// indistinguishable from the installed Vicuna.App. The frontend `<title>` in
/// `app.html` stays plain "Vicuna" on purpose — `scripts/desktop-dev.sh`
/// probes for it to tell our own dev server from a foreign :1420 occupant.
pub fn app_title() -> &'static str {
    if cfg!(debug_assertions) {
        "Vicuna (Dev)"
    } else {
        "Vicuna"
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Finder/Dock launches get launchd's minimal PATH: read the user's shell
    // PATH in the background so engine detection sees what a terminal sees.
    exe_resolve::warm_up();
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Signed in-app updates (same flow as cojudge): the frontend checks
        // `plugins.updater.endpoints` (latest.json on the newest GitHub
        // release), installs, then relaunches via the process plugin.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(OllamaServerState::default())
        .manage(TermState::default())
        .manage(BgExecState::default())
        .manage(AgentShellState::default())
        .manage(UndoState::default())
        .manage(work_agent::WorkAgentState::default())
        .manage(acp::AcpState::default())
        .manage(AppQuitState::default())
        .manage(browser::BrowserState::default())
        .manage(mcp::McpManager::default())
        .manage(ZoomState {
            factor: Mutex::new(1.0),
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            chats_save,
            ollama_list_models,
            ollama_list_running,
            ollama_warm_model,
            ollama_chat_stream,
            ollama_generate_stream,
            ollama_pull_model,
            ollama_delete_model,
            ollama_model_show,
            ollama_fetch_text,
            ollama_server_version,
            ollama_start_server,
            ollama_stop_server,
            work_validate_root,
            work_list_dir,
            work_tree,
            work_import_file,
            work_rename,
            work_delete,
            upload_stage_file,
            upload_read_file,
            app_build_info,
            github_latest_release,
            github_download_asset,
            work_read_file,
            work_read_image,
            work_grep,
            work_write_file,
            work_exec,
            work_fetch_url,
            work_exec_start,
            work_exec_status,
            work_exec_list,
            work_exec_kill,
            work_shell_exec,
            work_shell_status,
            work_shell_input,
            work_shell_interrupt,
            work_undo,
            work_redo,
            cli_status,
            cli_install,
            cli_uninstall,
            show_main_window,
            term_spawn,
            term_write,
            term_resize,
            term_kill,
            zoom_apply,
            zoom_step,
            quit_app,
            tray_set_status,
            mcp_list_servers,
            mcp_save_server,
            mcp_delete_server,
            mcp_connect,
            mcp_disconnect,
            mcp_list_tools,
            mcp_call_tool,
            mcp_signin,
            mcp_signout,
            mcp_login_present,
            browser_status,
            browser_start,
            browser_stop,
            browser_restart,
            browser_takeover,
            browser_ensure_engine,
            browser_mcp_command,
            work_agent::work_agent_start,
            work_agent::work_agent_stop,
            work_agent::work_agent_queue,
            work_agent::work_agent_remove,
            work_agent::work_agent_answer,
            work_engine_info,
            opencode::opencode_models,
            opencode::opencode_set_model,
            work_engine::work_engine_select,
            work_engine::work_models,
            work_engine::work_set_model,
            work_engine::work_set_approval
        ])
        .setup(|app| {
            // Warm the long-lived `opencode acp` process so the first Work
            // turn skips its cold start (no-op when opencode is missing).
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    handle.state::<acp::AcpState>().prewarm().await;
                });
            }
            // Debug builds say so in the window title (`npm run desktop:dev`
            // → `tauri dev`), so a dev window can't be mistaken for the
            // installed Vicuna.App on the Mac. Release builds keep "Vicuna".
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(app_title());
            }
            // System tray: closing the window hides here (streams keep
            // running); only Quit exits. The menu opens on either click;
            // Show reopens the window.
            let menu = tray_menu(app, "No running sessions", None)?;
            if let Some(icon) = app.default_window_icon().cloned() {
                TrayIconBuilder::with_id("main")
                    .icon(icon)
                    .tooltip(app_title())
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => {
                            allow_quit(app);
                            shutdown_children(app);
                            app.exit(0);
                        }
                        _ => {}
                    })
                    .build(app)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to the tray instead of quitting so the backend (streams,
            // terminals, self-started ollama) survives a window close.
            if let WindowEvent::CloseRequested { api, .. } = event {
                let allow = window
                    .app_handle()
                    .try_state::<AppQuitState>()
                    .and_then(|s| s.allow.lock().ok().map(|g| *g))
                    .unwrap_or(false);
                if !allow {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(on_run_event);
}

/// App event handler: macOS dock click with the window hidden reopens it.
/// `RunEvent::Reopen` only exists on macOS, so other platforms do nothing.
#[allow(unused_variables)]
fn on_run_event(app: &AppHandle, event: tauri::RunEvent) {
    #[cfg(target_os = "macos")]
    if let tauri::RunEvent::Reopen { .. } = event {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.show();
            let _ = w.set_focus();
        }
    }
}

#[cfg(test)]
mod work_tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_root(tag: &str) -> std::path::PathBuf {
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("vicuna-work-test-{tag}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// `npm run desktop:dev` (debug build) must be distinguishable from the
    /// installed app, and a release build must keep the plain name.
    #[test]
    fn dev_builds_are_titled_vicuna_dev() {
        if cfg!(debug_assertions) {
            assert_eq!(app_title(), "Vicuna (Dev)");
        } else {
            assert_eq!(app_title(), "Vicuna");
        }
    }

    #[test]
    fn validate_rejects_missing_and_relative() {
        assert!(work_canonical_root("/no/such/dir/xyz").is_err());
        assert!(work_canonical_root("relative/path").is_err());
        // filesystem root would crawl TCC-protected spots on macOS
        assert!(work_canonical_root("/").is_err());
        let d = tmp_root("validate");
        assert!(work_canonical_root(d.to_str().unwrap()).is_ok());
    }

    /// The persistent shell must keep cwd and exported env across exec calls
    /// (the whole point of replacing one-shot `sh -c`).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_persists_cwd_and_env() {
        let d = tmp_root("agentshell");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000)).await.unwrap();
        assert!(!r.running, "pwd should finish: {:?}", r.output);
        assert_eq!(r.exit_code, Some(0), "output: {:?}", r.output);
        assert!(r.output.contains("agentshell"), "got {:?}", r.output);
        // cd persists to the next call
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "cd sub".into(), Some(15000)).await.unwrap();
        assert!(!r.running);
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000)).await.unwrap();
        assert!(r.output.trim().ends_with("sub"), "cwd did not persist: {:?}", r.output);
        // exported env persists
        agent_shell_exec(&state, root.clone(), root.clone(), "export VICUNA_TEST=hello".into(), Some(15000))
            .await
            .unwrap();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "printf '%s' \"$VICUNA_TEST\"".into(), Some(15000))
            .await
            .unwrap();
        assert!(r.output.contains("hello"), "env did not persist: {:?}", r.output);
        // exit codes are reported (subshell so the session survives)
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "(exit 7)".into(), Some(15000)).await.unwrap();
        assert_eq!(r.exit_code, Some(7), "got {:?}", r.output);
        kill_agent_shells(&state);
    }

    /// A command that outlives the exec wait stays alive: exec_status polls,
    /// exec_kill interrupts, and the shell is reusable afterwards.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_timeout_poll_and_interrupt() {
        let d = tmp_root("agentshell-timeout");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "sleep 5; echo done".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running, "expected still-running, got {:?}", r.output);
        let s = agent_shell_status(&state, root.clone(), root.clone()).await.unwrap();
        assert!(s.running, "still sleeping, got {:?}", s.output);
        let r = agent_shell_interrupt(&state, root.clone(), root.clone()).await.unwrap();
        assert!(!r.running, "interrupt should return to prompt");
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "echo after-interrupt".into(), Some(5000))
            .await
            .unwrap();
        assert!(!r.running);
        assert!(r.output.contains("after-interrupt"), "got {:?}", r.output);
        kill_agent_shells(&state);
    }

    /// Interactive stdin: a command blocked on `read` can be answered with
    /// exec_input, then observed via exec_status.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_accepts_stdin() {
        let d = tmp_root("agentshell-stdin");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "read line; echo \"got:$line\"".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running, "read should still be waiting, got {:?}", r.output);
        let sent = agent_shell_input(&state, root.clone(), root.clone(), "hello-stdin".into()).await.unwrap();
        assert!(sent.contains("got:hello-stdin"), "input result: {sent:?}");
        // the finished command is reflected by the next status poll
        let s = agent_shell_status(&state, root.clone(), root.clone()).await.unwrap();
        assert!(!s.running, "shell should be idle again, got {:?}", s.output);
        kill_agent_shells(&state);
    }

    /// Shell environment must suppress pagers/editors: a pager would own the
    /// PTY and swallow every later command.
    /// Turn-start attribution: pre-existing user edits are surfaced, and a
    /// later turn reports the delta instead of the model guessing.
    #[tokio::test]
    async fn git_note_reports_existing_dirt_and_turn_delta() {
        let d = tmp_root("gitnote");
        let inited = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&d)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !inited {
            return; // git unavailable on this machine
        }
        let root = d.to_string_lossy().to_string();
        std::fs::write(d.join("user-edit.txt"), "x").unwrap();
        let state = UndoState::default();
        let note = workspace_git_note(&state, "k", &root).await.unwrap();
        assert!(note.contains("BEFORE this turn"), "{note}");
        assert!(note.contains("?? user-edit.txt"), "{note}");
        std::fs::write(d.join("agent-edit.txt"), "y").unwrap();
        let note = workspace_git_note(&state, "k", &root).await.unwrap();
        assert!(note.contains("CHANGED since your previous turn"), "{note}");
        assert!(note.contains("+ ?? agent-edit.txt"), "{note}");
        assert!(note.contains("move scratch to the system temp dir"), "{note}");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_sets_noninteractive_env() {
        let d = tmp_root("agentshell-env");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(
            &state,
            root.clone(),
            root.clone(),
            "printf '%s|%s|%s|%s|%s' \"$PAGER\" \"$GIT_PAGER\" \"$LESS\" \"$GIT_TERMINAL_PROMPT\" \"$TMPDIR\"".into(),
            Some(15000),
        )
        .await
        .unwrap();
        let out = r.output.trim();
        assert!(out.starts_with("cat|cat|FRX|0|"), "output: {out:?}");
        let tmp = std::env::temp_dir().to_string_lossy().trim_end_matches('/').to_string();
        assert!(
            out.trim_end_matches('/').ends_with(&tmp),
            "TMPDIR should match the file tools' temp root: {out:?} vs {tmp:?}"
        );
        kill_agent_shells(&state);
    }

    /// Multi-line payloads cannot be typed into an interactive PTY; refuse
    /// them before they corrupt anything and point at `write`.
    #[tokio::test]
    async fn agent_shell_rejects_multiline_and_heredocs() {
        let d = tmp_root("agentshell-multiline");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let err = agent_shell_exec(
            &state,
            root.clone(),
            root.clone(),
            "cat > f.txt <<'EOF'\nhello\nEOF".into(),
            Some(5000),
        )
        .await
        .unwrap_err();
        assert!(err.contains("one command line"), "got {err:?}");
        assert!(err.contains("write tool"), "got {err:?}");
        kill_agent_shells(&state);
    }

    /// The result carries the real cwd and wall-clock duration.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_reports_cwd_and_duration() {
        let d = tmp_root("agentshell-meta");
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        agent_shell_exec(&state, root.clone(), root.clone(), "cd sub".into(), Some(15000))
            .await
            .unwrap();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "pwd".into(), Some(15000))
            .await
            .unwrap();
        let cwd = r.cwd.unwrap_or_default();
        assert!(cwd.ends_with("sub"), "cwd was {cwd:?}");
        assert!(r.duration_ms.is_some());
        kill_agent_shells(&state);
    }

    /// Terminal echo can be re-enabled by a child program; the probe marker
    /// must still parse (previously the echoed command line was mistaken for
    /// the probe output, yielding "exit unknown" + a garbage cwd).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_exit_probe_survives_echo() {
        let d = tmp_root("agentshell-exit-echo");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        agent_shell_exec(&state, root.clone(), root.clone(), "stty echo".into(), Some(15000))
            .await
            .unwrap();
        let ok = agent_shell_exec(&state, root.clone(), root.clone(), "true".into(), Some(15000))
            .await
            .unwrap();
        assert_eq!(ok.exit_code, Some(0), "output={:?} cwd={:?}", ok.output, ok.cwd);
        assert!(
            ok.cwd.as_deref().unwrap_or("").contains("agentshell-exit-echo"),
            "cwd was {:?}",
            ok.cwd
        );
        let bad = agent_shell_exec(&state, root.clone(), root.clone(), "false".into(), Some(15000))
            .await
            .unwrap();
        assert_eq!(bad.exit_code, Some(1), "output={:?} cwd={:?}", bad.output, bad.cwd);
        kill_agent_shells(&state);
    }

    /// A busy shell reports how long it has been stuck (and, when available,
    /// which foreground process owns the TTY).
    #[cfg(not(windows))]
    #[tokio::test]
    async fn agent_shell_busy_error_is_actionable() {
        let d = tmp_root("agentshell-busy");
        let root = d.to_string_lossy().to_string();
        let state = AgentShellState::default();
        let r = agent_shell_exec(&state, root.clone(), root.clone(), "sleep 4".into(), Some(1000))
            .await
            .unwrap();
        assert!(r.running);
        let err = agent_shell_exec(&state, root.clone(), root.clone(), "echo nope".into(), Some(5000))
            .await
            .unwrap_err();
        assert!(err.contains("busy"), "got {err:?}");
        assert!(err.contains("exec_status"), "got {err:?}");
        agent_shell_interrupt(&state, root.clone(), root.clone()).await.unwrap();
        kill_agent_shells(&state);
    }

    /// Ranged reads make large files inspectable instead of blind-write-only.
    #[tokio::test]
    async fn read_offset_limit_pages_long_files() {
        let d = tmp_root("readrange");
        let d_str = d.to_string_lossy().to_string();
        let body: String = (1..=200).map(|i| format!("line {i}\n")).collect();
        std::fs::write(d.join("big.txt"), body).unwrap();
        let r = work_read_file_logic(d_str.clone(), "big.txt".into(), None, Some(50), Some(5))
            .await
            .unwrap();
        assert_eq!(r.content, "line 50\nline 51\nline 52\nline 53\nline 54\n");
        assert!(r.truncated);
        let all = work_read_file_logic(d_str, "big.txt".into(), None, None, None)
            .await
            .unwrap();
        assert!(!all.truncated);
        assert!(all.content.starts_with("line 1\n"));
        assert!(all.content.ends_with("line 200\n"));
    }

    #[test]
    fn join_rejects_escapes() {
        let d = tmp_root("join");
        let canon = work_canonical_root(d.to_str().unwrap()).unwrap();
        assert!(work_join(&canon, "").unwrap() == canon);
        assert!(work_join(&canon, "a/b/../c.txt").unwrap().starts_with(&canon));
        assert!(work_join(&canon, "../../etc/passwd").is_err());
        assert!(work_join(&canon, "/etc/passwd").is_err());
    }

    /// File tools can reach only the system temp dir outside the workspace —
    /// the sanctioned scratch location — and nothing else.
    #[test]
    fn join_allows_temp_but_keeps_the_boundary() {
        let d = tmp_root("join-temp");
        let canon = work_canonical_root(d.to_str().unwrap()).unwrap();
        let scratch = std::env::temp_dir().join("vicuna-join-temp-check.txt");
        let scratch_s = scratch.to_string_lossy().to_string();
        assert!(work_join_allow_temp(&canon, &scratch_s).is_ok(), "{scratch_s}");
        #[cfg(unix)]
        assert!(work_join_allow_temp(&canon, "/tmp/vicuna-join-temp-check.txt").is_ok());
        assert!(work_join(&canon, &scratch_s).is_err());
        assert!(work_join_allow_temp(&canon, "/etc/hosts").is_err());
        assert!(
            work_join_allow_temp(&canon, &format!("{}/../escape", std::env::temp_dir().display()))
                .is_err()
        );
    }

    #[tokio::test]
    async fn scratch_tools_reach_temp_but_not_other_abs_paths() {
        let d = tmp_root("scratch-tools");
        let root = d.to_string_lossy().to_string();
        let scratch = std::env::temp_dir().join(format!(
            "vicuna-scratch-tools-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&scratch).unwrap();
        let spec = scratch.join("spec.json");
        let spec_s = spec.to_string_lossy().to_string();

        work_write_file_logic(root.clone(), spec_s.clone(), "{\"ok\":true}".into())
            .await
            .unwrap();
        let read = work_read_file_logic(root.clone(), spec_s.clone(), None, None, None)
            .await
            .unwrap();
        assert_eq!(read.content, "{\"ok\":true}");
        let listed = work_list_dir_logic(root.clone(), Some(scratch.to_string_lossy().to_string()))
            .await
            .unwrap();
        assert!(listed.iter().any(|e| e.name == "spec.json"));

        assert!(work_write_file_logic(root.clone(), "/etc/hosts".into(), "nope".into())
            .await
            .is_err());
        assert!(work_read_file_logic(root, "/etc/hosts".into(), None, None, None)
            .await
            .is_err());
        std::fs::remove_dir_all(&scratch).ok();
    }

    #[tokio::test]
    async fn fetch_url_rejects_non_http() {
        assert!(work_fetch_url_logic("".into()).await.is_err());
        assert!(work_fetch_url_logic("not a url".into()).await.is_err());
        assert!(work_fetch_url_logic("file:///etc/passwd".into()).await.is_err());
        assert!(work_fetch_url_logic("ftp://example.com/x".into()).await.is_err());
        assert!(work_fetch_url_logic("data:text/plain,hi".into()).await.is_err());
    }

    #[test]
    fn fetch_entity_decode_covers_named_and_numeric() {
        assert_eq!(
            decode_html_entities("a &amp; b &lt;c&gt; &quot;q&quot; &#39;x&#39; &#x27;y&#x27;&nbsp;z"),
            "a & b <c> \"q\" 'x' 'y' z"
        );
        // unknown entities pass through; bare & copies literally
        assert_eq!(decode_html_entities("&foobar; fish & chips"), "&foobar; fish & chips");
    }

    /// KuCoin-style article page: headline in <title>, body in <article>,
    /// Next.js JSON + CSS noise in script/style. The agent must see the
    /// article, not the page chrome or the __NEXT_DATA__ blob.
    #[test]
    fn fetch_html_to_text_keeps_article_drops_chrome() {
        let html = r#"<!doctype html><html><head><title>OpenAI claims to disprove ETH &mdash; KuCoin</title>
<style>.ad{display:none}</style><script>window.__NEXT_DATA__={"props":{"x":1}}</script></head>
<body><nav><a href="/">Home</a><a href="/news">News</a></nav><article><h1>OpenAI claims to disprove the exponential time hypothesis</h1>
<p>ChainCatcher report: OpenAI recently announced that its AI model has <strong>disproven</strong> the hypothesis.</p>
<p>Cost grows exponentially &amp; no efficient algorithm exists &#8212; yet.</p></article>
<!-- a comment --><script>console.log("hi")</script></body></html>"#;
        let text = html_to_text(html);
        assert!(text.contains("OpenAI claims to disprove ETH &mdash; KuCoin"), "title leads, unknown entity kept: {text}");
        assert!(text.contains("OpenAI claims to disprove the exponential time hypothesis"), "{text}");
        assert!(text.contains("ChainCatcher report: OpenAI recently announced that its AI model has disproven the hypothesis."), "{text}");
        assert!(text.contains("Cost grows exponentially & no efficient algorithm exists — yet."), "{text}");
        assert!(!text.contains("__NEXT_DATA__"), "{text}");
        assert!(!text.contains("display:none"), "{text}");
        assert!(!text.contains("console.log"), "{text}");
        assert!(!text.contains("a comment"), "{text}");
        assert!(!text.contains('<'), "{text}");
    }


    #[tokio::test]
    async fn write_read_list_grep_roundtrip() {
        let d = tmp_root("roundtrip");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "hello.py".into(), "print('hi')\n".into())
            .await
            .unwrap();
        let entries = work_list_dir(root.clone(), None).await.unwrap();
        assert!(entries.iter().any(|e| e.name == "hello.py" && !e.is_dir));
        let content = work_read_file(root.clone(), "hello.py".into(), None, None, None)
            .await
            .unwrap();
        assert_eq!(content.content, "print('hi')\n");
        assert!(!content.truncated);
        let hits = work_grep(root.clone(), "print".into(), None).await.unwrap();
        assert!(hits.iter().any(|h| h.path == "hello.py" && h.line == 1));
        // outside-root access rejected at every entry point
        assert!(work_read_file(root.clone(), "../outside.txt".into(), None, None, None)
            .await
            .is_err());
    }

    /// The exact flow the agent runs for "write a script, then run it".
    #[tokio::test]
    async fn write_python_then_run_and_capture() {
        let d = tmp_root("fizzbuzz");
        let root = d.to_string_lossy().to_string();
        let script = r#"for i in range(1, 16):
    s = ("Fizz" if i % 3 == 0 else "") + ("Buzz" if i % 5 == 0 else "")
    print(s or i)
"#;
        work_write_file(root.clone(), "fizzbuzz.py".into(), script.into())
            .await
            .unwrap();
        let res = work_exec(root.clone(), "python3 fizzbuzz.py".into(), Some(30000))
            .await
            .unwrap();
        assert!(!res.timed_out);
        assert_eq!(res.exit_code, Some(0));
        let lines: Vec<&str> = res.stdout.lines().collect();
        assert_eq!(lines.len(), 15);
        assert_eq!(lines[2], "Fizz");
        assert_eq!(lines[4], "Buzz");
        assert_eq!(lines[14], "FizzBuzz");
    }

    #[tokio::test]
    async fn exec_echo_and_timeout() {
        let d = tmp_root("exec");
        let root = d.to_string_lossy().to_string();
        let res = work_exec(root.clone(), "echo hello-work".into(), None)
            .await
            .unwrap();
        assert_eq!(res.stdout.trim(), "hello-work");
        assert_eq!(res.exit_code, Some(0));
        let slow = work_exec(root, "sleep 5".into(), Some(1200)).await.unwrap();
        assert!(slow.timed_out);
    }

    #[test]
    fn full_disk_scan_matcher_cases() {
        // refused: recursive walkers rooted at storm-scale roots, including
        // the exact escalation that stormed TCC (`…; find / -name …`)
        for cmd in [
            "find / -name gh",
            "find / -name 'gh*' 2>/dev/null",
            "find /",
            "find -H / -name gh",
            "command -v gh; echo done; find / -name gh",
            "ls /opt/homebrew/bin && find / -name gh",
            "echo $(find / -name gh)",
            "find ~ -name '*.pem'",
            "find ~/ -name x",
            "find ~other -name x",
            "find $HOME -name x",
            "find ${HOME} -name x",
            "find \"/\" -name x",
            "find /Users -name x",
            "find /Users/waiyip -name x",
            "find /System/Volumes/Data -name x",
            "grep -r secret /",
            "grep -R --regexp=foo /",
            "rg --files /",
            "fd passwd /",
            "ls -R /",
            "du -sh /",
            "tree /",
            "sudo find / -name x",
            "FOO=1 find / -name x",
            "{ find / -name x; }",
        ] {
            assert!(
                refuse_full_disk_scan(cmd).is_some(),
                "should refuse: {cmd}"
            );
        }
        // $HOME spelled out is still a whole-home scan; $HOME/sub is bounded
        if let Ok(home) = std::env::var("HOME") {
            assert!(refuse_full_disk_scan(&format!("find {home} -name x")).is_some());
            assert!(refuse_full_disk_scan(&format!("find {home}/proj -name x")).is_none());
        }
        // allowed: narrow scans + everyday commands (incl. the good probes
        // from the same turn that produced the `find /` escalation)
        for cmd in [
            "command -v gh",
            "which gh && gh --version",
            "ls /opt/homebrew/bin/ | grep -i gh",
            "ls /usr/local/bin/ | grep -i gh",
            "/opt/homebrew/bin/gh --version",
            "find . -name '*.rs'",
            "find src -name x",
            "find -name x",
            "find /opt/homebrew -name gh",
            "find /tmp/work -name x",
            "find ~/proj -name x",
            "find $HOME/proj -name x",
            "find /Users/waiyip/Documents -name x",
            "grep -r needle src",
            "grep -e / needle.txt",
            "rg needle /tmp/thing",
            "fd clone src",
            "ls -R src",
            "ls -r /",
            "ls /",
            "du -sh /opt/homebrew",
            "echo /",
            "echo don't find / trouble",
            "python3 fizzbuzz.py",
        ] {
            assert!(
                refuse_full_disk_scan(cmd).is_none(),
                "should allow: {cmd}"
            );
        }
    }

    /// Both exec entry points refuse a full-disk scan with a coaching error
    /// instead of running it into the timeout + a TCC prompt storm.
    #[tokio::test]
    async fn exec_refuses_full_disk_scan() {
        let d = tmp_root("findguard");
        let root = d.to_string_lossy().to_string();
        let err = work_exec(root.clone(), "command -v gh; find / -name gh".into(), None)
            .await
            .unwrap_err();
        assert!(err.contains("whole disk"), "coaching error, got: {err}");
        assert!(err.contains("command -v"), "coaching error, got: {err}");
        // background start refuses the same way (no storm via detached jobs)
        let state = BgExecState::default();
        let err = bg_start(&state, root, "find / -name gh".into())
            .await
            .unwrap_err();
        assert!(err.contains("whole disk"), "coaching error, got: {err}");
        assert!(bg_list(&state).unwrap().is_empty());
    }

    #[tokio::test]
    async fn read_image_roundtrip_and_guardrails() {
        let d = tmp_root("readimage");
        let root = d.to_string_lossy().to_string();
        // 1x1 transparent PNG
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
        {
            use base64::Engine as _;
            let bytes = base64::engine::general_purpose::STANDARD.decode(png_b64).unwrap();
            std::fs::write(d.join("tiny.png"), &bytes).unwrap();
        }
        std::fs::write(d.join("note.txt"), "hello").unwrap();
        let img = work_read_image(root.clone(), "tiny.png".into()).await.unwrap();
        assert_eq!(img.mime, "image/png");
        assert_eq!(img.base64, png_b64);
        // non-image extension rejected; escapes rejected
        assert!(work_read_image(root.clone(), "note.txt".into()).await.is_err());
        assert!(work_read_image(root.clone(), "../outside.png".into()).await.is_err());
        // text reader now points binaries at view_image
        std::fs::write(d.join("raw.bin"), [0u8, 1, 2, 3]).unwrap();
        let err = work_read_file(root.clone(), "raw.bin".into(), None, None, None).await.unwrap_err();
        assert!(err.contains("view_image"));
    }

    #[test]
    fn show_context_length_picks_arch_window() {
        let info = serde_json::json!({
            "gptoss.context_length": 131072,
            "gptoss.rope.scaling.original_context_length": 4096,
            "general.architecture": "gptoss"
        });
        assert_eq!(parse_show_context_length(&info), Some(131072));
        let empty = serde_json::json!({"general.architecture": "unknown"});
        assert_eq!(parse_show_context_length(&empty), None);
    }

    #[tokio::test]
    async fn bg_exec_start_poll_kill_roundtrip() {
        let d = tmp_root("bgexec");
        let root = d.to_string_lossy().to_string();
        let state = BgExecState::default();
        // fast job: completes with captured output
        let id = bg_start(&state, root.clone(), "echo hello-bg".into())
            .await
            .unwrap();
        assert!(id.starts_with("bg-"));
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let done = loop {
            let s = bg_snapshot(&state, &id).unwrap();
            assert_eq!(s.command, "echo hello-bg");
            if !s.running {
                break s;
            }
            assert!(std::time::Instant::now() < deadline, "bg echo never finished");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(done.exit_code, Some(0));
        assert!(done.stdout.contains("hello-bg"));
        // list sees it as finished
        let list = bg_list(&state).unwrap();
        assert!(list.iter().any(|j| j.id == id && !j.running));
        // slow job: starts running, then a kill settles it
        let slow = bg_start(&state, root.clone(), "sleep 30".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(bg_snapshot(&state, &slow).unwrap().running);
        bg_kill(&state, &slow).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            let s = bg_snapshot(&state, &slow).unwrap();
            if !s.running {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "killed bg job never settled");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // validation + unknown ids rejected
        assert!(bg_start(&state, root.clone(), "   ".into()).await.is_err());
        assert!(bg_snapshot(&state, "bg-999999").is_err());
        assert!(bg_kill(&state, "bg-999999").is_err());
    }

    #[test]
    fn cli_scripts_carry_marker_and_scheme() {
        for script in [CLI_UNIX_SCRIPT, CLI_WINDOWS_SCRIPT] {
            assert!(script.contains(CLI_MARKER), "installer must be recognizable");
            assert!(script.contains("vicuna://open?path="), "must open the app via deep link");
        }
        // unix script defaults to the caller's directory when no arg is given
        assert!(CLI_UNIX_SCRIPT.contains("${1:-$PWD}"));
        assert!(CLI_WINDOWS_SCRIPT.contains("%CD%"));
    }

    #[test]
    fn cli_write_scan_uninstall_roundtrip() {
        let d = tmp_root("cli");
        let dest = cli_write_launcher(&d).unwrap();
        assert_eq!(dest.file_name().unwrap().to_str().unwrap(), cli_exe_name());
        assert!(cli_is_managed(&dest));
        // a foreign binary with the same name is never claimed or removed
        let other = d.join("shadow");
        std::fs::create_dir_all(&other).unwrap();
        let foreign = other.join(cli_exe_name());
        std::fs::write(&foreign, "echo not-vicuna\n").unwrap();
        assert!(!cli_is_managed(&foreign));
        std::fs::remove_file(&dest).unwrap();
        assert!(!dest.exists());
    }

    #[test]
    fn cli_scan_finds_path_entries_in_order() {
        let a = tmp_root("cli-scan-a");
        let b = tmp_root("cli-scan-b");
        std::fs::write(a.join(cli_exe_name()), CLI_MARKER).unwrap();
        std::fs::write(b.join(cli_exe_name()), CLI_MARKER).unwrap();
        let dirs = vec![a.clone(), b.clone()];
        assert_eq!(cli_scan_in(&dirs, cli_exe_name()).unwrap(), a.join(cli_exe_name()));
        assert!(cli_scan_in(&dirs, "no-such-launcher").is_none());
    }

    #[cfg(not(windows))]
    #[test]
    fn work_path_dirs_prioritize_login_path_and_keep_process_extras() {
        let login_bin = std::path::PathBuf::from("/login/bin");
        let shared = std::path::PathBuf::from("/shared/bin");
        let app_only = std::path::PathBuf::from("/app/bin");
        let dirs = work_path_dirs(
            &[login_bin.clone(), shared.clone()],
            &[shared.clone(), app_only.clone()],
        );
        assert_eq!(dirs, vec![login_bin, shared, app_only]);
    }

    #[test]
    fn cli_all_path_dirs_merges_without_dupes() {
        let a = std::path::PathBuf::from("/usr/bin");
        let b = std::path::PathBuf::from("/tmp/fake-home/.local/bin");
        let merged = cli_all_path_dirs(&[a.clone(), b.clone()]);
        // process PATH always contributes; shell extras append once each
        assert!(merged.contains(&b));
        assert_eq!(merged.iter().filter(|d| *d == &a).count(), 1);
        assert_eq!(merged.iter().filter(|d| *d == &b).count(), 1);
    }

    /// The Finder-launch case: the app's own PATH is sparse system dirs, the
    /// launcher lives in a home bin dir only the login shell sees — status
    /// must still find it (no HOME/PATH env mutation: all explicit params).
    #[tokio::test]
    async fn cli_status_finds_launcher_outside_process_path() {
        let home = tmp_root("cli-home");
        let bindir = home.join(".local").join("bin");
        let dest = cli_write_launcher(&bindir).unwrap();
        // effective path as a Finder-launched app would see it: nothing of
        // its own, only the login-shell dir (a bare slice — no process PATH,
        // so the real environment can't leak into this test)
        let all_dirs = vec![bindir.clone()];
        let st = cli_status_inner(&all_dirs, &home).unwrap();
        assert_eq!(st.found, Some(dest.to_string_lossy().to_string()));
        assert!(st.managed);
        assert!(st.on_path);
        assert_eq!(st.install_dir, bindir.to_string_lossy().to_string());
        // uninstall reaches it through the same effective path
        let removed = cli_uninstall_inner(&all_dirs, &home).unwrap();
        assert_eq!(removed, dest.to_string_lossy().to_string());
        assert!(!dest.exists());
    }

    /// Login-shell probe is read-only and must see at least the system dirs.
    #[cfg(not(windows))]
    #[tokio::test]
    async fn cli_shell_path_probe_sees_system_dirs() {
        let dirs = cli_shell_path_dirs().await;
        assert!(
            dirs.iter().any(|d| d == std::path::Path::new("/usr/bin") || d == std::path::Path::new("/bin")),
            "login shell PATH should contain system dirs: {dirs:?}"
        );
    }

    #[tokio::test]
    async fn tree_lists_nested_skips_build_dirs_and_import_roundtrips() {
        let d = tmp_root("tree");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "src/main.py".into(), "print(1)\n".into())
            .await
            .unwrap();
        work_write_file(root.clone(), ".hidden-note".into(), "hi\n".into())
            .await
            .unwrap();
        work_write_file(root.clone(), "node_modules/dep/index.js".into(), "x\n".into())
            .await
            .unwrap();
        let tree = work_tree(root.clone(), None).await.unwrap();
        assert!(tree.iter().any(|e| e.path == "src/main.py" && !e.is_dir && e.depth == 1));
        assert!(tree.iter().any(|e| e.path == ".hidden-note"));
        assert!(!tree.iter().any(|e| e.path.starts_with("node_modules")));
        let mut sorted = tree.clone();
        sorted.sort_by(|a, b| a.path.to_lowercase().cmp(&b.path.to_lowercase()));
        assert_eq!(
            tree.iter().map(|e| &e.path).collect::<Vec<_>>(),
            sorted.iter().map(|e| &e.path).collect::<Vec<_>>()
        );
        // import: binary bytes survive base64, escapes rejected
        let raw = vec![0u8, 1, 2, 255, 65, 66];
        let b64 = {
            const ALPH: &[u8; 64] =
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            let mut s = String::new();
            for c in raw.chunks(3) {
                let n = (c[0] as u32) << 16
                    | (*c.get(1).unwrap_or(&0) as u32) << 8
                    | (*c.get(2).unwrap_or(&0) as u32);
                s.push(ALPH[((n >> 18) & 63) as usize] as char);
                s.push(ALPH[((n >> 12) & 63) as usize] as char);
                s.push(if c.len() > 1 {
                    ALPH[((n >> 6) & 63) as usize] as char
                } else {
                    '='
                });
                s.push(if c.len() > 2 {
                    ALPH[(n & 63) as usize] as char
                } else {
                    '='
                });
            }
            s
        };
        work_import_file(root.clone(), "assets/blob.bin".into(), b64)
            .await
            .unwrap();
        assert_eq!(std::fs::read(d.join("assets/blob.bin")).unwrap(), raw);
        assert!(work_import_file(root.clone(), "../evil.txt".into(), "aGk=".into())
            .await
            .is_err());
        assert!(decode_base64("!!!").is_err());
    }

    #[tokio::test]
    async fn upload_stage_and_read_roundtrip() {
        let sid = format!("test-session-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
        let _ = std::fs::remove_dir_all(upload_session_dir(&sid).unwrap_or_default());
        // stage
        let abs = upload_stage_file(sid.clone(), "report.txt".into(), "aGVsbG8tdXBsb2Fk".into())
            .await
            .unwrap();
        assert!(abs.contains(&sid));
        assert!(abs.ends_with("report.txt"));
        // read back through the session-scoped reader
        let got = upload_read_file(sid.clone(), "report.txt".into()).await.unwrap();
        assert_eq!(got.content, "hello-upload");
        assert!(!got.truncated);
        // traversal + bad ids rejected (never outside ~/vicuna/uploads/<id>/)
        // traversal names flatten to their final component *inside* the
        // session dir (never outside it); bare "..", "/", "" rejected
        let p = upload_stage_file(sid.clone(), "../../evil.txt".into(), "aGk=".into())
            .await
            .unwrap();
        assert!(p.contains(&sid) && p.ends_with("evil.txt"));
        assert!(upload_read_file(sid.clone(), "evil.txt".into())
            .await
            .is_ok());
        for bad in ["..", "/", ""] {
            assert!(
                upload_stage_file(sid.clone(), bad.into(), "aGk=".into())
                    .await
                    .is_err(),
                "must reject {bad:?}"
            );
        }
        // subdirectories flatten to the bare file name, still contained
        let q = upload_stage_file(sid.clone(), "a/b.txt".into(), "aGk=".into())
            .await
            .unwrap();
        assert!(q.contains(&sid) && q.ends_with("b.txt"));
        assert!(upload_read_file("../other".into(), "report.txt".into())
            .await
            .is_err());
        assert!(upload_read_file(sid.clone(), "missing.txt".into())
            .await
            .is_err());
        // cleanup the temp session dir
        let _ = std::fs::remove_dir_all(upload_session_dir(&sid).unwrap());
    }

    #[test]
    fn open_installer_guard_rejects_outside_paths() {
        // the bridge exposes open_installer to plain browsers: it must never
        // become an arbitrary-file opener
        assert!(open_installer("/etc/hosts").is_err());
        assert!(open_installer("/").is_err());
        assert!(open_installer("").is_err());
        assert!(open_installer("~/Downloads/definitely-missing.dmg").is_err());
        // only installer extensions are allowed, even inside Downloads
        let probe = downloads_dir().unwrap().join("vicuna-guard-test.txt");
        let _ = std::fs::write(&probe, b"x");
        assert!(open_installer(&probe.to_string_lossy()).is_err());
        let _ = std::fs::remove_file(&probe);
    }

    #[test]
    fn release_info_parsing_skips_broken_assets() {        // canned /releases/tags/latest payload (offline test)
        let v = serde_json::json!({
            "tag_name": "latest",
            "published_at": "2026-10-28T18:29:59Z",
            "updated_at": "2026-10-31T15:53:28Z",
            "html_url": "https://github.com/anslwy/vicuna/releases/tag/latest",
            "assets": [
                {
                    "name": "Vicuna_0.1.0_universal.dmg",
                    "browser_download_url": "https://github.com/anslwy/vicuna/releases/download/latest/Vicuna_0.1.0_universal.dmg",
                    "size": 123,
                    "created_at": "2026-10-31T15:53:27Z",
                    "updated_at": "2026-10-31T15:53:28Z"
                },
                {"name": "broken-no-url"},
            ]
        });
        let info = parse_release_info(&v).unwrap();
        assert_eq!(info["tag"], "latest");
        assert_eq!(info["publishedAt"], "2026-10-28T18:29:59Z");
        assert_eq!(info["updatedAt"], "2026-10-31T15:53:28Z");
        let assets = info["assets"].as_array().unwrap();
        assert_eq!(assets.len(), 1);
        // asset timestamps survive — the frontend compares against these,
        // never the frozen release published_at
        assert_eq!(assets[0]["uploadedAt"], "2026-10-31T15:53:28Z");
        // created_at is the fallback when updated_at is missing
        let fallback = parse_release_info(&serde_json::json!({
            "tag_name": "latest",
            "published_at": "2026-10-28T18:29:59Z",
            "assets": [{
                "name": "a.dmg",
                "browser_download_url": "https://example.com/a.dmg",
                "created_at": "2026-10-30T00:00:00Z"
            }]
        }))
        .unwrap();
        assert_eq!(fallback["assets"][0]["uploadedAt"], "2026-10-30T00:00:00Z");
        assert!(parse_release_info(&serde_json::json!({"tag_name": "x"})).is_err());
    }

    #[tokio::test]
    async fn rename_delete_roundtrip_and_guardrails() {
        let d = tmp_root("renamedel");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "sub/old.txt".into(), "x".into())
            .await
            .unwrap();
        let p = work_rename(root.clone(), "sub/old.txt".into(), "new.txt".into())
            .await
            .unwrap();
        assert!(p.ends_with("new.txt"));
        assert!(work_read_file(root.clone(), "sub/new.txt".into(), None, None, None)
            .await
            .is_ok());
        assert!(work_read_file(root.clone(), "sub/old.txt".into(), None, None, None)
            .await
            .is_err());
        // rejects escapes, missing sources, slashed/off-root names, clashes
        assert!(work_rename(root.clone(), "../evil.txt".into(), "x".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "missing.txt".into(), "x.txt".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "sub/new.txt".into(), "a/b.txt".into())
            .await
            .is_err());
        assert!(work_rename(root.clone(), "sub/new.txt".into(), "sub/new.txt".into())
            .await
            .is_err());
        work_write_file(root.clone(), "dir/f.txt".into(), "y".into())
            .await
            .unwrap();
        work_delete(root.clone(), "dir".into()).await.unwrap();
        assert!(work_read_file(root.clone(), "dir/f.txt".into(), None, None, None)
            .await
            .is_err());
        work_write_file(root.clone(), "gone.txt".into(), "y".into())
            .await
            .unwrap();
        work_delete(root.clone(), "gone.txt".into()).await.unwrap();
        assert!(work_delete(root.clone(), ".".into()).await.is_err());
        assert!(work_delete(root.clone(), "../outside".into()).await.is_err());
        assert!(work_delete(root.clone(), "missing.txt".into()).await.is_err());
    }

    #[tokio::test]
    async fn undo_restores_modified_and_removes_created() {
        let d = tmp_root("undo");
        let root = d.to_string_lossy().to_string();
        std::fs::write(d.join("keep.txt"), b"before").unwrap();
        let state = UndoState::default();
        let key = "sess-undo-test";
        // turn 1: modify keep.txt + create new.txt
        undo_begin_turn(&state, key, &root);
        undo_record(&state, key, UndoOp::Modified { path: "keep.txt".into(), prev: b"before".to_vec(), next: b"after".to_vec() });
        work_write_file(root.clone(), "keep.txt".into(), "after".into()).await.unwrap();
        undo_record(&state, key, UndoOp::Created { path: "new.txt".into(), content: b"new".to_vec() });
        work_write_file(root.clone(), "new.txt".into(), "new".into()).await.unwrap();
        // turn 2 (empty): only an exec note
        undo_begin_turn(&state, key, &root);
        undo_record(&state, key, UndoOp::ExecNote { command: "echo hi".into() });
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("no file writes"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "after");
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file") && msg.contains("removed 1 created file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "before");
        assert!(!d.join("new.txt").exists());
        // /redo puts the turn's bytes back, then /undo can revert it again
        let msg = work_redo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file") && msg.contains("recreated 1 file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "after");
        assert_eq!(std::fs::read_to_string(d.join("new.txt")).unwrap(), "new");
        let msg = work_undo_logic(&state, key.to_string()).await.unwrap();
        assert!(msg.contains("restored 1 file"), "got {msg:?}");
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "before");
        // a fresh turn drops the redo history
        let msg = work_redo_logic(&state, "other-session".to_string()).await.unwrap_err();
        assert!(msg.contains("Nothing to redo"), "got {msg:?}");
        undo_begin_turn(&state, key, &root);
        assert!(work_redo_logic(&state, key.to_string()).await.is_err());
        assert_eq!(work_undo_logic(&state, key.to_string()).await.unwrap(), "Undid last turn: no file changes to revert.");
        assert!(work_undo_logic(&state, key.to_string()).await.is_err());
    }

    /// Real pty roundtrip: spawn a login shell, run echo, read it back,
    /// then kill and observe the exit event.
    #[test]
    fn pty_echo_roundtrip_and_kill() {
        let d = tmp_root("pty");
        let root = d.to_string_lossy().to_string();
        let out = std::sync::Arc::new(Mutex::new(String::new()));
        let out2 = out.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Option<i32>>();
        let h = spawn_pty(
            &root,
            Some(80),
            Some(24),
            move |t| {
                out2.lock().unwrap().push_str(&t);
            },
            move |c| {
                let _ = tx.send(c);
            },
        )
        .unwrap();
        // login shells need a moment; then a marker roundtrips through echo
        std::thread::sleep(Duration::from_secs(3));
        {
            let mut w = h.writer.lock().unwrap();
            w.write_all(b"echo pty-marker-84135\n").unwrap();
            w.flush().unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if out.lock().unwrap().contains("pty-marker-84135") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no echo from pty shell");
            std::thread::sleep(Duration::from_millis(200));
        }
        // kill by pid (same path term_kill uses) and expect the exit event
        kill_pid(h.pid.expect("shell has a pid"));
        let code = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("no exit event after kill");
        // SIGKILLed shells report no status code on unix
        #[cfg(not(windows))]
        assert_eq!(code, None);
        #[cfg(windows)]
        let _ = code;
    }
}
