// Claude Code as a Work engine: discovery of the user's `claude` CLI and the
// ACP adapter that drives it.
//
// Claude Code has no native ACP mode. The standard bridge is the ACP
// project's adapter (npm `@agentclientprotocol/claude-agent-acp`, formerly
// Zed's `@zed-industries/claude-code-acp`), which runs the Claude Agent SDK.
// Vicuna points it at the user's own `claude` binary through
// CLAUDE_CODE_EXECUTABLE, so the user's login, settings and plan apply.
// Nothing is bundled: both pieces must be installed, otherwise Work mode on
// this engine is disabled with an install hint (same contract as opencode).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use tokio::process::Command;

use crate::opencode::{
    common_dirs, find_exe_in, home_dir, parse_version, read_model_from, write_model_to, EngineSummary,
    WorkEngineInfo,
};

pub const CLI_INSTALL_URL: &str = "https://docs.claude.com/en/docs/claude-code/setup";
pub const ADAPTER_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp";
pub const ADAPTER_URL: &str = "https://www.npmjs.com/package/@agentclientprotocol/claude-agent-acp";
/// Exclusive overrides (absolute paths), like VICUNA_OPENCODE_BIN.
pub const BIN_ENV: &str = "VICUNA_CLAUDE_BIN";
pub const ADAPTER_ENV: &str = "VICUNA_CLAUDE_ACP_BIN";

#[cfg(windows)]
const CLI_NAMES: &[&str] = &["claude.exe", "claude.cmd", "claude"];
#[cfg(not(windows))]
const CLI_NAMES: &[&str] = &["claude"];
// current name first, then the deprecated Zed package's binary
#[cfg(windows)]
const ADAPTER_NAMES: &[&str] = &["claude-agent-acp.cmd", "claude-agent-acp.exe", "claude-code-acp.cmd", "claude-code-acp.exe"];
#[cfg(not(windows))]
const ADAPTER_NAMES: &[&str] = &["claude-agent-acp", "claude-code-acp"];

// ---------- detection ----------

/// Install locations after PATH: Claude's own local install dir, then the
/// shared list (npm/bun/pnpm globals, ~/.local/bin, Homebrew, /usr/local).
fn extra_dirs(home: Option<&Path>) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(h) = home {
        dirs.push(h.join(".claude").join("local"));
        dirs.push(h.join(".volta").join("bin"));
    }
    dirs.extend(common_dirs(home));
    dirs.extend(nvm_bins(home));
    dirs
}

/// `~/.nvm/versions/node/*/bin`, newest first (npm -g lands there with nvm).
fn nvm_bins(home: Option<&Path>) -> Vec<PathBuf> {
    let Some(h) = home else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(h.join(".nvm").join("versions").join("node")) else {
        return Vec::new();
    };
    let mut v: Vec<PathBuf> = rd.flatten().map(|e| e.path().join("bin")).filter(|p| p.is_dir()).collect();
    v.sort();
    v.reverse();
    v
}

pub fn find_claude_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(CLI_NAMES, override_bin, path_var, &extra_dirs(home))
}

pub fn find_adapter_in(override_bin: Option<&str>, path_var: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    find_exe_in(ADAPTER_NAMES, override_bin, path_var, &extra_dirs(home))
}

/// Like the user's terminal (login-shell PATH, process PATH, install
/// folders; newest `--version` when several exist), same as opencode.
pub fn resolve_claude() -> Option<crate::exe_resolve::Resolved> {
    crate::exe_resolve::resolve(CLI_NAMES, std::env::var(BIN_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
}

pub fn find_claude() -> Option<PathBuf> {
    resolve_claude().map(|r| r.path)
}

pub fn find_adapter() -> Option<PathBuf> {
    crate::exe_resolve::resolve(ADAPTER_NAMES, std::env::var(ADAPTER_ENV).ok().as_deref(), &extra_dirs(home_dir().as_deref()))
        .map(|r| r.path)
}

/// Both pieces, or the reason (with the install hint) why not.
pub fn find_parts() -> Result<(PathBuf, PathBuf), String> {
    let cli = find_claude();
    let adapter = find_adapter();
    match (cli, adapter) {
        (Some(c), Some(a)) => Ok((c, a)),
        (c, a) => Err(missing_reason(c.is_some(), a.is_some()).unwrap_or_default()),
    }
}

pub fn missing_reason(has_cli: bool, has_adapter: bool) -> Option<String> {
    match (has_cli, has_adapter) {
        (true, true) => None,
        (false, true) => Some(format!("Claude Code CLI not found, install it: {CLI_INSTALL_URL}")),
        (true, false) => Some(format!(
            "Claude ACP adapter not found, install it: npm install -g {ADAPTER_PACKAGE}"
        )),
        (false, false) => Some(format!(
            "Claude Code CLI and its ACP adapter not found, install them: {CLI_INSTALL_URL} and npm install -g {ADAPTER_PACKAGE}"
        )),
    }
}

// ---------- process ----------

/// PATH for the adapter: it is a `#!/usr/bin/env node` script, and a macOS
/// GUI app's PATH usually lacks node; put the adapter's and claude's own dirs
/// plus the usual install dirs in front.
fn child_path(adapter: &Path, cli: &Path) -> OsString {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in [adapter, cli] {
        // resolve npm's symlinked bin to find a co-installed node
        if let Some(d) = p.parent() {
            dirs.push(d.to_path_buf());
        }
        if let Ok(real) = std::fs::canonicalize(p) {
            if let Some(d) = real.parent() {
                dirs.push(d.to_path_buf());
            }
        }
    }
    dirs.extend(extra_dirs(home_dir().as_deref()));
    if let Some(existing) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&existing));
    }
    let mut seen = std::collections::HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    std::env::join_paths(dirs).unwrap_or_default()
}

