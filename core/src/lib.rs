// Vicuna core backend — everything needed to run an agent turn, with NO
// Tauri dependency.
//
// Consumers:
//   src-tauri  desktop shell (adds the #[tauri::command] surface)
//   bridge     dev bridge + remote worker (HTTP dispatch over the same fns)
//
// Keeping this crate Tauri-free is load-bearing: the remote worker must build
// with a plain Rust toolchain, without GTK/WebKit dev packages, so it can ship
// as a small binary to a GPU box or container.

pub mod browser;
pub mod events;
pub mod mcp;
pub mod work_agent;

pub use events::{CallbackSink, EventSink, NoopSink, Sink};

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

pub const OLLAMA_BASE: &str = "http://localhost:11434";

/// Base URL for the Ollama API. Defaults to [`OLLAMA_BASE`]; the env var
/// `VICUNA_OLLAMA_BASE` overrides it.
///
/// The desktop always talks to an Ollama on the same machine, but a remote
/// worker (container, GPU box) often does not run Ollama itself — it needs to
/// point at the host that does. Read at call time rather than stored in a
/// `OnceLock` so tests and reconnects can change it without a restart.
pub fn ollama_base() -> String {
    std::env::var("VICUNA_OLLAMA_BASE")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| OLLAMA_BASE.to_string())
}

