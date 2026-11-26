// Vicuna Browser tab: manage the `lb` (live-browser) sidecar daemon.
// The daemon (Chrome via CDP + JSON API + Svelte UI) is the shared engine:
// the side-panel tab iframes its web UI while the Work agent drives the
// same tabs through `lb mcp` (seeded as the default `browser` MCP server).
//
// Binary resolution: bundled `lb-sidecar/` resource dir only (staged
// by `scripts/fetch-lb-sidecar.sh`, shipped via `bundle.resources`).
// External `lb` binaries on PATH are never used, so Vicuna always runs
// the exact sidecar version it was built and tested with. Like
// `ollama serve`, an already reachable daemon is adopted, never
// double-spawned — and only a daemon Vicuna itself started can be
// stopped from here (foreign ones are left alone). Quit kills the owned
// child via `shutdown_children`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Port of the daemon Vicuna runs or talks to. Normally the base port;
/// moved to a free neighbour when another `lb` already holds the base
/// port (see `browser_start_logic`), so Vicuna never reuses a daemon it
/// did not start.
pub fn lb_port() -> u16 {
    match ACTIVE_PORT.load(std::sync::atomic::Ordering::Relaxed) {
        0 => base_port(),
        p => p,
    }
}

static ACTIVE_PORT: std::sync::atomic::AtomicU16 = std::sync::atomic::AtomicU16::new(0);

fn set_active_port(p: u16) {
    ACTIVE_PORT.store(p, std::sync::atomic::Ordering::Relaxed);
}

fn base_port() -> u16 {
    std::env::var("VICUNA_TEST_LB_PORT")
        .ok()
        .and_then(|s| s.parse::<u16>().ok())
        .filter(|p| *p > 0)
        .unwrap_or(LB_PORT)
}

/// Fallback ports tried when the base port is held by another `lb`:
/// the ten ports above the base, in order. Never the base itself.
pub fn candidate_ports(base: u16) -> Vec<u16> {
    (1..=10u16).filter_map(|i| base.checked_add(i)).collect()
}

/// Full port range Vicuna manages for its `lb` sidecar (`base` plus fallbacks).
pub fn managed_ports(base: u16) -> Vec<u16> {
    std::iter::once(base).chain(candidate_ports(base)).collect()
}

pub fn lb_server_url() -> String {
    format!("http://127.0.0.1:{}", lb_port())
}

/// Where the `lb` binary came from (reported in status for diagnostics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LbSource {
    Bundled,
}

