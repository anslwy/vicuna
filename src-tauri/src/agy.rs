// Google Antigravity (`agy`) as a Work engine: discovery of the user's `agy`
// CLI, its model list (`agy models`) and the saved model pick. The engine
// itself speaks ACP (`agy acp`), driven by acp.rs like opencode, with the
// user's own login: nothing bundled, no auth files touched. Without `agy`,
// Work mode on this engine is disabled with an install hint (same contract
// as the other engines).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tokio::process::Command;

use crate::opencode::{
    common_dirs, find_exe_in, home_dir, parse_version, read_model_from, write_model_to, OpencodeModel,
    WorkEngineInfo,
};

pub const CLI_INSTALL_URL: &str = "https://antigravity.google/";
/// Exclusive override (absolute path), like VICUNA_OPENCODE_BIN.
pub const BIN_ENV: &str = "VICUNA_AGY_BIN";
const MODELS_TIMEOUT: Duration = Duration::from_secs(60);

#[cfg(windows)]
const CLI_NAMES: &[&str] = &["agy.exe", "agy.cmd", "agy"];
#[cfg(not(windows))]
const CLI_NAMES: &[&str] = &["agy"];

// ---------- detection ----------

/// Install locations after PATH: the shared list (~/.local/bin, where the
/// agy installer puts it, Homebrew, /usr/local).
fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    common_dirs(home)
}

pub fn find_agy_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(CLI_NAMES, override_bin, path_var, &extra_dirs(home))
}

/// Like the user's terminal (login-shell PATH, process PATH, install
/// folders; newest `--version` when several exist), same as opencode.
pub fn resolve_agy() -> Option<crate::exe_resolve::Resolved> {
    crate::exe_resolve::resolve(CLI_NAMES, std::env::var(BIN_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
}

pub fn find_agy() -> Option<PathBuf> {
    resolve_agy().map(|r| r.path)
}

pub fn missing_message() -> String {
    format!("Antigravity CLI (agy) not found, install it: {CLI_INSTALL_URL}")
}

/// `agy acp` for one workspace. agy runs its tools in its own working
/// directory (it does not follow the ACP session's `cwd`), so each workspace
/// gets its own process started there. Tool approval: with Vicuna's "Ask
/// before running tools" off, agy approves everything itself
/// (`--dangerously-skip-permissions`); with it on, agy sends
/// `session/request_permission` and the user answers on the ask card.
pub fn command(bin: &Path, ask: bool, workspace: &Path) -> Command {
    let mut cmd = crate::opencode::command(bin);
    cmd.args(acp_args(ask));
    if workspace.is_dir() {
        cmd.current_dir(workspace);
    }
    cmd
}

pub fn acp_args(ask: bool) -> Vec<&'static str> {
    let mut args = vec!["acp", "--disable-telemetry"];
    if !ask {
        args.push("--dangerously-skip-permissions");
    }
    args
}

fn describe(p: &Path) -> (Option<String>, String) {
    // "1.5.6"
    let v = crate::exe_resolve::version_of(p).and_then(|o| parse_version(&o));
    let label = match &v {
        Some(v) => format!("{} ({v})", p.display()),
        None => p.display().to_string(),
    };
    (v, label)
}

pub async fn engine_info() -> WorkEngineInfo {
    // resolution may run `--version` on several binaries: off the async pool
    let (cli, version, other_labels) = tokio::task::spawn_blocking(|| {
        let found = resolve_agy();
        let version = found.as_ref().and_then(|f| describe(&f.path).0);
        let others: Vec<String> = found.as_ref().map(|f| f.others.iter().map(|o| describe(o).1).collect()).unwrap_or_default();
        (found.map(|f| f.path), version, others)
    })
    .await
    .unwrap_or((None, None, Vec::new()));
    let installed = cli.is_some();
    WorkEngineInfo {
        id: "agy".to_string(),
        label: "Antigravity".to_string(),
        installed,
        missing: (!installed).then(missing_message),
        engines: Vec::new(),
        path: cli.map(|c| c.display().to_string()),
        version,
        install_url: CLI_INSTALL_URL.to_string(),
        model: saved_model(),
        config_path: prefs_path().map(|p| p.display().to_string()),
        others: other_labels,
        warning: None,
        ask_permission: false,
    }
}

// ---------- models ----------

static MODELS: Mutex<Option<(PathBuf, Vec<OpencodeModel>)>> = Mutex::new(None);

fn provider_of(id: &str) -> &'static str {
    if id.starts_with("gemini") {
        "google"
    } else if id.starts_with("claude") {
        "anthropic"
    } else if id.starts_with("gpt") {
        "openai"
    } else {
        "agy"
    }
}