/// Windows: keep background children from flashing a console window on the
/// user's screen (agent `exec`, its timeout killer, self-started
/// `ollama serve`, pid kills). No-op on other platforms.
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x08000000;

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
pub enum ChatEvent {
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
pub enum GenerateEvent {
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
pub enum PullEvent {
    Progress {
        status: String,
        digest: Option<String>,
        total: Option<u64>,
        completed: Option<u64>,
    },
}

pub fn ollama_error(context: &str, status: reqwest::StatusCode, body: &str) -> String {
    format!("ollama {context} failed: {status} {body}")
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
pub struct ThinkingInfo {
    pub values: Vec<serde_json::Value>,
    pub default: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelShowInfo {
    pub context_length: Option<u64>,
    pub thinking: Option<ThinkingInfo>,
    pub family: Option<String>,
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
pub fn is_block_tag(name: &str) -> bool {
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
pub fn find_close_tag(hay: &[u8], name: &str) -> Option<usize> {
    let needle = format!("</{name}");
    let nb = needle.as_bytes();
    if hay.len() < nb.len() {
        return None;
    }
    hay.windows(nb.len()).position(|w| w.eq_ignore_ascii_case(nb))
}

/// Contents of the first `<title>` tag, entity-decoded and flattened to one
/// line. News pages put the headline there, so it leads the fetched text.
pub fn extract_html_title(html: &str) -> Option<String> {
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
pub fn decode_entity(ent: &str) -> Option<String> {
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
pub fn decode_html_entities(s: &str) -> String {
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
pub fn html_to_text(html: &str) -> String {
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
pub fn ollama_candidates() -> [&'static str; 3] {
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
pub struct OllamaServerState {
    pub child: Mutex<Option<Child>>,
}

impl OllamaServerState {
    /// Kill a self-started `ollama serve`, if any. Best-effort.
    pub fn kill_owned(&self) {
        if let Ok(mut g) = self.child.lock() {
            if let Some(mut child) = g.take() {
                let _ = child.kill();
            }
        }
    }
}

pub async fn ollama_version_inner(client: &reqwest::Client) -> Result<String, String> {
    let res = client
        .get(format!("{}/api/version", ollama_base()))
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
pub fn work_display_string(p: &std::path::Path) -> String {
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
pub fn decode_base64(s: &str) -> Result<Vec<u8>, ()> {
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

pub fn trunc_out(s: &str) -> (String, bool) {
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
pub fn refuse_full_disk_scan(cmd: &str) -> Option<String> {
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
pub fn full_disk_scan_error(frag: &str) -> String {
    format!(
        "refused: `{frag} …` crawls the whole disk (or a whole home folder), including macOS privacy-protected folders — macOS raises one consent dialog per folder for the entire scan. Locate tools with `command -v <name>` or `ls` on specific bin dirs (/opt/homebrew/bin, /usr/local/bin, /usr/bin), search inside the workspace with the grep tool, or ask the user for the path."
    )
}

/// PATH used by Work shell commands. GUI apps launched from Finder may have
/// a sparse process PATH; prefer the user's login-shell PATH and append any
/// app-only entries. Probe once so command execution does not pay the shell
/// startup cost on every invocation. If probing fails, inherit the app PATH.
#[cfg(not(windows))]
pub static WORK_EXEC_PATH: tokio::sync::OnceCell<Option<std::ffi::OsString>> =
    tokio::sync::OnceCell::const_new();

#[cfg(not(windows))]
pub fn work_path_dirs(
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
    let child = {
        let mut builder = tokio::process::Command::new("sh");
        builder
            .arg("-c")
            .arg(cmd)
            .current_dir(&canon)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
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

pub struct BgJobRecord {
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

impl BgExecState {
    /// Kill every still-running background job. Best-effort.
    pub fn kill_all(&self) {
        if let Ok(jobs) = self.jobs.lock() {
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

pub fn bg_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cap for a captured background stream (32KB like foreground exec).
pub const BG_STREAM_CAP: usize = 32768;

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
pub fn undo_write_file(canon: &std::path::Path, path: &str, bytes: &[u8]) -> Result<(), ()> {
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
pub fn undo_apply_reverse(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
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
pub fn undo_apply_forward(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
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
pub fn undo_message(verb: &str, restored: usize, other: usize, other_verb: &str, other_noun: &str, execs: &[String], skipped: &[String]) -> String {
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

/// Marker comment identifying a launcher this app installed (so status never
/// claims a foreign `vicuna` binary as its own).
pub const CLI_MARKER: &str = "Installed by Vicuna";

/// POSIX launcher (macOS + Linux): resolve the arg (default `$PWD`), fully
/// percent-encode it (python3 when present, portable `od` fallback — slashes
/// included, the app decodes fully), open the link. `&` so the terminal
/// doesn't hang on `xdg-open`.
pub const CLI_UNIX_SCRIPT: &str = r#"#!/bin/sh
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
pub const CLI_WINDOWS_SCRIPT: &str = "@echo off\r\nrem Installed by Vicuna (Settings > General > Vicuna CLI). Opens a folder in the Vicuna desktop app: `vicuna [path]` (default: current directory).\r\nsetlocal\r\nset \"P=%~f1\"\r\nif \"%P%\"==\"\" set \"P=%CD%\"\r\nfor /f \"delims=\" %%E in ('powershell -NoProfile -NonInteractive -Command \"[uri]::EscapeDataString($env:P)\"') do set \"ENC=%%E\"\r\nstart \"\" \"vicuna://open?path=%ENC%\"\r\n";

pub fn cli_exe_name() -> &'static str {
    if cfg!(windows) { "vicuna.cmd" } else { "vicuna" }
}

pub fn cli_script_body() -> &'static str {
    if cfg!(windows) { CLI_WINDOWS_SCRIPT } else { CLI_UNIX_SCRIPT }
}

/// PATH entries of this process. GUI apps launched from Finder get a sparse
/// system PATH, so this alone misses user bins like `~/.local/bin` — every
/// consumer below works off `cli_all_path_dirs` (process + login shell).
pub fn cli_path_dirs() -> Vec<std::path::PathBuf> {
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
pub async fn cli_shell_path_dirs() -> Vec<std::path::PathBuf> {
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
pub fn cli_all_path_dirs(shell_dirs: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
    let mut out = cli_path_dirs();
    for d in shell_dirs {
        if !out.iter().any(|e| e == d) {
            out.push(d.clone());
        }
    }
    out
}

#[cfg(not(windows))]
pub fn cli_is_system_dir(d: &std::path::Path) -> bool {
    matches!(
        d.to_string_lossy().as_ref(),
        "/bin" | "/sbin" | "/usr/bin" | "/usr/sbin" | "/System/Volumes/Data/bin"
    )
}

pub fn cli_home_dir() -> Option<std::path::PathBuf> {
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
pub fn cli_install_dir_in(path_dirs: &[std::path::PathBuf], home: &std::path::Path) -> std::path::PathBuf {
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

pub fn cli_unix_fallback_dir_in(home: &std::path::Path) -> Option<std::path::PathBuf> {
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
pub fn cli_scan_in(dirs: &[std::path::PathBuf], exe: &str) -> Option<std::path::PathBuf> {
    for d in dirs {
        let cand = d.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn cli_scan(exe: &str, all_dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    cli_scan_in(all_dirs, exe)
}

pub fn cli_is_managed(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(CLI_MARKER))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize)]
pub struct CliStatus {
    pub exe_name: String,
    /// resolved `vicuna` on PATH, if any (managed or foreign)
    pub found: Option<String>,
    /// whether `found` is a launcher this app installed
    pub managed: bool,
    /// where Install would write
    pub install_dir: String,
    /// whether `install_dir` is on the effective PATH (process + login
    /// shell; Windows also checks the user PATH registry, which the current
    /// process may predate — a fresh terminal is authoritative)
    pub on_path: bool,
}

pub fn cli_on_path_in(dir: &std::path::Path, all_dirs: &[std::path::PathBuf]) -> bool {
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

pub fn cli_status_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliStatus, String> {
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
pub struct CliInstallResult {
    pub location: String,
    pub on_path: bool,
    pub note: String,
}

/// Write the launcher file into `dir` (creating it). Unix sets 0o755.
pub fn cli_write_launcher(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
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
pub fn cli_ensure_user_path(dir: &std::path::Path, all_dirs: &[std::path::PathBuf]) -> bool {
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

pub fn cli_install_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliInstallResult, String> {
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
pub fn cli_uninstall_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<String, String> {
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
pub async fn cli_env() -> Result<(Vec<std::path::PathBuf>, std::path::PathBuf), String> {
    #[cfg(windows)]
    let shell_dirs = Vec::new();
    #[cfg(not(windows))]
    let shell_dirs = cli_shell_path_dirs().await;
    let all_dirs = cli_all_path_dirs(&shell_dirs);
    let home = cli_home_dir().ok_or_else(|| "cannot locate home folder".to_string())?;
    Ok((all_dirs, home))
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TermEvent {
    Output { text: String },
    Exit { code: Option<i32> },
}

pub struct TermHandle {
    pub master: Box<dyn portable_pty::MasterPty + Send>,
    pub writer: Mutex<Box<dyn Write + Send>>,
    pub pid: Option<u32>,
}

#[derive(Default)]
pub struct TermState {
    terms: Mutex<HashMap<String, TermHandle>>,
    seq: AtomicU64,
}

pub static TERM_SEQ: AtomicU64 = AtomicU64::new(1);

impl TermState {
    /// Register a freshly spawned pty under `id`.
    pub fn insert(&self, id: String, handle: TermHandle) -> Result<(), String> {
        self.terms
            .lock()
            .map_err(|_| "terminal state poisoned".to_string())?
            .insert(id, handle);
        Ok(())
    }

    /// Send keystrokes to a terminal.
    pub fn write(&self, id: &str, data: &str) -> Result<(), String> {
        let terms = self.terms.lock().map_err(|_| "terminal state poisoned".to_string())?;
        let h = terms.get(id.trim()).ok_or_else(|| "terminal closed".to_string())?;
        let mut w = h.writer.lock().map_err(|_| "terminal state poisoned".to_string())?;
        w.write_all(data.as_bytes())
            .map_err(|e| format!("terminal write failed: {e}"))?;
        w.flush().map_err(|e| format!("terminal write failed: {e}"))
    }

    /// Resize a terminal (columns x rows from the xterm fit addon).
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), String> {
        let terms = self.terms.lock().map_err(|_| "terminal state poisoned".to_string())?;
        let h = terms.get(id.trim()).ok_or_else(|| "terminal closed".to_string())?;
        h.master
            .resize(term_size(Some(cols), Some(rows)))
            .map_err(|e| format!("terminal resize failed: {e}"))
    }

    /// Kill a terminal's shell (by pid, best-effort) and drop the entry.
    pub fn kill(&self, id: &str) -> Result<(), String> {
        let handle = {
            let mut terms = self.terms.lock().map_err(|_| "terminal state poisoned".to_string())?;
            terms.remove(id.trim()).ok_or_else(|| "terminal closed".to_string())?
        };
        if let Some(pid) = handle.pid {
            kill_pid(pid);
        }
        Ok(())
    }

    /// Kill every live pty. Best-effort shutdown for the desktop quit path.
    pub fn kill_all(&self) {
        if let Ok(mut terms) = self.terms.lock() {
            for (_, h) in terms.drain() {
                if let Some(pid) = h.pid {
                    kill_pid(pid);
                }
            }
        }
    }
}

pub fn term_size(cols: Option<u16>, rows: Option<u16>) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: rows.unwrap_or(24).clamp(5, 200),
        cols: cols.unwrap_or(80).clamp(20, 400),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Kill by pid (unix kill -9 / windows taskkill). Best-effort.
pub fn kill_pid(pid: u32) {
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
pub fn spawn_pty<F, G>(
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

pub fn uploads_home_dir() -> Result<std::path::PathBuf, String> {
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

pub fn upload_session_dir(session_id: &str) -> Result<std::path::PathBuf, String> {
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

