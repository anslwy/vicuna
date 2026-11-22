use serde::Serialize;
use std::time::Duration;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
#[cfg(feature = "desktop")]
use tauri::AppHandle;

use crate::agent_shell::AGENT_NONINTERACTIVE_ENV;
use crate::cli_launcher::{cli_path_dirs, cli_shell_path_dirs};
use crate::skills;
use crate::updates_uploads::uploads_home_dir;
#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

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
pub(crate) fn work_display_string(p: &std::path::Path) -> String {
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
pub(crate) fn decode_base64(s: &str) -> Result<Vec<u8>, ()> {
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

pub(crate) fn trunc_out(s: &str) -> (String, bool) {
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
pub(crate) fn refuse_full_disk_scan(cmd: &str) -> Option<String> {
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
pub(crate) fn full_disk_scan_error(frag: &str) -> String {
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
pub(crate) fn work_path_dirs(
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
pub(crate) async fn work_exec_path() -> Option<std::ffi::OsString> {
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
pub(crate) async fn work_list_dir(root: String, path: Option<String>) -> Result<Vec<WorkDirEntry>, String> {
    work_list_dir_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_read_file(
    root: String,
    path: String,
    max_bytes: Option<u64>,
    offset: Option<u64>,
    limit: Option<u64>,
) -> Result<WorkFileContent, String> {
    work_read_file_logic(root, path, max_bytes, offset, limit).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_read_image(root: String, path: String) -> Result<WorkImageContent, String> {
    work_read_image_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_grep(root: String, pattern: String, include: Option<String>) -> Result<Vec<WorkGrepHit>, String> {
    work_grep_logic(root, pattern, include).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_write_file(root: String, path: String, content: String) -> Result<String, String> {
    work_write_file_logic(root, path, content).await
}

/// Open a workspace file/folder with the OS default app. Sandboxed through
/// `work_join` (the opener capability itself carries no path scope, so the
/// frontend's `openPath` was refused for every path).
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_open_path(app: AppHandle, root: String, path: String) -> Result<(), String> {
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

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_exec(root: String, command: String, timeout_ms: Option<u64>) -> Result<WorkExecResult, String> {
    work_exec_logic(root, command, timeout_ms).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_validate_root(path: String) -> Result<String, String> {
    work_validate_root_logic(path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_ensure_scratch_workspace(session_id: String) -> Result<String, String> {
    work_ensure_scratch_workspace_logic(session_id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_tree(root: String, max_depth: Option<usize>) -> Result<Vec<WorkTreeEntry>, String> {
    work_tree_logic(root, max_depth).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_rename(root: String, path: String, new_name: String) -> Result<String, String> {
    work_rename_logic(root, path, new_name).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_delete(root: String, path: String) -> Result<(), String> {
    work_delete_logic(root, path).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_import_file(root: String, path: String, base64: String) -> Result<String, String> {
    work_import_file_logic(root, path, base64).await
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
pub(crate) async fn fs_list_dirs(path: Option<String>, hidden: Option<bool>) -> Result<serde_json::Value, String> {
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
