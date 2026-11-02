// Vicuna Browser tab: manage the `lb` (live-browser) sidecar daemon.
// The daemon (Chrome via CDP + JSON API + Svelte UI) is the shared engine:
// the side-panel tab iframes its web UI while the Work agent drives the
// same tabs through `lb mcp` (seeded as the default `browser` MCP server).
//
// Binary resolution: the bundled `lb-sidecar/` resource dir first (staged
// by `scripts/fetch-lb-sidecar.sh`, shipped via `bundle.resources`), then
// `lb` on PATH (dev + user installs). Like `ollama serve`, an already
// reachable daemon is adopted, never double-spawned — and only a daemon
// Vicuna itself started can be stopped from here (foreign ones are left
// alone). Quit kills the owned child via `shutdown_children`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

/// Vicuna's browser port. Deliberately NOT live-browser's default 8765:
/// the tab drives an internal daemon, and sharing the default meant a
/// user-run `lb web` (own version, own tabs, own UI state) was adopted
/// instead — or blocked the tab outright. On this port Vicuna adopts only
/// its own kind (a second instance, a manual `--port` twin).
/// `VICUNA_TEST_LB_PORT` overrides it (headless E2E only).
pub const LB_PORT: u16 = 18765;

pub fn lb_port() -> u16 {
    std::env::var("VICUNA_TEST_LB_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(LB_PORT)
}

pub fn lb_server_url() -> String {
    format!("http://127.0.0.1:{}", lb_port())
}

/// Where the `lb` binary came from (reported in status for diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbSource {
    Bundled,
    Path,
}

impl LbSource {
    fn as_str(self) -> &'static str {
        match self {
            LbSource::Bundled => "bundled",
            LbSource::Path => "path",
        }
    }
}

/// The `lb web` child this app launched (if any). Adopted daemons (user's
/// terminal, another app) are never stored here, so stop/quit can't kill
/// a foreign browser session. `bin` is the resolved `lb` binary (needed
/// for graceful `lb web --port P stop`, which also reaps the daemon's
/// Chromium — a bare SIGKILL orphans it holding the profile SingletonLock
/// and the next start fails to launch a browser).
#[derive(Default)]
pub struct BrowserState {
    child: Mutex<Option<Child>>,
    bin: Mutex<Option<PathBuf>>,
    /// `lb --version` captured when our child spawned. Compared against
    /// the on-disk binary at status time: a fetch bump / rebuild under a
    /// live daemon leaves the old image running (old recover behavior
    /// included) with no other visible signal.
    spawned_version: Mutex<Option<String>>,
    /// On-disk identity (length + mtime) snapshot at spawn: the status
    /// fast-path skips the `--version` re-probe while it matches, so the
    /// 3s tab poll doesn't spawn a process every tick.
    spawned_id: Mutex<Option<(u64, u64)>>,
}

fn remember_bin(state: &BrowserState, bin: &Path) {
    if let Ok(mut g) = state.bin.lock() {
        g.replace(bin.to_path_buf());
    }
}

fn owned_bin(state: &BrowserState) -> Option<PathBuf> {
    state.bin.lock().ok().and_then(|g| g.clone())
}

/// Parsed `GET /api/health` (lb marker fields `ok` + `probe` prove the
/// occupant is live-browser, not some foreign service on our port).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LbHealth {
    pub browser_alive: bool,
    pub probe: String,
    pub tabs: u64,
    /// Whether the daemon actually serves its UI files. A daemon can
    /// outlive its own UI (app reinstall under a live daemon orphans
    /// ServeDir's startup-resolved root: `/` still 200s while every
    /// asset 404s → blank tab on a "healthy" daemon).
    pub ui_ok: bool,
}

/// Parse + validate one `/api/health` body. Split from the fetch so the
/// adopt-vs-foreign decision is unit-testable without a daemon.
pub fn parse_health_body(body: &str) -> Result<LbHealth, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "not live-browser: bad JSON".to_string())?;
    if v.get("ok").and_then(|o| o.as_bool()) != Some(true) {
        return Err("not live-browser: missing ok:true".to_string());
    }
    let probe = v
        .get("probe")
        .and_then(|p| p.as_str())
        .ok_or_else(|| "not live-browser: missing probe".to_string())?;
    Ok(LbHealth {
        browser_alive: v
            .get("browser_alive")
            .and_then(|b| b.as_bool())
            .unwrap_or(false),
        probe: probe.to_string(),
        tabs: v.get("tabs").and_then(|t| t.as_u64()).unwrap_or(0),
        // pure default; `probe_daemon` overwrites with the live check
        ui_ok: true,
    })
}

