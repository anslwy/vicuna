// Find agent CLIs (opencode, claude) the way the user's terminal does.
//
// A macOS app launched from Finder/Dock gets launchd's PATH
// (/usr/bin:/bin:/usr/sbin:/sbin), not the login shell's. Detection then
// fell through to fixed fallback folders and could pick a stale binary
// (e.g. an old ~/.opencode/bin/opencode) while `npm run desktop`, started
// from a terminal, used the newer one on the shell PATH. So:
//
// 1. Read the login shell's PATH once (`$SHELL -l -c 'printf … "$PATH"'`,
//    like fix-path-env) and also export it to this process at startup, so
//    engines and the MCP servers they spawn see the same PATH.
// 2. Collect every candidate from the login PATH, the process PATH and the
//    known install folders (deduplicated by real path).
// 3. With more than one candidate, prefer the newest `--version`; ties keep
//    the login-shell order (what `command -v` would return).

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const SHELL_TIMEOUT: Duration = Duration::from_secs(5);
const VERSION_TIMEOUT: Duration = Duration::from_secs(8);
const MARK: &str = "__VICUNA_LOGIN_PATH__";

// ---------- login shell PATH ----------

/// The login shell's PATH (cached; None on Windows or when the shell fails).
pub fn login_shell_path() -> Option<OsString> {
    static CACHE: OnceLock<Option<OsString>> = OnceLock::new();
    CACHE.get_or_init(read_login_path).clone()
}

#[cfg(unix)]
fn read_login_path() -> Option<OsString> {
    let shell = std::env::var("SHELL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/sh" }.to_string());
    let script = format!("printf '{MARK}%s{MARK}' \"$PATH\"");
    let child = Command::new(&shell)
        .args(["-l", "-c", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let out = wait_output(child, SHELL_TIMEOUT)?;
    parse_marked_path(&String::from_utf8_lossy(&out))
}

#[cfg(not(unix))]
fn read_login_path() -> Option<OsString> {
    None
}

/// The PATH between the markers (login scripts may print banners around it).
pub fn parse_marked_path(stdout: &str) -> Option<OsString> {
    let start = stdout.find(MARK)? + MARK.len();
    let len = stdout[start..].find(MARK)?;
    let p = stdout[start..start + len].trim();
    (!p.is_empty()).then(|| OsString::from(p))
}

/// Login PATH first, then entries only the process PATH has.
pub fn merge_paths(login: Option<&OsString>, process: Option<&OsString>) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in [login, process].into_iter().flatten() {
        for d in std::env::split_paths(p) {
            if !d.as_os_str().is_empty() && !dirs.contains(&d) {
                dirs.push(d);
            }
        }
    }
    if dirs.is_empty() {
        return None;
    }
    std::env::join_paths(dirs).ok()
}

/// Export the login PATH to this process (call once at startup, before any
/// threads exist), so every child (engines, MCP servers) inherits it.
pub fn import_login_path() {
    let merged = merge_paths(login_shell_path().as_ref(), std::env::var_os("PATH").as_ref());
    if let Some(m) = merged {
        // SAFETY: called at the top of `run()`, before Tauri or tokio start
        // any threads, so nothing reads the environment concurrently.
        unsafe { std::env::set_var("PATH", m) };
    }
}

// ---------- candidates + newest ----------

pub(crate) fn is_executable(p: &Path) -> bool {
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

/// Every executable named `names` in the PATH lists (in order) and then the
/// extra folders, deduplicated by real path (symlinks to one binary count once).
pub fn candidates(names: &[&str], path_lists: &[Option<OsString>], extra_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    for p in path_lists.iter().flatten() {
        dirs.extend(std::env::split_paths(p));
    }
    dirs.extend(extra_dirs.iter().cloned());
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for dir in dirs {
        for name in names {
            let c = dir.join(name);
            if !is_executable(&c) {
                continue;
            }
            let real = std::fs::canonicalize(&c).unwrap_or_else(|_| c.clone());
            if !seen.contains(&real) {
                seen.push(real);
                out.push(c);
            }
        }
    }
    out
}

/// "opencode v2.2.6", "2.1.322 (Claude Code)", "1.0.0-beta.3" -> [2,2,6] ...
pub fn parse_semver(s: &str) -> Option<Vec<u64>> {
    let word = s
        .split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))?;
    let core = word.split(['-', '+']).next()?;
    let nums: Vec<u64> = core.split('.').map_while(|p| p.parse().ok()).collect();
    (!nums.is_empty()).then_some(nums)
}

/// Newest candidate by version; unknown versions rank last; ties keep order.
pub fn pick_newest(cands: &[PathBuf], version_of: impl Fn(&Path) -> Option<String>) -> Option<PathBuf> {
    let mut best: Option<(&PathBuf, Option<Vec<u64>>)> = None;
    for c in cands {
        let v = version_of(c).as_deref().and_then(parse_semver);
        let better = match &best {
            None => true,
            Some((_, bv)) => match (&v, bv) {
                (Some(a), Some(b)) => a > b,
                (Some(_), None) => true,
                _ => false,
            },
        };
        if better {
            best = Some((c, v));
        }
    }
    best.map(|(p, _)| p.clone())
}

// ---------- `--version`, cached per binary ----------

fn wait_output(mut child: std::process::Child, timeout: Duration) -> Option<Vec<u8>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut out = Vec::new();
    use std::io::Read as _;
    child.stdout.take()?.read_to_end(&mut out).ok()?;
    Some(out)
}

