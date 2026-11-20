//! Minimal plugin system: install / upgrade / uninstall plugins from a GitHub
//! repo (public or private) or a direct manifest URL. A plugin is a
//! `vicuna-plugin.json` manifest; it can contribute themes (CSS color
//! variables, validated so a plugin can't inject arbitrary CSS) and side
//! panels (one self-contained HTML file each, shipped with the plugin).
//!
//! Panels are served by the server at `/plugin-panel/<id>/<cap>/<file>`,
//! where `<cap>` is a random per-plugin capability minted in memory each
//! server run and only handed out through the authenticated `plugin_list`
//! action. The route needs no bearer token (an iframe can't send one) but
//! serves only files the manifest lists, with a CSP that sandboxes the
//! document (opaque origin, scripts only) and blocks all network access.
//! Panels talk to the app only through the window's postMessage bridge
//! (`src/lib/pluginBridge.ts`).
//!
//! Source resolution for `owner/repo` / `https://github.com/owner/repo`:
//! the latest release's `vicuna-plugin.json` asset, else the file at the
//! default branch. Private repos authenticate with `GH_TOKEN` /
//! `GITHUB_TOKEN`, else `gh auth token` on the server.
//!
//! Installed plugins live in `~/vicuna/plugins/<id>/` (`VICUNA_PLUGINS_DIR`
//! overrides): `vicuna-plugin.json` + `install.json` (source, time).

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::PathBuf;

pub const MANIFEST: &str = "vicuna-plugin.json";
const GITHUB_API: &str = "https://api.github.com";
const MAX_MANIFEST: usize = 256 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Accent {
    pub bg: String,
    #[serde(default)]
    pub hover: Option<String>,
    #[serde(default)]
    pub fg: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    /// Which built-in palette the theme starts from: "light" | "dark".
    pub base: String,
    #[serde(default)]
    pub vars: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub accent: Option<Accent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Panel {
    pub id: String,
    pub title: String,
    /// Optional flat file name of an `.svg`/`.png` shipped with the plugin.
    #[serde(default)]
    pub icon: Option<String>,
    /// Flat file name of the panel's self-contained `.html` document.
    pub entry: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub themes: Vec<Theme>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<Panel>,
}

/// Per-file and total caps for files a plugin ships (panel HTML + icons).
pub const MAX_FILE: usize = 512 * 1024;
pub const MAX_FILES_TOTAL: usize = 2 * 1024 * 1024;
const MAX_PANELS: usize = 8;
/// Plugin storage caps (`plugin_storage_*`).
const MAX_KEY: usize = 128;
const MAX_VALUE: usize = 64 * 1024;
const MAX_STORE: usize = 1024 * 1024;
const MAX_KEYS: usize = 256;

/// The CSP every panel file is served with. `sandbox allow-scripts` makes
/// the document an opaque origin even if opened top-level (no app storage,
/// cookies or same-origin requests); `default-src 'none'` + inline-only
/// script/style + `connect-src 'none'` mean no network at all.
pub const PANEL_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; connect-src 'none'; frame-src 'none'; worker-src 'none'; object-src 'none'; manifest-src 'none'; form-action 'none'; base-uri 'none'";

/// A flat file name with an allowed extension (no paths, no dotfiles).
fn valid_file(name: &str, exts: &[&str]) -> bool {
    let ok_chars = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        && !name.contains("..");
    ok_chars && name.rsplit_once('.').is_some_and(|(_, e)| exts.contains(&e.to_ascii_lowercase().as_str()))
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).as_deref() {
        Some("html") => "text/html; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        _ => "application/octet-stream",
    }
}

impl Manifest {
    /// Every file the plugin ships (panel entries + icons), deduplicated.
    pub fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for p in &self.panels {
            for f in std::iter::once(&p.entry).chain(p.icon.iter()) {
                if !v.contains(f) {
                    v.push(f.clone());
                }
            }
        }
        v
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InstallInfo {
    source: String,
    #[serde(rename = "installedAt")]
    installed_at: i64,
}

fn plugins_dir() -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("VICUNA_PLUGINS_DIR") {
        return Ok(PathBuf::from(p));
    }
    Ok(crate::opencode::home_dir().ok_or("no home folder")?.join("vicuna").join("plugins"))
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') && !s.starts_with('-')
}

