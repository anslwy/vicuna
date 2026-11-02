// Vicuna desktop shell (Tauri).
//
// The agent loop, tools, MCP, browser management and all Ollama logic live in
// the `vicuna-core` crate (which has no Tauri dependency). What remains here
// is the desktop surface: `#[tauri::command]` wrappers, the tray/window/quit
// glue, webview zoom, and the embedded terminal.
//
// Re-exporting the core items keeps the command bodies unchanged — they still
// call `work_list_dir_logic`, `Channel`, etc. by their old names.

use futures_util::StreamExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::Duration;
use tauri::ipc::Channel;
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager, Runtime, State, WindowEvent};
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

pub use vicuna_core::browser;
pub use vicuna_core::mcp;
pub use vicuna_core::*;

pub mod work_agent;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn ollama_list_models() -> Result<serde_json::Value, String> {
    let res = reqwest::get(format!("{}/api/tags", ollama_base()))
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
    let res = reqwest::get(format!("{}/api/ps", ollama_base()))
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

#[tauri::command]
async fn ollama_model_show(model: String) -> Result<ModelShowInfo, String> {
    let client = reqwest::Client::new();
    let res = client
        .post(format!("{}/api/show", ollama_base()))
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
        .post(format!("{}/api/generate", ollama_base()))
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
        .post(format!("{}/api/chat", ollama_base()))
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
        .post(format!("{}/api/generate", ollama_base()))
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
        .post(format!("{}/api/pull", ollama_base()))
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
        .delete(format!("{}/api/delete", ollama_base()))
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

/// ---------- In-app update wrappers (same _logic pattern) ----------
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
    state.insert(id.clone(), handle)?;
    Ok(id)
}

/// Send keystrokes to a terminal.
#[tauri::command]
async fn term_write(id: String, data: String, state: State<'_, TermState>) -> Result<(), String> {
    state.write(&id, &data)
}

/// Resize a terminal (columns × rows from the xterm fit addon).
#[tauri::command]
async fn term_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TermState>,
) -> Result<(), String> {
    state.resize(&id, cols, rows)
}

/// Kill a terminal's shell (by pid, best-effort) and drop the entry.
#[tauri::command]
async fn term_kill(id: String, state: State<'_, TermState>) -> Result<(), String> {
    state.kill(&id)
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
        s.kill_owned();
    }
    if let Some(s) = app.try_state::<TermState>() {
        s.kill_all();
    }
    if let Some(s) = app.try_state::<BgExecState>() {
        s.kill_all();
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
        .manage(UndoState::default())
        .manage(work_agent::WorkAgentState::default())
        .manage(AppQuitState::default())
        .manage(browser::BrowserState::default())
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
