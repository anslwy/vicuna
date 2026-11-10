// opencode CLI discovery, model listing and Vicuna's per-process config.
//
// Work mode runs entirely on the user's installed `opencode` CLI (driven over
// ACP by acp.rs). Nothing is bundled: this module finds the binary (PATH plus
// the usual install locations, since a GUI app on macOS does not inherit the
// shell PATH), lists the models opencode itself knows (`opencode models`),
// and persists the model the user picked in Vicuna so they never need to open
// the opencode CLI.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::process::Command;

/// Where the "not installed" state points the user.
pub const INSTALL_URL: &str = "https://opencode.ai/docs/#install";
/// Explicit override for unusual installs (absolute path to the binary).
pub const BIN_ENV: &str = "VICUNA_OPENCODE_BIN";
const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";
const MODELS_TIMEOUT: Duration = Duration::from_secs(60);
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(windows)]
const EXE_NAMES: &[&str] = &["opencode.exe", "opencode.cmd", "opencode"];
#[cfg(not(windows))]
const EXE_NAMES: &[&str] = &["opencode"];

// ---------- detection ----------

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME");
    home.filter(|h| !h.is_empty()).map(PathBuf::from)
}

/// Install locations checked after PATH (official installer, npm/bun/pnpm
/// globals, Homebrew). Order matters: first hit wins.
fn common_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".opencode").join("bin"));
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".bun").join("bin"));
        dirs.push(h.join(".npm-global").join("bin"));
        dirs.push(h.join(".local").join("share").join("pnpm"));
        dirs.push(h.join("bin"));
        #[cfg(windows)]
        {
            dirs.push(h.join("AppData").join("Roaming").join("npm"));
            dirs.push(h.join("scoop").join("shims"));
        }
    }
    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/opt/homebrew/bin"));
        dirs.push(PathBuf::from("/usr/local/bin"));
        dirs.push(PathBuf::from("/usr/bin"));
    }
    dirs
}

