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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MANIFEST: &str = "vicuna-plugin.json";
const GITHUB_API: &str = "https://api.github.com";
const MAX_MANIFEST: usize = 256 * 1024;
const MAX_BINARY: usize = 64 * 1024 * 1024;

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
#[serde(rename_all = "camelCase")]
pub struct SetupStep {
    pub text: String,
    #[serde(default)]
    pub link_text: Option<String>,
    #[serde(default)]
    pub link_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct BridgeSpec {
    /// Display name shown in menus/modals (defaults to plugin name if empty).
    #[serde(default)]
    pub name: String,
    /// Brand accent color for the bridge icon (validated with `valid_color`).
    #[serde(default)]
    pub color: Option<String>,
    /// Safe SVG `<path d="...">` for a 24x24 viewBox.
    #[serde(default)]
    pub icon_path: Option<String>,
    /// Optional relative `.svg` file shipped with the plugin (rendered full-color inside the badge).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// Optional background color for the circular badge when `icon` is rendered (defaults to `#ffffff` in UI).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_bg: Option<String>,
    /// Placeholder text for the token input field.
    #[serde(default)]
    pub token_placeholder: String,
    /// Authentication mode: `"token"` (default, bot token input) or `"qr"` (linked-device QR flow).
    #[serde(default)]
    pub auth_mode: Option<String>,
    /// Short setup instructions shown before saving a token.
    #[serde(default)]
    pub setup_steps: Vec<SetupStep>,
    /// Command prefix shown in the pairing box (e.g. "!pair", "/start").
    #[serde(default)]
    pub pair_command: String,
    /// Whether this bridge supports voice memos (shows shared Whisper settings).
    #[serde(default)]
    pub supports_voice: bool,
    /// Platform -> release asset filename (e.g. "linux-x86_64" -> "vicuna-bridge-...").
    #[serde(default)]
    pub binaries: BTreeMap<String, String>,
    /// Optional relative path to the executable inside a local plugin folder.
    #[serde(default)]
    pub entrypoint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub themes: Vec<Theme>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub panels: Vec<Panel>,
    /// Work harnesses (ACP agents); see `plugin_harness.rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub harnesses: Vec<crate::plugin_harness::Harness>,
    /// MCP servers (http or stdio); see `plugin_mcp.rs`.
    #[serde(default, rename = "mcpServers", skip_serializing_if = "Vec::is_empty")]
    pub mcp_servers: Vec<crate::plugin_mcp::PluginMcpServer>,
    /// Messaging bridge (out-of-process binary speaking JSON-RPC over stdio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge: Option<BridgeSpec>,
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
        for f in self
            .icon
            .iter()
            .chain(self.harnesses.iter().filter_map(|h| h.icon.as_ref()))
            .chain(self.bridge.iter().filter_map(|b| b.icon.as_ref()))
        {
            if crate::plugin_icon::valid_path(f) && !v.contains(f) {
                v.push(f.clone());
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

pub fn valid_icon_path(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty()
        && p.len() <= 2048
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b" .,-".contains(&b))
}

pub(crate) fn valid_rel_path(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty()
        && !p.starts_with('/')
        && !p.starts_with('\\')
        && !p.contains("..")
        && !p.contains(':')
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_./".contains(&b))
}

pub(crate) fn valid_asset_name(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty()
        && !p.contains('/')
        && !p.contains('\\')
        && !p.contains("..")
        && p.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

pub fn current_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "windows" => "windows",
        other => other,
    };
    let arch = std::env::consts::ARCH;
    format!("{os}-{arch}")
}

pub fn validate(m: &Manifest) -> Result<(), String> {
    if !valid_id(&m.id) {
        return Err(format!("invalid plugin id {:?} (lowercase letters, digits, dashes)", m.id));
    }
    if m.name.trim().is_empty() || m.version.trim().is_empty() {
        return Err("manifest needs a name and a version".into());
    }
    if m.themes.is_empty() && m.panels.is_empty() && m.harnesses.is_empty() && m.mcp_servers.is_empty() && m.bridge.is_none() {
        return Err("this plugin contributes nothing Vicuna supports yet (themes, panels, harnesses, MCP servers or bridge)".into());
    }
    crate::plugin_harness::validate(&m.harnesses)?;
    crate::plugin_mcp::validate(&m.id, &m.mcp_servers)?;
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
    if let Some(b) = &m.bridge {
        if let Some(c) = &b.color {
            if !valid_color(c) {
                return Err(format!("bridge: color must be a color, got {c:?}"));
            }
        }
        if let Some(p) = &b.icon_path {
            if !valid_icon_path(p) {
                return Err("bridge: iconPath contains disallowed characters".into());
            }
        }
        if let Some(icon) = &b.icon {
            if !crate::plugin_icon::valid_path(icon) {
                return Err(format!("bridge: icon must be a safe relative .svg path, got {icon:?}"));
            }
        }
        if let Some(bg) = &b.icon_bg {
            if !valid_color(bg) {
                return Err(format!("bridge: iconBg must be a color, got {bg:?}"));
            }
        }
        if let Some(mode) = &b.auth_mode {
            if !matches!(mode.as_str(), "token" | "qr") {
                return Err(format!("bridge: authMode must be \"token\" or \"qr\", got {mode:?}"));
            }
        }
        for step in &b.setup_steps {
            if let Some(u) = &step.link_url {
                if !u.starts_with("https://") {
                    return Err(format!("bridge: setup step link must be https://, got {u:?}"));
                }
            }
        }
        if b.binaries.is_empty() && b.entrypoint.as_deref().unwrap_or("").trim().is_empty() {
            return Err("bridge: needs at least one binary or entrypoint".into());
        }
        for (plat, asset) in &b.binaries {
            if !valid_id(plat) && !plat.bytes().all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || x == b'-' || x == b'_') {
                return Err(format!("bridge: invalid platform key {plat:?}"));
            }
            if !valid_asset_name(asset) {
                return Err(format!("bridge: invalid binary asset name {asset:?}"));
            }
        }
        if let Some(ep) = &b.entrypoint {
            if !valid_rel_path(ep) {
                return Err(format!("bridge: invalid entrypoint {ep:?}"));
            }
        }
    }
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Source {
    Repo { owner: String, repo: String },
    Url(String),
    Local(PathBuf),
}

fn parse_source(s: &str) -> Result<Source, String> {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix("file://") {
        let p = PathBuf::from(rest);
        if p.is_absolute() {
            return Ok(Source::Local(p));
        }
    }
    if s.starts_with('/') || s.starts_with("./") || s.starts_with("../") || (s.len() >= 3 && s.as_bytes()[1] == b':' && (s.as_bytes()[2] == b'\\' || s.as_bytes()[2] == b'/')) {
        return Ok(Source::Local(PathBuf::from(s)));
    }
    let s = s.trim_end_matches('/');
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
    /// A local folder on disk.
    Local(PathBuf),
}

/// Fetch the manifest for a source (see module docs for the order).
async fn fetch_manifest(source: &str) -> Result<(Manifest, FileSrc), String> {
    let api = std::env::var("VICUNA_PLUGINS_GITHUB_API").unwrap_or_else(|_| GITHUB_API.into());
    let (bytes, src) = match parse_source(source)? {
        Source::Local(dir) => {
            let manifest_path = if dir.is_file() { dir.clone() } else { dir.join(MANIFEST) };
            let root = if dir.is_file() {
                dir.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."))
            } else {
                dir.clone()
            };
            let b = std::fs::read(&manifest_path)
                .map_err(|e| format!("cannot read {}: {e}", manifest_path.display()))?;
            if b.len() > MAX_MANIFEST {
                return Err("manifest is too large".into());
            }
            (b, FileSrc::Local(root))
        }
        Source::Url(u) => {
            let c = client()?;
            let b = get(&c, &u, "application/json", None).await?.ok_or(format!("{u}: not found"))?;
            let base = u.rsplit_once('/').map(|(b, _)| b.to_string()).unwrap_or(u.clone());
            (b, FileSrc::BaseUrl(base))
        }
        Source::Repo { owner, repo } => {
            let c = client()?;
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
            FileSrc::Local(root) => {
                let p = root.join(&name);
                let b = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
                if b.len() > MAX_FILE {
                    return Err(format!("{name}: file is larger than {} KB", MAX_FILE / 1024));
                }
                Some(b)
            }
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

/// Fetch the bridge plugin executable for this platform (if the manifest defines a bridge).
async fn fetch_bridge_binary(m: &Manifest, src: &FileSrc) -> Result<Option<(String, Vec<u8>)>, String> {
    let Some(b) = &m.bridge else { return Ok(None) };
    let plat = current_platform();
    match src {
        FileSrc::Local(root) => {
            let candidate = b.binaries.get(&plat).map(|n| root.join(n))
                .filter(|p| p.is_file())
                .or_else(|| b.entrypoint.as_deref().map(|ep| root.join(ep)).filter(|p| p.is_file()));
            let bin_path = candidate.ok_or_else(|| {
                let tried = b.entrypoint.as_deref().or_else(|| b.binaries.get(&plat).map(String::as_str)).unwrap_or("(none)");
                format!("bridge binary not found in {} (expected {tried}; build it first)", root.display())
            })?;
            let bytes = std::fs::read(&bin_path)
                .map_err(|e| format!("cannot read bridge binary {}: {e}", bin_path.display()))?;
            if bytes.len() > MAX_BINARY {
                return Err("bridge binary exceeds 64 MB limit".into());
            }
            let fname = bin_path.file_name().and_then(|f| f.to_str()).unwrap_or("bridge").to_string();
            Ok(Some((fname, bytes)))
        }
        FileSrc::Release { assets, token } => {
            let asset_name = b.binaries.get(&plat).ok_or_else(|| {
                format!("this bridge plugin does not publish a binary for {plat}")
            })?;
            let url = assets
                .iter()
                .find(|(n, _)| n == asset_name)
                .map(|(_, u)| u.clone())
                .ok_or_else(|| format!("release asset {asset_name} not found"))?;
            let c = client()?;
            let bytes = get_capped(&c, &url, "application/octet-stream", token.as_deref(), MAX_BINARY)
                .await?
                .ok_or_else(|| format!("release asset {asset_name} returned 404"))?;
            Ok(Some((asset_name.clone(), bytes)))
        }
        FileSrc::Branch { .. } => {
            Err("bridge plugins require a GitHub release with platform binaries (or a local folder install)".into())
        }
        FileSrc::BaseUrl(_) => {
            Err("bridge plugins must be installed from a GitHub repo (owner/repo) or a local folder".into())
        }
    }
}

/// Fetch this platform's bundled binary for every stdio MCP server that
/// declares `binaries`/`entrypoint`. Two servers may share an asset name only
/// when the bytes are identical.
async fn fetch_mcp_binaries(m: &Manifest, src: &FileSrc) -> Result<Vec<(String, Vec<u8>)>, String> {
    let plat = current_platform();
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    for s in m.mcp_servers.iter().filter(|s| s.transport == "stdio" && s.bundled()) {
        let (name, bytes) = match src {
            FileSrc::Local(root) => {
                let p = s.binaries.get(&plat).map(|n| root.join(n)).filter(|p| p.is_file())
                    .or_else(|| s.entrypoint.as_deref().map(|ep| root.join(ep)).filter(|p| p.is_file()))
                    .ok_or_else(|| {
                        let tried = s.binaries.get(&plat).map(String::as_str).or(s.entrypoint.as_deref()).unwrap_or("(none)");
                        format!("MCP server {}: no binary for platform {plat} in {} (expected {tried}; build it first)", s.id, root.display())
                    })?;
                let bytes = std::fs::read(&p).map_err(|e| format!("cannot read MCP binary {}: {e}", p.display()))?;
                if bytes.len() > MAX_BINARY {
                    return Err(format!("MCP server {}: binary exceeds 64 MB limit", s.id));
                }
                (p.file_name().and_then(|f| f.to_str()).unwrap_or(&s.id).to_string(), bytes)
            }
            FileSrc::Release { assets, token } => {
                let asset = s.binaries.get(&plat)
                    .ok_or_else(|| format!("MCP server {}: no binary for platform {plat}", s.id))?;
                let url = assets.iter().find(|(n, _)| n == asset).map(|(_, u)| u.clone())
                    .ok_or_else(|| format!("MCP server {}: release asset {asset} not found", s.id))?;
                let bytes = get_capped(&client()?, &url, "application/octet-stream", token.as_deref(), MAX_BINARY)
                    .await?
                    .ok_or_else(|| format!("MCP server {}: release asset {asset} returned 404", s.id))?;
                (asset.clone(), bytes)
            }
            FileSrc::Branch { .. } | FileSrc::BaseUrl(_) => {
                return Err(format!("MCP server {}: bundled binaries need a GitHub release (or a local folder install)", s.id));
            }
        };
        match out.iter().find(|(n, _)| *n == name) {
            Some((_, b)) if *b != bytes => return Err(format!("MCP servers ship two different binaries named {name}")),
            Some(_) => {}
            None => out.push((name, bytes)),
        }
    }
    Ok(out)
}

/// Every executable a plugin installs into `bin/` (bridge + MCP servers).
async fn fetch_binaries(m: &Manifest, src: &FileSrc) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = fetch_mcp_binaries(m, src).await?;
    if let Some((name, bytes)) = fetch_bridge_binary(m, src).await? {
        match out.iter().find(|(n, _)| *n == name) {
            Some((_, b)) if *b != bytes => return Err(format!("the bridge and an MCP server ship two different binaries named {name}")),
            Some(_) => {}
            None => out.push((name, bytes)),
        }
    }
    Ok(out)
}

/// Install a plugin as a whole: build it in a staging folder, then swap it
/// in, so an upgrade never leaves old and new files mixed. The plugin's own
/// storage (`data.json`) carries over.
fn write_plugin(m: &Manifest, source: &str, files: &[(String, Vec<u8>)], binaries: &[(String, Vec<u8>)]) -> Result<(), String> {
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
            let target = stage.join("files").join(name);
            std::fs::create_dir_all(target.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::write(target, bytes).map_err(|e| e.to_string())?;
        }
        for (fname, bytes) in binaries {
            let bin_dir = stage.join("bin");
            std::fs::create_dir_all(&bin_dir).map_err(|e| e.to_string())?;
            let target = bin_dir.join(fname);
            std::fs::write(&target, bytes).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755));
            }
        }
        // plugin storage + harness consent carry over; consent is bound to
        // the exact command, so a changed command is disabled until approved
        for keep in [DATA, "consent.json", crate::plugin_mcp::CONSENT_FILE] {
            if let Ok(data) = std::fs::read(dir.join(keep)) {
                std::fs::write(stage.join(keep), data).map_err(|e| e.to_string())?;
            }
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

pub fn now_ms() -> i64 {
    chrono_now()
}

/// An installed plugin's manifest and folder.
pub fn installed_manifest(id: &str) -> Option<(Manifest, PathBuf)> {
    if !valid_id(id) {
        return None;
    }
    let (m, _) = read_installed(id)?;
    Some((m, plugins_dir().ok()?.join(id)))
}

pub fn installed_manifests() -> Vec<(Manifest, PathBuf)> {
    let mut out = Vec::new();
    if let Ok(rd) = plugins_dir().and_then(|d| std::fs::read_dir(d).map_err(|e| e.to_string())) {
        for e in rd.flatten() {
            if let Some(x) = installed_manifest(&e.file_name().to_string_lossy()) {
                out.push(x);
            }
        }
    }
    out
}

/// Resolve the staged executable path for an installed bridge plugin.
pub fn bridge_binary_path(id: &str, spec: &BridgeSpec) -> Option<PathBuf> {
    let dir = plugins_dir().ok()?.join(id);
    let bin_dir = dir.join("bin");
    let plat = current_platform();
    if let Some(asset) = spec.binaries.get(&plat) {
        let p = bin_dir.join(asset);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(ep) = &spec.entrypoint {
        if let Some(fname) = Path::new(ep).file_name() {
            let p = bin_dir.join(fname);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(&bin_dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// List every installed bridge plugin along with its resolved executable path.
pub fn list_installed_bridges() -> Vec<(Manifest, PathBuf)> {
    let mut out = Vec::new();
    for (m, _) in installed_manifests() {
        if let Some(spec) = &m.bridge {
            if let Some(bin) = bridge_binary_path(&m.id, spec) {
                out.push((m, bin));
            }
        }
    }
    out.sort_by(|a, b| a.0.id.cmp(&b.0.id));
    out
}

fn entry(m: &Manifest, i: &InstallInfo) -> Value {
    let dir = plugins_dir().ok().map(|d| d.join(&m.id));
    entry_in_dir(m, i, dir.as_deref())
}

fn entry_in_dir(m: &Manifest, i: &InstallInfo, dir: Option<&Path>) -> Value {
    let mut e = json!({ "id": m.id, "name": m.name, "version": m.version, "description": m.description,
            "source": i.source, "installedAt": i.installed_at, "themes": m.themes, "panels": m.panels, "bridge": m.bridge });
    if let (Some(b), Some(dir)) = (&m.bridge, dir) {
        if let Some(data_url) = crate::plugin_icon::declared_icon(dir, b.icon.as_deref(), None) {
            if let Some(obj) = e["bridge"].as_object_mut() {
                obj.insert("iconDataUrl".into(), json!(data_url));
            }
        }
    }
    if !m.harnesses.is_empty() {
        let dir = dir.map(Path::to_path_buf).unwrap_or_default();
        let consent = crate::plugin_harness::read_consent(&dir);
        e["harnesses"] = json!(m.harnesses.iter().map(|h| crate::plugin_harness::describe(h, &consent, &m.id)).collect::<Vec<_>>());
        let pending: Vec<Value> = crate::plugin_harness::pending(&consent, &m.harnesses)
            .iter()
            .map(|h| crate::plugin_harness::describe(h, &consent, &m.id))
            .collect();
        e["pendingConsent"] = json!(pending);
    }
    if !m.mcp_servers.is_empty() {
        let dir = dir.map(Path::to_path_buf).unwrap_or_default();
        let consent = crate::plugin_mcp::read_consent(&dir);
        let d = |s: &crate::plugin_mcp::PluginMcpServer| crate::plugin_mcp::describe(s, &consent, &m.id, &dir);
        e["mcpServers"] = json!(m.mcp_servers.iter().map(d).collect::<Vec<_>>());
        e["pendingMcpConsent"] = json!(crate::plugin_mcp::pending(&dir, &consent, &m.mcp_servers).into_iter().map(d).collect::<Vec<_>>());
    }
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
    let binaries = fetch_binaries(&m, &src).await?;
    write_plugin(&m, source, &files, &binaries)?;
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
    let binaries = fetch_binaries(&m, &src).await?;
    let pdir = plugins_dir()?.join(id);
    let dir = pdir.join("files");
    let files_changed = files.iter().any(|(n, b)| std::fs::read(dir.join(n)).ok().as_deref() != Some(b.as_slice()));
    let bins_changed = binaries.iter().any(|(n, b)| std::fs::read(pdir.join("bin").join(n)).ok().as_deref() != Some(b.as_slice()));
    let changed = m != old || files_changed || bins_changed;
    if changed {
        write_plugin(&m, &info.source, &files, &binaries)?;
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
    // user-supplied MCP secrets live outside the plugin folder
    crate::plugin_mcp::remove_secrets(id)?;
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

// ---------------------------------------------------------------------------
// Explore: the official plugin index (github.com/anslwy/vicuna-plugins).
// Fetched by the server (so remote/headless windows work), cached for a few
// minutes, and treated as untrusted: entries that break any rule are
// dropped, never repaired.

/// The official index. `VICUNA_PLUGINS_INDEX_URL` overrides it (tests).
pub const INDEX_URL: &str = "https://raw.githubusercontent.com/anslwy/vicuna-plugins/main/index.json";
const INDEX_TTL: std::time::Duration = std::time::Duration::from_secs(300);
const MAX_INDEX: usize = 512 * 1024;
const MAX_INDEX_ENTRIES: usize = 500;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub description: String,
    pub author: String,
    pub source: String,
    pub kinds: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot: Option<String>,
    pub tags: Vec<String>,
    #[serde(rename = "minVicuna", skip_serializing_if = "Option::is_none")]
    pub min_vicuna: Option<String>,
}

/// Images may only come from GitHub over https (no other hosts, no
/// credentials/ports, no quotes or whitespace that could break out of an
/// attribute).
pub fn valid_image_url(u: &str) -> bool {
    if u.len() > 512 || u.bytes().any(|b| b <= b' ' || b"\"'<>\\`".contains(&b) || b >= 0x7f) {
        return false;
    }
    ["https://raw.githubusercontent.com/", "https://github.com/"].iter().any(|p| u.strip_prefix(p).is_some_and(|r| !r.is_empty() && !r.starts_with('/')))
}

/// Plain one-line text: trimmed, 1..=max chars, no control characters.
fn clean_text(v: &Value, max: usize) -> Option<String> {
    let s = v.as_str()?.trim();
    (!s.is_empty() && s.chars().count() <= max && !s.chars().any(|c| c.is_control() && c != '\n')).then(|| s.to_string())
}

fn valid_semver(s: &str) -> bool {
    let p: Vec<&str> = s.split('.').collect();
    p.len() == 3 && p.iter().all(|x| !x.is_empty() && x.len() <= 6 && x.bytes().all(|b| b.is_ascii_digit()))
}

/// One index entry, or why it was rejected.
fn parse_entry(v: &Value) -> Result<CatalogEntry, String> {
    let id = v["id"].as_str().filter(|s| valid_id(s)).ok_or("invalid id")?.to_string();
    let name = clean_text(&v["name"], 60).ok_or("invalid name")?;
    let description = clean_text(&v["description"], 300).ok_or("invalid description")?;
    let author = clean_text(&v["author"], 60).ok_or("invalid author")?;
    let source = v["source"].as_str().ok_or("missing source")?;
    // only owner/repo: the index can't point installs at arbitrary URLs
    let Ok(Source::Repo { owner, repo }) = parse_source(source) else { return Err("source must be owner/repo".into()) };
    if source.contains("://") || source.starts_with("github.com") {
        return Err("source must be owner/repo".into());
    }
    let mut kinds: Vec<String> = Vec::new();
    for k in v["kinds"].as_array().ok_or("missing kinds")? {
        let k = k.as_str().filter(|k| matches!(*k, "theme" | "panel" | "harness" | "mcp" | "bridge")).ok_or("unknown kind")?;
        if !kinds.iter().any(|x| x == k) {
            kinds.push(k.to_string());
        }
    }
    if kinds.is_empty() {
        return Err("no kinds".into());
    }
    let img = |k: &str| -> Result<Option<String>, String> {
        match &v[k] {
            Value::Null => Ok(None),
            Value::String(s) if valid_image_url(s) => Ok(Some(s.clone())),
            _ => Err(format!("{k} must be an https GitHub URL")),
        }
    };
    let (icon, screenshot) = (img("icon")?, img("screenshot")?);
    let tags = match &v["tags"] {
        Value::Null => vec![],
        Value::Array(a) if a.len() <= 8 => a.iter().map(|t| clean_text(t, 24).filter(|t| !t.contains('\n'))).collect::<Option<Vec<_>>>().ok_or("invalid tag")?,
        _ => return Err("invalid tags".into()),
    };
    let min_vicuna = match &v["minVicuna"] {
        Value::Null => None,
        Value::String(s) if valid_semver(s) => Some(s.clone()),
        _ => return Err("invalid minVicuna".into()),
    };
    if name.contains('\n') || author.contains('\n') {
        return Err("name/author must be one line".into());
    }
    Ok(CatalogEntry { id, name, description, author, source: format!("{owner}/{repo}"), kinds, icon, screenshot, tags, min_vicuna })
}

/// Parse a whole index: `{"version": 1, "plugins": [...]}`. Bad entries
/// (and duplicate ids after the first) are skipped and counted.
pub fn parse_index(bytes: &[u8]) -> Result<(Vec<CatalogEntry>, usize), String> {
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("invalid plugin index: {e}"))?;
    if v["version"].as_u64() != Some(1) {
        return Err("unsupported plugin index version".into());
    }
    let list = v["plugins"].as_array().ok_or("invalid plugin index: no plugins list")?;
    let (mut out, mut skipped) = (Vec::<CatalogEntry>::new(), 0usize);
    for e in list.iter().take(MAX_INDEX_ENTRIES) {
        match parse_entry(e) {
            Ok(c) if !out.iter().any(|o| o.id == c.id) => out.push(c),
            _ => skipped += 1,
        }
    }
    skipped += list.len().saturating_sub(MAX_INDEX_ENTRIES);
    Ok((out, skipped))
}

fn semver_lt(a: &str, b: &str) -> bool {
    let p = |s: &str| s.split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    p(a) < p(b)
}

/// Same repo, whatever spelling the install used (`owner/repo`, URL, `.git`).
fn same_source(a: &str, b: &str) -> bool {
    match (parse_source(a), parse_source(b)) {
        (Ok(Source::Repo { owner: o1, repo: r1 }), Ok(Source::Repo { owner: o2, repo: r2 })) => o1.eq_ignore_ascii_case(&o2) && r1.eq_ignore_ascii_case(&r2),
        _ => false,
    }
}

type IndexCache = Option<(std::time::Instant, String, Vec<CatalogEntry>, usize)>;
fn index_cache() -> &'static std::sync::Mutex<IndexCache> {
    static C: std::sync::OnceLock<std::sync::Mutex<IndexCache>> = std::sync::OnceLock::new();
    C.get_or_init(Default::default)
}

/// `plugin_catalog {refresh?}`: the index entries, each with its install
/// state (`installed` = version or null, `conflict` = same id installed from
/// another source) and whether this Vicuna is new enough.
pub async fn catalog(refresh: bool) -> Result<Value, String> {
    let url = std::env::var("VICUNA_PLUGINS_INDEX_URL").unwrap_or_else(|_| INDEX_URL.into());
    let cached = {
        let g = index_cache().lock().unwrap_or_else(|e| e.into_inner());
        g.as_ref().filter(|(t, u, ..)| !refresh && *u == url && t.elapsed() < INDEX_TTL).map(|(t, _, e, s)| (*t, e.clone(), *s))
    };
    let (fetched_at, entries, skipped) = match cached {
        Some(c) => c,
        None => {
            let bytes = get_capped(&client()?, &url, "application/json", None, MAX_INDEX).await?.ok_or_else(|| format!("{url}: not found"))?;
            let (e, s) = parse_index(&bytes)?;
            let now = std::time::Instant::now();
            *index_cache().lock().unwrap_or_else(|e| e.into_inner()) = Some((now, url.clone(), e.clone(), s));
            (now, e, s)
        }
    };
    let this = env!("CARGO_PKG_VERSION");
    let rows: Vec<Value> = entries
        .iter()
        .map(|e| {
            let inst = read_installed(&e.id);
            let mut v = serde_json::to_value(e).unwrap();
            v["installed"] = json!(inst.as_ref().map(|(m, _)| m.version.clone()));
            v["conflict"] = json!(inst.as_ref().is_some_and(|(_, i)| !same_source(&i.source, &e.source)));
            v["compatible"] = json!(e.min_vicuna.as_deref().is_none_or(|m| !semver_lt(this, m)));
            v
        })
        .collect();
    let age = fetched_at.elapsed().as_secs();
    Ok(json!({ "url": url, "plugins": rows, "skipped": skipped, "ageSecs": age, "vicunaVersion": this }))
}

/// Server API actions: `plugin_catalog {refresh?} | plugin_list | plugin_install {source} |
/// plugin_upgrade {id} | plugin_uninstall {id} | plugin_storage_get|keys|
/// set|remove {id, key?, value?}`.
pub async fn action(cmd: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or(format!("missing {k}"));
    match cmd {
        "plugin_list" => Ok(list()),
        "plugin_catalog" => catalog(args.get("refresh").and_then(|v| v.as_bool()).unwrap_or(false)).await,
        "plugin_install" => install(&s("source")?).await,
        "plugin_upgrade" => upgrade(&s("id")?).await,
        "plugin_uninstall" => uninstall(&s("id")?),
        "plugin_harness_consent" => {
            let id = s("id")?;
            let (m, dir) = installed_manifest(&id).ok_or_else(|| format!("plugin {id} is not installed"))?;
            let approved = args.get("harnesses").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            crate::plugin_harness::grant(&dir, &m.harnesses, &approved, chrono_now())?;
            let (m, i) = read_installed(&id).ok_or("plugin disappeared")?;
            Ok(entry(&m, &i))
        }
        "plugin_harness_revoke" => {
            let id = s("id")?;
            let (_, dir) = installed_manifest(&id).ok_or_else(|| format!("plugin {id} is not installed"))?;
            crate::plugin_harness::revoke(&dir, args.get("harness").and_then(|v| v.as_str()))?;
            let (m, i) = read_installed(&id).ok_or("plugin disappeared")?;
            Ok(entry(&m, &i))
        }
        "plugin_mcp_consent" => {
            let id = s("id")?;
            let (m, dir) = installed_manifest(&id).ok_or_else(|| format!("plugin {id} is not installed"))?;
            let approved = args.get("servers").and_then(|v| v.as_array()).cloned().unwrap_or_default();
            crate::plugin_mcp::grant(&dir, &m.mcp_servers, &approved, chrono_now())?;
            let (m, i) = read_installed(&id).ok_or("plugin disappeared")?;
            Ok(entry(&m, &i))
        }
        "plugin_mcp_revoke" => {
            let id = s("id")?;
            let (_, dir) = installed_manifest(&id).ok_or_else(|| format!("plugin {id} is not installed"))?;
            crate::plugin_mcp::revoke(&dir, args.get("server").and_then(|v| v.as_str()))?;
            let (m, i) = read_installed(&id).ok_or("plugin disappeared")?;
            Ok(entry(&m, &i))
        }
        "plugin_mcp_set_secrets" | "plugin_mcp_clear_secrets" => {
            let id = s("id")?;
            let server = s("server")?;
            let (m, _) = installed_manifest(&id).ok_or_else(|| format!("plugin {id} is not installed"))?;
            let srv = m.mcp_servers.iter().find(|x| x.id == server).ok_or_else(|| format!("this plugin has no MCP server {server:?}"))?;
            if srv.secrets.is_empty() {
                return Err(format!("MCP server {server} declares no secrets"));
            }
            let dir = crate::plugin_mcp::secrets_dir()?;
            if cmd == "plugin_mcp_set_secrets" {
                let values = args.get("secrets").and_then(|v| v.as_object()).ok_or("missing secrets")?;
                crate::plugin_mcp::set_secrets_in(&dir, &id, srv, values)?;
            } else {
                crate::plugin_mcp::clear_secrets_in(&dir, &id, &server)?;
            }
            let (m, i) = read_installed(&id).ok_or("plugin disappeared")?;
            Ok(entry(&m, &i))
        }
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
    fn harness_icons_are_optional_and_shipped() {
        let mut m = manifest();
        assert!(m.icon.is_none());
        let h = serde_json::json!({"id":"agent","label":"Agent","acp":{"command":"agent"},"icon":"icons/agent.svg"});
        m.harnesses = vec![serde_json::from_value(h).unwrap()];
        m.icon = Some("fallback.svg".into());
        assert_eq!(m.files(), vec!["fallback.svg", "icons/agent.svg"]);
        m.harnesses[0].icon = Some("../outside.svg".into());
        assert_eq!(m.files(), vec!["fallback.svg"]);
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

    fn idx_entry() -> Value {
        json!({"id":"lagoon","name":"Lagoon","description":"Teal themes.","author":"anslwy","source":"anslwy/vicuna-theme-lagoon",
            "kinds":["theme"],"icon":"https://raw.githubusercontent.com/anslwy/vicuna-plugins/main/icons/lagoon.svg",
            "screenshot":"https://github.com/anslwy/vicuna-plugins/raw/main/s.jpg","tags":["dark"],"minVicuna":"0.0.45"})
    }

    #[test]
    fn index_entries_are_validated_not_repaired() {
        let ok = parse_entry(&idx_entry()).unwrap();
        assert_eq!((ok.source.as_str(), ok.kinds.clone(), ok.min_vicuna.as_deref()), ("anslwy/vicuna-theme-lagoon", vec!["theme".to_string()], Some("0.0.45")));
        let bad = |k: &str, v: Value| {
            let mut e = idx_entry();
            e[k] = v;
            parse_entry(&e).is_err()
        };
        for (k, v) in [
            ("id", json!("Bad Id")), ("id", json!("../x")), ("name", json!("")), ("name", json!("x".repeat(61))),
            ("name", json!("a\nb")), ("description", json!("x".repeat(301))), ("description", json!("a\u{7}b")), ("author", Value::Null),
            ("source", json!("https://evil.example/p.json")), ("source", json!("https://github.com/a/b")), ("source", json!("a/b/c")),
            ("kinds", json!([])), ("kinds", json!(["theme", "script"])), ("kinds", json!("theme")),
            ("icon", json!("http://raw.githubusercontent.com/a/b.svg")), ("icon", json!("https://evil.example/i.svg")),
            ("icon", json!("https://raw.githubusercontent.com.evil.example/i.svg")), ("icon", json!("javascript:alert(1)")),
            ("screenshot", json!("https://github.com/a/b\" onerror=\"x")), ("screenshot", json!("https://github.com//evil.example/x")),
            ("icon", json!("data:image/svg+xml,<svg/>")), ("tags", json!(["x".repeat(25)])), ("tags", json!((0..9).map(|i| i.to_string()).collect::<Vec<_>>())),
            ("minVicuna", json!("1.0")), ("minVicuna", json!(">=1.0.0")),
        ] {
            assert!(bad(k, v.clone()), "{k} = {v}");
        }
        // optional fields may be absent; duplicate kinds collapse
        let mut e = idx_entry();
        for k in ["icon", "screenshot", "tags", "minVicuna"] {
            e.as_object_mut().unwrap().remove(k);
        }
        e["kinds"] = json!(["mcp"]);
        assert_eq!(parse_entry(&e).unwrap().kinds, vec!["mcp".to_string()]);
        e["kinds"] = json!(["panel", "panel", "theme"]);
        let c = parse_entry(&e).unwrap();
        assert_eq!((c.icon, c.tags.len(), c.kinds), (None, 0, vec!["panel".to_string(), "theme".to_string()]));
    }

    #[test]
    fn index_skips_bad_and_duplicate_entries() {
        let mut bad = idx_entry();
        bad["icon"] = json!("https://evil.example/x.svg");
        let mut other = idx_entry();
        other["id"] = json!("other");
        let body = json!({"version": 1, "plugins": [idx_entry(), bad, idx_entry(), other, 42]}).to_string();
        let (list, skipped) = parse_index(body.as_bytes()).unwrap();
        assert_eq!((list.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), skipped), (vec!["lagoon", "other"], 3));
        assert!(parse_index(br#"{"version": 2, "plugins": []}"#).is_err());
        assert!(parse_index(br#"{"version": 1}"#).is_err());
        assert!(parse_index(b"<html>").is_err());
        assert!(same_source("https://github.com/Me/Repo.git", "me/repo") && !same_source("me/repo", "me/other"));
        assert!(semver_lt("0.0.9", "0.0.45") && !semver_lt("0.1.0", "0.0.45"));
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
                } else if req.starts_with("GET /repos/me/agent/contents/vicuna-plugin.json") {
                    // a harness plugin whose args change with the version
                    let args = if v == "1.0.0" { json!(["acp"]) } else { json!(["acp", "--other"]) };
                    let m = json!({"id": "agent", "name": "Agent", "version": v,
                        "harnesses": [{"id": "demo", "label": "Demo", "acp": {"command": "true", "args": args}}]});
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /repos/me/tools/contents/vicuna-plugin.json") {
                    // an MCP plugin whose stdio args change in 3.0.0
                    let args = if v == "3.0.0" { json!(["--stdio", "--write"]) } else { json!(["--stdio"]) };
                    let m = json!({"id": "tools", "name": "Tools", "version": v, "mcpServers": [
                        {"id": "web", "label": "Web", "type": "http", "url": "https://mcp.example.com/mcp"},
                        {"id": "files", "type": "stdio", "command": "files-mcp", "args": args,
                         "secrets": [{"key": "FILES_API_KEY", "label": "API key", "helpUrl": "https://files.example/keys"}]}]});
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /repos/me/binmcp/releases/latest") {
                    let a = |n: &str| json!({"name": n, "url": format!("http://127.0.0.1:{port}/assets/{n}")});
                    ("200 OK", json!({"assets": [a("vicuna-plugin.json"), a("img-mcp-bin")]}).to_string())
                } else if req.starts_with("GET /assets/vicuna-plugin.json") {
                    let m = json!({"id": "binmcp", "name": "Bin", "version": v, "mcpServers": [
                        {"id": "img", "type": "stdio", "binaries": {crate::plugins::current_platform(): "img-mcp-bin", "windows-x86_64": "img-mcp.exe"}, "args": ["--stdio"]}]});
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /assets/img-mcp-bin") {
                    // bytes change only in 3.0.0
                    ("200 OK", if v == "3.0.0" { "BIN-v3".to_string() } else { "BIN-v1".to_string() })
                } else if req.starts_with("GET /repos/me/evilmcp/contents/vicuna-plugin.json") {
                    let m = json!({"id": "evilmcp", "name": "E", "version": "1", "mcpServers": [
                        {"id": "x", "type": "stdio", "command": "x", "env": {"LD_PRELOAD": "/x.so"}}]});
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /repos/me/evil/contents/vicuna-plugin.json") {
                    let m = json!({"id": "evil", "name": "Evil", "version": "1",
                        "harnesses": [{"id": "x", "label": "X", "acp": {"command": "/tmp/x", "args": []}}]});
                    ("200 OK", m.to_string())
                } else if req.starts_with("GET /index.json") {
                    let e = |id: &str, src: &str| json!({"id": id, "name": id, "description": "d", "author": "me", "source": src, "kinds": ["panel"], "minVicuna": "999.0.0"});
                    ("200 OK", json!({"version": 1, "plugins": [e("nord", "me/nord"), e("clash", "someone/clash"), {"id": "BAD"}]}).to_string())
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
            std::env::set_var("VICUNA_PLUGIN_SECRETS_DIR", dir.join("secrets"));
            std::env::set_var("VICUNA_PLUGINS_GITHUB_API", format!("http://127.0.0.1:{port}"));
            std::env::set_var("GH_TOKEN", "test");
            std::env::set_var("VICUNA_PLUGINS_INDEX_URL", format!("http://127.0.0.1:{port}/index.json"));
        }

        // catalog: install state, bad entries skipped, minVicuna gate
        let c = action("plugin_catalog", &json!({})).await.unwrap();
        assert_eq!((c["skipped"].clone(), c["plugins"].as_array().unwrap().len()), (json!(1), 2));
        assert_eq!((c["plugins"][0]["installed"].clone(), c["plugins"][0]["compatible"].clone()), (Value::Null, json!(false)));
        // a hand-made install of id "clash" from elsewhere shows as a conflict
        std::fs::create_dir_all(dir.join("clash")).unwrap();
        let mut cm = manifest();
        cm.id = "clash".into();
        std::fs::write(dir.join("clash").join(MANIFEST), serde_json::to_vec(&cm).unwrap()).unwrap();
        std::fs::write(dir.join("clash").join("install.json"), json!({"source": "evil/clash", "installedAt": 0}).to_string()).unwrap();

        let e = action("plugin_install", &json!({"source": "me/nord"})).await.unwrap();
        assert_eq!(e["version"], "1.0.0");
        // cached index, fresh install state
        let c = action("plugin_catalog", &json!({})).await.unwrap();
        assert_eq!((c["plugins"][0]["installed"].clone(), c["plugins"][0]["conflict"].clone()), (json!("1.0.0"), json!(false)));
        assert_eq!(c["plugins"][1]["conflict"], json!(true));
        std::fs::remove_dir_all(dir.join("clash")).unwrap();
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

        // ---- harness plugins: consent is enforced server-side ----
        use crate::work_engine::Engine;
        let err = action("plugin_install", &json!({"source": "me/evil"})).await.unwrap_err();
        assert!(err.contains("bare program name"), "{err}");
        *version.lock().unwrap() = "1.0.0".into();
        let e = action("plugin_install", &json!({"source": "me/agent"})).await.unwrap();
        assert_eq!(e["pendingConsent"][0]["command"], "true");
        assert_eq!(e["harnesses"][0]["enabled"], false);
        let engine = Engine::parse("plugin:agent/demo").unwrap();
        assert!(!engine.is_present(), "no consent -> disabled");
        let ws = std::env::temp_dir().display().to_string();
        let err = crate::plugin_harness::launch("agent/demo", &ws).unwrap_err();
        assert!(err.contains("not enabled"), "{err}");
        assert!(Engine::all().contains(&engine));
        // a grant must echo the exact command
        assert!(action("plugin_harness_consent", &json!({"id": "agent", "harnesses": [{"id": "demo", "command": "true", "args": ["acp", "--x"]}]})).await.is_err());
        let e = action("plugin_harness_consent", &json!({"id": "agent", "harnesses": [{"id": "demo", "command": "true", "args": ["acp"]}]})).await.unwrap();
        assert_eq!((e["harnesses"][0]["enabled"].clone(), e["pendingConsent"].as_array().unwrap().len()), (json!(true), 0));
        assert!(engine.is_present());
        assert!(crate::plugin_harness::launch("agent/demo", &ws).is_ok());
        assert!(dir.join("agent").join("consent.json").exists(), "consent lives next to install.json");
        // an upgrade with the same command keeps consent ...
        assert_eq!(action("plugin_upgrade", &json!({"id": "agent"})).await.unwrap()["changed"], false);
        assert!(engine.is_present());
        // ... a changed command needs fresh consent
        *version.lock().unwrap() = "2.0.0".into();
        let e = action("plugin_upgrade", &json!({"id": "agent"})).await.unwrap();
        assert_eq!(e["pendingConsent"][0]["args"], json!(["acp", "--other"]));
        assert!(!engine.is_present());
        assert!(crate::plugin_harness::launch("agent/demo", &ws).unwrap_err().contains("not enabled"));
        action("plugin_harness_consent", &json!({"id": "agent", "harnesses": [{"id": "demo", "command": "true", "args": ["acp", "--other"]}]})).await.unwrap();
        assert!(engine.is_present());
        action("plugin_harness_revoke", &json!({"id": "agent", "harness": "demo"})).await.unwrap();
        assert!(!engine.is_present());
        // uninstall: sessions on it get a clear fallback message
        action("plugin_uninstall", &json!({"id": "agent"})).await.unwrap();
        assert!(engine.missing_message().contains("no longer installed"), "{}", engine.missing_message());
        assert!(!Engine::all().contains(&engine));
        assert_eq!(serve_panel(&format!("{base}notes.html")).0, 404);
        assert!(action("plugin_uninstall", &json!({"id": "nord"})).await.is_err());

        // ---- MCP plugins: default-disabled, consent bound to the launch ----
        assert!(action("plugin_install", &json!({"source": "me/evilmcp"})).await.unwrap_err().contains("LD_PRELOAD"));
        *version.lock().unwrap() = "1.0.0".into();
        let e = action("plugin_install", &json!({"source": "me/tools"})).await.unwrap();
        assert_eq!(e["mcpServers"][0]["serverId"], "tools-web");
        assert_eq!(e["pendingMcpConsent"].as_array().unwrap().len(), 2);
        assert!(crate::plugin_mcp::consented_servers().is_empty(), "installing never adds servers");
        assert_eq!(crate::plugin_mcp::owner("tools-files").as_deref(), Some("Tools"));
        let web = json!({"id": "web", "type": "http", "url": "https://mcp.example.com/mcp"});
        let files = |args: Value| json!({"id": "files", "type": "stdio", "command": "files-mcp", "args": args});
        assert!(action("plugin_mcp_consent", &json!({"id": "tools", "servers": [files(json!(["--other"]))]})).await.is_err());
        let e = action("plugin_mcp_consent", &json!({"id": "tools", "servers": [web, files(json!(["--stdio"]))]})).await.unwrap();
        assert_eq!(e["pendingMcpConsent"].as_array().unwrap().len(), 0);
        let ids: Vec<String> = crate::plugin_mcp::consented_servers().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["tools-files".to_string(), "tools-web".into()]);
        assert!(crate::plugin_mcp::consented_servers().iter().all(|c| !c.enabled && c.plugin_id.as_deref() == Some("tools")));
        assert!(dir.join("tools").join(crate::plugin_mcp::CONSENT_FILE).exists());
        // unchanged upgrade (new version, same launches) keeps consent ...
        *version.lock().unwrap() = "2.0.0".into();
        assert_eq!(action("plugin_upgrade", &json!({"id": "tools"})).await.unwrap()["pendingMcpConsent"].as_array().unwrap().len(), 0);
        assert_eq!(crate::plugin_mcp::consented_servers().len(), 2);
        // ... changed args invalidate only that server
        *version.lock().unwrap() = "3.0.0".into();
        let e = action("plugin_upgrade", &json!({"id": "tools"})).await.unwrap();
        assert_eq!(e["pendingMcpConsent"][0]["args"], json!(["--stdio", "--write"]));
        let ids: Vec<String> = crate::plugin_mcp::consented_servers().into_iter().map(|c| c.id).collect();
        assert_eq!(ids, vec!["tools-web".to_string()]);
        action("plugin_mcp_revoke", &json!({"id": "tools", "server": "web"})).await.unwrap();
        assert!(crate::plugin_mcp::consented_servers().is_empty());
        // secrets: masked status, refused on servers without declared secrets
        assert!(action("plugin_mcp_set_secrets", &json!({"id": "tools", "server": "web", "secrets": {"X": "y"}})).await.is_err());
        let e = action("plugin_mcp_set_secrets", &json!({"id": "tools", "server": "files", "secrets": {"FILES_API_KEY": "fk-live-123456789"}})).await.unwrap();
        let st = &e["mcpServers"][1]["secretStatus"][0];
        assert_eq!((st["configured"].as_bool(), st["tail"].as_str()), (Some(true), Some("6789")));
        assert!(!e.to_string().contains("fk-live"));
        let secrets_file = dir.join("secrets").join("tools-mcp-secrets.json");
        assert!(secrets_file.exists() && !dir.join("tools").join("tools-mcp-secrets.json").exists());
        action("plugin_mcp_clear_secrets", &json!({"id": "tools", "server": "files"})).await.unwrap();
        assert!(!secrets_file.exists());
        action("plugin_mcp_set_secrets", &json!({"id": "tools", "server": "files", "secrets": {"FILES_API_KEY": "fk-live-123456789"}})).await.unwrap();
        action("plugin_uninstall", &json!({"id": "tools"})).await.unwrap();
        assert!(!secrets_file.exists(), "uninstall forgets the plugin's secrets");
        assert!(crate::plugin_mcp::owner("tools-web").is_none());

        // ---- bundled MCP binaries: downloaded 0755, consent bound to sha256 ----
        *version.lock().unwrap() = "1.0.0".into();
        let e = action("plugin_install", &json!({"source": "me/binmcp"})).await.unwrap();
        let bin = dir.join("binmcp").join("bin").join("img-mcp-bin");
        assert_eq!(std::fs::read(&bin).unwrap(), b"BIN-v1");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&bin).unwrap().permissions().mode() & 0o777, 0o755);
        }
        let p = &e["pendingMcpConsent"][0];
        let sha1 = crate::plugin_mcp::sha256_hex(b"BIN-v1");
        assert_eq!((p["bundled"].as_bool(), p["binaryAsset"].as_str(), p["binarySha256"].as_str(), p["command"].clone()),
            (Some(true), Some("img-mcp-bin"), Some(sha1.as_str()), Value::Null));
        let img = |sha: &str| json!({"id": "img", "type": "stdio", "args": ["--stdio"], "binaryAsset": "img-mcp-bin", "binarySha256": sha});
        action("plugin_mcp_consent", &json!({"id": "binmcp", "servers": [img(&sha1)]})).await.unwrap();
        let cfgs = crate::plugin_mcp::consented_servers();
        let cmd = cfgs[0].command.clone().unwrap();
        assert_eq!(cmd[0], bin.canonicalize().unwrap().to_string_lossy());
        assert_eq!(cmd[1], "--stdio");
        // upgrade with the same binary bytes keeps consent
        *version.lock().unwrap() = "2.0.0".into();
        let e = action("plugin_upgrade", &json!({"id": "binmcp"})).await.unwrap();
        assert_eq!((e["changed"].as_bool(), e["pendingMcpConsent"].as_array().unwrap().len()), (Some(true), 0));
        assert_eq!(crate::plugin_mcp::consented_servers().len(), 1);
        // new binary bytes drop consent
        *version.lock().unwrap() = "3.0.0".into();
        let e = action("plugin_upgrade", &json!({"id": "binmcp"})).await.unwrap();
        assert_eq!(std::fs::read(&bin).unwrap(), b"BIN-v3");
        let sha3 = crate::plugin_mcp::sha256_hex(b"BIN-v3");
        assert_eq!(e["pendingMcpConsent"][0]["binarySha256"].as_str(), Some(sha3.as_str()));
        assert!(crate::plugin_mcp::consented_servers().is_empty());
        assert!(action("plugin_mcp_consent", &json!({"id": "binmcp", "servers": [img(&sha1)]})).await.is_err());
        action("plugin_mcp_consent", &json!({"id": "binmcp", "servers": [img(&sha3)]})).await.unwrap();
        assert_eq!(crate::plugin_mcp::consented_servers().len(), 1);
        action("plugin_uninstall", &json!({"id": "binmcp"})).await.unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn bridge_manifest_validation() {
        let mut m: Manifest = serde_json::from_value(json!({
            "id": "demo-bridge",
            "name": "Demo Bridge",
            "version": "0.1.0",
            "bridge": {
                "name": "Demo",
                "color": "#5865f2",
                "iconPath": "M12 2L2 22h20L12 2z",
                "entrypoint": "bin/demo"
            }
        })).unwrap();
        assert!(validate(&m).is_ok());

        m.bridge.as_mut().unwrap().icon_path = Some("M0 0\"><script>".into());
        assert!(validate(&m).is_err());

        m.bridge.as_mut().unwrap().icon_path = Some("M0 0".into());
        m.bridge.as_mut().unwrap().entrypoint = Some("../escape".into());
        assert!(validate(&m).is_err());

        m.bridge.as_mut().unwrap().entrypoint = Some("bin/demo".into());
        m.bridge.as_mut().unwrap().color = Some("url(evil)".into());
        assert!(validate(&m).is_err());

        m.bridge.as_mut().unwrap().color = Some("#5865f2".into());
        m.bridge.as_mut().unwrap().auth_mode = Some("qr".into());
        assert!(validate(&m).is_ok());

        m.bridge.as_mut().unwrap().auth_mode = Some("token".into());
        assert!(validate(&m).is_ok());

        m.bridge.as_mut().unwrap().auth_mode = Some("oauth".into());
        assert!(validate(&m).is_err());

        m.bridge.as_mut().unwrap().auth_mode = Some("token".into());
        m.bridge.as_mut().unwrap().icon = Some("slack.svg".into());
        m.bridge.as_mut().unwrap().icon_bg = Some("#ffffff".into());
        assert!(validate(&m).is_ok());
        assert!(m.files().contains(&"slack.svg".to_string()));

        for bad_icon in ["../slack.svg", "/etc/x.svg", "slack.png", "https://a/x.svg", ""] {
            m.bridge.as_mut().unwrap().icon = Some(bad_icon.into());
            assert!(validate(&m).is_err(), "expected error for icon {bad_icon:?}");
        }
        m.bridge.as_mut().unwrap().icon = Some("slack.svg".into());
        m.bridge.as_mut().unwrap().icon_bg = Some("url(evil)".into());
        assert!(validate(&m).is_err());
    }

    #[test]
    fn bridge_entry_serializes_safe_icon_data_url() {
        let dir = std::env::temp_dir().join(format!("vicuna-bridge-icon-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let plugin_dir = dir.join("slack-demo");
        std::fs::create_dir_all(plugin_dir.join("files")).unwrap();
        let svg = b"<svg viewBox=\"0 0 24 24\"><path fill=\"#E01E5A\" d=\"M0 0h10v10H0z\"/></svg>";
        std::fs::write(plugin_dir.join("files/slack.svg"), svg).unwrap();
        std::fs::write(plugin_dir.join("files/evil.svg"), b"<svg onload=\"alert(1)\"/>").unwrap();

        let info = InstallInfo { source: "local/slack-demo".into(), installed_at: 1 };
        let mut m: Manifest = serde_json::from_value(json!({
            "id": "slack-demo",
            "name": "Slack",
            "version": "0.1.2",
            "bridge": {
                "name": "Slack",
                "color": "#4a154b",
                "icon": "slack.svg",
                "iconBg": "#ffffff",
                "iconPath": "M0 0",
                "entrypoint": "bin/slack"
            }
        })).unwrap();

        let out = entry_in_dir(&m, &info, Some(&plugin_dir));
        let data_url = out["bridge"]["iconDataUrl"].as_str().expect("iconDataUrl present");
        assert!(data_url.starts_with("data:image/svg+xml;base64,"), "{data_url}");
        assert_eq!(out["bridge"]["iconBg"], "#ffffff");
        assert_eq!(out["bridge"]["color"], "#4a154b");

        // Unsafe SVG or missing file omits iconDataUrl so UI falls back to monochrome iconPath.
        m.bridge.as_mut().unwrap().icon = Some("evil.svg".into());
        assert!(entry_in_dir(&m, &info, Some(&plugin_dir))["bridge"].get("iconDataUrl").is_none());

        m.bridge.as_mut().unwrap().icon = Some("missing.svg".into());
        assert!(entry_in_dir(&m, &info, Some(&plugin_dir))["bridge"].get("iconDataUrl").is_none());

        // No bridge.icon declared omits iconDataUrl even if top-level m.icon is set (keeps Discord/Signal monochrome badges).
        m.icon = Some("slack.svg".into());
        m.bridge.as_mut().unwrap().icon = None;
        assert!(entry_in_dir(&m, &info, Some(&plugin_dir))["bridge"].get("iconDataUrl").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