fn bundled_dir(resource_dir: &Path) -> PathBuf {
    resource_dir.join("lb-sidecar")
}

/// Candidate binaries inside the bundled dir, native arch first. macOS
/// builds are universal but `lb` is per-arch, so both are bundled
/// (`lb-arm64` + `lb-x64`) and the runtime arch picks.
pub fn bundled_candidates_for_arch(resource_dir: &Path, arch: &str) -> Vec<PathBuf> {
    let dir = bundled_dir(resource_dir);
    #[cfg(windows)]
    {
        let _ = arch;
        vec![dir.join("lb.exe")]
    }
    #[cfg(target_os = "macos")]
    {
        let (first, second) = if arch == "aarch64" {
            ("lb-arm64", "lb-x64")
        } else {
            ("lb-x64", "lb-arm64")
        };
        vec![dir.join(first), dir.join(second)]
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = arch;
        vec![dir.join("lb")]
    }
}

pub fn bundled_candidates(resource_dir: &Path) -> Vec<PathBuf> {
    bundled_candidates_for_arch(resource_dir, std::env::consts::ARCH)
}

/// `lb` on PATH plus the usual GUI-launch locations (Finder-launched apps
/// get a sparse PATH — same problem as the `ollama` lookup).
fn find_on_path_dirs(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    dirs.iter()
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn path_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home).join(".local/bin"));
    }
    #[cfg(windows)]
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("live-browser"));
    }
    for extra in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        dirs.push(PathBuf::from(extra));
    }
    dirs
}

#[cfg(windows)]
const LB_PATH_NAME: &str = "lb.exe";
#[cfg(not(windows))]
const LB_PATH_NAME: &str = "lb";

/// Binary location inside a `chromium-{version}/` cache entry (mirrors the
/// cloakbrowser wrapper's `getBinaryPath`, same as lb's own inventory).
fn cloak_rel_path() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        PathBuf::from("Chromium.app").join("Contents").join("MacOS").join("Chromium")
    }
    #[cfg(windows)]
    {
        PathBuf::from("chrome.exe")
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        PathBuf::from("chrome")
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(windows))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

/// `~/.cloakbrowser` (honors `CLOAKBROWSER_CACHE_DIR`, like lb/the wrapper).
fn cloak_cache_dir() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("CLOAKBROWSER_CACHE_DIR") {
        if !dir.trim().is_empty() {
            return Some(PathBuf::from(dir));
        }
    }
    home_dir().map(|h| h.join(".cloakbrowser"))
}

/// Newest cached CloakBrowser binary, if any — so output of
/// `npx cloakbrowser install` (or lb's own Settings → Browser download)
/// is picked up as an explicit `--chrome` fallback at start.
pub fn find_cloak_in(cache: &Path) -> Option<PathBuf> {
    let entries = std::fs::read_dir(cache).ok()?;
    let mut hits: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|x| x.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("chromium-"))
        })
        .map(|p| p.join(cloak_rel_path()))
        .filter(|p| p.is_file())
        .collect();
    hits.sort();
    hits.pop()
}

pub fn find_cloak_browser() -> Option<PathBuf> {
    cloak_cache_dir().and_then(|c| find_cloak_in(&c))
}

/// lb's cache base, mirrored (`%LOCALAPPDATA%` / `~/Library/Caches` /
/// `$XDG_CACHE_HOME`): the Vicuna browser profile lives in lb's own tree
/// (`profile-vicuna`, plus lb's `-cloak` kind suffix when on CloakBrowser)
/// so its pidfiles/logs stay findable by plain `lb` commands, while the
/// profile itself never touches a personal daemon's.
fn lb_cache_base() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            if !local.trim().is_empty() {
                return PathBuf::from(local);
            }
        }
        if let Ok(home) = std::env::var("USERPROFILE") {
            if !home.trim().is_empty() {
                return PathBuf::from(home).join("AppData").join("Local");
            }
        }
        return std::env::temp_dir();
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        return PathBuf::from(home).join("Library").join("Caches");
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
            if !xdg.trim().is_empty() {
                return PathBuf::from(xdg);
            }
        }
        if let Ok(home) = std::env::var("HOME") {
            if !home.trim().is_empty() {
                return PathBuf::from(home).join(".cache");
            }
        }
        return std::env::temp_dir();
    }
}

pub fn vicuna_profile_under(base: &Path) -> PathBuf {
    base.join("live-browser").join("profile-vicuna")
}

pub fn vicuna_profile_dir() -> PathBuf {
    vicuna_profile_under(&lb_cache_base())
}