fn is_executable(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        p.metadata().map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Pure lookup (all inputs explicit, so tests can point it at temp dirs):
/// explicit override, then every PATH entry, then the common install dirs.
pub fn find_opencode_in(
    override_bin: Option<&str>,
    path_var: Option<OsString>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(o) = override_bin.map(str::trim).filter(|o| !o.is_empty()) {
        let p = PathBuf::from(o);
        return is_executable(&p).then_some(p);
    }
    let mut dirs: Vec<PathBuf> = path_var
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend(common_dirs(home));
    for dir in dirs {
        for name in EXE_NAMES {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Live lookup against this process's environment. Cheap (a few stats), so
/// it runs at startup and again whenever the UI asks.
pub fn find_opencode() -> Option<PathBuf> {
    find_opencode_in(
        std::env::var(BIN_ENV).ok().as_deref(),
        std::env::var_os("PATH"),
        home_dir().as_deref(),
    )
}

/// What the Work engine row, header badge and send gate render.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkEngineInfo {
    pub installed: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub install_url: String,
    /// Model saved from Vicuna's picker (None = opencode's own default).
    pub model: Option<String>,
    pub config_path: Option<String>,
}

pub fn not_installed_info() -> WorkEngineInfo {
    WorkEngineInfo {
        installed: false,
        path: None,
        version: None,
        install_url: INSTALL_URL.to_string(),
        model: saved_model(),
        config_path: prefs_path().map(|p| p.display().to_string()),
    }
}

/// `opencode --version` prints e.g. "opencode v2.2.6" or "2.2.6".
pub fn parse_version(stdout: &str) -> Option<String> {
    stdout
        .split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(str::to_string)
}

pub async fn engine_info() -> WorkEngineInfo {
    let Some(bin) = find_opencode() else {
        return not_installed_info();
    };
    let version = run_capture(&bin, &["--version"], VERSION_TIMEOUT)
        .await
        .ok()
        .and_then(|out| parse_version(&out));
    WorkEngineInfo {
        installed: true,
        path: Some(bin.display().to_string()),
        version,
        ..not_installed_info()
    }
}

// ---------- per-process config ----------

/// Vicuna's additions to opencode's config for the processes it starts,
/// merged over any OPENCODE_CONFIG_CONTENT the user already exports (theirs
/// wins for everything except these keys). Only the process env changes:
/// the user's own opencode config files are never touched.
///
/// - `experimental.location_idle_timeout=false`: opencode otherwise unloads a
///   location's services (and its MCP connections) after inactivity, and a
///   warm ACP process would then resume sessions without their MCP tools.
/// - `autoupdate=false`: a background self-update must not replace the binary
///   under a long-lived Work process.
pub fn config_content(existing: Option<&str>) -> String {
    let mut config = existing
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    let obj = config.as_object_mut().expect("checked above");
    if !obj.get("experimental").is_some_and(Value::is_object) {
        obj.insert("experimental".into(), json!({}));
    }
    obj["experimental"]
        .as_object_mut()
        .expect("inserted above")
        .insert("location_idle_timeout".into(), Value::Bool(false));
    obj.insert("autoupdate".into(), Value::Bool(false));
    config.to_string()
}

/// Environment for every opencode process Vicuna starts.
pub fn process_env() -> Vec<(String, String)> {
    let existing = std::env::var(CONFIG_CONTENT_ENV).ok();
    vec![
        (CONFIG_CONTENT_ENV.to_string(), config_content(existing.as_deref())),
        ("NO_COLOR".to_string(), "1".to_string()),
    ]
}

pub fn command(bin: &Path) -> Command {
    let mut cmd = Command::new(bin);
    for (k, v) in process_env() {
        cmd.env(k, v);
    }
    if let Some(h) = home_dir() {
        cmd.current_dir(h);
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.as_std_mut().creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    cmd
}

async fn run_capture(bin: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = command(bin);
    cmd.args(args);
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("opencode {} timed out", args.join(" ")))?
        .map_err(|e| format!("cannot run opencode ({}): {e}", bin.display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "opencode {} failed: {}",
            args.join(" "),
            if err.is_empty() { out.status.to_string() } else { err }
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

// ---------- models ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpencodeModel {
    /// `provider/model` exactly as opencode accepts it.
    pub id: String,
    pub provider: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct OpencodeModels {
    pub models: Vec<OpencodeModel>,
    /// The saved Vicuna pick, if any (may be absent from `models` when the
    /// user removed that provider; the UI keeps showing it).
    pub current: Option<String>,
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for n in chars.by_ref() {
                    if n.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

/// `opencode models` prints one `provider/model` per line. Anything else (log
/// noise, banners, blank lines, ANSI colour) is ignored; duplicates collapse
/// and the original order (opencode's own) is kept.
pub fn parse_models_output(stdout: &str) -> Vec<OpencodeModel> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for raw in stdout.lines() {
        let line = strip_ansi(raw);
        let line = line.trim();
        if line.is_empty() || line.chars().any(char::is_whitespace) {
            continue;
        }
        let Some((provider, model)) = line.split_once('/') else {
            continue;
        };
        if provider.is_empty()
            || model.is_empty()
            || !provider
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            continue;
        }
        if seen.insert(line.to_string()) {
            out.push(OpencodeModel {
                id: line.to_string(),
                provider: provider.to_string(),
            });
        }
    }
    out
}

pub async fn list_models() -> Result<OpencodeModels, String> {
    let bin = find_opencode()
        .ok_or_else(|| format!("opencode CLI not found, install it: {INSTALL_URL}"))?;
    let out = run_capture(&bin, &["models"], MODELS_TIMEOUT).await?;
    Ok(OpencodeModels {
        models: parse_models_output(&out),
        current: saved_model(),
    })
}

// ---------- persisted model choice ----------

pub fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("opencode-work.json"))
}

pub fn read_model_from(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string)
}

pub fn write_model_to(path: &Path, model: Option<&str>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let mut doc = std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(m) => doc["model"] = json!(m),
        None => {
            doc.as_object_mut().map(|o| o.remove("model"));
        }
    }
    let text = serde_json::to_string_pretty(&doc).unwrap_or_default();
    std::fs::write(path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

pub fn saved_model() -> Option<String> {
    read_model_from(&prefs_path()?)
}

pub fn save_model(model: Option<&str>) -> Result<(), String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_model_to(&path, model)
}

// ---------- Tauri commands ----------

#[tauri::command]
pub async fn opencode_models() -> Result<OpencodeModels, String> {
    list_models().await
}

#[tauri::command]
pub async fn opencode_set_model(model: Option<String>) -> Result<(), String> {
    save_model(model.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::var_os("TMPDIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("vicuna-oc-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake_bin(dir: &Path) -> PathBuf {
        let p = dir.join(EXE_NAMES[0]);
        std::fs::write(&p, "#!/bin/sh\necho 'opencode v9.9.9'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[test]
    fn detection_missing_when_nowhere() {
        let empty = tmp("empty");
        let home = tmp("home-empty");
        let path = std::env::join_paths([&empty]).unwrap();
        // common dirs under the fake home are empty; system dirs are only
        // consulted when they exist, so assert on a home-scoped lookup
        let found = find_opencode_in(None, Some(path), Some(&home));
        if let Some(p) = &found {
            // a real system-wide install (/usr/local/bin etc.) is fine, but
            // nothing may come from the empty PATH or fake home
            assert!(!p.starts_with(&empty) && !p.starts_with(&home));
        }
    }

    #[test]
    fn detection_finds_binary_on_path() {
        let dir = tmp("path");
        let bin = fake_bin(&dir);
        let path = std::env::join_paths([&dir]).unwrap();
        assert_eq!(find_opencode_in(None, Some(path), None), Some(bin));
    }

    #[test]
    fn detection_finds_official_installer_dir_without_path() {
        let home = tmp("home");
        let dir = home.join(".opencode").join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = fake_bin(&dir);
        assert_eq!(find_opencode_in(None, None, Some(&home)), Some(bin));
    }

    #[test]
    fn detection_path_wins_over_common_dirs() {
        let home = tmp("home2");
        let dir = home.join(".opencode").join("bin");
        std::fs::create_dir_all(&dir).unwrap();
        fake_bin(&dir);
        let on_path = tmp("path2");
        let first = fake_bin(&on_path);
        let path = std::env::join_paths([&on_path]).unwrap();
        assert_eq!(find_opencode_in(None, Some(path), Some(&home)), Some(first));
    }

    #[test]
    fn detection_override_is_exclusive() {
        let dir = tmp("override");
        let bin = fake_bin(&dir);
        let other = tmp("other");
        let path = std::env::join_paths([&other]).unwrap();
        assert_eq!(
            find_opencode_in(Some(bin.to_str().unwrap()), Some(path.clone()), None),
            Some(bin)
        );
        // a bad override reports missing instead of silently using PATH
        let on_path = tmp("path3");
        fake_bin(&on_path);
        let path = std::env::join_paths([&on_path]).unwrap();
        assert_eq!(find_opencode_in(Some("/nonexistent/opencode"), Some(path), None), None);
    }

    #[cfg(unix)]
    #[test]
    fn detection_skips_non_executable_files() {
        let dir = tmp("noexec");
        std::fs::write(dir.join("opencode"), "not a program").unwrap();
        let path = std::env::join_paths([&dir]).unwrap();
        let found = find_opencode_in(None, Some(path), None);
        assert!(found.map_or(true, |p| !p.starts_with(&dir)));
    }

    #[test]
    fn not_installed_info_points_at_install_docs() {
        let info = not_installed_info();
        assert!(!info.installed);
        assert!(info.path.is_none());
        assert_eq!(info.install_url, INSTALL_URL);
        let v = serde_json::to_value(&info).unwrap();
        assert_eq!(v["installed"], json!(false));
        assert!(v.get("installUrl").is_some(), "camelCase for the frontend");
    }

    #[test]
    fn version_parsing() {
        assert_eq!(parse_version("opencode v2.2.6\n"), Some("2.2.6".into()));
        assert_eq!(parse_version("1.0.0"), Some("1.0.0".into()));
        assert_eq!(parse_version("garbage"), None);
    }

    #[test]
    fn models_parse_real_output() {
        let out = "ollama/beam:cloud\nollama/deepseek-v4.1-flash:cloud\nopencode/big-pickle\n";
        let m = parse_models_output(out);
        assert_eq!(m.len(), 3);
        assert_eq!(m[1].id, "ollama/deepseek-v4.1-flash:cloud");
        assert_eq!(m[1].provider, "ollama");
        assert_eq!(m[2].provider, "opencode");
    }

    #[test]
    fn models_parse_ignores_noise_and_dupes() {
        let out = "\u{1b}[32mopenrouter/anthropic/claude-x\u{1b}[0m\n\nINFO loading config\n\
                   Warning: something / odd\nollama/a\nollama/a\n/bad\nbad/\nhttps://x/y\n  lmstudio/qwen3  \n";
        let ids: Vec<String> = parse_models_output(out).into_iter().map(|m| m.id).collect();
        assert_eq!(ids, vec!["openrouter/anthropic/claude-x", "ollama/a", "lmstudio/qwen3"]);
    }

    #[test]
    fn models_parse_empty() {
        assert!(parse_models_output("").is_empty());
    }

    #[test]
    fn config_merges_over_user_content() {
        let merged: Value = serde_json::from_str(&config_content(Some(
            r#"{"model":"x/y","experimental":{"foo":1},"autoupdate":true}"#,
        )))
        .unwrap();
        assert_eq!(merged["model"], json!("x/y"));
        assert_eq!(merged["experimental"]["foo"], json!(1));
        assert_eq!(merged["experimental"]["location_idle_timeout"], json!(false));
        assert_eq!(merged["autoupdate"], json!(false));
        let fresh: Value = serde_json::from_str(&config_content(Some("not json"))).unwrap();
        assert_eq!(fresh["experimental"]["location_idle_timeout"], json!(false));
    }

    #[test]
    fn model_choice_round_trips() {
        let path = tmp("prefs").join("opencode-work.json");
        assert_eq!(read_model_from(&path), None);
        write_model_to(&path, Some("ollama/deepseek-v4.1-flash:cloud")).unwrap();
        assert_eq!(read_model_from(&path).as_deref(), Some("ollama/deepseek-v4.1-flash:cloud"));
        write_model_to(&path, None).unwrap();
        assert_eq!(read_model_from(&path), None);
    }
}
