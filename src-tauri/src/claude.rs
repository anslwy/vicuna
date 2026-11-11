// Claude Code as a Work engine: discovery of the user's `claude` CLI and the
// saved model pick. The engine itself (claude_cli.rs) drives that CLI headless
// with its stream-json protocol: no SDK, no adapter, nothing bundled. Without
// `claude`, Work mode on this engine is disabled with an install hint (same
// contract as opencode).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::opencode::{
    common_dirs, find_exe_in, home_dir, parse_version, read_model_from, write_model_to, EngineSummary,
    WorkEngineInfo,
};

pub const CLI_INSTALL_URL: &str = "https://docs.claude.com/en/docs/claude-code/setup";
/// Exclusive override (absolute path), like VICUNA_OPENCODE_BIN.
pub const BIN_ENV: &str = "VICUNA_CLAUDE_BIN";

#[cfg(windows)]
const CLI_NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude"];
#[cfg(not(windows))]
const CLI_NAMES: &[&str] = &["claude"];

// ---------- detection ----------

/// Install locations after PATH: Claude's own local install dir, then the
/// shared list (~/.local/bin, Homebrew, /usr/local). Node version managers
/// (nvm, fnm, volta, asdf, npm globals) are searched by exe_resolve.
fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".claude").join("local"));
    }
    dirs.extend(common_dirs(home));
    dirs
}

pub fn find_claude_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(CLI_NAMES, override_bin, path_var, &extra_dirs(home))
}

/// Like the user's terminal (login-shell PATH, process PATH, install
/// folders; newest `--version` when several exist), same as opencode.
pub fn resolve_claude() -> Option<crate::exe_resolve::Resolved> {
    crate::exe_resolve::resolve(CLI_NAMES, std::env::var(BIN_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
}

pub fn find_claude() -> Option<PathBuf> {
    resolve_claude().map(|r| r.path)
}

pub fn missing_message() -> String {
    format!("Claude Code CLI not found, install it: {CLI_INSTALL_URL}")
}

fn describe(p: &Path) -> (Option<String>, String) {
    // "2.1.322 (Claude Code)"
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
        let found = resolve_claude();
        let version = found.as_ref().and_then(|f| describe(&f.path).0);
        let others: Vec<String> = found.as_ref().map(|f| f.others.iter().map(|o| describe(o).1).collect()).unwrap_or_default();
        (found.map(|f| f.path), version, others)
    })
    .await
    .unwrap_or((None, None, Vec::new()));
    let installed = cli.is_some();
    WorkEngineInfo {
        id: "claude".to_string(),
        label: "Claude Code".to_string(),
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

pub fn summary(info: &WorkEngineInfo) -> EngineSummary {
    EngineSummary {
        id: info.id.clone(),
        label: info.label.clone(),
        installed: info.installed,
        version: info.version.clone(),
        missing: info.missing.clone(),
    }
}

// ---------- persisted model choice ----------

pub fn prefs_path() -> Option<PathBuf> {
    Some(home_dir()?.join("vicuna").join("config").join("claude-work.json"))
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
        let d = base.join(format!("vicuna-claude-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\necho '2.1.322 (Claude Code)'\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        p
    }

    #[cfg(unix)]
    #[test]
    fn finds_cli_on_path_and_in_install_dirs() {
        let path_dir = tmp("path");
        let home = tmp("home");
        let cli = fake(&path_dir, "claude");
        assert_eq!(find_claude_in(None, Some(path_dir.clone().into_os_string()), Some(&home)), Some(cli));
        // native installer location, which a GUI PATH misses
        let local = home.join(".claude").join("local");
        std::fs::create_dir_all(&local).unwrap();
        let native = fake(&local, "claude");
        assert_eq!(find_claude_in(None, Some(OsString::from("/nonexistent")), Some(&home)), Some(native));
        // an override that doesn't exist is exclusive: missing
        assert_eq!(find_claude_in(Some("/nonexistent/claude"), Some(path_dir.into_os_string()), Some(&home)), None);
        assert!(missing_message().contains(CLI_INSTALL_URL));
    }

    #[test]
    fn version_parses_claude_output() {
        assert_eq!(parse_version("2.1.322 (Claude Code)\n").as_deref(), Some("2.1.322"));
    }
}