/// Resource roots to search for the bundled binary. Release builds use
/// the Tauri resource dir alone (copied once at bundle time). Debug builds
/// search the crate dir FIRST: dev `resource_dir()` is `target/debug/`
/// with a tauri-build copy that is rewritten on every rebuild — spawning
/// from it races the copy (partial binary → instant SIGKILL → the MCP
/// "exited before answering" failure) and goes stale incrementally, while
/// the source tree only changes on explicit fetch runs.
fn bundled_search_roots(resource_dir: Option<&Path>) -> Vec<PathBuf> {
    #[cfg(debug_assertions)]
    {
        let mut roots = vec![PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
        roots.extend(resource_dir.map(|r| r.to_path_buf()));
        roots
    }
    #[cfg(not(debug_assertions))]
    {
        resource_dir.map(|r| r.to_path_buf()).into_iter().collect()
    }
}

/// Resolve the `lb` binary: bundled resource first, PATH fallback.
/// Returns the binary path plus where it came from.
pub fn resolve_lb_bin(resource_dir: Option<&Path>) -> Option<(PathBuf, LbSource)> {
    resolve_in_roots(&bundled_search_roots(resource_dir))
}

fn resolve_in_roots(roots: &[PathBuf]) -> Option<(PathBuf, LbSource)> {
    for root in roots {
        if let Some(hit) = bundled_candidates(root).into_iter().find(|p| p.is_file()) {
            return Some((hit, LbSource::Bundled));
        }
    }
    find_on_path_dirs(LB_PATH_NAME, &path_search_dirs()).map(|p| (p, LbSource::Path))
}

/// Bundled resources can lose the exec bit in some installers; restore it
/// best-effort before spawning (no-op on Windows / for PATH binaries).
#[cfg(unix)]
fn ensure_exec_bit(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perm = meta.permissions();
        if perm.mode() & 0o111 == 0 {
            perm.set_mode(0o755);
            let _ = std::fs::set_permissions(path, perm);
        }
    }
}

#[cfg(not(unix))]
fn ensure_exec_bit(_path: &Path) {}

/// What answers on our port: nobody, live-browser, or a foreign occupant.
enum Probe {
    Free,
    Lb(LbHealth),
    Foreign(String),
}

async fn probe_daemon(client: &reqwest::Client) -> Probe {
    let res = match client
        .get(format!("{}/api/health", lb_server_url()))
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return Probe::Free,
    };
    let body = res.text().await.unwrap_or_default();
    match parse_health_body(&body) {
        Ok(mut h) => {
            h.ui_ok = ui_servable(client).await;
            Probe::Lb(h)
        }
        Err(e) => Probe::Foreign(e),
    }
}

/// `globe.svg` is a stable dist filename, so a 200 proves the daemon's
/// UI root is intact; 404 means startup-resolved files are gone (app
/// reinstall under a live daemon) and the tab would render blank.
async fn ui_servable(client: &reqwest::Client) -> bool {
    matches!(
        client.get(format!("{}/globe.svg", lb_server_url())).send().await,
        Ok(r) if r.status().is_success()
    )
}

fn probe_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Parse `lb --version` output (`lb 0.4.17`) down to the version
/// number. Split from the probe so skew fixtures don't need a binary.
pub fn parse_lb_version(out: &str) -> Option<String> {
    let first = out.lines().next()?.trim();
    let mut parts = first.split_whitespace();
    match (parts.next(), parts.next()) {
        (Some("lb"), Some(v)) if !v.is_empty() => Some(v.to_string()),
        _ => None,
    }
}

/// Identity of the on-disk binary (length + mtime secs). Pure stat,
/// no spawn — the skew fast-path in status.
fn bin_id(bin: &Path) -> Option<(u64, u64)> {
    let meta = std::fs::metadata(bin).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some((meta.len(), mtime))
}

/// Last non-empty stdout line of `lb browsers download` (the installed
/// binary path — that command's contract). Split out so fixtures don't
/// need a network download.
pub fn parse_browsers_download_output(out: &str) -> Option<String> {
    out.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .last()
        .map(|s| s.to_string())
}

/// CloakBrowser engine status: binary path + whether this call fetched it.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub path: String,
    pub downloaded: bool,
}

