// In-app upgrade of a headless `vicuna serve` (desktop server menu → Upgrade).
//
// `server_upgrade_check` compares this build with the newest stable GitHub
// release; `server_upgrade` runs `<this exe> upgrade` detached, so the CLI's
// own checksum-verified install + db backup + restart (bridge/src/upgrade.rs)
// does the work exactly like a terminal `vicuna upgrade`. The desktop's
// embedded server never upgrades itself: the app updater owns that binary.

const REPO: &str = "anslwy/vicuna";

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((parse_version(latest), parse_version(current)), (Some(l), Some(c)) if l > c)
}

/// Strip Linux's ` (deleted)` suffix from `/proc/self/exe` when the binary on
/// disk was atomically replaced while this server process is still running
/// (e.g. after `vicuna upgrade --no-restart`, `install-cli.sh`, or a failed
/// restart), so retrying an in-app upgrade execs the replacement binary
/// instead of failing with ENOENT.
pub fn strip_deleted_suffix(exe: &std::path::Path) -> std::path::PathBuf {
    if let Some(s) = exe.to_str() {
        if let Some(stripped) = s.strip_suffix(" (deleted)") {
            return std::path::PathBuf::from(stripped);
        }
    }
    exe.to_path_buf()
}

/// True when this process may replace its own binary from the UI.
pub fn can_upgrade() -> bool {
    if cfg!(feature = "desktop") || cfg!(windows) {
        return false;
    }
    match std::env::current_exe() {
        Ok(exe) => {
            let exe = strip_deleted_suffix(&exe);
            !exe.components().any(|c| c.as_os_str() == "target")
        }
        Err(_) => false,
    }
}