impl LbSource {
    fn as_str(self) -> &'static str {
        match self {
            LbSource::Bundled => "bundled",
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
    /// Set when the owned daemon is running on a system Chromium because the
    /// CloakBrowser engine was unavailable (failed/slow download, blocked
    /// first launch). Surfaced as the status warning so the tab explains the
    /// fallback instead of silently looking normal.
    fallback: Mutex<Option<String>>,
}

/// The one `BrowserState` of this process. The desktop shell (Tauri
/// commands, quit, local Work turns) and the embedded server (UI via the
/// API, server turns, vicuna-mcp) used to hold one each: each believed it
/// owned no daemon, so the second start found 18765 busy and spawned a
/// second `lb` on 18766 with the SAME `profile-vicuna` — two daemons
/// fighting over one SingletonLock (each `recover` reaped the other's
/// Chromium). Every production caller must use this shared instance.
pub fn shared() -> std::sync::Arc<BrowserState> {
    static SHARED: std::sync::OnceLock<std::sync::Arc<BrowserState>> = std::sync::OnceLock::new();
    SHARED.get_or_init(|| std::sync::Arc::new(BrowserState::default())).clone()
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

/// Chromium-family binaries that ship with the machine, in preference order.
/// Vicuna still launches CloakBrowser first; these exist only as a launch
/// fallback. Windows also lists Edge — it is Chromium, present on every
/// Windows 10/11 box, and immune to the unsigned-binary stalls that make a
/// first CloakBrowser launch hang behind SmartScreen/Defender scans.
pub fn system_chrome_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(windows)]
    {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(pf) = std::env::var("ProgramFiles") {
            dirs.push(PathBuf::from(pf));
        }
        // The env var name literally contains parentheses.
        if let Some(pf86) = std::env::var_os("ProgramFiles(x86)") {
            dirs.push(PathBuf::from(pf86));
        }
        if let Ok(local) = std::env::var("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local));
        }
        for base in &dirs {
            out.push(base.join("Google").join("Chrome").join("Application").join("chrome.exe"));
        }
        for base in &dirs {
            out.push(base.join("Microsoft").join("Edge").join("Application").join("msedge.exe"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        let mut roots: Vec<PathBuf> = vec![PathBuf::from("/Applications")];
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(home).join("Applications"));
        }
        for root in &roots {
            out.push(root.join("Google Chrome.app").join("Contents").join("MacOS").join("Google Chrome"));
            out.push(root.join("Chromium.app").join("Contents").join("MacOS").join("Chromium"));
        }
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        for p in [
            "/usr/bin/google-chrome-stable",
            "/usr/bin/google-chrome",
            "/usr/bin/chromium",
            "/usr/bin/chromium-browser",
            "/usr/bin/microsoft-edge",
        ] {
            out.push(PathBuf::from(p));
        }
    }
    out
}

/// Only binaries that actually exist (a candidate list is guesses).
pub fn existing_binaries(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    candidates.into_iter().filter(|p| p.is_file()).collect()
}

/// Explicit engine override: `VICUNA_BROWSER_CHROME=/path/to/chrome`. Skips
/// the CloakBrowser download entirely and launches that Chromium first — the
/// manual escape hatch for a machine where the bundled engine cannot start.
pub fn user_chrome_override() -> Option<PathBuf> {
    std::env::var_os("VICUNA_BROWSER_CHROME")
        .map(PathBuf::from)
        .filter(|p| p.is_file())
}

/// Whether `os` (a `std::env::consts::OS` value) launches an installed system
/// browser ahead of CloakBrowser. Windows only: CloakBrowser is unstable
/// there, while Edge ships with every Windows 10/11 install. macOS/Linux keep
/// CloakBrowser first.
pub fn prefers_system_browser(os: &str) -> bool {
    os == "windows"
}

/// Ordered attempt list for `--chrome` on this OS (see [`start_attempts_for`]).
pub fn start_attempts(engine: Option<&Path>, system: &[PathBuf]) -> Vec<String> {
    start_attempts_for(std::env::consts::OS, None, engine, system)
}

/// Ordered attempt list for `--chrome`. `user_override`
/// (`VICUNA_BROWSER_CHROME`) always comes first. Then, on macOS/Linux:
/// CloakBrowser, then each installed system Chromium in the order given. On
/// Windows: Edge, then the other system browsers (Chrome), then CloakBrowser
/// last. Duplicates are dropped. Pure, with `os` injected, so every
/// platform's ordering is testable on any CI host.
pub fn start_attempts_for(
    os: &str,
    user_override: Option<&Path>,
    engine: Option<&Path>,
    system: &[PathBuf],
) -> Vec<String> {
    let mut ordered: Vec<&Path> = Vec::new();
    ordered.extend(user_override);
    if prefers_system_browser(os) {
        let is_edge = |p: &&PathBuf| p.display().to_string().to_ascii_lowercase().contains("msedge");
        ordered.extend(system.iter().filter(is_edge).map(|p| p.as_path()));
        ordered.extend(system.iter().filter(|p| !is_edge(p)).map(|p| p.as_path()));
        ordered.extend(engine);
    } else {
        ordered.extend(engine);
        ordered.extend(system.iter().map(|p| p.as_path()));
    }
    let mut out: Vec<String> = Vec::new();
    for p in ordered {
        let s = p.display().to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

/// Is this a CloakBrowser engine path (lb derives its profile kind from the
/// same heuristic: `cloak` → a `-cloak` profile sibling).
pub fn is_cloak_engine(path: &str) -> bool {
    path.contains(".cloakbrowser") || path.contains("cloak")
}

/// Human label for a browser binary in fallback messages.
pub fn engine_label(path: &str) -> &'static str {
    if is_cloak_engine(path) {
        "CloakBrowser"
    } else if path.to_ascii_lowercase().contains("msedge") {
        "Microsoft Edge"
    } else if path.to_ascii_lowercase().contains("chromium") {
        "Chromium"
    } else {
        "Google Chrome"
    }
}

/// Warning shown in the tab when the owned daemon runs on a system Chromium.
pub fn fallback_warning(engine: &str, engine_error: Option<&str>) -> String {
    let why = engine_error
        .map(|e| e.trim())
        .filter(|e| !e.is_empty())
        .map(|e| {
            let short: String = e.chars().take(200).collect();
            format!(" (built-in engine failed: {short})")
        })
        .unwrap_or_default();
    format!(
        "Running on {} instead of Vicuna's built-in CloakBrowser engine{why} — use this tab's restart button to retry the built-in engine once it can launch.",
        engine_label(engine)
    )
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

/// Profile for a daemon on `port`: the base port keeps `profile-vicuna`
/// (logins persist), a fallback port gets its own `profile-vicuna-<port>`.
/// A fallback daemon only exists because another live lb holds the base
/// port — sharing its profile would make the two Chromiums fight over the
/// SingletonLock (and each lb's recover reaps the other's browser).
pub fn profile_for_port_under(base: &Path, port: u16, base_port: u16) -> PathBuf {
    let dir = vicuna_profile_under(base);
    if port == base_port {
        dir
    } else {
        dir.with_file_name(format!("profile-vicuna-{port}"))
    }
}

fn vicuna_profile_for_port(port: u16) -> PathBuf {
    profile_for_port_under(&lb_cache_base(), port, base_port())
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

/// Resource root for the headless `vicuna` binary (`vicuna serve`/`bridge`),
/// which has no Tauri resource dir. The CLI release ships `lb-sidecar/` in
/// its tarball and `install-cli.sh` puts it in `<prefix>/share/vicuna/`
/// beside `<prefix>/bin/vicuna`; an unpacked tarball keeps it next to the
/// binary. Without this the server fell back to whatever `lb` was on PATH
/// (often older than the version the release was tested with).
/// `VICUNA_RESOURCE_DIR` overrides.
pub fn headless_resource_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("VICUNA_RESOURCE_DIR").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(d));
    }
    let exe = std::env::current_exe().ok()?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    headless_root_for_exe(&exe)
}

fn headless_root_for_exe(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    let mut roots = vec![dir.to_path_buf()];
    if let Some(prefix) = dir.parent() {
        roots.push(prefix.join("share").join("vicuna"));
    }
    roots.into_iter().find(|r| bundled_candidates(r).iter().any(|p| p.is_file()))
}

/// Resolve the bundled `lb` binary. External `lb` binaries on PATH are
/// never used so the built-in browser always runs the pinned sidecar.
pub fn resolve_lb_bin(resource_dir: Option<&Path>) -> Option<(PathBuf, LbSource)> {
    resolve_in_roots(&bundled_search_roots(resource_dir))
}

fn resolve_in_roots(roots: &[PathBuf]) -> Option<(PathBuf, LbSource)> {
    roots
        .iter()
        .find_map(|root| bundled_candidates(root).into_iter().find(|p| p.is_file()))
        .map(|b| (b, LbSource::Bundled))
}

/// `lb --version` for a binary, cached per (path, size, mtime) so the
/// 3s status poll doesn't spawn it every time. Blocking, but only on a
/// cache miss.
fn cached_lb_version(bin: &Path) -> Option<String> {
    static CACHE: Mutex<Vec<(PathBuf, Option<(u64, u64)>, Option<String>)>> = Mutex::new(Vec::new());
    let id = bin_id(bin);
    if let Ok(g) = CACHE.lock() {
        if let Some((_, _, v)) = g.iter().find(|(p, i, _)| p == bin && *i == id) {
            return v.clone();
        }
    }
    let mut cmd = Command::new(bin);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    let version = cmd
        .output()
        .ok()
        .and_then(|o| parse_lb_version(&String::from_utf8_lossy(&o.stdout)));
    if let Ok(mut g) = CACHE.lock() {
        g.retain(|(p, _, _)| p != bin);
        g.push((bin.to_path_buf(), id, version.clone()));
    }
    version
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
    probe_port(client, lb_port()).await
}

async fn probe_port(client: &reqwest::Client, port: u16) -> Probe {
    let url = format!("http://127.0.0.1:{port}");
    let res = match client.get(format!("{url}/api/health")).send().await {
        Ok(r) => r,
        Err(_) => return Probe::Free,
    };
    let body = res.text().await.unwrap_or_default();
    match parse_health_body(&body) {
        Ok(mut h) => {
            h.ui_ok = ui_servable_at(client, &url).await;
            Probe::Lb(h)
        }
        Err(e) => Probe::Foreign(e),
    }
}

/// First fallback port (see `candidate_ports`) that nothing answers on.
async fn first_free_candidate(client: &reqwest::Client) -> Option<u16> {
    for port in candidate_ports(base_port()) {
        if matches!(probe_port(client, port).await, Probe::Free) {
            return Some(port);
        }
    }
    None
}

/// `globe.svg` is a stable dist filename, so a 200 proves the daemon's
/// UI root is intact; 404 means startup-resolved files are gone (app
/// reinstall under a live daemon) and the tab would render blank.
async fn ui_servable_at(client: &reqwest::Client, url: &str) -> bool {
    matches!(
        client.get(format!("{url}/globe.svg")).send().await,
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

/// Live CloakBrowser download progress exposed on `BrowserStatus.download`
/// while `lb browsers download` is running in the background.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineDownloadProgress {
    /// "downloading" | "extracting" | "ready" | "error"
    pub phase: String,
    pub downloaded_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// CloakBrowser engine status: binary path + whether this call fetched it.
#[derive(Debug, Clone, Serialize)]
pub struct EngineInfo {
    pub path: String,
    pub downloaded: bool,
    /// True when the caller passed `background: true` and a download is
    /// running in the background (poll `browser_status().download` for
    /// progress and completion without hitting remote HTTP timeouts).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub downloading: bool,
}

struct DownloadJob {
    progress: Mutex<EngineDownloadProgress>,
    done: tokio::sync::watch::Receiver<Option<Result<EngineInfo, String>>>,
}

static ENGINE_DOWNLOAD: OnceLock<Mutex<Option<Arc<DownloadJob>>>> = OnceLock::new();

fn engine_download_slot() -> &'static Mutex<Option<Arc<DownloadJob>>> {
    ENGINE_DOWNLOAD.get_or_init(|| Mutex::new(None))
}

/// Scan `cache` for `lb browsers download`'s in-flight temp archive
/// (`.lb-download-<ver>-<pid>.tar.gz` or `.zip`) and return its current size.
pub fn scan_download_temp_bytes(cache: &Path, pid: Option<u32>) -> Option<u64> {
    let entries = std::fs::read_dir(cache).ok()?;
    let pid_tag = pid.map(|p| format!("-{p}."));
    let pid_end = pid.map(|p| format!("-{p}"));
    let mut best_any: Option<(std::time::SystemTime, u64)> = None;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(s) = name.to_str() else { continue };
        if !s.starts_with(".lb-download-") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if let (Some(tag), Some(end)) = (pid_tag.as_deref(), pid_end.as_deref()) {
            if s.contains(tag) || s.ends_with(end) {
                return Some(meta.len());
            }
        }
        let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
        if best_any.map(|(t, _)| mtime >= t).unwrap_or(true) {
            best_any = Some((mtime, meta.len()));
        }
    }
    best_any.map(|(_, len)| len)
}

/// Parse one stderr line/chunk from `lb browsers download` for phase or `%`.
pub fn parse_download_stderr_line(line: &str, prog: &mut EngineDownloadProgress) {
    let lower = line.to_ascii_lowercase();
    if lower.contains("extract") || lower.contains("unpack") || lower.contains("decompress") {
        prog.phase = "extracting".to_string();
    }
    // Look for a percentage token like "42%" or "42.5%"
    for tok in line.split_whitespace() {
        let trimmed = tok.trim_matches(|c: char| !c.is_ascii_digit() && c != '.' && c != '%');
        if let Some(num) = trimmed.strip_suffix('%') {
            if let Ok(v) = num.parse::<f64>() {
                if (0.0..=100.0).contains(&v) {
                    prog.percent = Some(v.round() as u8);
                    break;
                }
            }
        }
    }
}

/// Snapshot of any in-flight (or recently failed) CloakBrowser download.
pub fn current_engine_download() -> Option<EngineDownloadProgress> {
    let job = engine_download_slot().lock().ok()?.clone()?;
    let done = job.done.borrow().clone();
    match done {
        None => job.progress.lock().ok().map(|p| p.clone()),
        Some(Ok(_)) => None,
        Some(Err(_)) => {
            if find_cloak_browser().is_some() {
                None
            } else {
                job.progress.lock().ok().map(|p| p.clone())
            }
        }
    }
}

fn start_or_attach_download(bin: PathBuf) -> Arc<DownloadJob> {
    let mut slot = engine_download_slot().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(existing) = slot.as_ref() {
        if existing.done.borrow().is_none() {
            return Arc::clone(existing);
        }
    }
    let (done_tx, done_rx) = tokio::sync::watch::channel(None);
    let job = Arc::new(DownloadJob {
        progress: Mutex::new(EngineDownloadProgress {
            phase: "downloading".to_string(),
            downloaded_bytes: 0,
            total_bytes: None,
            percent: None,
            message: None,
        }),
        done: done_rx,
    });
    *slot = Some(Arc::clone(&job));
    drop(slot);

    let job_task = Arc::clone(&job);
    tokio::spawn(async move {
        let res = run_cloak_download(&bin, &job_task).await;
        if let Ok(mut p) = job_task.progress.lock() {
            match &res {
                Ok(_) => {
                    p.phase = "ready".to_string();
                    p.percent = Some(100);
                    p.message = None;
                }
                Err(e) => {
                    p.phase = "error".to_string();
                    p.message = Some(e.clone());
                }
            }
        }
        let _ = done_tx.send(Some(res));
    });
    job
}

async fn run_cloak_download(bin: &Path, job: &Arc<DownloadJob>) -> Result<EngineInfo, String> {
    use tokio::io::AsyncReadExt as _;

    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["browsers", "download"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not run engine download: {e}"))?;
    let pid = child.id();

    let mut stdout_pipe = child.stdout.take();
    let stdout_task = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(ref mut r) = stdout_pipe {
            let _ = r.read_to_end(&mut buf).await;
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let mut stderr_pipe = child.stderr.take();
    let job_err = Arc::clone(job);
    let stderr_task = tokio::spawn(async move {
        let mut all = String::new();
        let Some(ref mut r) = stderr_pipe else {
            return all;
        };
        let mut chunk = [0u8; 2048];
        let mut pending = String::new();
        loop {
            match r.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let s = String::from_utf8_lossy(&chunk[..n]);
                    all.push_str(&s);
                    pending.push_str(&s);
                    while let Some(idx) = pending.find(['\n', '\r']) {
                        let line = pending[..idx].to_string();
                        pending.drain(..=idx);
                        if !line.trim().is_empty() {
                            if let Ok(mut p) = job_err.progress.lock() {
                                parse_download_stderr_line(&line, &mut p);
                            }
                        }
                    }
                }
            }
        }
        if !pending.trim().is_empty() {
            if let Ok(mut p) = job_err.progress.lock() {
                parse_download_stderr_line(&pending, &mut p);
            }
        }
        all
    });

    let cache_dir = cloak_cache_dir();
    let job_poll = Arc::clone(job);
    let wait_fut = async {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        let mut saw_bytes = false;
        loop {
            tokio::select! {
                status = child.wait() => return status,
                _ = tick.tick() => {
                    if let Some(ref dir) = cache_dir {
                        match scan_download_temp_bytes(dir, pid) {
                            Some(bytes) => {
                                if bytes > 0 {
                                    saw_bytes = true;
                                }
                                if let Ok(mut p) = job_poll.progress.lock() {
                                    p.downloaded_bytes = bytes;
                                }
                            }
                            None if saw_bytes => {
                                if let Ok(mut p) = job_poll.progress.lock() {
                                    if p.phase == "downloading" {
                                        p.phase = "extracting".to_string();
                                    }
                                }
                            }
                            None => {}
                        }
                    }
                }
            }
        }
    };

    // ~150MB+ plus extract; generous ceiling for slow links
    let status = tokio::time::timeout(Duration::from_secs(15 * 60), wait_fut)
        .await
        .map_err(|_| {
            "CloakBrowser download timed out after 15 minutes — check your connection and retry"
                .to_string()
        })
        .and_then(|r| r.map_err(|e| format!("could not run engine download: {e}")))?;

    let stdout = stdout_task.await.unwrap_or_default();
    let stderr = stderr_task.await.unwrap_or_default();

    if !status.success() {
        // filled concurrently (second instance raced us)? take the win
        if let Some(hit) = find_cloak_browser() {
            return Ok(EngineInfo {
                path: hit.display().to_string(),
                downloaded: false,
                downloading: false,
            });
        }
        if stderr.contains("unrecognized subcommand") {
            return Err(
                "this `lb` is too old to fetch engines (need ≥0.4.18) — upgrade live-browser or reinstall Vicuna"
                    .to_string(),
            );
        }
        let tail = stderr
            .lines()
            .filter(|l| !l.trim().is_empty())
            .last()
            .unwrap_or("");
        return Err(format!(
            "could not download the CloakBrowser engine{} — check your connection and retry",
            if tail.is_empty() {
                String::new()
            } else {
                format!(" ({tail})")
            }
        ));
    }
    if let Some(path) = parse_browsers_download_output(&stdout) {
        return Ok(EngineInfo { path, downloaded: true, downloading: false });
    }
    find_cloak_browser()
        .map(|hit| EngineInfo {
            path: hit.display().to_string(),
            downloaded: true,
            downloading: false,
        })
        .ok_or_else(|| "engine download reported success but no binary appeared — retry".to_string())
}

/// Ensure the CloakBrowser engine exists, downloading it (one-time) when
/// the cache is empty. Concurrent callers share one in-flight download
/// instead of spawning duplicate `lb browsers download` processes. When
/// `background` is true, returns immediately with `downloading: true` so
/// remote clients can poll `browser_status().download` for progress without
/// hitting HTTP request timeouts.
pub async fn browser_ensure_engine_logic(
    resource_dir: Option<PathBuf>,
    background: bool,
) -> Result<EngineInfo, String> {
    if let Some(hit) = user_chrome_override() {
        return Ok(EngineInfo {
            path: hit.display().to_string(),
            downloaded: false,
            downloading: false,
        });
    }
    let (bin, _) = resource_dir
        .as_deref()
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .ok_or_else(|| "no `lb` browser engine found".to_string())?;
    if let Some(hit) = find_cloak_browser() {
        return Ok(EngineInfo {
            path: hit.display().to_string(),
            downloaded: false,
            downloading: false,
        });
    }
    let job = start_or_attach_download(bin);
    if background {
        return Ok(EngineInfo {
            path: String::new(),
            downloaded: false,
            downloading: true,
        });
    }
    let mut rx = job.done.clone();
    loop {
        if let Some(res) = rx.borrow().clone() {
            return res;
        }
        if rx.changed().await.is_err() {
            return Err("engine download task ended unexpectedly — retry".to_string());
        }
    }
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
    /// `lb --version` of `bin` (shown in Settings).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lb_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<LbHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
    /// Live CloakBrowser download progress while a background engine fetch is
    /// running (or its error before the next retry).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download: Option<EngineDownloadProgress>,
}

/// Daemon status for the Browser tab + MCP seeding.
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
    // Engine skew outranks the fallback note (it needs a restart); the
    // fallback note only makes sense for our own daemon.
    let fallback_warning = if owned {
        state.fallback.lock().ok().and_then(|g| g.clone())
    } else {
        None
    };
    let base = BrowserStatus {
        running: false,
        owned,
        engine_stale,
        url: lb_server_url(),
        bin: resolved.as_ref().map(|(p, _)| p.display().to_string()),
        source: resolved.as_ref().map(|(_, s)| s.as_str().to_string()),
        lb_version: resolved.as_ref().and_then(|(p, _)| cached_lb_version(p)),
        health: None,
        warning: stale_warning.or(fallback_warning),
        download: current_engine_download(),
    };
    match probe_daemon(&client).await {
        Probe::Free => Ok(base),
        // A daemon we did not start is never reused: it may be another lb
        // binary or version. Start runs ours on a free port instead.
        Probe::Lb(_) if !owned => Ok(BrowserStatus {
            warning: Some(format!(
                "another live-browser is running on port {} (not started by Vicuna); Vicuna starts its own on a free port",
                lb_port()
            )),
            ..base
        }),
        Probe::Lb(h) => Ok(BrowserStatus { running: true, health: Some(h), download: None, ..base }),
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
    if owned_child_alive(state) && matches!(probe_daemon(&client).await, Probe::Lb(_)) {
        return browser_status_logic(resource_dir, state).await;
    }
    let (bin, _source) = resource_dir
        .as_deref()
        .and_then(|r| resolve_lb_bin(Some(r)))
        .or_else(|| resolve_lb_bin(None))
        .ok_or_else(|| {
            "no bundled `lb` browser engine found (lb-sidecar missing) — run scripts/fetch-lb-sidecar.sh or reinstall Vicuna".to_string()
        })?;
    ensure_exec_bit(&bin);
    // Reap orphaned Vicuna-owned `lb` daemons across our managed port range
    // (left behind by prior crashes, force-quits, or app updates) before
    // choosing a port. Live sibling Vicuna instances and unrelated user-run
    // `lb` daemons are left untouched.
    reap_orphaned_vicuna_daemons(&bin).await;
    // Reset to the base port whenever we have no live owned child so we
    // reclaim 18765 once orphans are cleared instead of sticking on a fallback.
    set_active_port(base_port());
    match probe_daemon(&client).await {
        Probe::Lb(_) => {
            // Another live lb (a live sibling Vicuna or a personal `lb web`)
            // holds the base port. Never reuse it: run ours on a free port.
            let port = first_free_candidate(&client).await.ok_or_else(|| {
                format!(
                    "port {} is held by another live-browser and no fallback port ({:?}) is free — stop that daemon first",
                    lb_port(),
                    candidate_ports(base_port())
                )
            })?;
            set_active_port(port);
        }
        Probe::Foreign(e) => {
            return Err(format!("port {} is busy ({e}) — stop it first", lb_port()));
        }
        Probe::Free => {}
    }
    // CloakBrowser first (Vicuna's default engine; downloaded on first run),
    // then any installed system Chromium. The fallback exists because a
    // Windows first launch of the unsigned CloakBrowser binary can stall for
    // minutes behind SmartScreen/Defender — the tab used to sit at
    // "Starting browser…" with no recourse. An explicit `--chrome` plus our
    // private `--profile` keeps lb from auto-detecting, joining a running
    // instance, or touching a real profile (the original cloak-only reason).
    let mut engine_error: Option<String> = None;
    let override_chrome = user_chrome_override();
    let system: Vec<PathBuf> = existing_binaries(system_chrome_candidates());
    // Windows prefers an installed Edge/Chrome over CloakBrowser: when one
    // exists, skip the CloakBrowser download and use only an
    // already-cached copy, tried last.
    let prefer_system = prefers_system_browser(std::env::consts::OS) && !system.is_empty();
    // An explicit override means "use this Chromium", not "download one
    // first": skip the engine fetch so a blocked download can't delay it.
    let engine = if override_chrome.is_some() {
        None
    } else if prefer_system {
        find_cloak_browser()
    } else {
        match browser_ensure_engine_logic(resource_dir.clone(), false).await {
            Ok(info) => Some(PathBuf::from(info.path)),
            Err(e) => {
                engine_error = Some(e);
                None
            }
        }
    };
    let attempts = start_attempts_for(
        std::env::consts::OS,
        override_chrome.as_deref(),
        engine.as_deref(),
        &system,
    );
    if attempts.is_empty() {
        return Err(format!(
            "no browser engine could be used: {} — install Google Chrome/Edge, or retry the built-in CloakBrowser download",
            engine_error.unwrap_or_else(|| "no CloakBrowser engine".to_string())
        ));
    }
    // Reap orphaned Chromium left on OUR private profile by a crash / hard
    // kill / app reinstall: their daemon is gone but they still hold the
    // profile SingletonLock, so the next launch dies instantly (hit live;
    // reproduced as `Failed to create .../profile-vicuna-cloak/SingletonLock`).
    // The port is free here (we adopted above otherwise), and `stop` is scoped
    // to the pidfile + our profile dirs. A live pidfile is left alone: `stop`
    // would TERM that PID, which may have been recycled by an unrelated app.
    if !daemon_pidfile_alive_at(&daemon_pidfile_path()) {
        graceful_stop(&bin).await;
    }
    let mut last_err = String::from("no attempt ran");
    for target in &attempts {
        let extra = vec!["--chrome".to_string(), target.clone()];
        match spawn_attempt(&bin, &extra, &client).await {
            Ok(child) => {
                let lb_pid = child.id();
                state
                    .child
                    .lock()
                    .map_err(|_| "browser state poisoned".to_string())?
                    .replace(child);
                write_vicuna_owner_record_at(
                    &vicuna_owner_file(lb_port()),
                    lb_pid,
                    std::process::id(),
                );
                remember_bin(state, &bin);
                let version = probe_lb_version(&bin).await;
                if let Ok(mut g) = state.spawned_version.lock() {
                    *g = version;
                }
                let id = bin_id(&bin);
                if let Ok(mut g) = state.spawned_id.lock() {
                    *g = id;
                }
                if let Ok(mut g) = state.fallback.lock() {
                    let explicit = override_chrome.as_deref() == Some(Path::new(target.as_str()));
                    // A system browser picked as Windows' preferred
                    // engine is not a fallback: no "instead of CloakBrowser"
                    // warning for it.
                    *g = if is_cloak_engine(target) || explicit || prefer_system {
                        None
                    } else {
                        Some(fallback_warning(target, engine_error.as_deref()))
                    };
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
    let tried: Vec<String> = attempts
        .iter()
        .map(|a| {
            if is_cloak_engine(a) {
                return "CloakBrowser".to_string();
            }
            Path::new(a)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| a.clone())
        })
        .collect();
    Err(format!(
        "`lb web` failed to start ({last_err}). Tried: {}. Install Chrome/Edge, or run `lb web --port {}` in a terminal for details",
        tried.join(", "),
        lb_port()
    ))
}

/// Where our foreground daemon's own stdout/stderr go. Matches lb's detached
/// log naming so `lb web --port P stop` and plain `lb` tooling stay coherent,
/// and so a Windows launch stall leaves a tail we can quote in the error
/// (without a console window, there is no other way to see it).
pub fn daemon_log_path() -> PathBuf {
    lb_cache_base()
        .join("live-browser")
        .join(format!("daemon-{}.log", lb_port()))
}

/// Strip ANSI SGR/CSI escapes (lb's tracing writes colors even to a file).
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                // CSI: params/intermediates end at a byte in @..~
                for n in chars.by_ref() {
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // OSC: ends at BEL or ESC \
                while let Some(n) = chars.next() {
                    if n == '\u{7}' {
                        break;
                    }
                    if n == '\u{1b}' && matches!(chars.peek(), Some('\\')) {
                        chars.next();
                        break;
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Last `lines` non-empty lines of the daemon log (bounded; "" when absent).
pub fn log_tail(path: &Path, lines: usize) -> String {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return String::new(),
    };
    let clean = strip_ansi(&text);
    let all: Vec<&str> = clean.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = all.len().saturating_sub(lines);
    let tail = all[start..].join("\n");
    tail.chars().take(1200).collect()
}

/// One `lb web --port P --foreground` spawn; resolves with the live child
/// once /api/health serves lb. A timing-out child is killed AND reaped
/// (`stop`'s stale-pidfile path) so its half-launched Chromium can't wedge
/// the next attempt with a SingletonLock.
///
/// Readiness waits up to `READY_TIMEOUT`: the daemon allows Chrome 60s to
/// come up, and a Windows first launch of the unsigned CloakBrowser binary
/// adds an antivirus scan on top — the old 20s window reported a healthy
/// engine as broken.
async fn spawn_attempt(
    bin: &Path,
    extra: &[String],
    client: &reqwest::Client,
) -> Result<Child, String> {
    const READY_TIMEOUT: Duration = Duration::from_secs(90);
    let log_path = daemon_log_path();
    if let Some(dir) = log_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Append (not truncate): a prior attempt's tail stays available when the
    // fallback also fails.
    let log_out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let log_err = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(bin);
    cmd.args(["web", "--port", &lb_port().to_string(), "--foreground"])
        .args(["--profile", &vicuna_profile_for_port(lb_port()).display().to_string()])
        .args(extra)
        .stdin(Stdio::null())
        .stdout(log_out)
        .stderr(log_err);
    // the bundled binary finds its web UI via its cwd fast-path
    // (`lb-sidecar/web/dist` ships next to it)
    if let Some(dir) = bin.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let mut child = cmd.spawn().map_err(|e| format!("could not spawn: {e}"))?;
    let deadline = std::time::Instant::now() + READY_TIMEOUT;
    while std::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if matches!(probe_daemon(client).await, Probe::Lb(_)) {
            return Ok(child);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(with_log_tail(format!("exited ({status})"), &log_path));
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
    Err(with_log_tail(
        format!(
            "engine did not become ready within {}s",
            READY_TIMEOUT.as_secs()
        ),
        &log_path,
    ))
}

/// Append the daemon log tail to a failure message (best-effort).
fn with_log_tail(msg: String, log_path: &Path) -> String {
    let tail = log_tail(log_path, 8);
    if tail.is_empty() {
        return msg;
    }
    format!("{msg} — daemon log: {tail}")
}

/// Graceful `lb web --port P stop` (TERM→KILL the daemon plus reaping
/// its Chromium). Scoped to `port` + our private `profile-vicuna` dir.
async fn graceful_stop_port(bin: &Path, port: u16) {
    let mut cmd = tokio::process::Command::new(bin);
    // `--profile` matters: `stop` reaps browsers holding THAT profile. Without
    // it lb reaps its default profile dir, leaving our private
    // `profile-vicuna(-cloak)` Chromium alive to lock the next launch.
    cmd.args(["web", "--port", &port.to_string()])
        .args(["--profile", &vicuna_profile_for_port(port).display().to_string()])
        .arg("stop")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW: no console flash
    let run = cmd.output();
    let _ = tokio::time::timeout(Duration::from_secs(15), run).await;
}

/// Graceful `lb web --port P stop` for the current active port. Only ever
/// called for OUR child (checked alive first — the port is ours then, so
/// `stop` can't hit a foreign daemon).
async fn graceful_stop(bin: &Path) {
    graceful_stop_port(bin, lb_port()).await;
}

/// Is `pid` a live process? Mirrors lb's own liveness probe (never signals).
#[cfg(unix)]
fn pid_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(windows)]
fn pid_alive(pid: u32) -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
    cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    cmd.output()
        .map(|o| o.status.success() && o.stdout.windows(4).any(|w| w == b".exe"))
        .unwrap_or(false)
}

/// Does the daemon pidfile for our port name a LIVE process? `stop` trusts a
/// live pid and TERMs it, so a recycled PID (Windows recycles aggressively)
/// must block the pre-start orphan reap rather than kill an unrelated app.
fn daemon_pidfile_alive_at(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<u32>() else {
        return false;
    };
    pid > 0 && pid_alive(pid)
}

fn daemon_pidfile_path() -> PathBuf {
    lb_cache_base()
        .join("live-browser")
        .join(format!("daemon-{}.pid", lb_port()))
}

/// Vicuna-owned daemon ownership record (`<lb_pid> <vicuna_pid>`) written next
/// to lb's own `daemon-<port>.pid`. Lets a restarted Vicuna identify daemons
/// left behind by a dead Vicuna instance on all platforms (including Windows).
pub fn vicuna_owner_file_under(base: &Path, port: u16) -> PathBuf {
    base.join("live-browser")
        .join(format!("vicuna-owner-{port}.pid"))
}

fn vicuna_owner_file(port: u16) -> PathBuf {
    vicuna_owner_file_under(&lb_cache_base(), port)
}

pub fn parse_vicuna_owner_record(text: &str) -> Option<(u32, u32)> {
    let mut parts = text.split_whitespace();
    let lb_pid = parts.next()?.parse::<u32>().ok()?;
    let owner_pid = parts.next()?.parse::<u32>().ok()?;
    if lb_pid == 0 || owner_pid == 0 {
        return None;
    }
    Some((lb_pid, owner_pid))
}

fn write_vicuna_owner_record_at(path: &Path, lb_pid: u32, owner_pid: u32) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, format!("{lb_pid} {owner_pid}\n"));
}

fn remove_vicuna_owner_record_at(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Snapshot row from `ps -eww -o pid=,ppid=,args=`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcEntry {
    pub pid: u32,
    pub ppid: u32,
    pub args: String,
}

/// Parse `ps -eww -o pid=,ppid=,args=` output into `ProcEntry` rows.
pub fn parse_ps_output(output: &str) -> Vec<ProcEntry> {
    let mut out = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let Some(sp1) = trimmed.find(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = trimmed[..sp1].parse::<u32>() else {
            continue;
        };
        let rest = trimmed[sp1..].trim_start();
        let Some(sp2) = rest.find(char::is_whitespace) else {
            continue;
        };
        let Ok(ppid) = rest[..sp2].parse::<u32>() else {
            continue;
        };
        let args = rest[sp2..].trim().to_string();
        if !args.is_empty() {
            out.push(ProcEntry { pid, ppid, args });
        }
    }
    out
}

/// Extract `--port <N>` or `--port=<N>` from a command line.
pub fn extract_port_arg(args: &str) -> Option<u16> {
    let mut iter = args.split_whitespace();
    while let Some(tok) = iter.next() {
        if tok == "--port" {
            return iter.next().and_then(|v| v.parse::<u16>().ok());
        }
        if let Some(v) = tok.strip_prefix("--port=") {
            return v.parse::<u16>().ok();
        }
    }
    None
}

/// True when `args` is a long-running `lb web` daemon (not `lb web stop`,
/// `lb kill`, `lb mcp`, or an unrelated binary).
pub fn is_lb_web_daemon_args(args: &str) -> bool {
    let Some((bin_part, rest)) = args.split_once(" web ") else {
        return false;
    };
    let bin_name = bin_part
        .trim()
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .trim_matches(['"', '\'']);
    if !matches!(bin_name, "lb" | "lb-arm64" | "lb-x64" | "lb.exe") {
        return false;
    }
    !rest
        .split_whitespace()
        .any(|t| matches!(t, "stop" | "kill" | "status"))
}

/// True when `args` is a Vicuna-launched `lb web` daemon (matches Vicuna's
/// private `profile-vicuna` profile dir or bundled `lb-sidecar` binary path).
/// Never matches a personal user-run `lb web` on a different profile.
pub fn is_vicuna_owned_lb_args(args: &str, profile_dir: &Path) -> bool {
    if !is_lb_web_daemon_args(args) {
        return false;
    }
    let Some((bin_part, rest)) = args.split_once(" web ") else {
        return false;
    };
    let profile_str = profile_dir.to_string_lossy();
    let has_profile = (!profile_str.is_empty() && rest.contains(profile_str.as_ref()))
        || rest.contains("live-browser/profile-vicuna")
        || rest.contains("live-browser\\profile-vicuna");
    let has_sidecar = bin_part.contains("/lb-sidecar/") || bin_part.contains("\\lb-sidecar\\");
    has_profile || has_sidecar
}

/// True when a parent process command line is an OS orphan reaper (`launchd`,
/// `init`, `systemd --user`, `tini`, etc.), as opposed to a live Vicuna
/// instance or a user's interactive shell.
pub fn is_orphan_reaper_args(args: &str) -> bool {
    let Some(first) = args.split_whitespace().next() else {
        return false;
    };
    let cleaned = first.trim_matches(|c| c == '(' || c == ')' || c == ':' || c == '-');
    let base = cleaned.rsplit(['/', '\\']).next().unwrap_or(cleaned);
    matches!(
        base,
        "launchd" | "init" | "systemd" | "upstart" | "tini" | "dumb-init"
    )
}

/// Identify orphaned Vicuna-owned `lb` daemons in `procs` occupying any port
/// in `managed`. Returns sorted, deduplicated `(port, pid)` pairs.
///
/// A daemon is considered orphaned only when BOTH:
/// 1. Its command line identifies it as Vicuna-owned (`profile-vicuna` or
///    bundled `lb-sidecar`) on a port in `managed`.
/// 2. Its parent is gone (`ppid <= 1`, missing from `procs`, or an OS orphan
///    reaper like `launchd`/`systemd`/`init`). Daemons whose parent is a live
///    sibling Vicuna (or a user's shell) are never returned.
pub fn find_orphaned_vicuna_daemons(
    procs: &[ProcEntry],
    managed: &[u16],
    profile_dir: &Path,
    self_pid: u32,
) -> Vec<(u16, u32)> {
    let by_pid: std::collections::HashMap<u32, &ProcEntry> =
        procs.iter().map(|p| (p.pid, p)).collect();
    let mut found: Vec<(u16, u32)> = Vec::new();
    for p in procs {
        if p.pid == 0 || p.pid == self_pid || p.ppid == self_pid {
            continue;
        }
        if !is_vicuna_owned_lb_args(&p.args, profile_dir) {
            continue;
        }
        let Some(port) = extract_port_arg(&p.args) else {
            continue;
        };
        if !managed.contains(&port) {
            continue;
        }
        let orphaned = if p.ppid <= 1 {
            true
        } else {
            match by_pid.get(&p.ppid) {
                None => true,
                Some(parent) => is_orphan_reaper_args(&parent.args),
            }
        };
        if orphaned {
            found.push((port, p.pid));
        }
    }
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(unix)]
fn snapshot_processes() -> Vec<ProcEntry> {
    Command::new("ps")
        .args(["-eww", "-o", "pid=,ppid=,args="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_ps_output(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

#[cfg(unix)]
fn signal_pid(pid: u32, sig: &str) {
    let _ = Command::new("kill")
        .arg(sig)
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Detect and cleanly stop orphaned Vicuna-owned `lb` daemons across
/// `base_port()..=base_port()+10`. Uses both Unix process inspection (catching
/// orphans from older Vicuna builds that predated owner pidfiles) and
/// `vicuna-owner-<port>.pid` records.
async fn reap_orphaned_vicuna_daemons(bin: &Path) {
    let managed = managed_ports(base_port());
    let profile_dir = vicuna_profile_dir();
    let self_pid = std::process::id();
    let mut targets: Vec<(u16, u32)> = Vec::new();

    #[cfg(unix)]
    let procs = {
        let procs = snapshot_processes();
        targets.extend(find_orphaned_vicuna_daemons(
            &procs,
            &managed,
            &profile_dir,
            self_pid,
        ));
        procs
    };

    // Also consult Vicuna's own owner pidfiles (handles Windows and any case
    // where `ps` output was unavailable).
    for &port in &managed {
        let path = vicuna_owner_file(port);
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some((lb_pid, owner_pid)) = parse_vicuna_owner_record(&text) else {
            remove_vicuna_owner_record_at(&path);
            continue;
        };
        if !pid_alive(lb_pid) {
            remove_vicuna_owner_record_at(&path);
            continue;
        }
        if owner_pid != self_pid && !pid_alive(owner_pid) {
            #[cfg(unix)]
            let verified = procs
                .iter()
                .find(|p| p.pid == lb_pid)
                .is_none_or(|p| is_vicuna_owned_lb_args(&p.args, &profile_dir));
            #[cfg(not(unix))]
            let verified = true;
            if verified && !targets.iter().any(|&(p, pid)| p == port && pid == lb_pid) {
                targets.push((port, lb_pid));
            }
        }
    }

    for (port, pid) in targets {
        stop_orphaned_daemon(bin, port, pid).await;
    }
}

async fn stop_orphaned_daemon(bin: &Path, port: u16, pid: u32) {
    graceful_stop_port(bin, port).await;
    #[cfg(unix)]
    if pid_alive(pid) {
        signal_pid(pid, "-TERM");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while pid_alive(pid) && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if pid_alive(pid) {
            signal_pid(pid, "-KILL");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        // Reap any Chromium that was holding our private profile SingletonLock
        graceful_stop_port(bin, port).await;
    }
    #[cfg(windows)]
    if pid_alive(pid) {
        let mut kill_cmd = tokio::process::Command::new(bin);
        kill_cmd
            .args(["kill", "--port", &port.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000);
        let _ = tokio::time::timeout(Duration::from_secs(10), kill_cmd.output()).await;
    }
    remove_vicuna_owner_record_at(&vicuna_owner_file(port));
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
    if let Ok(mut g) = state.fallback.lock() {
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
    remove_vicuna_owner_record_at(&vicuna_owner_file(lb_port()));
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
    // Best-effort: a failed CloakBrowser download now falls back to a system
    // Chromium in `browser_start_logic`, so it must not block the takeover.
    if let Err(e) = browser_ensure_engine_logic(resource_dir.clone(), false).await {
        let _ = e;
    }
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
    if let Ok(mut g) = state.fallback.lock() {
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

/// Open the OAuth page in the browser on the same host as the loopback callback.
pub(crate) async fn open_sign_in_tab(resource_dir: Option<&Path>, url: &str) -> Result<(), String> {
    let (bin, _) = resolve_lb_bin(resource_dir)
        .ok_or_else(|| "no `lb` browser engine found".to_string())?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(["--server", &lb_server_url(), "open", "--new-tab", "--", url])
        .env_remove("LB_TAB")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x08000000);
    let status = tokio::time::timeout(Duration::from_secs(30), cmd.status())
        .await
        .map_err(|_| "mcp: opening the browser tab timed out".to_string())?
        .map_err(|e| format!("mcp: could not open the browser tab: {e}"))?;
    if !status.success() {
        return Err(format!("mcp: could not open the browser tab ({status})"));
    }
    Ok(())
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

/// Gracefully stop the owned daemon during server shutdown, waiting for it
/// to exit and release its port (and for `lb stop` to reap its Chromium).
/// No-op when Vicuna never started a daemon (foreign/adopted daemons are
/// never touched).
pub async fn stop_owned(state: &BrowserState) {
    let mut child = match state.child.lock() {
        Ok(mut g) => match g.take() {
            Some(c) => c,
            None => return,
        },
        Err(_) => return,
    };
    if let Ok(mut g) = state.spawned_version.lock() {
        g.take();
    }
    if let Ok(mut g) = state.spawned_id.lock() {
        g.take();
    }
    if let Ok(mut g) = state.fallback.lock() {
        g.take();
    }
    let bin = owned_bin(state);
    if let Some(bin) = bin.as_deref() {
        let _ = tokio::time::timeout(Duration::from_secs(5), graceful_stop(bin)).await;
    }
    if matches!(child.try_wait(), Ok(None)) {
        let _ = child.kill();
        let _ = child.wait();
        if let Some(bin) = bin.as_deref() {
            let _ = tokio::time::timeout(Duration::from_secs(3), graceful_stop(bin)).await;
        }
    }
    remove_vicuna_owner_record_at(&vicuna_owner_file(lb_port()));
}

/// Kill the owned daemon at Quit (never an adopted one — it isn't
/// tracked). Safe to call more than once (returns immediately once the
/// tracked child has been taken). Fire-and-forget `lb web --port P stop`
/// first so the daemon's Chromium is reaped even though the app exits
/// without waiting for it.
pub fn kill_owned(state: &BrowserState) {
    let child = state.child.lock().ok().and_then(|mut g| g.take());
    let Some(mut child) = child else {
        return;
    };
    let port = lb_port();
    remove_vicuna_owner_record_at(&vicuna_owner_file(port));
    if let Some(bin) = owned_bin(state) {
        let mut reap = Command::new(&bin);
        reap.args(["web", "--port", &port.to_string()])
            .args(["--profile", &vicuna_profile_for_port(port).display().to_string()])
            .arg("stop")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        reap.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let _ = reap.spawn();
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headless_root_finds_sidecar_beside_or_in_share() {
        let t = std::env::temp_dir().join(format!("vicuna-hl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        let bin = t.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("vicuna");
        assert_eq!(headless_root_for_exe(&exe), None, "nothing bundled");
        let share = t.join("share").join("vicuna");
        for p in bundled_candidates(&share) {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        assert_eq!(headless_root_for_exe(&exe), Some(share), "installed layout");
        for p in bundled_candidates(&bin) {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b"x").unwrap();
        }
        assert_eq!(headless_root_for_exe(&exe), Some(bin), "unpacked tarball wins");
        let _ = std::fs::remove_dir_all(&t);
    }

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
        #[cfg(target_os = "macos")]
        {
            assert_eq!(arm[0].file_name().unwrap(), "lb-arm64");
            assert_eq!(arm[1].file_name().unwrap(), "lb-x64");
        }
        #[cfg(not(target_os = "macos"))]
        assert_eq!(arm[0].file_name().unwrap(), "lb");
        let x64 = bundled_candidates_for_arch(&root, "x86_64");
        #[cfg(target_os = "macos")]
        assert_eq!(x64[0].file_name().unwrap(), "lb-x64");
        #[cfg(not(target_os = "macos"))]
        let _ = x64;
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
    fn resolve_in_roots_uses_bundled_only() {
        // explicit roots (not resolve_lb_bin: debug builds prepend the
        // ambient crate dir, which would win over the fake root whenever
        // the checkout has a staged sidecar)
        let root = std::env::temp_dir().join("vicuna-lb-bundled");
        let _ = std::fs::remove_dir_all(&root);
        // Empty root must return None (never fall back to any external `lb` on PATH).
        assert_eq!(resolve_in_roots(&[root.clone()]), None);
        assert_eq!(resolve_in_roots(&[]), None);
        let sidecar = root.join("lb-sidecar");
        let _ = std::fs::create_dir_all(&sidecar);
        let expected = bundled_candidates(&root)[0].clone();
        std::fs::write(&expected, "x").expect("write bundled lb");
        let (hit, src) = resolve_in_roots(&[root.clone()]).expect("bundled must resolve");
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
    fn fallback_ports_never_share_the_base_profile() {
        let base = Path::new("/cache");
        assert_eq!(
            profile_for_port_under(base, 18765, 18765),
            PathBuf::from("/cache/live-browser/profile-vicuna")
        );
        let fb = profile_for_port_under(base, 18766, 18765);
        assert_eq!(fb, PathBuf::from("/cache/live-browser/profile-vicuna-18766"));
        // still recognized as Vicuna-owned by the orphan reaper
        let args = format!("/x/lb-sidecar/lb web --port 18766 --foreground --profile {}", fb.display());
        assert!(is_vicuna_owned_lb_args(&args, &vicuna_profile_under(base)));
    }

    #[test]
    fn shell_and_server_share_one_browser_state() {
        assert!(std::sync::Arc::ptr_eq(&shared(), &shared()));
        let st = crate::server::BridgeState::default();
        assert!(std::sync::Arc::ptr_eq(&st.browser, &shared()));
    }

    #[test]
    fn fallback_ports_skip_the_base_port() {
        let ports = candidate_ports(18765);
        assert_eq!(ports.len(), 10);
        assert_eq!(ports.first(), Some(&18766));
        assert_eq!(ports.last(), Some(&18775));
        assert!(!ports.contains(&18765));
        assert!(candidate_ports(u16::MAX).is_empty());
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

    #[test]
    fn start_attempts_windows_prefers_edge_then_chrome_then_cloak() {
        let cloak = PathBuf::from(r"C:\Users\u\.cloakbrowser\chromium-146\chrome.exe");
        let chrome = PathBuf::from(r"C:\Program Files\Google\Chrome\Application\chrome.exe");
        let edge = PathBuf::from(r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe");
        let s = |p: &PathBuf| p.display().to_string();
        // system_chrome_candidates() lists Chrome before Edge; Edge still wins
        let a = start_attempts_for("windows", None, Some(&cloak), &[chrome.clone(), edge.clone()]);
        assert_eq!(a, vec![s(&edge), s(&chrome), s(&cloak)]);
        // no system browser: CloakBrowser alone
        assert_eq!(start_attempts_for("windows", None, Some(&cloak), &[]), vec![s(&cloak)]);
        // VICUNA_BROWSER_CHROME still overrides everything, deduped
        let custom = PathBuf::from(r"D:\chromium\chrome.exe");
        let b = start_attempts_for("windows", Some(&custom), None, &[chrome.clone(), edge.clone()]);
        assert_eq!(b, vec![s(&custom), s(&edge), s(&chrome)]);
        let c = start_attempts_for("windows", Some(&chrome), Some(&cloak), &[chrome.clone(), edge.clone()]);
        assert_eq!(c, vec![s(&chrome), s(&edge), s(&cloak)]);
        assert!(prefers_system_browser("windows"));
    }

    #[test]
    fn start_attempts_mac_linux_keep_cloak_first() {
        let cloak = PathBuf::from("/home/u/.cloakbrowser/chromium-146/chrome");
        let chrome = PathBuf::from("/usr/bin/google-chrome");
        let edge = PathBuf::from("/usr/bin/microsoft-edge");
        let s = |p: &PathBuf| p.display().to_string();
        for os in ["linux", "macos"] {
            assert!(!prefers_system_browser(os));
            let a = start_attempts_for(os, None, Some(&cloak), &[chrome.clone(), edge.clone()]);
            assert_eq!(a, vec![s(&cloak), s(&chrome), s(&edge)], "{os}");
            let b = start_attempts_for(os, Some(&edge), Some(&cloak), &[chrome.clone(), edge.clone()]);
            assert_eq!(b, vec![s(&edge), s(&cloak), s(&chrome)], "{os}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn start_attempts_put_cloak_first_then_system() {
        let cloak = PathBuf::from("/home/u/.cloakbrowser/chromium-146/chrome");
        let chrome = PathBuf::from("/usr/bin/google-chrome");
        let edge = PathBuf::from("/usr/bin/microsoft-edge");
        let a = start_attempts(Some(&cloak), &[chrome.clone(), edge.clone()]);
        assert_eq!(
            a,
            vec![
                cloak.display().to_string(),
                chrome.display().to_string(),
                edge.display().to_string()
            ]
        );
        // no cloak (failed download): system engines only, order kept
        let b = start_attempts(None, &[chrome.clone(), edge.clone()]);
        assert_eq!(b.len(), 2);
        assert_eq!(b[0], chrome.display().to_string());
        // a duplicate path is never attempted twice
        let c = start_attempts(Some(&chrome), &[chrome.clone()]);
        assert_eq!(c, vec![chrome.display().to_string()]);
        // nothing at all: empty (caller errors with the engine reason)
        assert!(start_attempts(None, &[]).is_empty());
    }

    #[test]
    fn existing_binaries_filters_missing_paths() {
        let dir = std::env::temp_dir().join("vicuna-browser-candidates");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdirs");
        let real = dir.join("chrome.exe");
        std::fs::write(&real, "x").expect("write fake chrome");
        let kept = existing_binaries(vec![dir.join("nope.exe"), real.clone()]);
        assert_eq!(kept, vec![real]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn engine_labels_and_fallback_warning() {
        assert_eq!(engine_label("/home/u/.cloakbrowser/chromium-146/chrome"), "CloakBrowser");
        assert_eq!(engine_label(r"C:\Program Files\Microsoft\Edge\Application\msedge.exe"), "Microsoft Edge");
        assert_eq!(engine_label("/usr/bin/chromium"), "Chromium");
        assert_eq!(engine_label("/usr/bin/google-chrome"), "Google Chrome");
        assert!(is_cloak_engine("/x/.cloakbrowser/y"));
        assert!(!is_cloak_engine("/usr/bin/google-chrome"));

        let w = fallback_warning("/Program Files/Google/Chrome/Application/chrome.exe", Some("download timed out"));
        assert!(w.contains("Google Chrome"), "{w}");
        assert!(w.contains("download timed out"), "{w}");
        assert!(w.contains("CloakBrowser"), "{w}");
        // no reason still explains the fallback
        let w2 = fallback_warning("C:\\x\\msedge.exe", None);
        assert!(w2.contains("Microsoft Edge") && w2.contains("CloakBrowser"), "{w2}");
        assert!(!w2.contains("failed:"), "no empty reason: {w2}");
    }

    #[test]
    fn daemon_pidfile_liveness_parses_safely() {
        let dir = std::env::temp_dir().join("vicuna-lb-pidfile");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdirs");
        let f = dir.join("daemon-1.pid");
        // our own live pid is trusted
        std::fs::write(&f, format!("{}\n", std::process::id())).expect("write pid");
        assert!(daemon_pidfile_alive_at(&f));
        // garbage / missing / pid 0 are all "not our live daemon"
        std::fs::write(&f, "not-a-pid").expect("write garbage");
        assert!(!daemon_pidfile_alive_at(&f));
        std::fs::write(&f, "0").expect("write zero");
        assert!(!daemon_pidfile_alive_at(&f));
        assert!(!daemon_pidfile_alive_at(&dir.join("missing.pid")));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn strip_ansi_removes_sgr_and_osc() {
        assert_eq!(strip_ansi("plain"), "plain");
        assert_eq!(
            strip_ansi("\u{1b}[2m2026-01-01\u{1b}[0m INFO done"),
            "2026-01-01 INFO done"
        );
        assert_eq!(strip_ansi("a\u{1b}[38;5;196mred\u{1b}[0mb"), "aredb");
        // OSC sequence terminated by BEL
        assert_eq!(strip_ansi("x\u{1b}]0;title\u{7}y"), "xy");
        // unterminated escape is swallowed, not echoed as garbage
        assert_eq!(strip_ansi("tail\u{1b}["), "tail");
    }

    #[test]
    fn log_tail_keeps_last_lines_and_is_bounded() {
        let dir = std::env::temp_dir().join("vicuna-lb-logtail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdirs");
        let f = dir.join("daemon.log");
        let body: Vec<String> = (0..20).map(|i| format!("line {i}")).collect();
        std::fs::write(&f, body.join("\n")).expect("write log");
        let tail = log_tail(&f, 3);
        assert!(tail.ends_with("line 19"), "{tail}");
        assert!(tail.contains("line 17"), "{tail}");
        assert!(!tail.contains("line 16"), "{tail}");
        assert!(log_tail(&dir.join("missing.log"), 3).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn stop_owned_is_noop_without_child() {
        let state = BrowserState::default();
        stop_owned(&state).await;
        assert!(!owned_child_alive(&state));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stop_owned_kills_and_reaps_tracked_child() {
        let state = BrowserState::default();
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        state.child.lock().unwrap().replace(child);
        *state.spawned_version.lock().unwrap() = Some("0.4.20".into());
        *state.fallback.lock().unwrap() = Some("warn".into());

        assert!(pid_alive(pid));
        stop_owned(&state).await;
        assert!(!owned_child_alive(&state));
        assert!(!pid_alive(pid), "tracked child should be killed and reaped");
        assert!(state.spawned_version.lock().unwrap().is_none());
        assert!(state.fallback.lock().unwrap().is_none());
    }

    #[test]
    fn scan_download_temp_bytes_prefers_matching_pid() {
        let dir = std::env::temp_dir().join("vicuna-lb-dlbytes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdirs");
        assert_eq!(scan_download_temp_bytes(&dir, Some(79)), None);
        std::fs::write(dir.join(".lb-download-146.0-99.tar.gz"), vec![0u8; 100]).unwrap();
        std::fs::write(dir.join(".lb-download-146.0-79.tar.gz"), vec![0u8; 4096]).unwrap();
        assert_eq!(scan_download_temp_bytes(&dir, Some(79)), Some(4096));
        assert_eq!(scan_download_temp_bytes(&dir, Some(99)), Some(100));
        assert!(scan_download_temp_bytes(&dir, None).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_download_stderr_line_detects_percent_and_extracting() {
        let mut p = EngineDownloadProgress {
            phase: "downloading".to_string(),
            downloaded_bytes: 0,
            total_bytes: None,
            percent: None,
            message: None,
        };
        parse_download_stderr_line("Downloading... 42.6%", &mut p);
        assert_eq!(p.phase, "downloading");
        assert_eq!(p.percent, Some(43));
        parse_download_stderr_line("Extracting archive...", &mut p);
        assert_eq!(p.phase, "extracting");
    }

    #[test]
    fn managed_ports_includes_base_and_fallbacks() {
        let ports = managed_ports(18765);
        assert_eq!(ports.len(), 11);
        assert_eq!(ports.first(), Some(&18765));
        assert_eq!(ports.last(), Some(&18775));
    }

    #[test]
    fn vicuna_owner_record_round_trips_and_rejects_invalid() {
        let dir = std::env::temp_dir().join(format!("vicuna-lb-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = vicuna_owner_file_under(&dir, 18765);
        write_vicuna_owner_record_at(&path, 4242, 1234);
        let text = std::fs::read_to_string(&path).expect("owner file written");
        assert_eq!(parse_vicuna_owner_record(&text), Some((4242, 1234)));
        remove_vicuna_owner_record_at(&path);
        assert!(!path.exists());

        assert_eq!(parse_vicuna_owner_record(""), None);
        assert_eq!(parse_vicuna_owner_record("4242"), None);
        assert_eq!(parse_vicuna_owner_record("0 1234"), None);
        assert_eq!(parse_vicuna_owner_record("4242 0"), None);
        assert_eq!(parse_vicuna_owner_record("abc def"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_ps_output_handles_macos_and_linux_lines() {
        let sample = "\
  1   0 /sbin/launchd
 42   1 /Applications/My Apps/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18765 --foreground --profile /Users/a b/Library/Caches/live-browser/profile-vicuna
bad line
999 42 /usr/bin/sleep 60
";
        let rows = parse_ps_output(sample);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0], ProcEntry { pid: 1, ppid: 0, args: "/sbin/launchd".into() });
        assert_eq!(rows[1].pid, 42);
        assert_eq!(rows[1].ppid, 1);
        assert!(rows[1].args.contains("lb-arm64 web --port 18765"));
        assert_eq!(rows[2].pid, 999);
        assert_eq!(rows[2].ppid, 42);
    }

    #[test]
    fn extract_port_arg_handles_space_and_equals() {
        assert_eq!(extract_port_arg("lb web --port 18765 --foreground"), Some(18765));
        assert_eq!(extract_port_arg("lb web --port=18770 --foreground"), Some(18770));
        assert_eq!(extract_port_arg("lb web --foreground"), None);
        assert_eq!(extract_port_arg("lb web --port notanumber"), None);
    }

    #[test]
    fn vicuna_owned_lb_args_distinguishes_vicuna_from_personal_daemons() {
        let profile = Path::new("/Users/alice/Library/Caches/live-browser/profile-vicuna");

        // Bundled sidecar + Vicuna profile
        assert!(is_vicuna_owned_lb_args(
            "/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18765 --foreground --profile /Users/alice/Library/Caches/live-browser/profile-vicuna",
            profile,
        ));
        // PATH lb + Vicuna profile
        assert!(is_vicuna_owned_lb_args(
            "/opt/homebrew/bin/lb web --port 18766 --foreground --profile /Users/alice/Library/Caches/live-browser/profile-vicuna",
            profile,
        ));
        // Bundled sidecar even without explicit profile arg
        assert!(is_vicuna_owned_lb_args(
            "/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18765 --foreground",
            profile,
        ));

        // Personal user-run lb daemons MUST NOT match
        assert!(!is_vicuna_owned_lb_args(
            "/opt/homebrew/bin/lb web --port 18765 --foreground",
            profile,
        ));
        assert!(!is_vicuna_owned_lb_args(
            "/opt/homebrew/bin/lb web --port 18765 --profile /Users/alice/my-profile",
            profile,
        ));
        // Control subcommands and unrelated binaries MUST NOT match
        assert!(!is_vicuna_owned_lb_args(
            "/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18765 --profile /Users/alice/Library/Caches/live-browser/profile-vicuna stop",
            profile,
        ));
        assert!(!is_vicuna_owned_lb_args(
            "/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 mcp",
            profile,
        ));
        assert!(!is_vicuna_owned_lb_args(
            "/usr/bin/node web --port 18765 --profile /Users/alice/Library/Caches/live-browser/profile-vicuna",
            profile,
        ));
    }

    #[test]
    fn orphan_reaper_detection_matches_os_reapers_only() {
        assert!(is_orphan_reaper_args("/sbin/launchd"));
        assert!(is_orphan_reaper_args("/sbin/init splash"));
        assert!(is_orphan_reaper_args("/lib/systemd/systemd --user"));
        assert!(is_orphan_reaper_args("(systemd)"));
        assert!(is_orphan_reaper_args("/usr/bin/tini -- /app"));
        assert!(!is_orphan_reaper_args("/Applications/Vicuna.app/Contents/MacOS/Vicuna"));
        assert!(!is_orphan_reaper_args("-zsh"));
        assert!(!is_orphan_reaper_args("/bin/bash"));
    }

    #[test]
    fn find_orphaned_vicuna_daemons_reaps_all_11_orphans_and_spares_live_and_personal() {
        let profile = Path::new("/Users/alice/Library/Caches/live-browser/profile-vicuna");
        let managed = managed_ports(18765);
        let self_pid = 9000;

        // Reproduce the reported 11-orphan accumulation on macOS (PPID=1)
        let eleven: Vec<ProcEntry> = (0..11u16)
            .map(|i| ProcEntry {
                pid: 2000 + u32::from(i),
                ppid: 1,
                args: format!(
                    "/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port {} --foreground --profile {}",
                    18765 + i,
                    profile.display()
                ),
            })
            .collect();
        let found = find_orphaned_vicuna_daemons(&eleven, &managed, profile, self_pid);
        assert_eq!(found.len(), 11);
        assert_eq!(found[0], (18765, 2000));
        assert_eq!(found[10], (18775, 2010));

        // Mixed process table: only genuinely orphaned Vicuna daemons in managed range match
        let mixed = vec![
            ProcEntry { pid: 1, ppid: 0, args: "/sbin/launchd".into() },
            ProcEntry { pid: 1000, ppid: 1, args: "/lib/systemd/systemd --user".into() },
            ProcEntry { pid: 500, ppid: 1, args: "/Applications/Vicuna.app/Contents/MacOS/Vicuna".into() },
            ProcEntry { pid: 600, ppid: 1, args: "-zsh".into() },
            // 18765: orphaned (PPID=1) Vicuna sidecar -> REAP
            ProcEntry {
                pid: 3001,
                ppid: 1,
                args: format!("/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18765 --foreground --profile {}", profile.display()),
            },
            // 18766: orphaned (Linux systemd --user subreaper) Vicuna PATH lb -> REAP
            ProcEntry {
                pid: 3002,
                ppid: 1000,
                args: format!("/usr/local/bin/lb web --port 18766 --foreground --profile {}", profile.display()),
            },
            // 18767: owned by live sibling Vicuna (PID 500) -> SPARE
            ProcEntry {
                pid: 3003,
                ppid: 500,
                args: format!("/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18767 --foreground --profile {}", profile.display()),
            },
            // 18768: our own child -> SPARE
            ProcEntry {
                pid: 3004,
                ppid: self_pid,
                args: format!("/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 18768 --foreground --profile {}", profile.display()),
            },
            // 18769: personal user-run lb (PPID=1, default profile) -> SPARE
            ProcEntry {
                pid: 3005,
                ppid: 1,
                args: "/opt/homebrew/bin/lb web --port 18769 --foreground".into(),
            },
            // 18770: personal user-run lb on different profile -> SPARE
            ProcEntry {
                pid: 3006,
                ppid: 1,
                args: "/opt/homebrew/bin/lb web --port 18770 --profile /Users/alice/custom".into(),
            },
            // 18771: manually run from interactive shell (PPID=600 zsh) -> SPARE
            ProcEntry {
                pid: 3007,
                ppid: 600,
                args: format!("/opt/homebrew/bin/lb web --port 18771 --profile {}", profile.display()),
            },
            // 8765: outside Vicuna's managed port range -> SPARE
            ProcEntry {
                pid: 3008,
                ppid: 1,
                args: format!("/Applications/Vicuna.app/Contents/Resources/lb-sidecar/lb-arm64 web --port 8765 --foreground --profile {}", profile.display()),
            },
        ];
        let mixed_found = find_orphaned_vicuna_daemons(&mixed, &managed, profile, self_pid);
        assert_eq!(mixed_found, vec![(18765, 3001), (18766, 3002)]);
    }

    #[cfg(unix)]
    #[test]
    fn kill_owned_is_idempotent_and_reaps_child() {
        let state = BrowserState::default();
        // No child: safe no-op
        kill_owned(&state);
        assert!(!owned_child_alive(&state));

        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        state.child.lock().unwrap().replace(child);
        assert!(pid_alive(pid));

        kill_owned(&state);
        assert!(!owned_child_alive(&state));
        assert!(!pid_alive(pid), "kill_owned must kill and reap the tracked child");

        // Second call (e.g. quit_now followed by RunEvent::Exit) is a no-op
        kill_owned(&state);
    }
}
