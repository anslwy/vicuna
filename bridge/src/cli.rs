// `vicuna` CLI helpers: Tailscale exposure, the background service, and
// `vicuna open` (the old desktop launcher's job: open a folder in the app).

use std::path::{Path, PathBuf};
use std::process::Command;

fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn run_ok(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(stdout)
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if err.is_empty() { stdout.trim().to_string() } else { err })
    }
}

// ---------------------------------------------------------------- tailscale

/// The tailscale CLI: PATH first, then the usual install spots (SteamOS keeps
/// it in /opt/tailscale; the macOS app ships the CLI inside the bundle).
fn tailscale_bin() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let p = dir.join("tailscale");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    [
        "/opt/tailscale/tailscale",
        "/usr/bin/tailscale",
        "/usr/local/bin/tailscale",
        "/opt/homebrew/bin/tailscale",
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Pick the HTTPS port: one already proxying to us (idempotent restarts), else
/// the first free one from 8443 up. `status` is `tailscale serve status --json`.
pub fn pick_https_port(status: &serde_json::Value, backend_port: u16, wanted: Option<u16>) -> u16 {
    let target = format!("http://127.0.0.1:{backend_port}");
    if let Some(web) = status.get("Web").and_then(|w| w.as_object()) {
        for (hostport, cfg) in web {
            let proxies_us = cfg
                .get("Handlers")
                .and_then(|h| h.as_object())
                .is_some_and(|h| h.values().any(|v| v.get("Proxy").and_then(|p| p.as_str()) == Some(target.as_str())));
            if proxies_us {
                if let Some(p) = hostport.rsplit(':').next().and_then(|p| p.parse().ok()) {
                    if wanted.is_none_or(|w| w == p) {
                        return p;
                    }
                }
            }
        }
    }
    if let Some(w) = wanted {
        return w;
    }
    let used: Vec<u16> = status
        .get("TCP")
        .and_then(|t| t.as_object())
        .map(|t| t.keys().filter_map(|k| k.parse().ok()).collect())
        .unwrap_or_default();
    (8443..=8499).find(|p| !used.contains(p)).unwrap_or(8443)
}

/// `tailscale serve --bg --https=<p> http://127.0.0.1:<port>` and return the
/// tailnet URL. Errors carry a hint for the usual permission problem.
pub fn tailscale_expose(backend_port: u16, wanted: Option<u16>) -> Result<String, String> {
    let ts = tailscale_bin().ok_or("tailscale is not installed (https://tailscale.com/download)")?;
    let status: serde_json::Value = run_ok(Command::new(&ts).args(["serve", "status", "--json"]))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let https = pick_https_port(&status, backend_port, wanted);
    run_ok(Command::new(&ts).args(["serve", "--bg", &format!("--https={https}"), &format!("http://127.0.0.1:{backend_port}")]))
        .map_err(|e| {
            if e.to_lowercase().contains("access denied") || e.to_lowercase().contains("operator") {
                format!("{e}\n  fix: sudo tailscale set --operator=$USER   (then run this again)")
            } else {
                e
            }
        })?;
    let me: serde_json::Value = run_ok(Command::new(&ts).args(["status", "--self", "--json"]))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let dns = me
        .pointer("/Self/DNSName")
        .and_then(|d| d.as_str())
        .map(|d| d.trim_end_matches('.').to_string())
        .filter(|d| !d.is_empty())
        .ok_or("tailscale serve is on, but MagicDNS/HTTPS isn't enabled for this tailnet (admin console → DNS)")?;
    Ok(format!("https://{dns}:{https}"))
}

// ---------------------------------------------------------------- service

const SERVICE_NAME: &str = "vicuna";
const LAUNCHD_LABEL: &str = "com.vicuna.server";

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn systemd_unit(exe: &Path, serve_args: &[String]) -> String {
    let mut cmd = vec![shell_quote(&exe.to_string_lossy()), "serve".to_string()];
    cmd.extend(serve_args.iter().map(|a| shell_quote(a)));
    format!(
        "[Unit]\nDescription=Vicuna server (vicuna serve)\nAfter=network-online.target\n\n[Service]\nExecStart={}\nRestart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
        cmd.join(" ")
    )
}

pub fn launchd_plist(exe: &Path, serve_args: &[String], log: &Path) -> String {
    let mut args = vec![exe.to_string_lossy().to_string(), "serve".to_string()];
    args.extend(serve_args.iter().cloned());
    let items: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml_escape(a))).collect();
    let log = xml_escape(&log.to_string_lossy());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key><string>{LAUNCHD_LABEL}</string>\n  <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  <key>RunAtLoad</key><true/>\n  <key>KeepAlive</key><true/>\n  <key>StandardOutPath</key><string>{log}</string>\n  <key>StandardErrorPath</key><string>{log}</string>\n</dict>\n</plist>\n"
    )
}

