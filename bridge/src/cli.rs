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

const TAILSCALE_SPOTS: &[&str] = &[
    "/opt/tailscale/tailscale",
    "/usr/bin/tailscale",
    "/usr/local/bin/tailscale",
    "/usr/sbin/tailscale",
    "/opt/homebrew/bin/tailscale",
    "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
];

/// Where distrobox/toolbox mount the host's filesystem inside a container.
const HOST_ROOT: &str = "/run/host";

const TAILSCALE_SOCKETS: &[&str] = &["/run/tailscale/tailscaled.sock", "/var/run/tailscale/tailscaled.sock"];

/// The tailscale CLI and, when the daemon's socket is only reachable through
/// the host mount, an explicit `--socket`. PATH first, then the usual install
/// spots (SteamOS keeps it in /opt/tailscale; the macOS app ships the CLI
/// inside the bundle), then the same spots under `/run/host`: inside a
/// distrobox/toolbox container the host's tailscale isn't on the container's
/// filesystem, but the host root is mounted there (SteamOS + distrobox, hit
/// live). `exists` is injected for tests.
pub(crate) fn find_tailscale(path_env: Option<&std::ffi::OsStr>, exists: impl Fn(&Path) -> bool) -> Option<(PathBuf, Option<PathBuf>)> {
    let on_path = path_env.into_iter().flat_map(std::env::split_paths).map(|d| d.join("tailscale"));
    let spots = TAILSCALE_SPOTS.iter().map(PathBuf::from);
    let host = TAILSCALE_SPOTS.iter().map(|s| Path::new(HOST_ROOT).join(s.trim_start_matches('/')));
    let bin = on_path.chain(spots).chain(host).find(|p| exists(p))?;
    // the CLI's default socket paths; distrobox usually links them to the
    // host's, but a plain container may not
    let socket = if TAILSCALE_SOCKETS.iter().any(|s| exists(Path::new(s))) {
        None
    } else {
        TAILSCALE_SOCKETS
            .iter()
            .map(|s| Path::new(HOST_ROOT).join(s.trim_start_matches('/')))
            .find(|p| exists(p))
    };
    Some((bin, socket))
}

/// `tailscale [--socket=…] <args>` for the CLI found by `find_tailscale`.
fn tailscale_cmd(ts: &(PathBuf, Option<PathBuf>), args: &[&str]) -> Command {
    let mut c = Command::new(&ts.0);
    if let Some(sock) = &ts.1 {
        c.arg(format!("--socket={}", sock.display()));
    }
    c.args(args);
    c
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
    let path_env = std::env::var_os("PATH");
    let ts = find_tailscale(path_env.as_deref(), |p| p.exists()).ok_or(
        "tailscale is not installed (https://tailscale.com/download). In a container, install it on the host; \
         Vicuna also looks under /run/host",
    )?;
    let status: serde_json::Value = run_ok(&mut tailscale_cmd(&ts, &["serve", "status", "--json"]))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(serde_json::Value::Null);
    let https = pick_https_port(&status, backend_port, wanted);
    run_ok(&mut tailscale_cmd(&ts, &["serve", "--bg", &format!("--https={https}"), &format!("http://127.0.0.1:{backend_port}")]))
        .map_err(|e| {
            if e.to_lowercase().contains("access denied") || e.to_lowercase().contains("operator") {
                format!("{e}\n  fix: sudo tailscale set --operator=$USER   (then run this again)")
            } else {
                e
            }
        })?;
    let me: serde_json::Value = run_ok(&mut tailscale_cmd(&ts, &["status", "--self", "--json"]))
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
    fn tailscale_found_on_path_spots_or_host_mount() {
        let has = |set: &'static [&'static str]| move |p: &Path| set.iter().any(|s| Path::new(s) == p);
        // PATH wins
        let path = std::ffi::OsString::from("/x/bin:/y/bin");
        let (bin, sock) = find_tailscale(Some(&path), has(&["/y/bin/tailscale", "/opt/tailscale/tailscale", "/run/tailscale/tailscaled.sock"])).unwrap();
        assert_eq!(bin, PathBuf::from("/y/bin/tailscale"));
        assert_eq!(sock, None);
        // SteamOS host
        let (bin, _) = find_tailscale(None, has(&["/opt/tailscale/tailscale", "/run/tailscale/tailscaled.sock"])).unwrap();
        assert_eq!(bin, PathBuf::from("/opt/tailscale/tailscale"));
        // distrobox on SteamOS: CLI only under /run/host, socket already linked
        let (bin, sock) = find_tailscale(Some(&path), has(&["/run/host/opt/tailscale/tailscale", "/run/tailscale/tailscaled.sock"])).unwrap();
        assert_eq!(bin, PathBuf::from("/run/host/opt/tailscale/tailscale"));
        assert_eq!(sock, None);
        // plain container: socket only through the host mount
        let (_, sock) = find_tailscale(None, has(&["/run/host/usr/bin/tailscale", "/run/host/run/tailscale/tailscaled.sock"])).unwrap();
        assert_eq!(sock, Some(PathBuf::from("/run/host/run/tailscale/tailscaled.sock")));
        assert!(find_tailscale(Some(&path), has(&[])).is_none());
    }

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
