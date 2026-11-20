//! Minimal plugin system: install / upgrade / uninstall plugins from a GitHub
//! repo (public or private) or a direct manifest URL. A plugin is a
//! `vicuna-plugin.json` manifest; today it can only contribute themes
//! (CSS color variables, validated so a plugin can't inject arbitrary CSS).
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
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub themes: Vec<Theme>,
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
    if m.themes.is_empty() {
        return Err("this plugin contributes nothing Vicuna supports yet (themes)".into());
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
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    if bytes.len() > MAX_MANIFEST {
        return Err("manifest is too large".into());
    }
    Ok(Some(bytes.to_vec()))
}

/// Fetch the manifest for a source (see module docs for the order).
async fn fetch_manifest(source: &str) -> Result<Manifest, String> {
    let api = std::env::var("VICUNA_PLUGINS_GITHUB_API").unwrap_or_else(|_| GITHUB_API.into());
    let c = client()?;
    let bytes = match parse_source(source)? {
        Source::Url(u) => get(&c, &u, "application/json", None).await?.ok_or(format!("{u}: not found"))?,
        Source::Repo { owner, repo } => {
            let token = github_token();
            let tok = token.as_deref();
            let mut found = None;
            if let Some(rel) = get(&c, &format!("{api}/repos/{owner}/{repo}/releases/latest"), "application/vnd.github+json", tok).await? {
                let rel: Value = serde_json::from_slice(&rel).map_err(|e| e.to_string())?;
                let asset = rel["assets"].as_array().into_iter().flatten().find(|a| a["name"] == MANIFEST);
                if let Some(url) = asset.and_then(|a| a["url"].as_str()) {
                    found = get(&c, url, "application/octet-stream", tok).await?;
                }
            }
            if found.is_none() {
                found = get(&c, &format!("{api}/repos/{owner}/{repo}/contents/{MANIFEST}"), "application/vnd.github.raw", tok).await?;
            }
            found.ok_or_else(|| {
                let hint = if tok.is_none() { " (private repo? sign in with `gh auth login` or set GH_TOKEN on the server)" } else { "" };
                format!("no {MANIFEST} found in {owner}/{repo} (latest release asset or default branch){hint}")
            })?
        }
    };
    let m: Manifest = serde_json::from_slice(&bytes).map_err(|e| format!("invalid {MANIFEST}: {e}"))?;
    validate(&m)?;
    Ok(m)
}

fn write_plugin(m: &Manifest, source: &str) -> Result<(), String> {
    let dir = plugins_dir()?.join(&m.id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, serde_json::to_vec_pretty(m).unwrap()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(MANIFEST)).map_err(|e| e.to_string())?;
    let info = InstallInfo { source: source.trim().to_string(), installed_at: chrono_now() };
    std::fs::write(dir.join("install.json"), serde_json::to_vec_pretty(&info).unwrap()).map_err(|e| e.to_string())
}

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
    json!({ "id": m.id, "name": m.name, "version": m.version, "description": m.description,
            "source": i.source, "installedAt": i.installed_at, "themes": m.themes })
}

pub fn list() -> Value {
    let mut out = Vec::new();
    if let Ok(rd) = plugins_dir().and_then(|d| std::fs::read_dir(d).map_err(|e| e.to_string())) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if let Some((m, i)) = read_installed(&name) {
                out.push(entry(&m, &i));
            }
        }
    }
    out.sort_by(|a, b| a["name"].as_str().unwrap_or("").to_lowercase().cmp(&b["name"].as_str().unwrap_or("").to_lowercase()));
    Value::Array(out)
}

pub async fn install(source: &str) -> Result<Value, String> {
    let m = fetch_manifest(source).await?;
    if let Some((old, i)) = read_installed(&m.id) {
        if i.source.trim() != source.trim() {
            return Err(format!("a plugin with id {:?} is already installed from {} — uninstall it first", m.id, i.source));
        }
        let _ = old;
    }
    write_plugin(&m, source)?;
    let (m, i) = read_installed(&m.id).ok_or("install failed")?;
    Ok(entry(&m, &i))
}

pub async fn upgrade(id: &str) -> Result<Value, String> {
    let (old, info) = read_installed(id).ok_or_else(|| format!("plugin {id} is not installed"))?;
    let m = fetch_manifest(&info.source).await?;
    if m.id != old.id {
        return Err(format!("the source now publishes plugin {:?}, not {:?}", m.id, old.id));
    }
    let changed = m != old;
    if changed {
        write_plugin(&m, &info.source)?;
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

/// Server API actions: `plugin_list | plugin_install {source} |
/// plugin_upgrade {id} | plugin_uninstall {id}`.
pub async fn action(cmd: &str, args: &Value) -> Result<Value, String> {
    let s = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_string).ok_or(format!("missing {k}"));
    match cmd {
        "plugin_list" => Ok(list()),
        "plugin_install" => install(&s("source")?).await,
        "plugin_upgrade" => upgrade(&s("id")?).await,
        "plugin_uninstall" => uninstall(&s("id")?),
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
                let (code, body) = if req.starts_with("GET /repos/me/nord/contents/vicuna-plugin.json") {
                    let mut m = serde_json::to_value(manifest()).unwrap();
                    m["version"] = json!(*v2.lock().unwrap());
                    ("200 OK", m.to_string())
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
        let e = action("plugin_upgrade", &json!({"id": "nord"})).await.unwrap();
        assert_eq!(e["changed"], false);
        *version.lock().unwrap() = "1.1.0".into();
        let e = action("plugin_upgrade", &json!({"id": "nord"})).await.unwrap();
        assert_eq!((e["changed"].clone(), e["version"].clone(), e["previousVersion"].clone()), (json!(true), json!("1.1.0"), json!("1.0.0")));
        assert!(action("plugin_install", &json!({"source": "me/missing"})).await.is_err());
        action("plugin_uninstall", &json!({"id": "nord"})).await.unwrap();
        assert!(list().as_array().unwrap().is_empty());
        assert!(action("plugin_uninstall", &json!({"id": "nord"})).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
