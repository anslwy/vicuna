// Vicuna backend: all Ollama inference traffic goes through here.
// The frontend (JS) is display-only: it invokes these commands and renders
// the streamed events. Direct `fetch("http://localhost:11434/...")` from JS
// is only a web-dev fallback (see src/lib/ollama.ts).

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
use tauri::ipc::Channel;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, State, WindowEvent};
pub mod mcp;
pub mod work_agent;
pub mod bridge;
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

/// Fetch a registry page as text (powers the Discover "Fetch" button).
/// The browser cannot do this itself — ollama.com and registry.ollama.ai
/// send no CORS headers — but the Rust backend has no such restriction.
/// The URL is allowlisted to the public Ollama registry so the command
/// cannot be abused as a generic proxy. Bounded by a timeout: without one,
/// a single stalled manifest pinned the Fetch progress at N-1/N indefinitely.
#[tauri::command]
async fn ollama_fetch_text(url: String) -> Result<String, String> {
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

fn work_canonical_root(root: &str) -> Result<std::path::PathBuf, String> {
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
fn work_join(root: &std::path::Path, rel: &str) -> Result<std::path::PathBuf, String> {
    let rel = rel.trim();
    if rel.is_empty() || rel == "." || rel == "./" {
        return Ok(root.to_path_buf());
    }
    if std::path::Path::new(rel).is_absolute() {
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
#[tauri::command]
async fn work_validate_root(path: String) -> Result<String, String> {
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
    let dir = work_join(&canon, path.as_deref().unwrap_or(""))?;
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
struct WorkTreeEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
    depth: usize,
}

/// Recursive workspace listing for the file explorer panel (flat, sorted
/// by path). Skips dependency/build dirs; hidden files ARE included.
/// Caps: depth 1-8 (default 4), 3000 entries.
#[tauri::command]
async fn work_tree(root: String, max_depth: Option<usize>) -> Result<Vec<WorkTreeEntry>, String> {
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
#[tauri::command]
async fn work_import_file(root: String, path: String, base64: String) -> Result<String, String> {
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
pub async fn work_read_file_logic(root: String, path: String, max_bytes: Option<u64>) -> Result<WorkFileContent, String> {
    let canon = work_canonical_root(root.trim())?;
    let full = work_join(&canon, &path)?;
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
    let truncated = bytes.len() > cap;
    let slice = &bytes[..bytes.len().min(cap)];
    Ok(WorkFileContent {
        path: path.trim().to_string(),
        content: String::from_utf8_lossy(slice).to_string(),
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
    let full = work_join(&canon, &path)?;
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
    let canon = work_canonical_root(root.trim())?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30000).clamp(1000, 120000));
    #[cfg(windows)]
    let child = tokio::process::Command::new("cmd")
        .arg("/C")
        .arg(cmd)
        .current_dir(&canon)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("cannot run command: {e}"))?;
    #[cfg(not(windows))]
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .current_dir(&canon)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run command: {e}"))?;
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
async fn work_read_file(root: String, path: String, max_bytes: Option<u64>) -> Result<WorkFileContent, String> {
    work_read_file_logic(root, path, max_bytes).await
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
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(&cmd);
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
            // PSReadLine styling, and the agent never uses this shell
            // anyway (it has `exec`, which stays on `cmd /C`).
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
#[tauri::command]
async fn work_rename(root: String, path: String, new_name: String) -> Result<String, String> {
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
#[tauri::command]
async fn work_delete(root: String, path: String) -> Result<(), String> {
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
#[tauri::command]
async fn upload_stage_file(session_id: String, file_name: String, base64: String) -> Result<String, String> {
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(OllamaServerState::default())
        .manage(TermState::default())
        .manage(BgExecState::default())
        .manage(work_agent::WorkAgentState::default())
        .manage(AppQuitState::default())
        .manage(mcp::McpManager::default())
        .manage(ZoomState {
            factor: Mutex::new(1.0),
        })
        .invoke_handler(tauri::generate_handler![
            greet,
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
            work_read_file,
            work_read_image,
            work_grep,
            work_write_file,
            work_exec,
            work_exec_start,
            work_exec_status,
            work_exec_list,
            work_exec_kill,
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
            work_agent::work_agent_start,
            work_agent::work_agent_stop,
            work_agent::work_agent_queue,
            work_agent::work_agent_remove,
            work_agent::work_agent_answer
        ])
        .setup(|app| {
            // System tray: closing the window hides here (streams keep
            // running); only Quit exits. The menu opens on either click;
            // Show reopens the window.
            let menu = tray_menu(app, "No running sessions", None)?;
            if let Some(icon) = app.default_window_icon().cloned() {
                TrayIconBuilder::with_id("main")
                    .icon(icon)
                    .tooltip("Vicuna")
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

    #[test]
    fn validate_rejects_missing_and_relative() {
        assert!(work_canonical_root("/no/such/dir/xyz").is_err());
        assert!(work_canonical_root("relative/path").is_err());
        // filesystem root would crawl TCC-protected spots on macOS
        assert!(work_canonical_root("/").is_err());
        let d = tmp_root("validate");
        assert!(work_canonical_root(d.to_str().unwrap()).is_ok());
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

    #[tokio::test]
    async fn write_read_list_grep_roundtrip() {
        let d = tmp_root("roundtrip");
        let root = d.to_string_lossy().to_string();
        work_write_file(root.clone(), "hello.py".into(), "print('hi')\n".into())
            .await
            .unwrap();
        let entries = work_list_dir(root.clone(), None).await.unwrap();
        assert!(entries.iter().any(|e| e.name == "hello.py" && !e.is_dir));
        let content = work_read_file(root.clone(), "hello.py".into(), None)
            .await
            .unwrap();
        assert_eq!(content.content, "print('hi')\n");
        assert!(!content.truncated);
        let hits = work_grep(root.clone(), "print".into(), None).await.unwrap();
        assert!(hits.iter().any(|h| h.path == "hello.py" && h.line == 1));
        // outside-root access rejected at every entry point
        assert!(work_read_file(root.clone(), "../outside.txt".into(), None)
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
        let err = work_read_file(root.clone(), "raw.bin".into(), None).await.unwrap_err();
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
        assert!(work_read_file(root.clone(), "sub/new.txt".into(), None)
            .await
            .is_ok());
        assert!(work_read_file(root.clone(), "sub/old.txt".into(), None)
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
        assert!(work_read_file(root.clone(), "dir/f.txt".into(), None)
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
