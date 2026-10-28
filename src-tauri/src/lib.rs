// Vicuna backend: all Ollama inference traffic goes through here.
// The frontend (JS) is display-only: it invokes these commands and renders
// the streamed events. Direct `fetch("http://localhost:11434/...")` from JS
// is only a web-dev fallback (see src/lib/ollama.ts).

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::State;

const OLLAMA_BASE: &str = "http://localhost:11434";

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[derive(Debug, Deserialize, Serialize)]
struct ChatMessage {
    role: String,
    content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    images: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ChatEvent {
    Token { text: String },
    Think { text: String },
    Done { done_reason: Option<String> },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum GenerateEvent {
    Token { text: String },
    Done { done_reason: Option<String> },
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

/// Load/unload a model by generating one throwaway token with the given
/// keep-alive. Same 32k ctx as chat so follow-ups don't trigger a reload.
#[tauri::command]
async fn ollama_warm_model(model: String, keep_alive: String) -> Result<(), String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{OLLAMA_BASE}/api/generate"))
        .json(&serde_json::json!({
            "model": model,
            "prompt": "hi",
            "stream": false,
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
    think: Option<bool>,
    temperature: Option<f64>,
    stop: Option<Vec<String>>,
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
    let res = client
        .post(format!("{OLLAMA_BASE}/api/chat"))
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
            "think": think.unwrap_or(true),
            "options": options,
        }))
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
    let _ = on_event.send(ChatEvent::Done { done_reason });
    Ok(full)
}

/// Raw text completion (no chat template). Kept for parity with the old JS
/// helper; Continue does NOT use this (see AGENTS.md).
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
            if let Some(t) = obj.get("response").and_then(|v| v.as_str()) {
                if !t.is_empty() {
                    full.push_str(t);
                    let _ = on_event.send(GenerateEvent::Token { text: t.to_string() });
                }
            }
        }
    }
    let _ = on_event.send(GenerateEvent::Done { done_reason });
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
        match Command::new(bin)
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .manage(OllamaServerState::default())
        .invoke_handler(tauri::generate_handler![
            greet,
            ollama_list_models,
            ollama_list_running,
            ollama_warm_model,
            ollama_chat_stream,
            ollama_generate_stream,
            ollama_pull_model,
            ollama_delete_model,
            ollama_server_version,
            ollama_start_server,
            ollama_stop_server
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