/// Ensure the CloakBrowser engine exists, downloading it (one-time) when
/// the cache is empty. Vicuna is cloak-only: system Chrome is never
/// launched (real profiles, picker settings, and running-instance joins
/// caused all the Windows trouble), so a failed download is a hard error,
/// not a fallback trigger. Idempotent — warm cache returns instantly.
pub async fn browser_ensure_engine_logic(
    resource_dir: Option<PathBuf>,
) -> Result<EngineInfo, String> {
    let (bin, _) = resource_dir
        .as_deref()
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .ok_or_else(|| "no `lb` browser engine found".to_string())?;
    if let Some(hit) = find_cloak_browser() {
        return Ok(EngineInfo { path: hit.display().to_string(), downloaded: false });
    }
    let mut cmd = tokio::process::Command::new(&bin);
    cmd.args(["browsers", "download"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    // ~150MB+ plus extract; generous ceiling for slow links
    let out = tokio::time::timeout(Duration::from_secs(15 * 60), cmd.output())
        .await
        .map_err(|_| {
            "CloakBrowser download timed out after 15 minutes — check your connection and retry"
                .to_string()
        })
        .and_then(|r| r.map_err(|e| format!("could not run engine download: {e}")))?;
    if !out.status.success() {
        // filled concurrently (second instance raced us)? take the win
        if let Some(hit) = find_cloak_browser() {
            return Ok(EngineInfo { path: hit.display().to_string(), downloaded: false });
        }
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("unrecognized subcommand") {
            return Err(
                "this `lb` is too old to fetch engines (need ≥0.4.18) — upgrade live-browser or reinstall Vicuna"
                    .to_string(),
            );
        }
        let tail = err.lines().filter(|l| !l.trim().is_empty()).last().unwrap_or("");
        return Err(format!(
            "could not download the CloakBrowser engine{} — check your connection and retry",
            if tail.is_empty() { String::new() } else { format!(" ({tail})") }
        ));
    }
    if let Some(path) = parse_browsers_download_output(&String::from_utf8_lossy(&out.stdout)) {
        return Ok(EngineInfo { path, downloaded: true });
    }
    find_cloak_browser()
        .map(|hit| EngineInfo { path: hit.display().to_string(), downloaded: true })
        .ok_or_else(|| "engine download reported success but no binary appeared — retry".to_string())
}

/// `bin --version`, best-effort (failures mean "unknown", never fatal).
async fn probe_lb_version(bin: &Path) -> Option<String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let run = cmd.output();
    let out = tokio::time::timeout(Duration::from_secs(5), run).await.ok()?.ok()?;
    if !out.status.success() {
        return None;
    }
    parse_lb_version(&String::from_utf8_lossy(&out.stdout))
}

/// Stale-engine warning when the on-disk binary moved past the running
/// owned daemon. Adopted daemons report no running version, so they
/// never warn (and are never restarted from here).
pub fn stale_engine_warning(spawned: Option<&str>, current: Option<&str>) -> Option<String> {
    match (spawned, current) {
        (Some(s), Some(c)) if s != c => Some(format!(
            "browser engine updated on disk ({s} → {c}) — restart the browser to use the new build"
        )),
        _ => None,
    }
}

/// Is our owned child still alive? Reaps it when it exited on its own
/// (so status stops claiming `owned` after a crash).
fn owned_child_alive(state: &BrowserState) -> bool {
    let mut guard = match state.child.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    match guard.as_mut() {
        None => false,
        Some(child) => match child.try_wait() {
            Ok(None) => true,
            _ => {
                guard.take();
                false
            }
        },
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BrowserStatus {
    pub running: bool,
    pub owned: bool,
    /// Owned daemon whose image predates the on-disk binary (fetch bump
    /// or rebuild under a live daemon). The tab offers a restart then.
    pub engine_stale: bool,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<LbHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// Daemon status for the Browser tab + MCP seeding. `resource_dir` is
/// `None` under the headless bridge (no app bundle there — PATH only).
pub async fn browser_status_logic(
    resource_dir: Option<PathBuf>,
    state: &BrowserState,
) -> Result<BrowserStatus, String> {
    let resolved = resource_dir
        .as_deref()
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None));
    let client = probe_client();
    let owned = owned_child_alive(state);
    // stale check (owned only): one `--version` spawn, and only when we
    // captured a running version at spawn — adopted daemons skip it.
    let mut engine_stale = false;
    let mut stale_warning: Option<String> = None;
    if owned {
        let spawned = state.spawned_version.lock().ok().and_then(|g| g.clone());
        if spawned.is_some() {
            if let Some((bin, _)) = resolved.as_ref() {
                // fast path: an unchanged binary can't have a new version,
                // so skip the `--version` spawn (a spawn on every 3s poll
                // flashes a console window on Windows). Any stat mismatch
                // — or unknown snapshot — falls through to the probe.
                let spawned_id = state.spawned_id.lock().ok().and_then(|g| g.clone());
                if spawned_id != bin_id(bin) {
                    let current = probe_lb_version(bin).await;
                    stale_warning = stale_engine_warning(spawned.as_deref(), current.as_deref());
                    engine_stale = stale_warning.is_some();
                }
            }
        }
    }
    let base = BrowserStatus {
        running: false,
        owned,
        engine_stale,
        url: lb_server_url(),
        bin: resolved.as_ref().map(|(p, _)| p.display().to_string()),
        source: resolved.as_ref().map(|(_, s)| s.as_str().to_string()),
        health: None,
        warning: stale_warning,
    };
    match probe_daemon(&client).await {
        Probe::Free => Ok(base),
        Probe::Lb(h) => Ok(BrowserStatus { running: true, health: Some(h), ..base }),
        Probe::Foreign(e) => Ok(BrowserStatus {
            warning: Some(format!("port {} is busy ({e})", lb_port())),
            ..base
        }),
    }
}

/// Start `lb web --port P --foreground` when nothing answers, adopting
/// an already-reachable daemon instead. `--foreground` keeps the daemon
/// as our child (`lb web` backgrounds itself by default, which would
/// orphan it from our stop/quit tracking). `--profile` pins a
/// Vicuna-private browser profile: lb's default profile dir is shared by
/// every daemon on the machine, so without this a personal `lb web`
/// running next to Vicuna would SingletonLock-fight ours (and share
/// cookies/logins with it).
pub async fn browser_start_logic(
    resource_dir: Option<PathBuf>,
    state: &BrowserState,
) -> Result<BrowserStatus, String> {
    let client = probe_client();
    match probe_daemon(&client).await {
        Probe::Lb(_) => return browser_status_logic(resource_dir, state).await,
        Probe::Foreign(e) => {
            return Err(format!("port {} is busy ({e}) — stop it first", lb_port()));
        }
        Probe::Free => {}
    }
    let (bin, source) = resource_dir
        .as_deref()
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .ok_or_else(|| {
            "no `lb` browser engine found (bundled sidecar missing and `lb` not on PATH) — install live-browser (https://github.com/anslwy/live-browser) or rebuild with scripts/fetch-lb-sidecar.sh".to_string()
        })?;
    if source == LbSource::Bundled {
        ensure_exec_bit(&bin);
    }
    // Cloak-only: ensure the engine (downloads on first run), then
    // launch with it explicit. There is deliberately no plain attempt —
    // system Chrome is never used (real profiles, picker settings, and
    // running-instance joins caused all the Windows trouble).
    let engine = browser_ensure_engine_logic(resource_dir.clone()).await?;
    let attempts: Vec<Vec<String>> =
        vec![vec!["--chrome".to_string(), engine.path.clone()]];
    let mut last_err = String::from("no attempt ran");
    for extra in &attempts {
        match spawn_attempt(&bin, source, extra, &client).await {
            Ok(child) => {
                state
                    .child
                    .lock()
                    .map_err(|_| "browser state poisoned".to_string())?
                    .replace(child);
                remember_bin(state, &bin);
                let version = probe_lb_version(&bin).await;
                if let Ok(mut g) = state.spawned_version.lock() {
                    *g = version;
                }
                let id = bin_id(&bin);
                if let Ok(mut g) = state.spawned_id.lock() {
                    *g = id;
                }
                return browser_status_logic(resource_dir, state).await;
            }
            Err(e) => {
                // our spawn lost a race (a sibling `lb web` grabbed the
                // port first)? Adopt the winner instead of erroring.
                if matches!(probe_daemon(&client).await, Probe::Lb(_)) {
                    return browser_status_logic(resource_dir, state).await;
                }
                last_err = e;
            }
        }
    }
    Err(format!(
        "`lb web` failed to start ({last_err}). The CloakBrowser engine is installed but wouldn't launch — retry, or run `lb web --port {}` in a terminal for details",
        lb_port()
    ))
}

/// One `lb web --port P --foreground` spawn; resolves with the live child
/// once /api/health serves lb. A timing-out child is killed AND reaped
/// (`stop`'s stale-pidfile path) so its half-launched Chromium can't wedge
/// the next attempt with a SingletonLock.
async fn spawn_attempt(
    bin: &Path,
    source: LbSource,
    extra: &[String],
    client: &reqwest::Client,
) -> Result<Child, String> {
    let mut cmd = Command::new(bin);
    cmd.args(["web", "--port", &lb_port().to_string(), "--foreground"])
        .args(["--profile", &vicuna_profile_dir().display().to_string()])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // the bundled binary finds its web UI via its cwd fast-path
    // (`lb-sidecar/web/dist` ships next to it)
    if source == LbSource::Bundled {
        if let Some(dir) = bin.parent() {
            cmd.current_dir(dir);
        }
    }
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let mut child = cmd.spawn().map_err(|e| format!("could not spawn: {e}"))?;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if matches!(probe_daemon(client).await, Probe::Lb(_)) {
            return Ok(child);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!("exited ({status})"));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = child.kill();
                return Err(format!("could not watch: {e}"));
            }
        }
    }
    let _ = child.kill();
    graceful_stop(bin).await;
    Err("did not become ready in time".to_string())
}

