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

/// True when this process may replace its own binary from the UI.
pub fn can_upgrade() -> bool {
    if cfg!(feature = "desktop") || cfg!(windows) {
        return false;
    }
    match std::env::current_exe() {
        Ok(exe) => !exe.components().any(|c| c.as_os_str() == "target"),
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
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let dir = super::server_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let log_path = dir.join("upgrade.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("{}: {e}", log_path.display()))?;
        let err = log.try_clone().map_err(|e| e.to_string())?;
        std::process::Command::new(&exe)
            .arg("upgrade")
            .process_group(0)
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err)
            .spawn()
            .map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok(serde_json::json!({ "started": true, "log": log_path.to_string_lossy() }))
    }
    #[cfg(not(unix))]
    {
        Err("Run `vicuna upgrade` on the server.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newer_compares_numerically() {
        assert!(is_newer("v0.0.10", "0.0.9"));
        assert!(!is_newer("v0.0.33", "0.0.33"));
        assert!(!is_newer("garbage", "0.0.1"));
    }
}