/// Install + start `vicuna serve <args>` as a per-user service that survives
/// logout/reboot: a systemd user unit (Linux) or a launchd agent (macOS).
pub fn install_service(serve_args: &[String]) -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if cfg!(target_os = "macos") {
        let dir = home().join("Library/LaunchAgents");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let plist = dir.join(format!("{LAUNCHD_LABEL}.plist"));
        let log = home().join("Library/Logs/vicuna-server.log");
        std::fs::write(&plist, launchd_plist(&exe, serve_args, &log)).map_err(|e| e.to_string())?;
        let uid = run_ok(Command::new("id").arg("-u"))?.trim().to_string();
        let domain = format!("gui/{uid}");
        let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{LAUNCHD_LABEL}")]).output();
        run_ok(Command::new("launchctl").args(["bootstrap", &domain, &plist.to_string_lossy()]))?;
        Ok(format!("Installed {} (log: {}).", plist.display(), log.display()))
    } else if cfg!(target_os = "linux") {
        let dir = home().join(".config/systemd/user");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let unit = dir.join(format!("{SERVICE_NAME}.service"));
        std::fs::write(&unit, systemd_unit(&exe, serve_args)).map_err(|e| e.to_string())?;
        run_ok(Command::new("systemctl").args(["--user", "daemon-reload"]))
            .map_err(|e| format!("systemctl --user failed ({e}). In a container, run the service on the host instead."))?;
        run_ok(Command::new("systemctl").args(["--user", "enable", "--now", SERVICE_NAME]))?;
        let mut msg = format!("Installed {} and started it (logs: journalctl --user -u {SERVICE_NAME} -f).", unit.display());
        let user = std::env::var("USER").unwrap_or_default();
        let linger = run_ok(Command::new("loginctl").args(["show-user", &user, "-p", "Linger"])).unwrap_or_default();
        if !linger.contains("Linger=yes") {
            msg.push_str(&format!("\nTo keep it running after you log out: sudo loginctl enable-linger {user}"));
        }
        Ok(msg)
    } else {
        Err("install-service supports Linux (systemd) and macOS (launchd).".into())
    }
}

pub fn uninstall_service() -> Result<String, String> {
    if cfg!(target_os = "macos") {
        let plist = home().join(format!("Library/LaunchAgents/{LAUNCHD_LABEL}.plist"));
        let uid = run_ok(Command::new("id").arg("-u"))?.trim().to_string();
        let _ = Command::new("launchctl").args(["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")]).output();
        let _ = std::fs::remove_file(&plist);
        Ok(format!("Removed {}.", plist.display()))
    } else if cfg!(target_os = "linux") {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", SERVICE_NAME]).output();
        let unit = home().join(format!(".config/systemd/user/{SERVICE_NAME}.service"));
        let _ = std::fs::remove_file(&unit);
        let _ = Command::new("systemctl").args(["--user", "daemon-reload"]).output();
        Ok(format!("Removed {}.", unit.display()))
    } else {
        Err("uninstall-service supports Linux (systemd) and macOS (launchd).".into())
    }
}

// ---------------------------------------------------------------- open

/// RFC 3986 unreserved stays, everything else (incl. `/`) is %-encoded — the
/// app's `vicuna://open?path=` parser decodes it (extractOpenPath).
pub fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Open a folder in the Vicuna desktop app through its `vicuna://` link.
pub fn open_in_app(path: Option<&str>) -> Result<String, String> {
    let raw = path.map(PathBuf::from).unwrap_or(std::env::current_dir().map_err(|e| e.to_string())?);
    let abs = std::fs::canonicalize(&raw).map_err(|e| format!("{}: {e}", raw.display()))?;
    if !abs.is_dir() {
        return Err(format!("{} is not a folder", abs.display()));
    }
    let url = format!("vicuna://open?path={}", percent_encode(&abs.to_string_lossy()));
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(&url);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.args(["/C", "start", "", &url]);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(&url);
        c
    };
    run_ok(&mut cmd).map_err(|e| format!("could not open the Vicuna app ({e}). Is it installed?"))?;
    Ok(abs.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn https_port_reuses_ours_else_first_free() {
        let status = json!({
            "TCP": { "8443": {"HTTPS": true}, "8444": {"HTTPS": true}, "8446": {"HTTPS": true} },
            "Web": {
                "deck.ts.net:8443": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:6767" } } },
                "deck.ts.net:8446": { "Handlers": { "/": { "Proxy": "http://127.0.0.1:7421" } } }
            }
        });
        assert_eq!(pick_https_port(&status, 7421, None), 8446); // already ours
        assert_eq!(pick_https_port(&status, 7999, None), 8445); // first free
        assert_eq!(pick_https_port(&status, 7999, Some(9000)), 9000);
        assert_eq!(pick_https_port(&serde_json::Value::Null, 7421, None), 8443);
    }

    #[test]
    fn service_files_quote_and_escape() {
        let unit = systemd_unit(Path::new("/home/me/.local/bin/vicuna"), &["--port".into(), "7421".into(), "--tailscale".into()]);
        assert!(unit.contains("ExecStart=/home/me/.local/bin/vicuna serve --port 7421 --tailscale"));
        let unit2 = systemd_unit(Path::new("/opt/my apps/vicuna"), &[]);
        assert!(unit2.contains("ExecStart='/opt/my apps/vicuna' serve"));
        let plist = launchd_plist(Path::new("/Users/me/bin/vicuna"), &["--tailscale".into()], Path::new("/tmp/v&.log"));
        assert!(plist.contains("<string>serve</string>") && plist.contains("<string>--tailscale</string>"));
        assert!(plist.contains("/tmp/v&amp;.log"));
    }

    #[test]
    fn open_links_encode_every_separator() {
        assert_eq!(percent_encode("/Users/me/My Repo"), "%2FUsers%2Fme%2FMy%20Repo");
        assert_eq!(percent_encode("a-b_c.d~e"), "a-b_c.d~e");
    }
}
