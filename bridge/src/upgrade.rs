// `vicuna upgrade`: replace this binary with the newest GitHub release (same
// asset + checksum as scripts/install-cli.sh), back up the server database,
// and restart a running `vicuna serve` so it runs the new version.
//
// Detection and the restart itself live in `server` — `vicuna restart` and
// `vicuna upgrade` must restart a server exactly the same way. `--dry-run`
// prints every step without downloading, installing, backing up or spawning.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::server;

const REPO: &str = "anslwy/vicuna";

#[derive(Default, Debug)]
pub struct Opts {
    pub check: bool,
    pub force: bool,
    pub no_restart: bool,
    pub dry_run: bool,
}

pub fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts::default();
    for a in args {
        match a.as_str() {
            "--check" => o.check = true,
            "--force" => o.force = true,
            "--no-restart" => o.no_restart = true,
            "--dry-run" => o.dry_run = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

// ---------------------------------------------------------------- versions

pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = s.trim().trim_start_matches('v');
    let core = s.split(['-', '+']).next()?;
    let mut it = core.split('.').map(|p| p.parse::<u64>().ok());
    let v = (it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0));
    Some(v)
}

pub fn is_newer(latest: &str, current: &str) -> bool {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

/// Release asset target, same names as install-cli.sh.
pub fn target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("linux-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("macos", _) => Some("macos-universal"),
        _ => None,
    }
}