/// Graceful `lb web --port P stop` (TERM→KILL the daemon plus reaping
/// its Chromium). Only ever called for OUR child (checked alive first —
/// the port is ours then, so `stop` can't hit a foreign daemon).
async fn graceful_stop(bin: &Path) {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["web", "--port", &lb_port().to_string(), "stop"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let run = cmd.output();
    let _ = tokio::time::timeout(Duration::from_secs(15), run).await;
}

/// Stop the daemon Vicuna itself started. Refuses adopted/foreign ones
/// (they were never tracked here). Graceful `lb stop` first — it TERMs the
/// daemon and reaps its Chromium (a bare SIGKILL orphans the browser
/// holding the profile SingletonLock, wedging the next start); SIGKILL
/// only when the child survives, followed by another `stop` whose
/// stale-pidfile path reaps the freshly orphaned browsers. Also covers a
/// crashed child (dead handle, browsers lingering).
pub async fn browser_stop_logic(state: &BrowserState) -> Result<BrowserStatus, String> {
    // scoped: the std MutexGuard must not live across the awaits below
    let mut taken = {
        let mut guard = state
            .child
            .lock()
            .map_err(|_| "browser state poisoned".to_string())?;
        match guard.take() {
            Some(c) => c,
            None => {
                return Err(
                    "no browser daemon was started by Vicuna — stop your own `lb web` in its terminal"
                        .to_string(),
                )
            }
        }
    };
    let was_alive = matches!(taken.try_wait(), Ok(None));
    if let Ok(mut g) = state.spawned_version.lock() {
        g.take();
    }
    if let Ok(mut g) = state.spawned_id.lock() {
        g.take();
    }
    if let Some(bin) = owned_bin(state) {
        graceful_stop(&bin).await;
    }
    if was_alive && matches!(taken.try_wait(), Ok(None)) {
        let _ = taken.kill();
        if let Some(bin) = owned_bin(state) {
            graceful_stop(&bin).await;
        }
    }
    browser_status_logic(None, state).await
}

/// Restart an owned daemon (stop + start): picks up an on-disk engine
/// update and clears wedged browser state the in-place recover can't.
/// Refuses adopted daemons — `browser_stop_logic` owns that refusal.
pub async fn browser_restart_logic(
    resource_dir: Option<PathBuf>,
    state: &BrowserState,
) -> Result<BrowserStatus, String> {
    browser_stop_logic(state).await?;
    browser_start_logic(resource_dir, state).await
}

/// Force-stop whatever `lb` answers on our port (owned or adopted) and
/// start our own daemon. The frontend confirms first: this kills foreign
/// sessions (user's terminal, another app) and their tabs. Refuses
/// foreign (non-lb) occupants — killing those is never our call. Uses
/// the RESOLVED binary's `kill` (pidfile/port-scoped, so the victim's
/// version doesn't matter), falling back to `web --port P stop` for
/// pre-`kill` binaries, and errors (instead of adopting) when the port
/// still serves lb afterwards.
pub async fn browser_takeover_logic(
    resource_dir: Option<PathBuf>,
    state: &BrowserState,
) -> Result<BrowserStatus, String> {
    // Engine first: never kill the adopted session before knowing our own
    // replacement can start (a failed download must not cost their tabs).
    browser_ensure_engine_logic(resource_dir.clone()).await?;
    let client = probe_client();
    match probe_daemon(&client).await {
        Probe::Free => {}
        Probe::Foreign(e) => {
            return Err(format!(
                "port {} is busy ({e}) — stop it first; takeover only stops live-browser",
                lb_port()
            ));
        }
        Probe::Lb(_) => {
            force_stop_port_lb(resource_dir.as_deref()).await;
            if matches!(probe_daemon(&client).await, Probe::Lb(_)) {
                return Err(format!(
                    "could not stop the browser on port {} — stop it in its terminal (`lb web --port {} stop`) and retry",
                    lb_port(),
                    lb_port()
                ));
            }
        }
    }
    // our own child handle (if any) is stale now — drop it so the fresh
    // spawn below re-tracks from clean state.
    if let Ok(mut g) = state.child.lock() {
        g.take();
    }
    if let Ok(mut g) = state.spawned_version.lock() {
        g.take();
    }
    browser_start_logic(resource_dir, state).await
}

/// `bin kill --port P` against whoever holds our port (never our tracked
/// child specifically — that's the point), `web stop` fallback for
/// pre-`kill` binaries.
async fn force_stop_port_lb(resource_dir: Option<&Path>) {
    let bin = resource_dir
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .map(|(p, _)| p);
    let bin = match bin {
        Some(b) => b,
        None => return,
    };
    let mut kill_cmd = tokio::process::Command::new(&bin);
    kill_cmd
        .args(["kill", "--port", &lb_port().to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    kill_cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let kill = kill_cmd.output();
    match tokio::time::timeout(Duration::from_secs(20), kill).await {
        Ok(Ok(out)) if out.status.success() => return,
        _ => {}
    }
    graceful_stop(&bin).await;
}

/// Absolute `lb mcp` argv for the default `browser` MCP server entry.
/// Absolute paths pass `McpManager`'s resolver straight through, which is
/// what makes the bundled (non-PATH) binary work as an MCP command.
pub fn browser_mcp_command(resource_dir: Option<&Path>) -> Result<Vec<String>, String> {
    let (bin, _) = resource_dir
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .ok_or_else(|| "no `lb` browser engine found".to_string())?;
    Ok(vec![bin.display().to_string(), "mcp".to_string()])
}

/// Kill the owned daemon at Quit (never an adopted one — it isn't
/// tracked). Fire-and-forget `lb web --port P stop` first so the daemon's
/// Chromium is reaped even though the app exits without waiting for it.
pub fn kill_owned(state: &BrowserState) {
    if let Some(bin) = owned_bin(state) {
        let mut reap = Command::new(&bin);
        reap.args(["web", "--port", &lb_port().to_string(), "stop"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        reap.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let _ = reap.spawn();
    }
    if let Ok(mut g) = state.child.lock() {
        if let Some(mut child) = g.take() {
            let _ = child.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_parses_lb_probe() {
        let h = parse_health_body(
            r#"{"ok":true,"browser_alive":true,"probe":"ok","tabs":3,"hint":"x"}"#,
        )
        .expect("lb health must parse");
        assert_eq!(
            h,
            LbHealth { browser_alive: true, probe: "ok".to_string(), tabs: 3, ui_ok: true }
        );
    }

    #[test]
    fn health_parses_dead_browser_as_lb() {
        // a dead browser is still OUR daemon (adopt + offer recover, don't respawn)
        let h = parse_health_body(r#"{"ok":true,"browser_alive":false,"probe":"dead","tabs":0}"#)
            .expect("dead probe must parse");
        assert!(!h.browser_alive);
        assert_eq!(h.probe, "dead");
    }

    #[test]
    fn health_rejects_foreign_occupants() {
        assert!(parse_health_body(r#"{"ok":true}"#).is_err(), "missing probe");
        assert!(parse_health_body(r#"{"status":"up"}"#).is_err(), "missing ok");
        assert!(parse_health_body(r#"{"ok":false,"probe":"ok"}"#).is_err(), "ok:false");
        assert!(parse_health_body("not json").is_err(), "garbage");
        assert!(parse_health_body("").is_err(), "empty");
    }

    #[test]
    fn bundled_prefers_native_arch_on_macos() {
        let root = PathBuf::from("/res");
        let arm = bundled_candidates_for_arch(&root, "aarch64");
        assert_eq!(arm[0].file_name().unwrap(), "lb-arm64");
        #[cfg(target_os = "macos")]
        assert_eq!(arm[1].file_name().unwrap(), "lb-x64");
        let x64 = bundled_candidates_for_arch(&root, "x86_64");
        #[cfg(target_os = "macos")]
        assert_eq!(x64[0].file_name().unwrap(), "lb-x64");
        #[cfg(not(target_os = "macos"))]
        let _ = x64;
    }

    #[test]
    fn path_search_finds_explicit_dirs() {
        let dir = std::env::temp_dir().join("vicuna-lb-probe");
        let _ = std::fs::create_dir_all(&dir);
        let fake = dir.join(LB_PATH_NAME);
        std::fs::write(&fake, "x").expect("write fake lb");
        let hit = find_on_path_dirs(LB_PATH_NAME, &[dir.clone()]);
        assert_eq!(hit, Some(fake));
        assert!(find_on_path_dirs(LB_PATH_NAME, &[dir.join("nope")]).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cloak_cache_picks_newest_binary() {
        let cache = std::env::temp_dir().join("vicuna-cloak-probe");
        let _ = std::fs::remove_dir_all(&cache);
        for v in ["chromium-145.0", "chromium-146.0", "other-dir"] {
            let bin = cache.join(v).join(cloak_rel_path());
            std::fs::create_dir_all(bin.parent().unwrap()).expect("mkdirs");
            std::fs::write(&bin, "x").expect("write fake chrome");
        }
        let hit = find_cloak_in(&cache).expect("cloak must resolve");
        assert!(hit.to_string_lossy().contains("chromium-146.0"), "{hit:?}");
        assert!(find_cloak_in(&cache.join("missing")).is_none());
        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn first_root_wins() {
        let base = std::env::temp_dir().join("vicuna-lb-roots");
        let _ = std::fs::remove_dir_all(&base);
        let mut expected: Option<PathBuf> = None;
        for name in ["first", "second"] {
            let root = base.join(name);
            let bin = bundled_candidates(&root)[0].clone();
            std::fs::create_dir_all(bin.parent().unwrap()).expect("mkdirs");
            std::fs::write(&bin, "x").expect("write fake lb");
            if expected.is_none() {
                expected = Some(bin);
            }
        }
        let roots = vec![base.join("first"), base.join("second")];
        let (hit, src) = resolve_in_roots(&roots).expect("first root must win");
        assert_eq!(hit, expected.unwrap());
        assert_eq!(src, LbSource::Bundled);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn debug_builds_search_crate_dir() {
        let roots = bundled_search_roots(None);
        #[cfg(debug_assertions)]
        {
            assert!(
                roots.iter().any(|r| r.join("Cargo.toml").is_file()),
                "debug fallback must point at the crate dir: {roots:?}"
            );
            assert!(
                roots.first().is_some_and(|r| r.join("Cargo.toml").is_file()),
                "crate dir must win over the racy target/ copy: {roots:?}"
            );
        }
        #[cfg(not(debug_assertions))]
        assert!(roots.is_empty(), "release builds use the app dir alone");
    }

    #[test]
    fn bundled_beats_path() {
        // explicit roots (not resolve_lb_bin: debug builds prepend the
        // ambient crate dir, which would win over the fake root whenever
        // the checkout has a staged sidecar)
        let root = std::env::temp_dir().join("vicuna-lb-bundled");
        let sidecar = root.join("lb-sidecar");
        let _ = std::fs::create_dir_all(&sidecar);
        let expected = bundled_candidates(&root)[0].clone();
        std::fs::write(&expected, "x").expect("write bundled lb");
        let (hit, src) = resolve_in_roots(&[root.clone()]).expect("bundled must win");
        assert_eq!(hit, expected);
        assert_eq!(src, LbSource::Bundled);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn browsers_download_output_takes_last_line() {
        assert_eq!(
            parse_browsers_download_output("/cache/chromium-1/x\n"),
            Some("/cache/chromium-1/x".to_string())
        );
        assert_eq!(parse_browsers_download_output("\n\n"), None);
        assert_eq!(parse_browsers_download_output(""), None);
    }

    #[test]
    fn lb_version_parses() {
        assert_eq!(parse_lb_version("lb 0.4.17\n"), Some("0.4.17".to_string()));
        assert_eq!(parse_lb_version("lb 0.4.17"), Some("0.4.17".to_string()));
        assert_eq!(parse_lb_version("garbage\n"), None);
        assert_eq!(parse_lb_version(""), None);
        assert_eq!(parse_lb_version("lb\n"), None);
    }

    #[test]
    fn bin_id_tracks_identity() {
        let dir = std::env::temp_dir().join("vicuna-lb-id");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("lb");
        std::fs::write(&f, "x").expect("write fake lb");
        let id = bin_id(&f).expect("stat must work");
        assert_eq!(bin_id(&f), Some(id), "same file, same id");
        assert_eq!(bin_id(&dir.join("missing")), None);
        std::fs::write(&f, "longer bytes").expect("rewrite");
        assert_ne!(bin_id(&f), Some(id), "changed file, changed id");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vicuna_profile_is_private_to_lb_tree() {
        let p = vicuna_profile_under(Path::new("/cache"));
        assert_eq!(p, PathBuf::from("/cache/live-browser/profile-vicuna"));
        // must differ from a personal daemon's default profile dir
        assert!(!p.ends_with("live-browser/profile"));
    }

    #[test]
    fn stale_warns_only_on_real_skew() {
        // the sleep/wake silent-recover case: daemon runs the pre-bump
        // image while the bundle moved on — warn with both versions
        let w = stale_engine_warning(Some("0.4.16"), Some("0.4.17")).expect("skew must warn");
        assert!(w.contains("0.4.16") && w.contains("0.4.17"), "{w}");
        assert!(w.contains("restart"), "{w}");
        assert_eq!(stale_engine_warning(Some("0.4.17"), Some("0.4.17")), None);
        // unknown on either side (adopted daemon, failed probe): no claim
        assert_eq!(stale_engine_warning(None, Some("0.4.17")), None);
        assert_eq!(stale_engine_warning(Some("0.4.16"), None), None);
        assert_eq!(stale_engine_warning(None, None), None);
    }
}
