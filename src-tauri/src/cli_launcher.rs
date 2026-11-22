use serde::Serialize;
use std::time::Duration;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

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
pub(crate) const CLI_MARKER: &str = "Installed by Vicuna";

/// POSIX launcher (macOS + Linux): resolve the arg (default `$PWD`), fully
/// percent-encode it (python3 when present, portable `od` fallback — slashes
/// included, the app decodes fully), open the link. `&` so the terminal
/// doesn't hang on `xdg-open`.
pub(crate) const CLI_UNIX_SCRIPT: &str = r#"#!/bin/sh
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
pub(crate) const CLI_WINDOWS_SCRIPT: &str = "@echo off\r\nrem Installed by Vicuna (Settings > General > Vicuna CLI). Opens a folder in the Vicuna desktop app: `vicuna [path]` (default: current directory).\r\nsetlocal\r\nset \"P=%~f1\"\r\nif \"%P%\"==\"\" set \"P=%CD%\"\r\nfor /f \"delims=\" %%E in ('powershell -NoProfile -NonInteractive -Command \"[uri]::EscapeDataString($env:P)\"') do set \"ENC=%%E\"\r\nstart \"\" \"vicuna://open?path=%ENC%\"\r\n";

pub(crate) fn cli_exe_name() -> &'static str {
    if cfg!(windows) { "vicuna.cmd" } else { "vicuna" }
}

pub(crate) fn cli_script_body() -> &'static str {
    if cfg!(windows) { CLI_WINDOWS_SCRIPT } else { CLI_UNIX_SCRIPT }
}

/// PATH entries of this process. GUI apps launched from Finder get a sparse
/// system PATH, so this alone misses user bins like `~/.local/bin` — every
/// consumer below works off `cli_all_path_dirs` (process + login shell).
pub(crate) fn cli_path_dirs() -> Vec<std::path::PathBuf> {
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
pub(crate) fn default_unix_shell() -> String {
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
pub(crate) async fn cli_shell_path_dirs() -> Vec<std::path::PathBuf> {
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
pub(crate) fn cli_all_path_dirs(shell_dirs: &[std::path::PathBuf]) -> Vec<std::path::PathBuf> {
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
pub(crate) fn cli_install_dir_in(path_dirs: &[std::path::PathBuf], home: &std::path::Path) -> std::path::PathBuf {
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
pub(crate) fn cli_scan_in(dirs: &[std::path::PathBuf], exe: &str) -> Option<std::path::PathBuf> {
    for d in dirs {
        let cand = d.join(exe);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub(crate) fn cli_scan(exe: &str, all_dirs: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    cli_scan_in(all_dirs, exe)
}

pub(crate) fn cli_is_managed(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|s| s.contains(CLI_MARKER))
        .unwrap_or(false)
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct CliStatus {
    pub(crate) exe_name: String,
    /// resolved `vicuna` on PATH, if any (managed or foreign)
    pub(crate) found: Option<String>,
    /// whether `found` is a launcher this app installed
    pub(crate) managed: bool,
    /// where Install would write
    pub(crate) install_dir: String,
    /// whether `install_dir` is on the effective PATH (process + login
    /// shell; Windows also checks the user PATH registry, which the current
    /// process may predate — a fresh terminal is authoritative)
    pub(crate) on_path: bool,
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

pub(crate) fn cli_status_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliStatus, String> {
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
pub(crate) struct CliInstallResult {
    location: String,
    on_path: bool,
    note: String,
}

/// Write the launcher file into `dir` (creating it). Unix sets 0o755.
pub(crate) fn cli_write_launcher(dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
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

pub(crate) fn cli_install_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<CliInstallResult, String> {
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
pub(crate) fn cli_uninstall_inner(all_dirs: &[std::path::PathBuf], home: &std::path::Path) -> Result<String, String> {
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
pub(crate) async fn cli_status() -> Result<CliStatus, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_status_inner(&all_dirs, &home)
}

/// Install (or reinstall) the `vicuna` launcher script.
#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn cli_install() -> Result<CliInstallResult, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_install_inner(&all_dirs, &home)
}

/// Remove the launcher this app installed. Refuses foreign binaries.
#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn cli_uninstall() -> Result<String, String> {
    let (all_dirs, home) = cli_env().await?;
    cli_uninstall_inner(&all_dirs, &home)
}