/// `agy models` prints `<id>\t<display name>` per line (after a
/// "Fetching available models..." line, which has no tab).
pub fn parse_models_output(out: &str) -> Vec<OpencodeModel> {
    let mut models: Vec<OpencodeModel> = Vec::new();
    for line in out.lines() {
        let Some((id, name)) = line.split_once('\t') else { continue };
        let id = id.trim();
        if id.is_empty() || id.contains(' ') || models.iter().any(|m| m.id == id) {
            continue;
        }
        let name = name.trim();
        models.push(OpencodeModel {
            id: id.to_string(),
            provider: provider_of(id).to_string(),
            name: (!name.is_empty()).then(|| name.to_string()),
        });
    }
    models
}

pub async fn list_models() -> Result<Vec<OpencodeModel>, String> {
    let cli = find_agy().ok_or_else(missing_message)?;
    if let Some((p, cached)) = MODELS.lock().ok().and_then(|g| g.clone()) {
        if p == cli {
            return Ok(cached);
        }
    }
    let mut cmd = crate::opencode::command(&cli);
    cmd.arg("models");
    let out = tokio::time::timeout(MODELS_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "agy models timed out".to_string())?
        .map_err(|e| format!("cannot run agy ({}): {e}", cli.display()))?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    let models = parse_models_output(&text);
    if models.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "agy models listed nothing{}",
            if err.is_empty() { String::new() } else { format!(": {err}") }
        ));
    }
    if let Ok(mut g) = MODELS.lock() {
        *g = Some((cli, models.clone()));
    }
    Ok(models)
}

// ---------- persisted model choice ----------

pub fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("agy-work.json"))
}

pub fn saved_model() -> Option<String> {
    read_model_from(&prefs_path()?)
}

pub fn save_model(model: Option<&str>) -> Result<(), String> {
    let path = prefs_path().ok_or("cannot locate home folder")?;
    write_model_to(&path, model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("vicuna-agy-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    #[test]
    fn finds_cli_on_path_and_in_local_bin() {
        use std::os::unix::fs::PermissionsExt as _;
        let fake = |dir: &Path| {
            let p = dir.join("agy");
            std::fs::write(&p, "#!/bin/sh\necho 1.5.6\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        };
        let path_dir = tmp("path");
        let home = tmp("home");
        let cli = fake(&path_dir);
        assert_eq!(find_agy_in(None, Some(path_dir.clone().into_os_string()), Some(&home)), Some(cli));
        // the installer's ~/.local/bin, which a GUI PATH misses
        let local = home.join(".local").join("bin");
        std::fs::create_dir_all(&local).unwrap();
        let native = fake(&local);
        assert_eq!(find_agy_in(None, Some(OsString::from("/nonexistent")), Some(&home)), Some(native));
        assert_eq!(find_agy_in(Some("/nonexistent/agy"), Some(path_dir.into_os_string()), Some(&home)), None);
        assert!(missing_message().contains(CLI_INSTALL_URL));
    }

    #[test]
    fn models_output_parses_ids_names_and_providers() {
        let out = "Fetching available models...\ngemini-3.8-flash-medium\tGemini 3.8 Flash (Medium)\nclaude-opus-5-5-low\tClaude Opus 5.5 (Low)\ngpt-oss-120b-medium\tGPT-OSS 120B (Medium)\n\n";
        let m = parse_models_output(out);
        assert_eq!(m.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), ["gemini-3.8-flash-medium", "claude-opus-5-5-low", "gpt-oss-120b-medium"]);
        assert_eq!(m[0].name.as_deref(), Some("Gemini 3.8 Flash (Medium)"));
        assert_eq!(m.iter().map(|x| x.provider.as_str()).collect::<Vec<_>>(), ["google", "anthropic", "openai"]);
        assert_eq!(parse_version("1.5.6\n").as_deref(), Some("1.5.6"));
    }

    #[test]
    fn skip_permissions_only_without_ask() {
        assert_eq!(acp_args(true), ["acp", "--disable-telemetry"]);
        assert_eq!(acp_args(false), ["acp", "--disable-telemetry", "--dangerously-skip-permissions"]);
    }
}