/// First hex token of a `sha256sum`-style line.
pub fn parse_sha256(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_ascii_lowercase();
    (h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then_some(h)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// `YYYYMMDD-HHMMSSZ` (UTC) for a unix time; no date crate needed.
pub fn stamp(unix: u64) -> String {
    let (days, secs) = ((unix / 86400) as i64, unix % 86400);
    // civil-from-days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}Z", secs / 3600, secs / 60 % 60, secs % 60)
}

// ---------------------------------------------------------------- network

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("vicuna-cli/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| e.to_string())
}

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let r = client()?.get(url).send().await.map_err(|e| format!("{url}: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("{url}: HTTP {}", r.status()));
    }
    r.bytes().await.map(|b| b.to_vec()).map_err(|e| format!("{url}: {e}"))
}

/// Latest stable release tag (`releases/latest` skips the rolling `latest`
/// prerelease). `VICUNA_RELEASE_API` overrides the URL for tests/mirrors.
async fn latest_tag() -> Result<String, String> {
    let url =
        std::env::var("VICUNA_RELEASE_API").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"));
    let body = fetch(&url).await?;
    let v: serde_json::Value = serde_json::from_slice(&body).map_err(|e| format!("release JSON: {e}"))?;
    v.get("tag_name").and_then(|t| t.as_str()).map(String::from).ok_or_else(|| "release has no tag_name".into())
}

// ---------------------------------------------------------------- install

/// Write `bytes` next to `dest` and rename over it. A rename swaps the
/// directory entry, so a running `vicuna` keeps its old inode — writing into
/// the running file would fail with ETXTBSY ("text file busy") on Linux.
pub fn install_atomic(bytes: &[u8], dest: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let dir = dest.parent().ok_or("install path has no folder")?;
    let tmp = dir.join(format!(".vicuna.upgrade-{}", std::process::id()));
    let res = (|| {
        std::fs::write(&tmp, bytes).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        // refuse a binary that can't even print its version
        let v = Command::new(&tmp).arg("--version").output().map_err(|e| format!("new binary won't run: {e}"))?;
        if !String::from_utf8_lossy(&v.stdout).starts_with("vicuna ") {
            return Err("new binary did not report a vicuna version".into());
        }
        if cfg!(target_os = "macos") {
            let _ = Command::new("xattr").args(["-d", "com.apple.quarantine"]).arg(&tmp).output();
        }
        std::fs::rename(&tmp, dest).map_err(|e| format!("replace {}: {e}", dest.display()))
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// The binary `upgrade` replaces: this one. A `…/target/…` path is a cargo
/// build tree, not the installed CLI — replacing it would surprise a
/// developer mid-build, so that needs `--force`.
pub fn install_exe(force: bool) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if is_dev_build(&exe) && !force {
        return Err(format!(
            "{} looks like a development build (…/target/…). Run `vicuna upgrade` from the installed CLI, or pass --force.",
            exe.display()
        ));
    }
    Ok(exe)
}

/// A cargo target dir (`bridge/target/debug/vicuna`, `src-tauri/target/…`).
pub fn is_dev_build(exe: &Path) -> bool {
    exe.components().any(|c| c.as_os_str() == "target")
}

// ---------------------------------------------------------------- main flow

pub fn run(opts: Opts, server_dir: &Path) -> Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string())?;
    let current = env!("CARGO_PKG_VERSION");
    let dry = if opts.dry_run { "[dry run] " } else { "" };

    println!("==> Checking the latest release of {REPO}");
    let tag = rt.block_on(latest_tag())?;
    println!("    installed: {current}   latest: {tag}");
    let newer = is_newer(&tag, current);

    if opts.check {
        if newer {
            println!("Update available: {current} → {tag}. Run `vicuna upgrade` to install it.");
        } else {
            println!("No newer release: {current} is the latest (--force would reinstall {tag}).");
        }
        return Ok(());
    }
    if !newer && !opts.force {
        println!("vicuna {current} is already the latest version (--force reinstalls {tag}).");
        return Ok(());
    }

    let tgt = target(std::env::consts::OS, std::env::consts::ARCH)
        .ok_or_else(|| format!("no release build for {}/{}", std::env::consts::OS, std::env::consts::ARCH))?;
    let base = std::env::var("VICUNA_DOWNLOAD_BASE")
        .unwrap_or_else(|_| format!("https://github.com/{REPO}/releases/download/{tag}"));
    let asset = format!("vicuna-{tgt}.tar.gz");

    if opts.dry_run {
        println!("==> {dry}Would download {base}/{asset}, verify its .sha256 and replace {}", install_exe(opts.force)?.display());
        return finish(&opts, None, &tag, server_dir, dry);
    }

    println!("==> Downloading {asset}");
    let archive = rt.block_on(fetch(&format!("{base}/{asset}")))?;
    let sum = rt.block_on(fetch(&format!("{base}/{asset}.sha256")))?;
    let want = parse_sha256(&String::from_utf8_lossy(&sum)).ok_or("bad .sha256 file")?;
    let got = sha256_hex(&archive);
    if want != got {
        return Err(format!("checksum mismatch for {asset} (want {want}, got {got})"));
    }
    println!("    sha256 OK ({got})");

    let work = std::env::temp_dir().join(format!("vicuna-upgrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let result = (|| {
        std::fs::write(work.join(&asset), &archive).map_err(|e| e.to_string())?;
        let st = Command::new("tar")
            .arg("-xzf")
            .arg(work.join(&asset))
            .arg("-C")
            .arg(&work)
            .status()
            .map_err(|e| format!("tar: {e}"))?;
        if !st.success() {
            return Err("could not extract the archive".to_string());
        }
        let bin = std::fs::read(work.join("vicuna")).map_err(|_| "archive has no vicuna binary".to_string())?;
        finish(&opts, Some(&bin), &tag, server_dir, dry)
    })();
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn finish(opts: &Opts, bin: Option<&[u8]>, tag: &str, server_dir: &Path, dry: &str) -> Result<(), String> {
    let exe = install_exe(opts.force)?;
    println!("==> {dry}Installing {tag} to {}", exe.display());
    if opts.dry_run {
        match bin {
            Some(b) => println!("    would write {} bytes to a temp file beside it and rename it into place", b.len()),
            None => println!("    would verify the download, write it beside the binary and rename it into place"),
        }
    } else {
        let bytes = bin.ok_or("internal: nothing to install")?;
        install_atomic(bytes, &exe)?;
        println!("    installed");
    }

    let (procs, running) = server::detect_live();
    let Some(server) = running else {
        println!("==> No running `vicuna serve` found; nothing to restart.");
        return done(tag, dry);
    };
    println!("==> Running server: {}", server.label());
    if opts.no_restart {
        println!("    --no-restart: leaving it on the old version until it restarts.");
        return done(tag, dry);
    }

    // back up the db the running server uses before it restarts on new code
    let args = server::serve_args_of_server(Some(&server), &procs);
    let db = server::db_path(&args, &server_dir.join("vicuna.db"));
    if db.exists() {
        let unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dest_dir = server_dir.join("backups").join(stamp(unix));
        let dest = dest_dir.join("vicuna.db");
        println!("==> {dry}Backing up {} → {}", db.display(), dest.display());
        if !opts.dry_run {
            std::fs::create_dir_all(&dest_dir).map_err(|e| format!("{}: {e}", dest_dir.display()))?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(vicuna_lib::server_store::backup_to(&db, &dest))
                .map_err(|e| format!("backup failed, not restarting: {e}"))?;
            println!("    backup saved: {}", dest.display());
        }
    } else {
        println!("==> No database at {}; skipping backup.", db.display());
    }

    let log = server::serve_log();
    println!("==> {dry}Restarting (detached): {}", server::restart_summary(&server, &exe, &log));
    if !opts.dry_run {
        server::spawn_restart(&server, &exe, &log)?;
        println!("    restart started; the server comes back on {tag} in a few seconds (`vicuna status`).");
    }
    done(tag, dry)
}

fn done(tag: &str, dry: &str) -> Result<(), String> {
    println!("{dry}Done: vicuna {tag}.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_numerically() {
        assert_eq!(parse_version("v0.0.19"), Some((0, 0, 19)));
        assert_eq!(parse_version("1.2"), Some((1, 2, 0)));
        assert_eq!(parse_version("latest"), None);
        assert!(is_newer("v0.0.19", "0.0.18"));
        assert!(is_newer("v0.1.0", "0.0.99"));
        assert!(!is_newer("v0.0.18", "0.0.18"));
        assert!(!is_newer("v0.0.9", "0.0.18"));
        assert!(!is_newer("latest", "0.0.18"));
    }

    #[test]
    fn opts_and_targets() {
        let o = parse_opts(&["--check".into(), "--force".into(), "--no-restart".into(), "--dry-run".into()]).unwrap();
        assert!(o.check && o.force && o.no_restart && o.dry_run);
        assert!(parse_opts(&["--nope".into()]).is_err());
        assert_eq!(target("linux", "x86_64"), Some("linux-x86_64"));
        assert_eq!(target("macos", "aarch64"), Some("macos-universal"));
        assert_eq!(target("windows", "x86_64"), None);
        assert_eq!(parse_sha256(&format!("{}  vicuna.tar.gz\n", "A".repeat(64))), Some("a".repeat(64)));
        assert_eq!(parse_sha256("nothex  x"), None);
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(stamp(0), "19700101-000000Z");
        assert_eq!(stamp(1_794_700_800 + 3723), "20261115-010203Z");
        assert_eq!(stamp(951_782_400), "20000229-000000Z");
    }

    #[test]
    fn dev_builds_are_not_overwritten_without_force() {
        assert!(is_dev_build(Path::new("/home/me/repo/bridge/target/debug/vicuna")));
        assert!(is_dev_build(Path::new("/Users/me/src/vicuna/src-tauri/target/release/vicuna")));
        assert!(!is_dev_build(Path::new("/home/me/.local/bin/vicuna")));
        assert!(!is_dev_build(Path::new("/usr/local/bin/vicuna")));
    }

    #[test]
    fn atomic_install_replaces_a_running_binary() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("vicuna-upgrade-inst-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("vicuna");
        // the "running" old binary: busy while it sleeps
        std::fs::write(&dest, "#!/bin/sh\necho 'vicuna 0.0.1'\nsleep 5\n").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut running = Command::new(&dest).stdout(std::process::Stdio::null()).spawn().unwrap();
        install_atomic(b"#!/bin/sh\necho 'vicuna 9.9.9'\n", &dest).unwrap();
        let out = Command::new(&dest).arg("--version").output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "vicuna 9.9.9");
        // a binary that doesn't answer like vicuna is refused, old one kept
        assert!(install_atomic(b"#!/bin/sh\necho nope\n", &dest).is_err());
        assert_eq!(String::from_utf8_lossy(&Command::new(&dest).output().unwrap().stdout).trim(), "vicuna 9.9.9");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "no temp files left behind");
        let _ = running.kill();
        let _ = running.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