/// Only plain colors: `#rgb[a]`/`#rrggbb[aa]`, `rgb(a)(…)`, `hsl(a)(…)` with
/// digits/spaces/`.,%/` inside. No `url()`, `var()`, `;`, braces or quotes.
pub fn valid_color(v: &str) -> bool {
    let v = v.trim();
    if let Some(h) = v.strip_prefix('#') {
        return matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit());
    }
    for f in ["rgb(", "rgba(", "hsl(", "hsla("] {
        if let Some(rest) = v.strip_prefix(f) {
            if let Some(inner) = rest.strip_suffix(')') {
                return !inner.is_empty() && inner.bytes().all(|b| b.is_ascii_digit() || b" .,%/deg".contains(&b));
            }
        }
    }
    false
}

fn valid_var(name: &str) -> bool {
    name.len() <= 64
        && name.strip_prefix("--").is_some_and(|r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

pub fn validate(m: &Manifest) -> Result<(), String> {
    if !valid_id(&m.id) {
        return Err(format!("invalid plugin id {:?} (lowercase letters, digits, dashes)", m.id));
    }
    if m.name.trim().is_empty() || m.version.trim().is_empty() {
        return Err("manifest needs a name and a version".into());
    }
    if m.themes.is_empty() && m.panels.is_empty() {
        return Err("this plugin contributes nothing Vicuna supports yet (themes or panels)".into());
    }
    if m.panels.len() > MAX_PANELS {
        return Err(format!("at most {MAX_PANELS} panels per plugin"));
    }
    let mut seen = std::collections::HashSet::new();
    for p in &m.panels {
        if !valid_id(&p.id) || !seen.insert(p.id.as_str()) {
            return Err(format!("panel {:?}: invalid or duplicate id", p.id));
        }
        let t = p.title.trim();
        if t.is_empty() || t.chars().count() > 40 {
            return Err(format!("panel {}: title must be 1-40 characters", p.id));
        }
        if p.entry.contains("://") || p.entry.starts_with("//") {
            return Err(format!("panel {}: entry must be a file shipped with the plugin, not a URL", p.id));
        }
        if !valid_file(&p.entry, &["html"]) {
            return Err(format!("panel {}: entry must be a flat .html file name (got {:?})", p.id, p.entry));
        }
        if let Some(i) = &p.icon {
            if !valid_file(i, &["svg", "png"]) {
                return Err(format!("panel {}: icon must be a flat .svg/.png file name (got {i:?})", p.id));
            }
        }
    }
    for t in &m.themes {
        if !valid_id(&t.id) || t.name.trim().is_empty() {
            return Err(format!("theme {:?}: invalid id or empty name", t.id));
        }
        if t.base != "light" && t.base != "dark" {
            return Err(format!("theme {}: base must be \"light\" or \"dark\"", t.id));
        }
        for (k, v) in &t.vars {
            if !valid_var(k) {
                return Err(format!("theme {}: invalid variable name {k:?}", t.id));
            }
            if !valid_color(v) {
                return Err(format!("theme {}: {k} must be a color, got {v:?}", t.id));
            }
        }
        if let Some(a) = &t.accent {
            for c in [Some(&a.bg), a.hover.as_ref(), a.fg.as_ref()].into_iter().flatten() {
                if !valid_color(c) {
                    return Err(format!("theme {}: accent must be colors, got {c:?}", t.id));
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Source {
    Repo { owner: String, repo: String },
    Url(String),
}

fn parse_source(s: &str) -> Result<Source, String> {
    let s = s.trim().trim_end_matches('/');
    let repo_part = s
        .strip_prefix("https://github.com/")
        .or_else(|| s.strip_prefix("http://github.com/"))
        .or_else(|| s.strip_prefix("github.com/"));
    let candidate = match repo_part {
        Some(r) => Some(r.trim_end_matches(".git")),
        None if !s.contains("://") => Some(s),
        None => None,
    };
    if let Some(r) = candidate {
        let parts: Vec<&str> = r.split('/').collect();
        let ok = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        if parts.len() == 2 && ok(parts[0]) && ok(parts[1]) {
            return Ok(Source::Repo { owner: parts[0].into(), repo: parts[1].into() });
        }
        // Other github.com URLs only as a direct manifest link (a release
        // download), never a random page.
        if repo_part.is_none() || !s.ends_with(".json") {
            return Err(format!("not a GitHub repo or manifest URL: {s}"));
        }
    }
    if s.starts_with("https://") {
        return Ok(Source::Url(s.to_string()));
    }
    Err(format!("unsupported plugin source {s:?}: use owner/repo, a GitHub URL, or an https manifest URL"))
}

fn github_token() -> Option<String> {
    for k in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(v) = std::env::var(k) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    let mut cmd = std::process::Command::new("gh");
    cmd.args(["auth", "token"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd.output().ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !t.is_empty()).then_some(t)
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("vicuna-plugins")
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

async fn get(c: &reqwest::Client, url: &str, accept: &str, token: Option<&str>) -> Result<Option<Vec<u8>>, String> {
    get_capped(c, url, accept, token, MAX_MANIFEST).await
}

async fn get_capped(c: &reqwest::Client, url: &str, accept: &str, token: Option<&str>, cap: usize) -> Result<Option<Vec<u8>>, String> {
    let mut req = c.get(url).header("Accept", accept);
    if let Some(t) = token {
        req = req.bearer_auth(t);
    }
    let resp = req.send().await.map_err(|e| format!("{url}: {e}"))?;
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    if resp.content_length().is_some_and(|n| n as usize > cap) {
        return Err(format!("{url}: file is larger than {} KB", cap / 1024));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > cap {
        return Err(format!("{url}: file is larger than {} KB", cap / 1024));
    }
    Ok(Some(bytes.to_vec()))
}

/// Where a plugin's other files come from: the same place its manifest did.
enum FileSrc {
    /// Release assets by name → API asset URL.
    Release { assets: Vec<(String, String)>, token: Option<String> },
    /// `contents/<file>` on the default branch.
    Branch { api: String, owner: String, repo: String, token: Option<String> },
    /// Beside a direct manifest URL.
    BaseUrl(String),
}

/// Fetch the manifest for a source (see module docs for the order).
async fn fetch_manifest(source: &str) -> Result<(Manifest, FileSrc), String> {
    let api = std::env::var("VICUNA_PLUGINS_GITHUB_API").unwrap_or_else(|_| GITHUB_API.into());
    let c = client()?;
    let (bytes, src) = match parse_source(source)? {
        Source::Url(u) => {
            let b = get(&c, &u, "application/json", None).await?.ok_or(format!("{u}: not found"))?;
            let base = u.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or(u.clone());
            (b, FileSrc::BaseUrl(base))
        }
        Source::Repo { owner, repo } => {
            let token = github_token();
            let tok = token.as_deref();
            let mut found = None;
            if let Some(rel) = get(&c, &format!("{api}/repos/{owner}/{repo}/releases/latest"), "application/vnd.github+json", tok).await? {
                let rel: Value = serde_json::from_slice(&rel).map_err(|e| e.to_string())?;
                let assets: Vec<(String, String)> = rel["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|a| Some((a["name"].as_str()?.to_string(), a["url"].as_str()?.to_string())))
                    .collect();
                if let Some((_, url)) = assets.iter().find(|(n, _)| n == MANIFEST) {
                    if let Some(b) = get(&c, url, "application/octet-stream", tok).await? {
                        found = Some((b, FileSrc::Release { assets: assets.clone(), token: token.clone() }));
                    }
                }
            }
            if found.is_none() {
                if let Some(b) = get(&c, &format!("{api}/repos/{owner}/{repo}/contents/{MANIFEST}"), "application/vnd.github.raw", tok).await? {
                    found = Some((b, FileSrc::Branch { api: api.clone(), owner: owner.clone(), repo: repo.clone(), token: token.clone() }));
                }
            }
            found.ok_or_else(|| {
                let hint = if tok.is_none() { " (private repo? sign in with `gh auth login` or set GH_TOKEN on the server)" } else { "" };
                format!("no {MANIFEST} found in {owner}/{repo} (latest release asset or default branch){hint}")
            })?
        }
    };
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("invalid {MANIFEST}: {e}"))?;
    validate(&m)?;
    Ok((m, src))
}

/// Download every file the manifest ships, from where the manifest came
/// from, within the per-file and total caps.
async fn fetch_files(m: &Manifest, src: &FileSrc) -> Result<Vec<(String, Vec<u8>)>, String> {
    let c = client()?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for name in m.files() {
        let bytes = match src {
            FileSrc::Release { assets, token } => {
                let url = assets
                    .iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, u)| u.clone())
                    .ok_or_else(|| format!("the release has no asset named {name} (listed in {MANIFEST})"))?;
                get_capped(&c, &url, "application/octet-stream", token.as_deref(), MAX_FILE).await?
            }
            FileSrc::Branch { api, owner, repo, token } => {
                get_capped(&c, &format!("{api}/repos/{owner}/{repo}/contents/{name}"), "application/vnd.github.raw", token.as_deref(), MAX_FILE).await?
            }
            FileSrc::BaseUrl(base) => get_capped(&c, &format!("{base}/{name}"), "*/*", None, MAX_FILE).await?,
        }
        .ok_or_else(|| format!("{name} (listed in {MANIFEST}) was not found"))?;
        total += bytes.len();
        if total > MAX_FILES_TOTAL {
            return Err(format!("plugin files exceed {} MB in total", MAX_FILES_TOTAL / 1024 / 1024));
        }
        out.push((name, bytes));
    }
    Ok(out)
}

/// Install a plugin as a whole: build it in a staging folder, then swap it
/// in, so an upgrade never leaves old and new files mixed. The plugin's own
/// storage (`data.json`) carries over.
fn write_plugin(m: &Manifest, source: &str, files: &[(String, Vec<u8>)]) -> Result<(), String> {
    let root = plugins_dir()?;
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let dir = root.join(&m.id);
    let stage = root.join(format!(".{}.staging-{}", m.id, std::process::id()));
    let old = root.join(format!(".{}.old-{}", m.id, std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join("files")).map_err(|e| e.to_string())?;
    let result = (|| -> Result<(), String> {
        std::fs::write(stage.join(MANIFEST), serde_json::to_vec_pretty(m).unwrap()).map_err(|e| e.to_string())?;
        let info = InstallInfo { source: source.trim().to_string(), installed_at: chrono_now() };
        std::fs::write(stage.join("install.json"), serde_json::to_vec_pretty(&info).unwrap()).map_err(|e| e.to_string())?;
        for (name, bytes) in files {
            if !m.files().contains(name) {
                return Err(format!("unexpected file {name}"));
            }
            std::fs::write(stage.join("files").join(name), bytes).map_err(|e| e.to_string())?;
        }
        if let Ok(data) = std::fs::read(dir.join(DATA)) {
            std::fs::write(stage.join(DATA), data).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(e);
    }
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(&dir, &old).map_err(|e| e.to_string())?;
    }
    if let Err(e) = std::fs::rename(&stage, &dir) {
        let _ = std::fs::rename(&old, &dir);
        return Err(e.to_string());
    }
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

const DATA: &str = "data.json";

fn chrono_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn read_installed(id: &str) -> Option<(Manifest, InstallInfo)> {
    let dir = plugins_dir().ok()?.join(id);
    let m = serde_json::from_slice(&std::fs::read(dir.join(MANIFEST)).ok()?).ok()?;
    let i = serde_json::from_slice(&std::fs::read(dir.join("install.json")).ok()?).ok()?;
    Some((m, i))
}

fn entry(m: &Manifest, i: &InstallInfo) -> Value {
    let mut e = json!({ "id": m.id, "name": m.name, "version": m.version, "description": m.description,
            "source": i.source, "installedAt": i.installed_at, "themes": m.themes, "panels": m.panels });
    if !m.panels.is_empty() {
        // server-relative; the window prefixes its backend's base URL
        e["panelBase"] = json!(format!("/plugin-panel/{}/{}/", m.id, capability(&m.id)));
    }
    e
}

/// Random per-plugin capability for the panel route, minted once per
/// server run and kept in memory only.
fn capability(id: &str) -> String {
    static CAPS: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, String>>> = std::sync::OnceLock::new();
    let caps = CAPS.get_or_init(Default::default);
    let mut g = caps.lock().unwrap_or_else(|e| e.into_inner());
    g.entry(id.to_string())
        .or_insert_with(|| {
            let mut b = [0u8; 16];
            getrandom::fill(&mut b).expect("os randomness");
            b.iter().map(|x| format!("{x:02x}")).collect()
        })
        .clone()
}

fn cap_eq(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// A panel file response: status, content type, body. Only files the
/// installed manifest lists are served, and only with the right capability.
pub fn serve_panel(path: &str) -> (u16, &'static str, Vec<u8>) {
    let not_found = (404, "text/plain; charset=utf-8", b"not found".to_vec());
    let Some(rest) = path.strip_prefix("/plugin-panel/") else { return not_found };
    let parts: Vec<&str> = rest.split('/').collect();
    let [id, cap, file] = parts[..] else { return not_found };
    if !valid_id(id) || !cap_eq(cap, &capability(id)) {
        return not_found;
    }
    let Some((m, _)) = read_installed(id) else { return not_found };
    if !m.files().iter().any(|f| f == file) {
        return not_found;
    }
    let Ok(dir) = plugins_dir() else { return not_found };
    match std::fs::read(dir.join(id).join("files").join(file)) {
        Ok(b) if b.len() <= MAX_FILE => (200, content_type(file), b),
        _ => not_found,
    }
}

pub fn list() -> Value {
    let mut out = Vec::new();
    if let Ok(rd) = plugins_dir().and_then(|d| std::fs::read_dir(d).map_err(|e| e.to_string())) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !valid_id(&name) {
                continue; // staging/old folders from an interrupted swap
            }
            if let Some((m, i)) = read_installed(&name) {
                out.push(entry(&m, &i));
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    Value::Array(out)
}

pub async fn install(source: &str) -> Result<Value, String> {
    let (m, src) = fetch_manifest(source).await?;
    if let Some((_, i)) = read_installed(&m.id) {
        if i.source.trim() != source.trim() {
            return Err(format!("a plugin with id {:?} is already installed from {} — uninstall it first", m.id, i.source));
        }
    }
    let files = fetch_files(&m, &src).await?;
    write_plugin(&m, source, &files)?;
    let (m, i) = read_installed(&m.id).ok_or("install failed")?;
    Ok(entry(&m, &i))
}

pub async fn upgrade(id: &str) -> Result<Value, String> {
    let (old, info) = read_installed(id).ok_or_else(|| format!("plugin {id} is not installed"))?;
    let (m, src) = fetch_manifest(&info.source).await?;
    if m.id != old.id {
        return Err(format!("the source now publishes plugin {:?}, not {:?}", m.id, old.id));
    }
    let files = fetch_files(&m, &src).await?;
    let dir = plugins_dir()?.join(id).join("files");
    let files_changed = files.iter().any(|(n, b)| std::fs::read(dir.join(n)).ok().as_deref() != Some(b.as_slice()));
    let changed = m != old || files_changed;
    if changed {
        write_plugin(&m, &info.source, &files)?;
    }
    let (m, i) = read_installed(id).ok_or("upgrade failed")?;
    let mut e = entry(&m, &i);
    e["previousVersion"] = json!(old.version);
    e["changed"] = json!(changed);
    Ok(e)
}

pub fn uninstall(id: &str) -> Result<Value, String> {
    if !valid_id(id) {
        return Err("invalid plugin id".into());
    }
    let dir = plugins_dir()?.join(id);
    if !dir.join(MANIFEST).exists() {
        return Err(format!("plugin {id} is not installed"));
    }
    std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(json!({ "id": id, "removed": true }))
}

fn read_store(id: &str) -> serde_json::Map<String, Value> {
    plugins_dir()
        .ok()
        .and_then(|d| std::fs::read(d.join(id).join(DATA)).ok())
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= MAX_KEY && !k.chars().any(|c| c.is_control())
}

/// Plugin-scoped key/value storage (`~/vicuna/plugins/<id>/data.json`).
/// The window binds `id` to the panel that asked; a panel never names it.
pub fn storage(cmd: &str, id: &str, key: Option<&str>, value: Option<&Value>) -> Result<Value, String> {
    if !valid_id(id) || read_installed(id).is_none() {
        return Err(format!("plugin {id} is not installed"));
    }
    let mut store = read_store(id);
    match cmd {
        "plugin_storage_get" => {
            let k = key.ok_or("missing key")?;
            Ok(store.get(k).cloned().unwrap_or(Value::Null))
        }
        "plugin_storage_keys" => Ok(json!(store.keys().collect::<Vec<_>>())),
        "plugin_storage_set" | "plugin_storage_remove" => {
            let k = key.ok_or("missing key")?;
            if !valid_key(k) {
                return Err(format!("keys are 1-{MAX_KEY} characters, no control characters"));
            }
            match (cmd, value) {
                ("plugin_storage_set", Some(v)) if !v.is_null() => {
                    if serde_json::to_vec(v).map(|b| b.len()).unwrap_or(usize::MAX) > MAX_VALUE {
                        return Err(format!("a value is at most {} KB", MAX_VALUE / 1024));
                    }
                    if !store.contains_key(k) && store.len() >= MAX_KEYS {
                        return Err(format!("at most {MAX_KEYS} keys per plugin"));
                    }
                    store.insert(k.to_string(), v.clone());
                }
                _ => {
                    store.remove(k);
                }
            }
            let bytes = serde_json::to_vec(&store).map_err(|e| e.to_string())?;
            if bytes.len() > MAX_STORE {
                return Err(format!("plugin storage is limited to {} KB", MAX_STORE / 1024));
            }
            let path = plugins_dir()?.join(id).join(DATA);
            let tmp = path.with_extension("json.tmp");
            std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
            Ok(json!(true))
        }
        _ => Err(format!("unknown plugin storage action {cmd}")),
    }
}

/// Server API actions: `plugin_list | plugin_install {source} |
/// plugin_upgrade {id} | plugin_uninstall {id} | plugin_storage_get|keys|
/// set|remove {id, key?, value?}`.
pub async fn action(cmd: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or(format!("missing {k}"));
    match cmd {
        "plugin_list" => Ok(list()),
        "plugin_install" => install(&s("source")?).await,
        "plugin_upgrade" => upgrade(&s("id")?).await,
        "plugin_uninstall" => uninstall(&s("id")?),
        "plugin_storage_get" | "plugin_storage_keys" | "plugin_storage_set" | "plugin_storage_remove" => {
            let key = args.get("key").and_then(|v| v.as_str());
            storage(cmd, &s("id")?, key, args.get("value"))
        }
        _ => Err(format!("unknown plugin action {cmd}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_parse() {
        let repo = |o: &str, r: &str| Source::Repo { owner: o.into(), repo: r.into() };
        assert_eq!(parse_source("me/theme").unwrap(), repo("me", "theme"));
        assert_eq!(parse_source("https://github.com/me/theme.git/").unwrap(), repo("me", "theme"));
        assert_eq!(parse_source("github.com/me/t-1").unwrap(), repo("me", "t-1"));
        assert_eq!(parse_source("https://x.dev/p.json").unwrap(), Source::Url("https://x.dev/p.json".into()));
        assert!(parse_source("http://x.dev/p.json").is_err());
        assert!(parse_source("a/b/c").is_err());
        assert!(parse_source("https://github.com/a/b/c").is_err());
    }

    #[test]
    fn colors_and_vars_are_strict() {
        for ok in ["#fff", "#11223344", "rgb(1, 2, 3)", "hsla(210deg 40% 20% / 0.5)"] {
            assert!(valid_color(ok), "{ok}");
        }
        for bad in ["red", "#ggg", "url(x)", "rgb(1,2,3);x", "var(--a)", "#fff}"] {
            assert!(!valid_color(bad), "{bad}");
        }
        assert!(valid_var("--bg-2") && !valid_var("bg") && !valid_var("--a;b") && !valid_var("--"));
    }

    fn manifest() -> Manifest {
        serde_json::from_value(json!({"id":"nord","name":"Nord","version":"1.0.0",
            "themes":[{"id":"nord-dark","name":"Nord","base":"dark","vars":{"--bg":"#2e3440"},"accent":{"bg":"#88c0d0"}}]})).unwrap()
    }

    #[test]
    fn validation() {
        assert!(validate(&manifest()).is_ok());
        let mut m = manifest();
        m.themes[0].vars.insert("--x".into(), "url(evil)".into());
        assert!(validate(&m).is_err());
        let mut m = manifest();
        m.themes[0].base = "sepia".into();
        assert!(validate(&m).is_err());
        let mut m = manifest();
        m.id = "../x".into();
        assert!(validate(&m).is_err());
        let mut m = manifest();
        m.themes.clear();
        assert!(validate(&m).is_err());
    }

    #[test]
    fn panel_validation() {
        let panel = |entry: &str, icon: Option<&str>| Panel { id: "notes".into(), title: "Notes".into(), icon: icon.map(String::from), entry: entry.into() };
        let with = |p: Vec<Panel>| {
            let mut m = manifest();
            m.themes.clear();
            m.panels = p;
            validate(&m)
        };
        assert!(with(vec![panel("notes.html", Some("notes.svg"))]).is_ok());
        assert!(with(vec![panel("index.HTML", Some("i.png"))]).is_ok());
        for bad in ["https://evil.example/p.html", "//evil/p.html", "../p.html", "dir/p.html", "p.js", ".p.html", "p.html?x=1", ""] {
            assert!(with(vec![panel(bad, None)]).is_err(), "{bad}");
        }
        assert!(with(vec![panel("p.html", Some("../i.svg"))]).is_err());
        assert!(with(vec![panel("p.html", Some("i.html"))]).is_err());
        assert!(with(vec![panel("p.html", None), panel("q.html", None)]).is_err(), "duplicate ids");
        let mut t = panel("p.html", None);
        t.title = "x".repeat(41);
        assert!(with(vec![t]).is_err());
        assert!(with((0..9).map(|i| Panel { id: format!("p{i}"), ..panel("p.html", None) }).collect()).is_err());
        let mut m = manifest();
        m.panels = vec![panel("notes.html", Some("notes.svg")), Panel { id: "b".into(), ..panel("notes.html", None) }];
        assert_eq!(m.files(), vec!["notes.html".to_string(), "notes.svg".to_string()]);
    }

    #[test]
    fn panel_csp_locks_the_document_down() {
        for d in ["sandbox allow-scripts;", "default-src 'none'", "connect-src 'none'", "form-action 'none'", "frame-src 'none'", "base-uri 'none'"] {
            assert!(PANEL_CSP.contains(d), "{d}");
        }
        assert!(!PANEL_CSP.contains("allow-same-origin") && !PANEL_CSP.contains("'self'") && !PANEL_CSP.contains("http"));
    }

    /// Install → list → upgrade → uninstall against a fake GitHub API (the
    /// manifest comes from the default-branch contents fallback).
    #[tokio::test]
    async fn install_upgrade_uninstall_roundtrip() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!("vicuna-plugins-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let version = std::sync::Arc::new(std::sync::Mutex::new("1.0.0".to_string()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let v2 = version.clone();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let v = v2.lock().unwrap().clone();
                let (code, body) = if req.starts_with("GET /repos/me/nord/contents/vicuna-plugin.json") {
                    let mut m = serde_json::to_value(manifest()).unwrap();
                    m["version"] = json!(v);
                    m["panels"] = json!([{"id": "notes", "title": "Notes", "entry": "notes.html", "icon": "notes.svg"}]);
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /repos/me/nord/contents/notes.html") {
                    ("200 OK", format!("<p>notes {v}</p>"))
                } else if req.starts_with("GET /repos/me/nord/contents/notes.svg") {
                    ("200 OK", "<svg/>".to_string())
                } else if req.starts_with("GET /repos/me/huge/contents/vicuna-plugin.json") {
                    let mut m = serde_json::to_value(manifest()).unwrap();
                    m["id"] = json!("huge");
                    m["panels"] = json!([{"id": "big", "title": "Big", "entry": "big.html"}]);
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /repos/me/huge/contents/big.html") {
                    ("200 OK", "x".repeat(MAX_FILE + 1))
                } else {
                    ("404 Not Found", "{}".to_string())
                };
                let resp = format!("HTTP/1.1 {code}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(resp.as_bytes()).await;
            }
        });
        // SAFETY: only this test reads these variables.
        unsafe {
            std::env::set_var("VICUNA_PLUGINS_DIR", &dir);
            std::env::set_var("VICUNA_PLUGINS_GITHUB_API", format!("http://127.0.0.1:{port}"));
            std::env::set_var("GH_TOKEN", "test");
        }

        let e = action("plugin_install", &json!({"source": "me/nord"})).await.unwrap();
        assert_eq!(e["version"], "1.0.0");
        assert_eq!(list().as_array().unwrap().len(), 1);
        // panel files are served only under the capability, only listed files
        let base = e["panelBase"].as_str().unwrap().to_string();
        assert!(base.starts_with("/plugin-panel/nord/") && base.len() > "/plugin-panel/nord/".len() + 30, "{base}");
        let (st, ct, body) = serve_panel(&format!("{base}notes.html"));
        assert_eq!((st, ct, body.as_slice()), (200, "text/html; charset=utf-8", b"<p>notes 1.0.0</p>".as_slice()));
        assert_eq!(serve_panel(&format!("{base}notes.svg")).1, "image/svg+xml");
        assert_eq!(serve_panel("/plugin-panel/nord/00000000000000000000000000000000/notes.html").0, 404);
        assert_eq!(serve_panel(&format!("{base}vicuna-plugin.json")).0, 404);
        assert_eq!(serve_panel(&format!("{base}..%2Finstall.json")).0, 404);
        assert_eq!(serve_panel(&format!("{base}../install.json")).0, 404);
        // scoped storage survives an upgrade
        action("plugin_storage_set", &json!({"id": "nord", "key": "note:/ws", "value": "hello"})).await.unwrap();
        assert_eq!(action("plugin_storage_get", &json!({"id": "nord", "key": "note:/ws"})).await.unwrap(), json!("hello"));
        assert_eq!(action("plugin_storage_keys", &json!({"id": "nord"})).await.unwrap(), json!(["note:/ws"]));
        assert!(action("plugin_storage_set", &json!({"id": "nord", "key": "big", "value": "x".repeat(MAX_VALUE)})).await.is_err());
        assert!(action("plugin_storage_get", &json!({"id": "other", "key": "k"})).await.is_err());
        let e = action("plugin_upgrade", &json!({"id": "nord"})).await.unwrap();
        assert_eq!(e["changed"], false);
        *version.lock().unwrap() = "1.1.0".into();
        let e = action("plugin_upgrade", &json!({"id": "nord"})).await.unwrap();
        assert_eq!((e["changed"].clone(), e["version"].clone(), e["previousVersion"].clone()), (json!(true), json!("1.1.0"), json!("1.0.0")));
        assert_eq!(serve_panel(&format!("{base}notes.html")).2, b"<p>notes 1.1.0</p>");
        assert_eq!(action("plugin_storage_get", &json!({"id": "nord", "key": "note:/ws"})).await.unwrap(), json!("hello"));
        action("plugin_storage_remove", &json!({"id": "nord", "key": "note:/ws"})).await.unwrap();
        assert_eq!(action("plugin_storage_get", &json!({"id": "nord", "key": "note:/ws"})).await.unwrap(), Value::Null);
        // an oversized panel file refuses the whole install
        let err = action("plugin_install", &json!({"source": "me/huge"})).await.unwrap_err();
        assert!(err.contains("larger than"), "{err}");
        assert_eq!(list().as_array().unwrap().len(), 1);
        assert!(action("plugin_install", &json!({"source": "me/missing"})).await.is_err());
        action("plugin_uninstall", &json!({"id": "nord"})).await.unwrap();
        assert!(list().as_array().unwrap().is_empty());
        assert_eq!(serve_panel(&format!("{base}notes.html")).0, 404);
        assert!(action("plugin_uninstall", &json!({"id": "nord"})).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
