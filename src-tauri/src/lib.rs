// Headless server builds (`--no-default-features`, see Cargo.toml) keep every
// Tauri command wrapper compiled but uncalled; don't drown real warnings.
#![cfg_attr(not(feature = "desktop"), allow(dead_code, unused_imports))]
// Vicuna backend: all Ollama inference traffic goes through here.
// The frontend (JS) is display-only: it invokes these commands and renders
// the streamed events. Direct `fetch("http://localhost:11434/...")` from JS
// is only a web-dev fallback (see src/lib/ollama.ts).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use crate::ipc::Channel;
#[cfg(feature = "desktop")]
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
#[cfg(feature = "desktop")]
use tauri::tray::TrayIconBuilder;
#[cfg(feature = "desktop")]
use tauri::State;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter, Manager, Runtime, WindowEvent};
#[cfg(feature = "desktop")]
use tauri_plugin_sql::{DbInstances, DbPool};
pub mod ipc;
pub mod acp;
pub mod agy;
pub mod browser;
pub mod mcp;
pub mod opencode;
pub mod exe_resolve;
pub mod claude;
pub mod claude_cli;
pub mod codex;
pub mod codex_cli;
pub mod fx;
pub mod pi;
pub mod work_engine;
pub mod work_agent;
pub mod work_usage;
pub mod vicuna_mcp;
pub mod session_insight;
pub mod git_panel;
pub mod server_store;
pub mod servers;
pub mod server;
pub mod whisper;
pub mod plugins;
pub mod plugin_harness;
pub mod plugin_icon;
pub mod skills;
pub mod ollama;
pub mod web_fetch;
pub mod bg_exec;
pub mod agent_shell;
pub mod work_undo;

pub use ollama::{
    ollama_fetch_text_logic, parse_show_context_length, ChatMessage,
};
pub(crate) use ollama::{
    ollama_chat_stream, ollama_delete_model, ollama_generate_stream, ollama_list_models,
    ollama_list_running, ollama_model_show, ollama_pull_model, ollama_server_version,
    ollama_warm_model, OllamaServerState,
};
pub use web_fetch::{work_fetch_url_logic, WorkFetchContent};
pub use bg_exec::{bg_kill, bg_kill_running, bg_list, bg_snapshot, bg_start, BgExecState, BgJobStatus, BgJobSummary};
pub use agent_shell::{agent_shell_exec, agent_shell_input, agent_shell_interrupt, agent_shell_status, kill_agent_shells, AgentShellState, ShellRun};
pub(crate) use agent_shell::AGENT_NONINTERACTIVE_ENV;
pub use work_undo::{undo_begin_turn, undo_capture_prev, undo_key, undo_record, undo_record_write, work_redo_logic, work_undo_logic, workspace_git_note, UndoOp, UndoState, UndoTurn};
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

pub const OLLAMA_BASE: &str = "http://localhost:11434";

/// Windows: keep background children from flashing a console window on the
/// user's screen (agent `exec`, its timeout killer, self-started
/// `ollama serve`, pid kills). No-op on other platforms.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x08000000;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[cfg_attr(feature = "desktop", tauri::command)]
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
    workspace: Option<String>,
    images: Option<Vec<String>>,
    gen_ms: Option<f64>,
    gen_tps: Option<f64>,
    gen_cost: Option<f64>,
    summary: Option<bool>,
}

/// A string field that may also arrive as a JSON value: strings pass through,
/// null stays None, anything else is re-serialized to JSON text.
fn de_json_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct ChatSaveRow {
    id: String,
    title: String,
    created_at: i64,
    updated_at: Option<i64>,
    /// JSON text. The frontend sends it stringified, but `load_chats` (and so
    /// the desktop→server import) yields the parsed object: accept both, or
    /// one forked chat fails the whole batch ("invalid type: map").
    #[serde(deserialize_with = "de_json_text")]
    forked_from: Option<String>,
    fork_from_index: Option<i64>,
    mode: Option<String>,
    workspace: Option<String>,
    goal: Option<String>,
    /// per-session agent fields (engine/model/lead/pin) as JSON
    extra: Option<String>,
    messages: Vec<MsgSaveRow>,
    /// server merges only: the chat revision this client last saw. Older
    /// clients without a revision cannot overwrite server-recorded turns.
    base_rev: Option<i64>,
}