pub fn command(adapter: &Path, cli: &Path) -> Command {
    let mut cmd = Command::new(adapter);
    cmd.env("CLAUDE_CODE_EXECUTABLE", cli)
        .env("PATH", child_path(adapter, cli))
        .env("NO_COLOR", "1");
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
    let (cli, adapter, version, other_labels) = tokio::task::spawn_blocking(|| {
        let found = resolve_claude();
        let adapter = find_adapter();
        let version = found.as_ref().and_then(|f| describe(&f.path).0);
        let others: Vec<String> = found.as_ref().map(|f| f.others.iter().map(|o| describe(o).1).collect()).unwrap_or_default();
        (found.map(|f| f.path), adapter, version, others)
    })
    .await
    .unwrap_or((None, None, None, Vec::new()));
    let installed = cli.is_some() && adapter.is_some();
    let has_cli = cli.is_some();
    WorkEngineInfo {
        id: "claude".to_string(),
        label: "Claude Code".to_string(),
        installed,
        missing: missing_reason(has_cli, adapter.is_some()),
        adapter_path: adapter.map(|a| a.display().to_string()),
        engines: Vec::new(),
        path: cli.map(|c| c.display().to_string()),
        version,
        install_url: if installed || !has_cli { CLI_INSTALL_URL } else { ADAPTER_URL }.to_string(),
        model: saved_model(),
        config_path: prefs_path().map(|p| p.display().to_string()),
        others: other_labels,
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
    fn finds_cli_and_adapter_on_path_and_in_install_dirs() {
        let path_dir = tmp("path");
        let home = tmp("home");
        let cli = fake(&path_dir, "claude");
        assert_eq!(find_claude_in(None, Some(path_dir.clone().into_os_string()), Some(&home)), Some(cli));
        // adapter only in ~/.npm-global/bin (a GUI PATH misses it)
        let npm = home.join(".npm-global").join("bin");
        std::fs::create_dir_all(&npm).unwrap();
        let adapter = fake(&npm, "claude-agent-acp");
        assert_eq!(find_adapter_in(None, Some(OsString::from("/nonexistent")), Some(&home)), Some(adapter));
        // the deprecated Zed binary name still counts
        let legacy_home = tmp("legacy");
        let lb = legacy_home.join(".local").join("bin");
        std::fs::create_dir_all(&lb).unwrap();
        let legacy = fake(&lb, "claude-code-acp");
        assert_eq!(find_adapter_in(None, None, Some(&legacy_home)), Some(legacy));
        // an override that doesn't exist is exclusive: missing
        assert_eq!(find_claude_in(Some("/nonexistent/claude"), Some(path_dir.into_os_string()), Some(&home)), None);
    }

    #[test]
    fn missing_reason_names_what_to_install() {
        assert_eq!(missing_reason(true, true), None);
        assert!(missing_reason(false, true).unwrap().contains(CLI_INSTALL_URL));
        let a = missing_reason(true, false).unwrap();
        assert!(a.contains("npm install -g @agentclientprotocol/claude-agent-acp"), "{a}");
        let both = missing_reason(false, false).unwrap();
        assert!(both.contains(CLI_INSTALL_URL) && both.contains(ADAPTER_PACKAGE));
    }

    #[test]
    fn version_parses_claude_output() {
        assert_eq!(parse_version("2.1.322 (Claude Code)\n").as_deref(), Some("2.1.322"));
    }
}