pub async fn check() -> Result<serde_json::Value, String> {
    let url = std::env::var("VICUNA_RELEASE_API")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let r = reqwest::Client::builder()
        .user_agent(concat!("vicuna-server/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?
        .get(&url)
        .send()
        .await
        .map_err(|e| format!("release check failed: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("release check failed: HTTP {}", r.status()));
    }
    let v: serde_json::Value = r.json().await.map_err(|e| format!("release JSON: {e}"))?;
    let latest = v.get("tag_name").and_then(|t| t.as_str()).ok_or("release has no tag_name")?;
    let current = env!("CARGO_PKG_VERSION");
    Ok(serde_json::json!({
        "current": current,
        "latest": latest.trim_start_matches('v'),
        "newer": is_newer(latest, current),
        "canUpgrade": can_upgrade(),
    }))
}

/// Start `vicuna upgrade` in its own process group, logging to
/// `~/vicuna/server/upgrade.log`. Returns at once; the server restarts itself
/// a few seconds later and clients reconnect.
pub fn start() -> Result<serde_json::Value, String> {
    if !can_upgrade() {
        return Err("This server can't upgrade itself from the app (desktop or dev build). Run `vicuna upgrade` on it.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let exe = strip_deleted_suffix(&std::env::current_exe().map_err(|e| e.to_string())?);
        let dir = super::server_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let log_path = dir.join("upgrade.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("{}: {e}", log_path.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        let offset = log.metadata().map(|m| m.len()).unwrap_or(0);
        let child = std::process::Command::new(&exe)
            .arg("upgrade")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        *RUN.lock().unwrap() = Some(Run { log: log_path.clone(), offset, child });
        Ok(serde_json::json!({ "started": true, "log": log_path.to_string_lossy() }))
    }
    #[cfg(not(unix))]
    {
        Err("Run `vicuna upgrade` on the server.".into())
    }
}

struct Run {
    log: std::path::PathBuf,
    offset: u64,
    child: std::process::Child,
}

static RUN: std::sync::Mutex<Option<Run>> = std::sync::Mutex::new(None);

/// Progress of the upgrade this process started: `{running, stage, percent,
/// detail, failed, error}`. After the restart the new process knows no run
/// (`running: false, stage: null`): the client then waits for the version.
pub fn status() -> Result<serde_json::Value, String> {
    let mut guard = RUN.lock().unwrap();
    let Some(run) = guard.as_mut() else {
        return Ok(serde_json::json!({ "running": false, "stage": null }));
    };
    let exit = run.child.try_wait().ok().flatten();
    let text = std::fs::read(&run.log)
        .map(|b| String::from_utf8_lossy(b.get(run.offset as usize..).unwrap_or(&[])).into_owned())
        .unwrap_or_default();
    let mut p = parse_progress(&text);
    let failed = matches!(exit, Some(st) if !st.success());
    if failed {
        p["error"] = serde_json::json!(text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("upgrade failed").trim());
    }
    p["running"] = serde_json::json!(exit.is_none());
    p["failed"] = serde_json::json!(failed);
    Ok(p)
}

/// Turn the `vicuna upgrade` log into `{stage, step, steps, percent, detail}`.
pub fn parse_progress(log: &str) -> serde_json::Value {
    const STEPS: u64 = 5;
    let (mut stage, mut step, mut percent, mut detail) = (None::<String>, 0u64, None::<u64>, None::<String>);
    for line in log.lines() {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("==> ") {
            percent = None;
            detail = None;
            let (s, n) = if h.starts_with("Checking") {
                ("Checking the latest release…".to_string(), 1)
            } else if let Some(a) = h.strip_prefix("Downloading ") {
                (format!("Downloading {a}…"), 2)
            } else if h.starts_with("Installed the bundled browser") {
                ("Installing the browser engine…".into(), 3)
            } else if let Some(r) = h.strip_prefix("Installing ") {
                (format!("Installing {}…", r.split(' ').next().unwrap_or("")), 3)
            } else if h.starts_with("Running server") {
                continue;
            } else if h.starts_with("Backing up") || h.starts_with("No database") {
                ("Backing up the database…".into(), 4)
            } else if h.starts_with("Restarting") {
                ("Restarting the server…".into(), 5)
            } else {
                (h.to_string(), step)
            };
            stage = Some(s);
            step = n;
        } else if let Some(d) = t.strip_prefix("downloaded ") {
            detail = Some(d.to_string());
            percent = d.rsplit_once('(').and_then(|(_, p)| p.trim_end_matches(&['%', ')'][..]).parse().ok());
        } else if t.starts_with("sha256 OK") {
            detail = Some("checksum verified".into());
        } else if t.starts_with("Done:") {
            stage = Some("Done".into());
        }
    }
    serde_json::json!({ "stage": stage, "step": step, "steps": STEPS, "percent": percent, "detail": detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_follows_the_log() {
        let log = "==> Checking the latest release of anslwy/vicuna\n    installed: 0.0.37   latest: v0.0.38\n==> Downloading vicuna-linux-x86_64.tar.gz\n    downloaded 4.0 / 16.0 MB (25%)\n";
        let p = parse_progress(log);
        assert_eq!(p["stage"], "Downloading vicuna-linux-x86_64.tar.gz…");
        assert_eq!(p["percent"], 25);
        assert_eq!(p["step"], 2);
        let p = parse_progress(&format!("{log}    sha256 OK (ab)\n==> Running server: x\n==> Backing up a → b\n"));
        assert_eq!(p["stage"], "Backing up the database…");
        assert!(p["percent"].is_null());
        assert_eq!(p["step"], 4);
    }
    #[test]
    fn newer_compares_numerically() {
        assert!(is_newer("v0.0.10", "0.0.9"));
        assert!(!is_newer("v0.0.33", "0.0.33"));
        assert!(!is_newer("garbage", "0.0.1"));
    }
    #[test]
    fn strip_deleted_suffix_handles_linux_unlinked_exe() {
        let p = std::path::Path::new("/home/ubuntu/.local/bin/vicuna (deleted)");
        assert_eq!(strip_deleted_suffix(p), std::path::PathBuf::from("/home/ubuntu/.local/bin/vicuna"));
        let normal = std::path::Path::new("/home/ubuntu/.local/bin/vicuna");
        assert_eq!(strip_deleted_suffix(normal), normal);
    }
}