#[cfg(feature = "desktop")]
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
            "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
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
        .bind(chat.extra)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
        for (i, m) in chat.messages.iter().enumerate() {
            let images = m
                .images
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default());
            sqlx::query(
                "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost, workspace) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
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
            .bind(m.workspace.clone())
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

/// True when `path` points to a Vicuna per-session scratch workspace
/// (`~/vicuna/scratch/<session-id>`), used by "No workspace" mode.
pub fn is_scratch_workspace(path: &str) -> bool {
    let norm = path.trim().replace('\\', "/");
    let norm = norm.trim_end_matches('/');
    if let Some((_, tail)) = norm.rsplit_once("/vicuna/scratch/") {
        return !tail.is_empty()
            && !tail.contains('/')
            && tail.len() <= 64
            && tail
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    false
}

fn scratch_workspace_dir(session_id: &str) -> Result<std::path::PathBuf, String> {
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
        .join("scratch")
        .join(id))
}

pub async fn work_ensure_scratch_workspace_logic(session_id: String) -> Result<String, String> {
    let dir = scratch_workspace_dir(&session_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create scratch workspace: {e}"))?;
    let canon = work_canonical_root(
        dir.to_str()
            .ok_or_else(|| "invalid scratch workspace path".to_string())?,
    )?;
    Ok(work_display_string(&canon))
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
    let full = if let Some(skill_path) = skills::resolve_skills_absolute(std::path::Path::new(path.trim())) {
        skill_path
    } else {
        let canon = work_canonical_root(root.trim())?;
        work_join_allow_temp(&canon, &path)?
    };
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    // upper bound matches the write cap so the in-app editor can round-trip
    // anything it may save; agent reads keep the 64KB default
    let cap = max_bytes.unwrap_or(65536).clamp(1024, 524288) as usize;
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
/// Raw byte range of a workspace file as base64, for remote-server downloads
/// (the desktop pulls a server file chunk by chunk into ~/Downloads).
pub async fn work_read_chunk_logic(
    root: String,
    path: String,
    offset: u64,
    length: u64,
) -> Result<serde_json::Value, String> {
    use std::io::{Read as _, Seek as _};
    let canon = work_canonical_root(root.trim())?;
    let full = work_join_allow_temp(&canon, &path)?;
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    let len = length.clamp(1, 4 * 1024 * 1024).min(size.saturating_sub(offset));
    let mut f = std::fs::File::open(&full).map_err(|e| format!("cannot read file: {e}"))?;
    f.seek(std::io::SeekFrom::Start(offset.min(size)))
        .map_err(|e| format!("cannot read file: {e}"))?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).map_err(|e| format!("cannot read file: {e}"))?;
    use base64::Engine as _;
    Ok(serde_json::json!({
        "size": size,
        "data": base64::engine::general_purpose::STANDARD.encode(&buf),
    }))
}

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
    let (canon, full) = if let Some(skill_path) = skills::resolve_skills_absolute(std::path::Path::new(path.trim())) {
        (None, skill_path)
    } else {
        let canon = work_canonical_root(root.trim())?;
        let full = work_join_allow_temp(&canon, &path)?;
        (Some(canon), full)
    };
    if full.is_dir() {
        return Err("cannot overwrite a folder".to_string());
    }
    if let Some(parent) = full.parent() {
        let in_workspace = canon.as_ref().is_some_and(|c| parent.starts_with(c));
        if !in_workspace && !path_is_under_temp(parent) && !skills::path_is_under_skills(parent) {
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
#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_list_dir(root: String, path: Option<String>) -> Result<Vec<WorkDirEntry>, String> {
    work_list_dir_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_read_file(
    root: String,
    path: String,
    max_bytes: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<WorkFileContent, String> {
    work_read_file_logic(root, path, max_bytes, offset, limit).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_read_image(root: String, path: String) -> Result<WorkImageContent, String> {
    work_read_image_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_grep(root: String, pattern: String, include: Option<String>) -> Result<Vec<WorkGrepHit>, String> {
    work_grep_logic(root, pattern, include).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_write_file(root: String, path: String, content: String) -> Result<String, String> {
    work_write_file_logic(root, path, content).await
}

/// Open a workspace file/folder with the OS default app. Sandboxed through
/// `work_join` (the opener capability itself carries no path scope, so the
/// frontend's `openPath` was refused for every path).
#[cfg(feature = "desktop")]
#[tauri::command]
async fn work_open_path(app: AppHandle, root: String, path: String) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let canon = work_canonical_root(root.trim())?;
    let full = work_join(&canon, &path)?;
    if !full.exists() {
        return Err("file not found".to_string());
    }
    app.opener()
        .open_path(full.to_string_lossy().to_string(), None::<&str>)
        .map_err(|e| format!("cannot open: {e}"))
}

// ---- Git panel (side panel Git tab; logic in git_panel.rs, shared with the bridge) ----

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_status(root: String) -> Result<git_panel::GitStatus, String> {
    git_panel::git_status_logic(root).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_diff(root: String, path: String, staged: bool, untracked: bool) -> Result<git_panel::GitDiff, String> {
    git_panel::git_diff_logic(root, path, staged, untracked).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_stage(root: String, paths: Vec<String>) -> Result<(), String> {
    git_panel::git_stage_logic(root, paths).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_unstage(root: String, paths: Vec<String>) -> Result<(), String> {
    git_panel::git_unstage_logic(root, paths).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_discard(root: String, paths: Vec<String>, untracked: Vec<String>) -> Result<(), String> {
    git_panel::git_discard_logic(root, paths, untracked).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn git_apply(root: String, patch: String, cached: bool, reverse: bool, intent_to_add: Vec<String>) -> Result<(), String> {
    git_panel::git_apply_logic(root, patch, cached, reverse, intent_to_add).await
}

// ---- remote servers (see src/servers.rs) ----

#[cfg_attr(feature = "desktop", tauri::command)]
fn server_list() -> Vec<servers::ServerInfo> {
    servers::list()
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn server_connect(url: String, token: Option<String>, name: Option<String>) -> Result<servers::ServerInfo, String> {
    servers::connect(&url, token, name).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
fn server_touch(id: String) -> Result<(), String> {
    servers::touch(&id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
fn server_remove(id: String) -> Result<(), String> {
    servers::remove(&id)
}

#[cfg(feature = "desktop")]
#[tauri::command]
fn server_rename(app: AppHandle, id: String, name: Option<String>) -> Result<servers::ServerInfo, String> {
    let sv = servers::rename(&id, name)?;
    let label = format!("server-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_title(&format!("{} — {}", app_title(), sv.name));
    }
    let _ = app.emit("vicuna-servers-changed", ());
    Ok(sv)
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn server_call(
    id: String,
    path: String,
    body: Option<serde_json::Value>,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    servers::call(&id, &path, body, timeout_ms).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn server_download_file(id: String, root: String, path: String) -> Result<String, String> {
    servers::download_file(&id, &root, &path, &downloads_dir()?).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn server_lb_proxy_url(id: String) -> Result<String, String> {
    servers::lb_proxy_url(&id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn server_base_url(id: String) -> Result<String, String> {
    servers::base_url(&id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_exec(root: String, command: String, timeout_ms: Option<u64>) -> Result<WorkExecResult, String> {
    work_exec_logic(root, command, timeout_ms).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn upload_read_file(session_id: String, name: String) -> Result<WorkFileContent, String> {
    upload_read_file_logic(session_id, name).await
}

/// ---------- In-app update wrappers (same _logic pattern) ----------
#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_engine_info() -> Result<opencode::WorkEngineInfo, String> {
    // re-detects on every call: the UI's Refresh / window focus picks up an
    // engine installed while Vicuna is running. Returns the current engine
    // (opencode or Claude Code) plus a summary of every engine.
    Ok(work_engine::engines_info().await)
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn app_build_info() -> Result<serde_json::Value, String> {
    app_build_info_logic().await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn github_latest_release() -> Result<serde_json::Value, String> {
    github_latest_release_logic().await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn github_download_asset(url: String, file_name: String) -> Result<String, String> {
    github_download_asset_logic(url, file_name).await
}

// ---------- More command wrappers (same pattern: logic is pub + callable
// headlessly, wrappers preserve the exact IPC names). These cover the
// remaining backend surface the headless bridge drives.
#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_validate_root(path: String) -> Result<String, String> {
    work_validate_root_logic(path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_ensure_scratch_workspace(session_id: String) -> Result<String, String> {
    work_ensure_scratch_workspace_logic(session_id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_tree(root: String, max_depth: Option<usize>) -> Result<Vec<WorkTreeEntry>, String> {
    work_tree_logic(root, max_depth).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_rename(root: String, path: String, new_name: String) -> Result<String, String> {
    work_rename_logic(root, path, new_name).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_delete(root: String, path: String) -> Result<(), String> {
    work_delete_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn work_import_file(root: String, path: String, base64: String) -> Result<String, String> {
    work_import_file_logic(root, path, base64).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn upload_stage_file(session_id: String, file_name: String, base64: String) -> Result<String, String> {
    upload_stage_file_logic(session_id, file_name, base64).await
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

/// Pick the user's Unix shell: `$SHELL` if set and present on disk, otherwise
/// the first available shell on the platform (macOS prefers zsh; Linux and
/// containers often have bash or only sh).
#[cfg(not(windows))]
fn default_unix_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        let trimmed = s.trim();
        if !trimmed.is_empty() && std::path::Path::new(trimmed).is_file() {
            return trimmed.to_string();
        }
    }
    #[cfg(target_os = "macos")]
    let candidates = ["/bin/zsh", "/bin/bash", "/bin/sh"];
    #[cfg(not(target_os = "macos"))]
    let candidates = ["/bin/bash", "/usr/bin/bash", "/bin/zsh", "/usr/bin/zsh", "/bin/sh"];
    for c in candidates {
        if std::path::Path::new(c).is_file() {
            return c.to_string();
        }
    }
    "/bin/sh".to_string()
}

/// PATH as the user's terminal sees it: probe `$SHELL -l` (profiles
/// sourced). Best-effort with a short timeout — a slow/broken profile must
/// never hang Settings. Windows doesn't need this (the registry check in
/// `cli_on_path_in` covers it).
#[cfg(not(windows))]
async fn cli_shell_path_dirs() -> Vec<std::path::PathBuf> {
    let shell = default_unix_shell();
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
    let mut out = Vec::new();
    for d in cli_path_dirs().iter().chain(shell_dirs) {
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
#[cfg_attr(feature = "desktop", tauri::command)]
async fn cli_status() -> Result<CliStatus, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_status_inner(&all_dirs, &home)
}

/// Install (or reinstall) the `vicuna` launcher script.
#[cfg_attr(feature = "desktop", tauri::command)]
async fn cli_install() -> Result<CliInstallResult, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_install_inner(&all_dirs, &home)
}

/// Remove the launcher this app installed. Refuses foreign binaries.
#[cfg_attr(feature = "desktop", tauri::command)]
async fn cli_uninstall() -> Result<String, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_uninstall_inner(&all_dirs, &home)
}

/// Bring the main window forward (CLI deep links arrive while hidden in the
/// tray; the OS activates the app but doesn't un-hide the window).
#[cfg(feature = "desktop")]
#[tauri::command]
async fn show_main_window(app: AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "no main window".to_string())?;
    w.show().map_err(|e| format!("cannot show window: {e}"))?;
    w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
    Ok(())
}

/// Open (or focus) the window for a remote server: each server gets its own
/// window — the frontend reads `?server=<id>` once at load and routes every
/// backend call there (src/lib/bridge.ts) — so switching servers never
/// replaces or interrupts what the main window (This computer) is doing.
#[cfg(feature = "desktop")]
#[tauri::command]
async fn open_server_window(app: AppHandle, id: String, title: Option<String>) -> Result<(), String> {
    open_server_window_impl(&app, &id, title.as_deref(), None)
}

#[cfg(feature = "desktop")]
fn open_server_window_impl(app: &AppHandle, id: &str, title: Option<&str>, chat: Option<&str>) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("bad server id".into());
    }
    let resolved_name = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| servers::load().into_iter().find(|s| s.id == id).map(|s| s.name))
        .filter(|n| !n.trim().is_empty());
    let label = format!("server-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        if let Some(name) = resolved_name.as_deref() {
            let _ = w.set_title(&format!("{} — {name}", app_title()));
        }
        let _ = w.unminimize();
        w.show().map_err(|e| format!("cannot show window: {e}"))?;
        w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
        return Ok(());
    }
    let name = resolved_name.as_deref().unwrap_or(id);
    let mut url = format!("index.html?server={id}");
    if let Some(c) = chat.filter(|c| !c.is_empty()) {
        url.push_str(&format!("&chat={c}"));
    }
    tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App(url.into()))
        .title(format!("{} — {name}", app_title()))
        .inner_size(1200.0, 800.0)
        .build()
        .map_err(|e| format!("cannot open window: {e}"))?;
    Ok(())
}

/// Open a session from the search palette on the host it lives on: raise that
/// host's window (a server's own window, or the main window for This computer)
/// and have it navigate to `chat_id`. A fresh server window gets `?chat=`, an
/// already-open window (whose URL must not reload mid-turn) an event instead.
#[cfg(feature = "desktop")]
#[tauri::command]
async fn open_chat_window(app: AppHandle, server_id: Option<String>, chat_id: String, title: Option<String>) -> Result<(), String> {
    if chat_id.is_empty() || !chat_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("bad chat id".into());
    }
    let label = match server_id.as_deref() {
        Some(id) => {
            let label = format!("server-{id}");
            if app.get_webview_window(&label).is_none() {
                // fresh window: the URL carries the session, no event needed
                return open_server_window_impl(&app, id, title.as_deref(), Some(&chat_id));
            }
            label
        }
        None => "main".to_string(),
    };
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        w.show().map_err(|e| format!("cannot show window: {e}"))?;
        w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
    }
    app.emit_to(label.as_str(), "vicuna-open-chat", serde_json::json!({ "chatId": chat_id }))
        .map_err(|e| format!("cannot notify window: {e}"))?;
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
pub enum TermEvent {
    Output { text: String },
    Exit { code: Option<i32> },
}

struct TermHandle {
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Mutex<Box<dyn Write + Send>>,
    pid: Option<u32>,
}

#[derive(Default)]
pub struct TermState {
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
/// Kill a background job: its whole process group on Unix (bg jobs lead
/// their own group), else just the pid.
fn kill_job(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", "--"])
            .arg(format!("-{pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }
    kill_pid(pid);
}

/// Kill every terminal pty in `state` (Quit; the embedded server's terminals).
pub fn term_kill_all(state: &TermState) {
    if let Ok(mut terms) = state.terms.lock() {
        for (_, h) in terms.drain() {
            if let Some(pid) = h.pid {
                kill_pid(pid);
            }
        }
    }
}

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
            let shell = default_unix_shell();
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
/// over the channel; returns the terminal id for write/resize/kill. Shared by
/// the Tauri command and the headless bridge (remote-server terminals).
pub fn term_spawn_logic(
    state: &TermState,
    root: &str,
    cols: Option<u16>,
    rows: Option<u16>,
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
pub fn term_write_logic(state: &TermState, id: &str, data: &str) -> Result<(), String> {
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
pub fn term_resize_logic(state: &TermState, id: &str, cols: u16, rows: u16) -> Result<(), String> {
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
pub fn term_kill_logic(state: &TermState, id: &str) -> Result<(), String> {
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

#[cfg(feature = "desktop")]
#[tauri::command]
async fn term_spawn(
    root: String,
    cols: Option<u16>,
    rows: Option<u16>,
    state: State<'_, TermState>,
    on_event: Channel<TermEvent>,
) -> Result<String, String> {
    term_spawn_logic(&state, &root, cols, rows, on_event)
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn term_write(id: String, data: String, state: State<'_, TermState>) -> Result<(), String> {
    term_write_logic(&state, &id, &data)
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn term_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TermState>,
) -> Result<(), String> {
    term_resize_logic(&state, &id, cols, rows)
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn term_kill(id: String, state: State<'_, TermState>) -> Result<(), String> {
    term_kill_logic(&state, &id)
}

/// Folder picker (remote servers + browsers, where the native dialog can't
/// browse the backend machine): the sub-folders of `path` (home when empty,
/// `~` expanded), sorted case-insensitively, dot-folders only with `hidden`.
/// Symlinks to folders count as folders. Names only — no file listing.
pub fn fs_list_dirs_logic(path: Option<String>, hidden: bool) -> Result<serde_json::Value, String> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(std::path::PathBuf::from)
        .ok();
    let raw = path.map(|p| p.trim().to_string()).unwrap_or_default();
    let target = if raw.is_empty() {
        home.clone().ok_or("no home folder")?
    } else if raw == "~" || raw.starts_with("~/") {
        let h = home.clone().ok_or("no home folder")?;
        if raw == "~" { h } else { h.join(&raw[2..]) }
    } else {
        std::path::PathBuf::from(&raw)
    };
    if !target.is_absolute() {
        return Err(format!("Not an absolute path: {raw}"));
    }
    let canon = std::fs::canonicalize(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    if !canon.is_dir() {
        return Err(format!("Not a folder: {}", canon.display()));
    }
    const CAP: usize = 2000;
    let mut names: Vec<String> = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(&canon).map_err(|e| format!("{}: {e}", canon.display()))?.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !hidden && name.starts_with('.') {
            continue;
        }
        // metadata() follows symlinks, so a link to a folder lists as one
        if !std::fs::metadata(entry.path()).map(|m| m.is_dir()).unwrap_or(false) {
            continue;
        }
        if names.len() >= CAP {
            truncated = true;
            break;
        }
        names.push(name);
    }
    names.sort_by_key(|n| n.to_lowercase());
    Ok(serde_json::json!({
        "path": work_display_string(&canon),
        "parent": canon.parent().map(work_display_string),
        "home": home.as_deref().map(work_display_string),
        "sep": std::path::MAIN_SEPARATOR.to_string(),
        "entries": names,
        "truncated": truncated,
    }))
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn fs_list_dirs(path: Option<String>, hidden: Option<bool>) -> Result<serde_json::Value, String> {
    fs_list_dirs_logic(path, hidden.unwrap_or(false))
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

#[cfg(feature = "desktop")]
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

#[cfg(feature = "desktop")]
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
#[cfg(feature = "desktop")]
fn shutdown_children(app: &AppHandle) {
    server::embedded::shutdown();
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
        bg_kill_running(&s);
    }
    if let Some(s) = app.try_state::<AgentShellState>() {
        kill_agent_shells(&s);
    }
}

#[cfg(feature = "desktop")]
fn allow_quit(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppQuitState>() {
        if let Ok(mut g) = s.allow.lock() {
            *g = true;
        }
    }
}

/// What the frontend last reported about scheduled sends. They only go out
/// while the app runs, so a quit request with some pending asks first.
#[derive(Default)]
struct ScheduleState {
    count: Mutex<usize>,
}

#[cfg(feature = "desktop")]
fn quit_now(app: &AppHandle) {
    allow_quit(app);
    shutdown_children(app);
    app.exit(0);
}

/// Quit asked from the tray menu or the app menu (Cmd+Q). With scheduled
/// messages pending, the window comes up and the frontend shows the list
/// with a warning ("Quit anyway" then calls `quit_app`); otherwise quit.
#[cfg(feature = "desktop")]
fn request_quit(app: &AppHandle) {
    let pending = app
        .try_state::<ScheduleState>()
        .and_then(|s| s.count.lock().ok().map(|g| *g))
        .unwrap_or(0);
    if pending > 0 {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            if app.emit("vicuna-quit-requested", pending).is_ok() {
                return;
            }
        }
    }
    quit_now(app);
}

/// macOS app menu: the stock Quit item terminates the process directly, so
/// it is swapped for one that goes through `request_quit` (same id as the
/// tray's Quit; the tray's menu handler receives every menu event).
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
fn install_app_menu(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::MenuItemKind;
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_sub)) = menu.items()?.into_iter().next() {
        for item in app_sub.items()? {
            if let MenuItemKind::Predefined(p) = item {
                // muda labels it "&Quit" (the app name is added at draw time)
                if p
                    .text()
                    .map(|t| t.trim_start_matches('&').starts_with("Quit"))
                    .unwrap_or(false)
                {
                    app_sub.remove(&p)?;
                    let quit =
                        MenuItem::with_id(app, "quit", "Quit Vicuna", true, Some("CmdOrCtrl+Q"))?;
                    app_sub.append(&quit)?;
                    break;
                }
            }
        }
    }
    app.set_menu(menu)?;
    Ok(())
}

/// Explicit quit (Settings button, or "Quit anyway" in the scheduled-messages
/// prompt): kill everything Vicuna started, then exit. Window-close hides to the tray instead (see
/// `on_window_event` below) so background streams survive.
#[cfg(feature = "desktop")]
#[tauri::command]
async fn quit_app(
    app: AppHandle,
    _ollama: State<'_, OllamaServerState>,
    _terms: State<'_, TermState>,
) -> Result<(), String> {
    quit_now(&app);
    Ok(())
}

/// Build the tray menu: a disabled live-session status line ("No running
/// sessions" / "N running work session(s)"), a separator, Show / Quit —
/// plus, while sessions run, a second separator and a disabled elapsed line
/// ("32m 25s") under Quit. With scheduled messages pending, a disabled
/// section ("2 scheduled messages" + one row each) sits under the status
/// line. Rebuilt on every frontend report.
#[cfg(feature = "desktop")]
fn tray_menu<R: Runtime>(
    manager: &impl Manager<R>,
    status: &str,
    elapsed: Option<&str>,
    scheduled: &[String],
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
    let sched_sep = PredefinedMenuItem::separator(manager)?;
    let mut sched_items = Vec::new();
    for (i, line) in scheduled.iter().enumerate() {
        sched_items.push(MenuItem::with_id(
            manager,
            format!("scheduled-{i}"),
            line,
            false,
            None::<&str>,
        )?);
    }
    let mut items: Vec<&dyn IsMenuItem<R>> = vec![&status_item as &dyn IsMenuItem<R>];
    if !sched_items.is_empty() {
        items.push(&sched_sep as &dyn IsMenuItem<R>);
        for it in &sched_items {
            items.push(it as &dyn IsMenuItem<R>);
        }
    }
    items.extend([
        &sep as &dyn IsMenuItem<R>,
        &show as &dyn IsMenuItem<R>,
        &quit as &dyn IsMenuItem<R>,
    ]);
    if let Some(ref e) = elapsed_item {
        items.push(&sep2 as &dyn IsMenuItem<R>);
        items.push(e as &dyn IsMenuItem<R>);
    }
    Menu::with_items(manager, &items)
}

/// Refresh the tray menu (no-op when the tray is unavailable).
#[cfg(feature = "desktop")]
fn refresh_tray_menu(
    app: &AppHandle,
    status: &str,
    elapsed: Option<&str>,
    scheduled: &[String],
) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id("main") {
        let menu = tray_menu(app, status, elapsed, scheduled)
            .map_err(|e| format!("tray menu failed: {e}"))?;
        tray.set_menu(Some(menu))
            .map_err(|e| format!("tray menu failed: {e}"))?;
    }
    Ok(())
}

/// Update the tray menu: status line plus, while sessions run, an elapsed
/// line under Quit. The frontend owns streaming state, so it reports both
/// whenever relevant. Best-effort: never fails a turn.
#[cfg(feature = "desktop")]
#[tauri::command]
async fn tray_set_status(
    app: AppHandle,
    status: String,
    elapsed: Option<String>,
    scheduled: Option<Vec<String>>,
    scheduled_count: Option<usize>,
) -> Result<(), String> {
    // header row first ("2 scheduled messages"), then one row per message
    let scheduled: Vec<String> = scheduled
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.trim().chars().take(80).collect::<String>())
        .filter(|l| !l.is_empty())
        .take(8)
        .collect();
    // the count gates the quit prompt, so it is stored even without a tray
    if let Some(s) = app.try_state::<ScheduleState>() {
        if let Ok(mut g) = s.count.lock() {
            *g = scheduled_count.unwrap_or(0);
        }
    }
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
        &scheduled,
    )
}

/// ---------- MCP servers (Work mode only) ----------
///
/// Configs live in `~/vicuna/config/vicuna.json` (`mcp` map, opencode.json
/// shape) — the file is the source of truth, the UI edits it. Connections
/// are runtime-only in the McpManager (stdio children die on disconnect).

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_list_servers(app: AppHandle) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::session_servers(app.path().resource_dir().ok().as_deref())
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_set_defaults(app: AppHandle, enabled: mcp::EnabledMap) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::set_defaults(&enabled)?;
    mcp_list_servers(app).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_save_server(
    app: AppHandle,
    config: mcp::McpServerConfig,
    previous_id: Option<String>,
) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::save_file_server(&config, previous_id.as_deref())?;
    mcp_list_servers(app).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_delete_server(app: AppHandle, id: String) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::delete_file_server(&id)?;
    mcp_list_servers(app).await
}

// ---------- Skills (~/vicuna/skills/<slug>/SKILL.md) ----------

#[cfg_attr(feature = "desktop", tauri::command)]
async fn skills_list() -> Result<Vec<skills::SkillSummary>, String> {
    skills::list_skills()
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn skills_read(slug: String) -> Result<skills::SkillDetail, String> {
    skills::read_skill(&slug)
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn skills_save(
    slug: String,
    content: String,
) -> Result<skills::SkillSummary, String> {
    skills::save_skill(&slug, &content)
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn skills_set_defaults(skills_enabled: skills::EnabledMap) -> Result<(), String> {
    skills::set_defaults(&skills_enabled)
}

#[cfg_attr(feature = "desktop", tauri::command)]
async fn skills_remove(slug: String) -> Result<(), String> {
    skills::remove_skill(&slug)
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_connect(
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    state.connect(&config).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_disconnect(id: String, state: State<'_, mcp::McpManager>) -> Result<(), String> {
    state.disconnect(&id).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_list_tools(
    id: String,
    state: State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::McpToolInfo>, String> {
    state.list_tools(&id).await
}

#[cfg(feature = "desktop")]
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
#[cfg(feature = "desktop")]
#[tauri::command]
async fn mcp_signin(
    app: AppHandle,
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    use tauri_plugin_opener::OpenerExt;
    mcp::oauth_login(&config, |url| async move {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| format!("mcp: cannot open browser for sign-in: {e}"))
    })
    .await?;
    state.connect(&config).await
}

/// Forget the stored OAuth login and drop the connection, if any.
#[cfg(feature = "desktop")]
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
#[cfg_attr(feature = "desktop", tauri::command)]
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

#[cfg(feature = "desktop")]
fn browser_resource_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().resource_dir().ok()
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_status(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_status_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_start(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_start_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_stop(state: State<'_, browser::BrowserState>) -> Result<browser::BrowserStatus, String> {
    browser::browser_stop_logic(&state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_restart(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_restart_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_ensure_engine(
    app: AppHandle,
    background: Option<bool>,
) -> Result<browser::EngineInfo, String> {
    browser::browser_ensure_engine_logic(browser_resource_dir(&app), background.unwrap_or(false))
        .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
async fn browser_takeover(
    app: AppHandle,
    state: State<'_, browser::BrowserState>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_takeover_logic(browser_resource_dir(&app), &state).await
}

/// Absolute `lb mcp` argv for the default `browser` MCP server entry.
#[cfg(feature = "desktop")]
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

#[cfg(feature = "desktop")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Finder/Dock launches get launchd's minimal PATH: read the user's shell
    // PATH in the background so engine detection sees what a terminal sees.
    exe_resolve::warm_up();
    let builder = tauri::Builder::default();
    // Must be the first plugin: a second launch exits before anything else
    // starts and hands its args (incl. vicuna:// links, via the deep-link
    // feature) to this instance, which un-hides (tray) / restores / focuses
    // the existing window.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));
    builder
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
        .manage(ScheduleState::default())
        .manage(browser::BrowserState::default())
        .manage(mcp::McpManager::default())
        .manage(ZoomState {
            factor: Mutex::new(1.0),
        })
        .invoke_handler(tauri::generate_handler![
            greet,
            chats_save,
            ollama::ollama_list_models,
            ollama::ollama_list_running,
            ollama::ollama_warm_model,
            ollama::ollama_chat_stream,
            ollama::ollama_generate_stream,
            ollama::ollama_pull_model,
            ollama::ollama_delete_model,
            ollama::ollama_model_show,
            ollama::ollama_fetch_text,
            ollama::ollama_server_version,
            ollama::ollama_start_server,
            ollama::ollama_stop_server,
            work_validate_root,
            work_ensure_scratch_workspace,
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
            work_open_path,
            git_status,
            git_diff,
            git_stage,
            git_unstage,
            git_discard,
            git_apply,
            fs_list_dirs,
            open_server_window,
            open_chat_window,
            server_list,
            server_connect,
            server_touch,
            server_remove,
            server_rename,
            server_call,
            server_download_file,
            server_lb_proxy_url,
            server_base_url,
            work_exec,
            web_fetch::work_fetch_url,
            bg_exec::work_exec_start,
            bg_exec::work_exec_status,
            bg_exec::work_exec_list,
            bg_exec::work_exec_kill,
            agent_shell::work_shell_exec,
            agent_shell::work_shell_status,
            agent_shell::work_shell_input,
            agent_shell::work_shell_interrupt,
            work_undo::work_undo,
            work_undo::work_redo,
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
            mcp_set_defaults,
            mcp_save_server,
            mcp_delete_server,
            mcp_connect,
            mcp_disconnect,
            mcp_list_tools,
            mcp_call_tool,
            mcp_signin,
            mcp_signout,
            mcp_login_present,
            skills_list,
            skills_read,
            skills_save,
            skills_set_defaults,
            skills_remove,
            browser_status,
            browser_start,
            browser_stop,
            browser_restart,
            browser_takeover,
            browser_ensure_engine,
            browser_mcp_command,
            work_agent::work_agent_start,
            work_agent::work_agent_stop,
            work_agent::work_agent_turn_live,
            work_agent::work_agent_queue,
            work_agent::work_agent_remove,
            work_agent::work_agent_answer,
            work_engine_info,
            opencode::opencode_models,
            opencode::opencode_set_model,
            work_engine::work_engine_select,
            work_engine::work_models,
            work_engine::work_set_model,
            work_engine::work_set_approval,
            work_usage::work_usage
        ])
        .setup(|app| {
            // The main window's backend: the same server `vicuna serve` runs,
            // embedded here (or a `vicuna serve` already running on this
            // machine). The window reaches it as server "local"; the CLI's
            // default target is the same server. Then, once, the chats this
            // app kept in its own sqlite move over (read-only on that file).
            {
                let resources = app.path().resource_dir().ok();
                let desktop_db = app.path().app_config_dir().ok().map(|d| d.join("vicuna.db"));
                tauri::async_runtime::spawn(server::embedded::launch(resources, desktop_db));
                let opener = app.handle().clone();
                server::set_url_opener(move |url| {
                    use tauri_plugin_opener::OpenerExt;
                    opener.opener().open_url(url, None::<&str>).map_err(|e| format!("mcp: cannot open browser for sign-in: {e}"))
                });
            }
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
            #[cfg(target_os = "macos")]
            let _ = install_app_menu(app.handle());
            let menu = tray_menu(app, "No running sessions", None, &[])?;
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
                        "quit" => request_quit(app),
                        _ => {}
                    })
                    .build(app)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to the tray instead of quitting so the backend (streams,
            // terminals, self-started ollama) survives a window close.
            // Only the main window hides; server windows (open_server_window)
            // are just views of a remote server and close for real.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }
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
#[cfg(feature = "desktop")]
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
    use crate::web_fetch::{decode_html_entities, html_to_text};
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

    #[cfg(unix)]
    #[tokio::test]
    async fn bg_job_finishes_when_the_shell_does_despite_a_lingering_child() {
        let d = tmp_root("bglinger");
        let state = BgExecState::default();
        // the leftover sleep inherits stdout and keeps it open for 60 s
        let id = bg_start(&state, d.to_string_lossy().to_string(), "sleep 60 & echo started; exit 3".into()).await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let done = loop {
            let s = bg_snapshot(&state, &id).unwrap();
            if !s.running {
                break s;
            }
            assert!(std::time::Instant::now() < deadline, "job waited for the leftover child");
            tokio::time::sleep(Duration::from_millis(100)).await;
        };
        assert_eq!(done.exit_code, Some(3), "exit status kept");
        assert!(done.stdout.contains("started"));
        // killing a finished job is a no-op (the pid was released) and the
        // reported status stays the real one
        assert_eq!(bg_kill(&state, &id).unwrap().exit_code, Some(3));
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

#[cfg(test)]
mod fs_list_dirs_tests {
    use super::fs_list_dirs_logic;

    #[test]
    fn lists_only_folders_sorted_and_hides_dotdirs() {
        let root = std::env::temp_dir().join(format!("vicuna-fsl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in ["beta", "Alpha", ".hidden", "gamma/inner"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join("file.txt"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("beta"), root.join("link-to-beta")).unwrap();

        let v = fs_list_dirs_logic(Some(root.to_string_lossy().into()), false).unwrap();
        let names: Vec<&str> = v["entries"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
        #[cfg(unix)]
        assert_eq!(names, ["Alpha", "beta", "gamma", "link-to-beta"]);
        assert!(v["parent"].is_string());
        let with_hidden = fs_list_dirs_logic(Some(root.to_string_lossy().into()), true).unwrap();
        assert!(with_hidden["entries"].as_array().unwrap().iter().any(|e| e == ".hidden"));

        assert!(fs_list_dirs_logic(Some("relative/path".into()), false).is_err());
        assert!(fs_list_dirs_logic(Some(root.join("file.txt").to_string_lossy().into()), false).is_err());
        assert!(fs_list_dirs_logic(Some(root.join("missing").to_string_lossy().into()), false).is_err());
        // empty = home
        let home = fs_list_dirs_logic(None, false).unwrap();
        assert_eq!(home["path"], home["home"]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
