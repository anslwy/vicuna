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
        "think": think.unwrap_or(true),
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
/// cannot be abused as a generic proxy.
#[tauri::command]
async fn ollama_fetch_text(url: String) -> Result<String, String> {
    const ALLOWED: [&str; 2] = [
        "https://ollama.com/library",
        "https://registry.ollama.ai/v2/library/",
    ];
    if !ALLOWED.iter().any(|p| url.starts_with(p)) {
        return Err("refusing to fetch non-registry URL".to_string());
    }
    let res = reqwest::Client::new()
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
struct WorkDirEntry {
    name: String,
    path: String,
    is_dir: bool,
    size: Option<u64>,
}

/// List a workspace-relative directory (default: root). Deny hidden dirs
/// are still listed — the model decides what matters.
#[tauri::command]
async fn work_list_dir(root: String, path: Option<String>) -> Result<Vec<WorkDirEntry>, String> {
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
            if is_dir && SKIP.contains(&name.as_str()) {
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
struct WorkFileContent {
    path: String,
    content: String,
    truncated: bool,
    size: u64,
}

/// Read a workspace-relative text file (cap ~64KB; binary smells rejected).
#[tauri::command]
async fn work_read_file(root: String, path: String, max_bytes: Option<u64>) -> Result<WorkFileContent, String> {
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
        return Err("binary file — refusing to dump as text".to_string());
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
struct WorkGrepHit {
    path: String,
    line: usize,
    text: String,
}

/// Substring search (case-insensitive) across workspace text files.
/// Caps: 200 files scanned, 100 hits, 512KB per file, skips hidden dirs.
#[tauri::command]
async fn work_grep(root: String, pattern: String, include: Option<String>) -> Result<Vec<WorkGrepHit>, String> {
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
                if matches!(name.as_str(), "node_modules" | "target" | "build" | ".git" | "dist") {
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
#[tauri::command]
async fn work_write_file(root: String, path: String, content: String) -> Result<String, String> {
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
struct WorkExecResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
    truncated: bool,
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
#[tauri::command]
async fn work_exec(root: String, command: String, timeout_ms: Option<u64>) -> Result<WorkExecResult, String> {
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
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(term_size(cols, rows))
        .map_err(|e| format!("cannot open pty: {e}"))?;
    let mut cmd = {
        #[cfg(windows)]
        {
            let comspec =
                std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
            portable_pty::CommandBuilder::new(comspec)
        }
        #[cfg(not(windows))]
        {
            let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
            let mut c = portable_pty::CommandBuilder::new(shell);
            c.args(["-l"]);
            c
        }
    };
    cmd.cwd(&canon);
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(OllamaServerState::default())
        .manage(TermState::default())
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
            ollama_fetch_text,
            ollama_server_version,
            ollama_start_server,
            ollama_stop_server,
            work_validate_root,
            work_list_dir,
            work_tree,
            work_import_file,
            work_read_file,
            work_grep,
            work_write_file,
            work_exec,
            term_spawn,
            term_write,
            term_resize,
            term_kill,
            zoom_apply,
            zoom_step
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
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