/// `<bin> --version` stdout (first line with a version), cached by path +
/// size + mtime so repeated detection (focus, Refresh) stays cheap and an
/// upgrade in place is noticed.
pub fn version_of(bin: &Path) -> Option<String> {
    type Key = (PathBuf, u64, Option<std::time::SystemTime>);
    static CACHE: OnceLock<Mutex<HashMap<Key, Option<String>>>> = OnceLock::new();
    let meta = std::fs::metadata(bin).ok()?;
    let key = (bin.to_path_buf(), meta.len(), meta.modified().ok());
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok()?.get(&key) {
        return v.clone();
    }
    let mut cmd = Command::new(bin);
    cmd.arg("--version")
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = merge_paths(login_shell_path().as_ref(), std::env::var_os("PATH").as_ref()) {
        cmd.env("PATH", p);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let v = cmd
        .spawn()
        .ok()
        .and_then(|c| wait_output(c, VERSION_TIMEOUT))
        .map(|o| String::from_utf8_lossy(&o).to_string())
        .and_then(|s| s.lines().find(|l| parse_semver(l).is_some()).map(|l| l.trim().to_string()));
    if let Ok(mut c) = cache.lock() {
        c.insert(key, v.clone());
    }
    v
}

/// What detection found: the chosen binary and every other candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub path: PathBuf,
    pub others: Vec<PathBuf>,
}

/// Resolve `names`: exclusive override; else every candidate on the login
/// PATH, the process PATH and `extra_dirs`, newest version first.
pub fn resolve(names: &[&str], override_bin: Option<&str>, extra_dirs: &[PathBuf]) -> Option<Resolved> {
    if let Some(o) = override_bin.map(str::trim).filter(|o| !o.is_empty()) {
        let p = PathBuf::from(o);
        return is_executable(&p).then_some(Resolved { path: p, others: Vec::new() });
    }
    let cands = candidates(names, &[login_shell_path(), std::env::var_os("PATH")], extra_dirs);
    let path = if cands.len() > 1 { pick_newest(&cands, version_of)? } else { cands.first()?.clone() };
    let others = cands.into_iter().filter(|c| *c != path).collect();
    Some(Resolved { path, others })
}

/// Errors from engine CLIs arrive with terminal colours; drop the escapes.
pub fn strip_ansi(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::var_os("TMPDIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let d = base.join(format!("vicuna-resolve-{tag}-{n}"));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[cfg(unix)]
    fn fake(dir: &Path, name: &str, version: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\necho '{version}'\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn login_path_parsed_between_markers_despite_banners() {
        let out = format!("Welcome!\n{MARK}/opt/homebrew/bin:/usr/bin{MARK}\nbye");
        assert_eq!(parse_marked_path(&out), Some(OsString::from("/opt/homebrew/bin:/usr/bin")));
        assert_eq!(parse_marked_path("no markers"), None);
        let merged = merge_paths(Some(&OsString::from("/a:/b")), Some(&OsString::from("/b:/c"))).unwrap();
        assert_eq!(merged, OsString::from("/a:/b:/c"));
    }

    #[test]
    fn semver_parsing_and_order() {
        assert_eq!(parse_semver("opencode v2.2.6"), Some(vec![2, 2, 6]));
        assert_eq!(parse_semver("2.1.322 (Claude Code)"), Some(vec![2, 1, 322]));
        assert_eq!(parse_semver("1.10.0-beta.3"), Some(vec![1, 10, 0]));
        assert!(parse_semver("1.10.0").unwrap() > parse_semver("1.9.12").unwrap());
        assert_eq!(parse_semver("no version"), None);
    }

    /// The owner's bug: a Finder-launched app (PATH=/usr/bin:/bin) found an
    /// old opencode in a fallback folder while the terminal's PATH has a newer
    /// one. With the login PATH included, the newest binary wins.
    #[cfg(unix)]
    #[test]
    fn finder_launch_prefers_newest_over_stale_fallback() {
        let home = tmp("home");
        let stale_dir = home.join(".opencode").join("bin");
        std::fs::create_dir_all(&stale_dir).unwrap();
        let stale = fake(&stale_dir, "opencode", "opencode v0.3.1");
        let brew = tmp("brew");
        let fresh = fake(&brew, "opencode", "2.2.6");
        let finder_path = Some(OsString::from("/usr/bin:/bin"));
        // before: Finder PATH + fallback folders only -> the stale one
        let before = candidates(&["opencode"], &[finder_path.clone()], &[stale_dir.clone()]);
        assert_eq!(before, vec![stale.clone()]);
        // after: the login PATH adds brew; newest version wins regardless of order
        let after = candidates(&["opencode"], &[Some(brew.clone().into_os_string()), finder_path], &[stale_dir]);
        assert_eq!(after.len(), 2);
        assert_eq!(pick_newest(&after, version_of), Some(fresh.clone()));
        let reversed: Vec<PathBuf> = after.iter().rev().cloned().collect();
        assert_eq!(pick_newest(&reversed, version_of), Some(fresh));
        // unknown versions rank last; all unknown keeps the first
        assert_eq!(pick_newest(&[stale.clone()], |_| None), Some(stale));
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_duplicates_count_once() {
        let a = tmp("a");
        let b = tmp("b");
        let real = fake(&a, "claude", "2.1.322 (Claude Code)");
        std::os::unix::fs::symlink(&real, b.join("claude")).unwrap();
        let c = candidates(&["claude"], &[Some(a.into_os_string()), Some(b.into_os_string())], &[]);
        assert_eq!(c, vec![real]);
    }

    #[test]
    fn ansi_codes_are_stripped() {
        let raw = "\u{1b}[91m\u{1b}[1mError: \u{1b}[0mConfiguration is invalid";
        assert_eq!(strip_ansi(raw), "Error: Configuration is invalid");
    }
}
