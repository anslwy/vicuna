// Server detection + lifecycle for the `vicuna` CLI: `start`, `stop`,
// `restart` and `status` all share the detection below, and `upgrade` reuses
// the same restart, so there is exactly one answer to "how does `vicuna
// serve` run on this machine?".
//
// Detection takes its inputs (proc root, command runner, container flag) as
// arguments and every command first builds a `Step` plan, so tests assert on
// plans and never touch a real server.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const SERVICE: &str = "vicuna.service";
pub const LAUNCHD_LABEL: &str = "com.vicuna.server";

/// How long a fresh server may take to show up before we call it broken.
const READY_TIMEOUT: u64 = 20;
/// How long a stop may take before we report it stuck.
const STOP_TIMEOUT: u64 = 15;

pub fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

// ---------------------------------------------------------------- processes

#[derive(Debug, Clone, PartialEq)]
pub struct ServeProc {
    pub pid: u32,
    /// argv after `serve`
    pub serve_args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Server {
    /// systemd user unit; `via_host` = reached through `distrobox-host-exec`
    Systemd { via_host: bool },
    Launchd { domain: String },
    Process(ServeProc),
}

fn args_suffix(args: &[String]) -> String {
    if args.is_empty() {
        String::new()
    } else {
        format!(" {}", args.join(" "))
    }
}

impl Server {
    /// How it runs, in one line for humans.
    pub fn label(&self) -> String {
        match self {
            Server::Systemd { via_host: false } => format!("systemd user unit {SERVICE}"),
            Server::Systemd { via_host: true } => {
                format!("systemd user unit {SERVICE} on the host (via distrobox-host-exec)")
            }
            Server::Launchd { .. } => format!("launchd agent {LAUNCHD_LABEL}"),
            Server::Process(p) => format!("process {} (vicuna serve{})", p.pid, args_suffix(&p.serve_args)),
        }
    }

    /// The pid, when the server is a bare process (a service's pid is the
    /// process manager's business, though `serve_procs` usually knows it).
    pub fn pid(&self) -> Option<u32> {
        match self {
            Server::Process(p) => Some(p.pid),
            _ => None,
        }
    }

    pub fn serve_args(&self) -> &[String] {
        match self {
            Server::Process(p) => &p.serve_args,
            _ => &[],
        }
    }
}

/// `vicuna serve …` processes under a Linux-style proc root (`/proc`).
/// Matches argv[0] basename `vicuna` and argv[1] `serve`; skips `skip_pid`
/// (ourselves).
pub fn scan_proc(root: &Path, skip_pid: u32) -> Vec<ServeProc> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else { return out };
    for e in rd.flatten() {
        let Some(pid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else { continue };
        if pid == skip_pid {
            continue;
        }
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let argv: Vec<String> = raw
            .split(|b| *b == 0)
            .filter(|a| !a.is_empty())
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if let Some(args) = serve_args_of(&argv) {
            let env = std::fs::read(e.path().join("environ"))
                .map(|r| {
                    r.split(|b| *b == 0)
                        .filter_map(|kv| {
                            let kv = String::from_utf8_lossy(kv);
                            kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.push(ServeProc {
                pid,
                serve_args: args,
                cwd: std::fs::read_link(e.path().join("cwd")).ok(),
                env,
            });
        }
    }
    out.sort_by_key(|p| p.pid);
    out
}

/// `Some(args after serve)` when argv is `…/vicuna serve …`.
pub fn serve_args_of(argv: &[String]) -> Option<Vec<String>> {
    let base = Path::new(argv.first()?).file_name()?.to_str()?;
    (base == "vicuna" && argv.get(1).map(String::as_str) == Some("serve")).then(|| argv[2..].to_vec())
}

/// macOS has no /proc: `ps -axww -o pid=,command=` lines (args split on
/// whitespace, good enough for vicuna's own flags). No cwd/env there.
pub fn scan_ps(text: &str, skip_pid: u32) -> Vec<ServeProc> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid: u32 = it.next()?.parse().ok()?;
            let argv: Vec<String> = it.map(String::from).collect();
            (pid != skip_pid).then_some(())?;
            Some(ServeProc { pid, serve_args: serve_args_of(&argv)?, cwd: None, env: vec![] })
        })
        .collect()
}

/// Runs a command, `Ok(stdout)` on success. Injected so tests can fake it.
pub type Runner<'a> = &'a dyn Fn(&[&str]) -> Result<String, String>;

pub fn real_runner(argv: &[&str]) -> Result<String, String> {
    let out = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let so = String::from_utf8_lossy(&out.stdout).to_string();
    if out.status.success() {
        Ok(so)
    } else {
        let se = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(if se.is_empty() { so.trim().to_string() } else { se })
    }
}

/// Every `vicuna serve` process on this machine (ourselves excluded).
pub fn serve_procs() -> Vec<ServeProc> {
    if Path::new("/proc/self").exists() {
        scan_proc(Path::new("/proc"), std::process::id())
    } else {
        real_runner(&["ps", "-axww", "-o", "pid=,command="])
            .map(|t| scan_ps(&t, std::process::id()))
            .unwrap_or_default()
    }
}

/// How the server runs: a service manager wins over the bare process it
/// spawned (restarting the process directly would fight `Restart=`).
pub fn detect(os: &str, run: Runner, procs: &[ServeProc], in_container: bool) -> Option<Server> {
    if os == "linux" {
        let active = |r: Result<String, String>| r.map(|s| s.trim() == "active").unwrap_or(false);
        if active(run(&["systemctl", "--user", "is-active", SERVICE])) {
            return Some(Server::Systemd { via_host: false });
        }
        // inside distrobox/toolbox the unit lives on the host's user manager
        if in_container && active(run(&["distrobox-host-exec", "systemctl", "--user", "is-active", SERVICE])) {
            return Some(Server::Systemd { via_host: true });
        }
    }
    if os == "macos" {
        if let Ok(uid) = run(&["id", "-u"]) {
            let domain = format!("gui/{}", uid.trim());
            if let Ok(s) = run(&["launchctl", "print", &format!("{domain}/{LAUNCHD_LABEL}")]) {
                if s.contains("state = running") {
                    return Some(Server::Launchd { domain });
                }
            }
        }
    }
    procs.first().cloned().map(Server::Process)
}

pub fn in_container() -> bool {
    Path::new("/run/.containerenv").exists() || Path::new("/.dockerenv").exists() || std::env::var_os("CONTAINER_ID").is_some()
}

/// Live scan + detection, the way every command starts.
pub fn detect_live() -> (Vec<ServeProc>, Option<Server>) {
    let procs = serve_procs();
    let running = detect(std::env::consts::OS, &real_runner, &procs, in_container());
    (procs, running)
}

/// `--db PATH` from serve args, else the default.
pub fn db_path(serve_args: &[String], default: &Path) -> PathBuf {
    let mut it = serve_args.iter();
    while let Some(a) = it.next() {
        if a == "--db" {
            if let Some(p) = it.next() {
                return PathBuf::from(p);
            }
        } else if let Some(p) = a.strip_prefix("--db=") {
            return PathBuf::from(p);
        }
    }
    default.to_path_buf()
}

/// The serve flags of the running server: a unit's own ExecStart is not
/// readable here, so fall back to the process we scanned (same flags).
pub fn serve_args_of_server(server: Option<&Server>, procs: &[ServeProc]) -> Vec<String> {
    server
        .map(Server::serve_args)
        .filter(|a| !a.is_empty())
        .map(<[String]>::to_vec)
        .or_else(|| procs.first().map(|p| p.serve_args.clone()))
        .unwrap_or_default()
}

// ---------------------------------------------------------------- restarts

/// The restart command (argv). Service managers do the restart themselves
/// (`--no-block` / `kickstart` enqueue it and return), so it completes even
/// when this process lives in the server's own cgroup and gets killed.
/// A bare process: a detached `sh` stops it, waits up to 10 s (then
/// SIGKILL), and execs the new binary with the same args, logging to `log`.
pub fn restart_argv(server: &Server, exe: &Path, log: &Path) -> Vec<String> {
    let s = |x: &str| x.to_string();
    match server {
        Server::Systemd { via_host } => {
            let mut v = if *via_host { vec![s("distrobox-host-exec")] } else { vec![] };
            v.extend([s("systemctl"), s("--user"), s("restart"), s("--no-block"), s(SERVICE)]);
            v
        }
        Server::Launchd { domain } => vec![s("launchctl"), s("kickstart"), s("-k"), format!("{domain}/{LAUNCHD_LABEL}")],
        Server::Process(p) => {
            let script = "pid=$1; log=$2; shift 2; kill \"$pid\" 2>/dev/null; i=0; \
                while kill -0 \"$pid\" 2>/dev/null && [ $i -lt 50 ]; do sleep 0.2; i=$((i+1)); done; \
                kill -9 \"$pid\" 2>/dev/null; exec \"$@\" >>\"$log\" 2>&1 </dev/null";
            let mut v = vec![s("sh"), s("-c"), s(script), s("sh"), p.pid.to_string(), log.to_string_lossy().into_owned()];
            v.push(exe.to_string_lossy().into_owned());
            v.push(s("serve"));
            v.extend(p.serve_args.iter().cloned());
            v
        }
    }
}

/// One line explaining a restart, for `restart`/`upgrade --dry-run`.
pub fn restart_summary(server: &Server, exe: &Path, log: &Path) -> String {
    match server {
        Server::Process(p) => format!(
            "stop pid {} and start {} serve{} (log: {})",
            p.pid,
            exe.display(),
            args_suffix(&p.serve_args),
            log.display()
        ),
        other => restart_argv(other, exe, log).join(" "),
    }
}

/// `argv` in a new session (setsid when available, else a new process group),
/// so it outlives our terminal and our cgroup.
fn detached_command(argv: &[String]) -> Command {
    use std::os::unix::process::CommandExt;
    let has_setsid = Command::new("setsid")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok();
    if has_setsid {
        let mut c = Command::new("setsid");
        c.args(argv);
        c
    } else {
        let mut c = Command::new(&argv[0]);
        c.args(&argv[1..]).process_group(0);
        c
    }
}

fn spawn_detached(argv: &[String], proc_: Option<&ServeProc>) -> Result<(), String> {
    let mut c = detached_command(argv);
    if let Some(p) = proc_ {
        // keep the server's own environment (VICUNA_SERVER_TOKEN, PATH, …) and cwd
        if !p.env.is_empty() {
            c.env_clear().envs(p.env.iter().cloned());
        }
        if let Some(d) = p.cwd.as_ref().filter(|d| d.is_dir()) {
            c.current_dir(d);
        }
    }
    c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    c.spawn().map(|_| ()).map_err(|e| format!("{}: {e}", argv[0]))
}

/// The one restart used by both `vicuna restart` and `vicuna upgrade`.
pub fn spawn_restart(server: &Server, exe: &Path, log: &Path) -> Result<(), String> {
    let argv = restart_argv(server, exe, log);
    let proc_ = match server {
        Server::Process(p) => Some(p),
        _ => None,
    };
    spawn_detached(&argv, proc_)
}

/// `spawn_detached` for a server we start ourselves: its own stdout/stderr go
/// to the serve log (token, tailnet URL, warnings).
fn spawn_detached_logged(argv: &[String], log: &Path) -> Result<(), String> {
    if let Some(d) = log.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .map_err(|e| format!("{}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    let mut c = detached_command(argv);
    c.stdin(Stdio::null()).stdout(Stdio::from(out)).stderr(Stdio::from(err));
    c.spawn().map(|_| ()).map_err(|e| format!("{}: {e}", argv[0]))
}

// ---------------------------------------------------------------- services

/// The service file `vicuna install-service` writes, when one is installed.
#[derive(Debug, Clone, PartialEq)]
pub enum Installed {
    Systemd { unit: PathBuf },
    Launchd { plist: PathBuf, domain: String },
}

pub fn installed_service(home: &Path, os: &str, uid: &str) -> Option<Installed> {
    match os {
        "linux" => {
            let unit = home.join(".config/systemd/user").join(SERVICE);
            unit.exists().then_some(Installed::Systemd { unit })
        }
        "macos" => {
            let plist = home.join("Library/LaunchAgents").join(format!("{LAUNCHD_LABEL}.plist"));
            plist.exists().then(|| Installed::Launchd { plist, domain: format!("gui/{uid}") })
        }
        _ => None,
    }
}

/// `systemctl --user` for this environment: the local user manager, or the
/// host's when we sit in a distrobox/toolbox container whose own user manager
/// is unreachable (our real setup runs the unit on the host).
pub fn systemd_prefix(run: Runner, in_container: bool) -> Vec<String> {
    if run(&["systemctl", "--user", "show-environment"]).is_ok() {
        return vec!["systemctl".into(), "--user".into()];
    }
    if in_container && run(&["distrobox-host-exec", "systemctl", "--user", "show-environment"]).is_ok() {
        return vec!["distrobox-host-exec".into(), "systemctl".into(), "--user".into()];
    }
    vec!["systemctl".into(), "--user".into()]
}

fn systemd_argv(via_host: bool, verb: &str) -> Vec<String> {
    let mut v = if via_host { vec!["distrobox-host-exec".to_string()] } else { vec![] };
    v.extend([String::from("systemctl"), String::from("--user"), String::from(verb), String::from(SERVICE)]);
    v
}

// ---------------------------------------------------------------- plans

/// Everything a command does, as data: tests assert on plans, `execute` runs
/// them. Keeping process/spawn calls out of the planning is what lets the CLI
/// be tested without touching this machine's server.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Print this.
    Note(String),
    /// Run and wait; a non-zero exit fails the command.
    Run(Vec<String>),
    /// Try each argv in order, succeed on the first that works (launchd:
    /// bootstrap a fresh agent, kickstart an already-loaded one).
    Try(Vec<Vec<String>>),
    /// Launch a `vicuna serve` we own: detached, output appended to the log.
    Serve(Vec<String>),
    /// Restart the running server via the shared restart.
    Restart(Box<Server>),
    /// SIGTERM a pid, wait, then SIGKILL.
    Kill(u32),
    /// Wait until `vicuna serve` is running (seconds).
    WaitUp(u64),
    /// Wait until no `vicuna serve` is running (seconds).
    WaitDown(u64),
}

/// Paths, platform and probe results the plans are built from.
pub struct Ctx<'a> {
    pub os: &'static str,
    pub run: Runner<'a>,
    pub in_container: bool,
    /// `id -u` (macOS launchd domain)
    pub uid: String,
    pub home: PathBuf,
    pub exe: PathBuf,
    pub log: PathBuf,
    /// a tailscale CLI exists, so a bare `vicuna start` exposes the tailnet
    pub tailscale: bool,
}

pub fn live_ctx() -> Ctx<'static> {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("vicuna"));
    Ctx {
        os: std::env::consts::OS,
        run: &real_runner,
        in_container: in_container(),
        uid: real_runner(&["id", "-u"]).map(|s| s.trim().to_string()).unwrap_or_default(),
        home: home(),
        exe: std::fs::canonicalize(&exe).unwrap_or(exe),
        log: serve_log(),
        tailscale: crate::cli::find_tailscale(std::env::var_os("PATH").as_deref(), |p| p.exists()).is_some(),
    }
}

/// `vicuna serve` flags for a start with no explicit options.
pub fn effective_serve_args(c: &Ctx, args: &[String]) -> Vec<String> {
    if !args.is_empty() {
        args.to_vec()
    } else if c.tailscale {
        vec!["--tailscale".into()]
    } else {
        vec![]
    }
}

pub fn serve_argv(exe: &Path, args: &[String]) -> Vec<String> {
    let mut v = vec![exe.to_string_lossy().into_owned(), "serve".into()];
    v.extend(args.iter().cloned());
    v
}

/// Start the server: the installed unit when there is one, else a detached
/// `vicuna serve`. Already running → one Note, no action.
pub fn plan_start(c: &Ctx, running: Option<&Server>, args: &[String]) -> Vec<Step> {
    if let Some(s) = running {
        return vec![Step::Note(format!("vicuna serve is already running ({}).", s.label()))];
    }
    if let Some(svc) = installed_service(&c.home, c.os, &c.uid) {
        let ignored = if args.is_empty() {
            String::new()
        } else {
            format!("\n  (the installed service carries its own serve options; {}{} is ignored)", "vicuna start", args_suffix(args))
        };
        return match svc {
            Installed::Systemd { unit } => {
                let mut start = systemd_prefix(c.run, c.in_container);
                start.extend([String::from("start"), String::from(SERVICE)]);
                vec![
                    Step::Note(format!("Starting the installed systemd user unit {SERVICE} ({}).{ignored}", unit.display())),
                    Step::Run(start),
                    Step::WaitUp(READY_TIMEOUT),
                ]
            }
            Installed::Launchd { plist, domain } => {
                let label = format!("{domain}/{LAUNCHD_LABEL}");
                vec![
                    Step::Note(format!("Starting the installed launchd agent {LAUNCHD_LABEL} ({}).{ignored}", plist.display())),
                    Step::Try(vec![
                        vec![String::from("launchctl"), String::from("bootstrap"), domain, plist.to_string_lossy().into_owned()],
                        vec![String::from("launchctl"), String::from("kickstart"), String::from("-k"), label],
                    ]),
                    Step::WaitUp(READY_TIMEOUT),
                ]
            }
        };
    }
    let args = effective_serve_args(c, args);
    vec![
        Step::Note(format!(
            "Starting `{} serve{}` in the background (log: {}).",
            c.exe.display(),
            args_suffix(&args),
            c.log.display()
        )),
        Step::Serve(serve_argv(&c.exe, &args)),
        Step::WaitUp(READY_TIMEOUT),
    ]
}

/// Stop it however it runs; not running → one Note.
pub fn plan_stop(running: Option<&Server>) -> Vec<Step> {
    let Some(s) = running else {
        return vec![Step::Note("vicuna serve is not running.".into())];
    };
    match s {
        Server::Systemd { via_host } => vec![
            Step::Note(format!("Stopping {}.", s.label())),
            Step::Run(systemd_argv(*via_host, "stop")),
            Step::WaitDown(STOP_TIMEOUT),
        ],
        Server::Launchd { domain } => vec![
            Step::Note(format!("Stopping {}.", s.label())),
            Step::Run(vec![String::from("launchctl"), String::from("bootout"), format!("{domain}/{LAUNCHD_LABEL}")]),
            Step::WaitDown(STOP_TIMEOUT),
        ],
        Server::Process(p) => vec![
            Step::Note(format!("Stopping {}.", s.label())),
            Step::Kill(p.pid),
            Step::WaitDown(STOP_TIMEOUT),
            Step::Note("Stopped.".into()),
        ],
    }
}

/// Restart detached, so it finishes even when the caller is a session hosted
/// by the very server being replaced. Not running → start it instead.
pub fn plan_restart(c: &Ctx, running: Option<&Server>, args: &[String]) -> Vec<Step> {
    match running {
        None => {
            let mut steps = vec![Step::Note("vicuna serve is not running; starting it instead.".into())];
            steps.extend(plan_start(c, None, args));
            steps
        }
        Some(s) => vec![
            Step::Note(format!("Restarting {} (detached).", s.label())),
            Step::Restart(Box::new(s.clone())),
        ],
    }
}

// ---------------------------------------------------------------- status

fn exe_of(pid: u32) -> Option<PathBuf> {
    let p = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let s = p.to_string_lossy();
    Some(PathBuf::from(s.trim_end_matches(" (deleted)")))
}

pub fn version_of(exe: &Path) -> String {
    let p = exe.to_string_lossy().into_owned();
    real_runner(&[&p, "--version"])
        .map(|s| s.lines().next().unwrap_or_default().trim().trim_start_matches("vicuna ").to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn running_version(pid: Option<u32>, fallback: &Path) -> String {
    let exe = pid.and_then(exe_of).unwrap_or_else(|| fallback.to_path_buf());
    version_of(&exe)
}

fn log_hint(server: &Server, c: &Ctx) -> String {
    match server {
        Server::Systemd { via_host: false } => format!("journalctl --user -u {SERVICE} -f"),
        Server::Systemd { via_host: true } => format!("distrobox-host-exec journalctl --user -u {SERVICE} -f"),
        Server::Launchd { .. } => c.home.join("Library/Logs/vicuna-server.log").display().to_string(),
        Server::Process(_) => c.log.display().to_string(),
    }
}

/// The `vicuna status` report.
pub fn status_lines(c: &Ctx, running: Option<&Server>, procs: &[ServeProc]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(s) = running else {
        out.push("vicuna serve: not running".into());
        out.push(format!("  installed: {}", version_of(&c.exe)));
        match installed_service(&c.home, c.os, &c.uid) {
            Some(Installed::Systemd { unit }) => out.push(format!("  service: installed ({}) — `vicuna start` starts it", unit.display())),
            Some(Installed::Launchd { plist, .. }) => {
                out.push(format!("  service: installed ({}) — `vicuna start` starts it", plist.display()))
            }
            None => {}
        }
        out.push(format!("  log: {}", c.log.display()));
        return out;
    };
    out.push("vicuna serve: running".into());
    out.push(format!("  how: {}", s.label()));
    let pid = s.pid().or_else(|| procs.first().map(|p| p.pid));
    if let Some(pid) = pid {
        out.push(format!("  pid: {pid}"));
    }
    out.push(format!("  version: {}", running_version(pid, &c.exe)));
    let args = serve_args_of_server(Some(s), procs);
    if !args.is_empty() {
        out.push(format!("  args: {}", args.join(" ")));
    }
    out.push(format!("  db: {}", db_path(&args, &crate::server_dir().join("vicuna.db")).display()));
    out.push(format!("  log: {}", log_hint(s, c)));
    out
}

// ---------------------------------------------------------------- execute

fn run_argv(argv: &[String]) -> Result<(), String> {
    let refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    real_runner(&refs).map(|_| ()).map_err(|e| format!("{}: {e}", argv[0]))
}

fn pid_alive(pid: u32) -> bool {
    if Path::new("/proc/self").exists() {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        real_runner(&["kill", "-0", &pid.to_string()]).is_ok()
    }
}

fn signal(pid: u32, sig: &str) -> Result<(), String> {
    real_runner(&["kill", &format!("-{sig}"), &pid.to_string()]).map(|_| ()).map_err(|e| e.to_string())
}

fn terminate(pid: u32) -> Result<(), String> {
    println!("  stopping pid {pid} (SIGTERM)");
    let _ = signal(pid, "TERM");
    let until = Instant::now() + Duration::from_secs(10);
    while pid_alive(pid) && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(200));
    }
    if pid_alive(pid) {
        println!("  pid {pid} ignored SIGTERM for 10s; sending SIGKILL");
        let _ = signal(pid, "KILL");
        std::thread::sleep(Duration::from_millis(300));
    }
    if pid_alive(pid) {
        return Err(format!("pid {pid} is still running"));
    }
    Ok(())
}

fn wait_up(secs: u64) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if let (_, Some(s)) = detect_live() {
            println!("  up: {}", s.label());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("`vicuna serve` did not come up within {secs}s — run `vicuna status` and check the log"))
}

fn wait_down(secs: u64) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        if serve_procs().is_empty() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(format!("`vicuna serve` is still running after {secs}s"))
}

pub fn execute(c: &Ctx, steps: Vec<Step>) -> Result<(), String> {
    let mut started = false;
    for step in steps {
        match step {
            Step::Note(m) => println!("{m}"),
            Step::Run(argv) => {
                println!("  $ {}", argv.join(" "));
                run_argv(&argv)?;
            }
            Step::Try(alternatives) => {
                let mut last = String::new();
                let mut ok = false;
                for argv in &alternatives {
                    println!("  $ {}", argv.join(" "));
                    match run_argv(argv) {
                        Ok(()) => {
                            ok = true;
                            break;
                        }
                        Err(e) => last = e,
                    }
                }
                if !ok {
                    return Err(last);
                }
            }
            Step::Serve(argv) => {
                println!("  $ {} (detached)", argv.join(" "));
                spawn_detached_logged(&argv, &c.log)?;
                started = true;
            }
            Step::Restart(server) => {
                println!("  $ {}", restart_summary(&server, &c.exe, &c.log));
                spawn_restart(&server, &c.exe, &c.log)?;
            }
            Step::Kill(pid) => terminate(pid)?,
            Step::WaitUp(secs) => wait_up(secs)?,
            Step::WaitDown(secs) => wait_down(secs)?,
        }
    }
    if started {
        // informational pidfile for a server we launched ourselves
        if let Some(p) = serve_procs().into_iter().next() {
            write_pidfile(&p, &c.log)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- pidfile

pub fn serve_log() -> PathBuf {
    crate::server_dir().join("serve.log")
}

pub fn pid_file() -> PathBuf {
    crate::server_dir().join("vicuna.pid")
}

fn write_pidfile(p: &ServeProc, log: &Path) -> Result<(), String> {
    let dir = crate::server_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let body = serde_json::json!({
        "pid": p.pid,
        "args": p.serve_args,
        "log": log.display().to_string(),
    })
    .to_string();
    std::fs::write(pid_file(), body).map_err(|e| format!("{}: {e}", pid_file().display()))
}

// ---------------------------------------------------------------- commands

pub fn start(args: &[String]) -> Result<(), String> {
    let c = live_ctx();
    let (_, running) = detect_live();
    execute(&c, plan_start(&c, running.as_ref(), args))
}

pub fn stop() -> Result<(), String> {
    let c = live_ctx();
    let (_, running) = detect_live();
    let steps = plan_stop(running.as_ref());
    execute(&c, steps)?;
    let _ = std::fs::remove_file(pid_file());
    Ok(())
}

pub fn restart(args: &[String]) -> Result<(), String> {
    let c = live_ctx();
    let (_, running) = detect_live();
    execute(&c, plan_restart(&c, running.as_ref(), args))
}

/// `Ok(true)` when a server is running (main turns that into the exit code).
pub fn status() -> Result<bool, String> {
    let c = live_ctx();
    let (procs, running) = detect_live();
    for line in status_lines(&c, running.as_ref(), &procs) {
        println!("{line}");
    }
    Ok(running.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_cmds(_: &[&str]) -> Result<String, String> {
        Err("not available".into())
    }

    fn proc_(pid: u32, args: &[&str]) -> ServeProc {
        ServeProc { pid, serve_args: args.iter().map(|s| s.to_string()).collect(), cwd: None, env: vec![] }
    }

    fn ctx<'a>(home: &Path, run: Runner<'a>, tailscale: bool) -> Ctx<'a> {
        Ctx {
            os: "linux",
            run,
            in_container: false,
            uid: "501".into(),
            home: home.to_path_buf(),
            exe: PathBuf::from("/home/me/.local/bin/vicuna"),
            log: home.join("vicuna/server/serve.log"),
            tailscale,
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vicuna-server-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finds_serve_processes_in_proc_and_ps() {
        let root = std::env::temp_dir().join(format!("vicuna-server-proc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mk = |pid: u32, argv: &[&str], env: &str| {
            let d = root.join(pid.to_string());
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("cmdline"), argv.join("\0") + "\0").unwrap();
            std::fs::write(d.join("environ"), env).unwrap();
        };
        mk(10, &["/home/me/.local/bin/vicuna", "serve", "--tailscale"], "VICUNA_SERVER_TOKEN=t\0PATH=/bin\0");
        mk(11, &["vicuna", "upgrade"], "");
        mk(12, &["/usr/bin/notvicuna", "serve"], "");
        mk(13, &["/home/me/.local/bin/vicuna", "serve"], ""); // ourselves
        std::fs::create_dir_all(root.join("self")).unwrap();
        let found = scan_proc(&root, 13);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pid, 10);
        assert_eq!(found[0].serve_args, vec!["--tailscale"]);
        assert!(found[0].env.contains(&("VICUNA_SERVER_TOKEN".into(), "t".into())));
        let _ = std::fs::remove_dir_all(&root);

        let ps = "  1 /sbin/launchd\n 42 /Users/me/.local/bin/vicuna serve --port 7500\n 43 vicuna upgrade\n";
        let found = scan_ps(ps, 43);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].pid, found[0].serve_args.clone()), (42, vec!["--port".to_string(), "7500".to_string()]));
    }

    #[test]
    fn detection_prefers_service_managers() {
        let procs = vec![proc_(10, &["--tailscale"])];
        // plain systemd
        let r = |a: &[&str]| if a[0] == "systemctl" { Ok("active\n".into()) } else { Err("no".into()) };
        assert_eq!(detect("linux", &r, &procs, false), Some(Server::Systemd { via_host: false }));
        // distrobox: container systemctl fails, the host's answers
        let r =
            |a: &[&str]| if a[0] == "distrobox-host-exec" { Ok("active".into()) } else { Err("System has not been booted with systemd".into()) };
        assert_eq!(detect("linux", &r, &procs, true), Some(Server::Systemd { via_host: true }));
        assert_eq!(detect("linux", &r, &procs, false), Some(Server::Process(procs[0].clone())), "host only consulted in a container");
        // inactive unit → the bare process
        let r = |_: &[&str]| Ok::<_, String>("inactive".into());
        assert_eq!(detect("linux", &r, &procs, true), Some(Server::Process(procs[0].clone())));
        assert_eq!(detect("linux", &r, &[], true), None, "nothing running → nothing to restart");
        // launchd
        let r = |a: &[&str]| match a[0] {
            "id" => Ok("501\n".into()),
            "launchctl" => Ok("com.vicuna.server = {\n\tstate = running\n".into()),
            _ => Err("no".into()),
        };
        assert_eq!(detect("macos", &r, &procs, false), Some(Server::Launchd { domain: "gui/501".into() }));
    }

    #[test]
    fn restart_commands_and_db_path() {
        let exe = Path::new("/home/me/.local/bin/vicuna");
        let log = Path::new("/home/me/vicuna/server/serve.log");
        assert_eq!(
            restart_argv(&Server::Systemd { via_host: false }, exe, log).join(" "),
            "systemctl --user restart --no-block vicuna.service"
        );
        assert_eq!(
            restart_argv(&Server::Systemd { via_host: true }, exe, log).join(" "),
            "distrobox-host-exec systemctl --user restart --no-block vicuna.service"
        );
        assert_eq!(
            restart_argv(&Server::Launchd { domain: "gui/501".into() }, exe, log).join(" "),
            "launchctl kickstart -k gui/501/com.vicuna.server"
        );
        let v = restart_argv(&Server::Process(proc_(10, &["--db", "/x y/v.db"])), exe, log);
        assert_eq!(&v[..2], &["sh", "-c"]);
        assert_eq!(&v[3..], &["sh", "10", "/home/me/vicuna/server/serve.log", "/home/me/.local/bin/vicuna", "serve", "--db", "/x y/v.db"]);

        let def = Path::new("/d/vicuna.db");
        assert_eq!(db_path(&[], def), def);
        assert_eq!(db_path(&["--tailscale".into(), "--db".into(), "/a.db".into()], def), Path::new("/a.db"));
        assert_eq!(db_path(&["--db=/b.db".into()], def), Path::new("/b.db"));
    }

    #[test]
    fn plain_restart_script_stops_then_execs() {
        // run the real detached script against a sleeping stand-in "server"
        let dir = temp("restart");
        let mut old = Command::new("sleep").arg("30").spawn().unwrap();
        let fake = dir.join("vicuna");
        std::fs::write(&fake, "#!/bin/sh\necho \"started $*\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = dir.join("serve.log");
        let argv = restart_argv(&Server::Process(proc_(old.id(), &["--port", "7999"])), &fake, &log);
        spawn_detached(&argv, None).unwrap();
        let status = old.wait().unwrap(); // reaped by us: the script's kill landed
        assert!(!status.success());
        for _ in 0..50 {
            if std::fs::read_to_string(&log).unwrap_or_default().contains("started serve --port 7999") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(std::fs::read_to_string(&log).unwrap().contains("started serve --port 7999"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_noops_when_running_and_defaults_to_tailscale() {
        let home = temp("start");
        let c = ctx(&home, &no_cmds, true);
        let running = Server::Process(proc_(10, &["--tailscale"]));
        let steps = plan_start(&c, Some(&running), &[]);
        assert_eq!(steps.len(), 1, "already running: nothing to do");
        assert!(matches!(&steps[0], Step::Note(m) if m.contains("already running")));

        // nothing running, no service installed: detached serve, --tailscale by default
        let steps = plan_start(&c, None, &[]);
        assert!(matches!(&steps[0], Step::Note(_)));
        assert_eq!(steps[1], Step::Serve(serve_argv(&c.exe, &["--tailscale".into()])));
        assert_eq!(steps[2], Step::WaitUp(READY_TIMEOUT));

        // explicit options pass straight through
        let args = vec!["--port".to_string(), "7500".to_string()];
        assert_eq!(plan_start(&c, None, &args)[1], Step::Serve(serve_argv(&c.exe, &args)));

        // no tailscale → plain loopback serve
        let c = ctx(&home, &no_cmds, false);
        assert_eq!(plan_start(&c, None, &[])[1], Step::Serve(serve_argv(&c.exe, &[])));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn start_uses_an_installed_unit_and_ignores_its_own_options() {
        let home = temp("unit");
        let unit = home.join(".config/systemd/user");
        std::fs::create_dir_all(&unit).unwrap();
        std::fs::write(unit.join(SERVICE), "[Unit]\n").unwrap();

        // local user manager reachable
        let local = |a: &[&str]| if a[0] == "systemctl" { Ok(String::new()) } else { Err("no".into()) };
        let c = ctx(&home, &local, true);
        assert_eq!(installed_service(&home, "linux", "501"), Some(Installed::Systemd { unit: unit.join(SERVICE) }));
        let steps = plan_start(&c, None, &[]);
        assert_eq!(steps[1], Step::Run(vec!["systemctl".into(), "--user".into(), "start".into(), SERVICE.into()]));

        // inside distrobox: the container's systemctl is unreachable, the host's is used
        let host = |a: &[&str]| if a[0] == "distrobox-host-exec" { Ok(String::new()) } else { Err("no bus".into()) };
        let c = Ctx { in_container: true, ..ctx(&home, &host, true) };
        assert_eq!(
            plan_start(&c, None, &[])[1],
            Step::Run(vec!["distrobox-host-exec".into(), "systemctl".into(), "--user".into(), "start".into(), SERVICE.into()])
        );
        // options given for a unit we don't control are reported as ignored
        let args = vec!["--tailscale".to_string()];
        assert!(matches!(&plan_start(&c, None, &args)[0], Step::Note(m) if m.contains("ignored")));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn start_uses_an_installed_launchd_agent() {
        let home = temp("launchd");
        let agents = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(agents.join(format!("{LAUNCHD_LABEL}.plist")), "<plist/>").unwrap();
        let c = Ctx { os: "macos", ..ctx(&home, &no_cmds, true) };
        let steps = plan_start(&c, None, &[]);
        match &steps[1] {
            Step::Try(alts) => {
                assert_eq!(alts[0][0], "launchctl");
                assert_eq!(alts[0][1], "bootstrap", "load + start a fresh agent");
                assert_eq!(alts[1][1], "kickstart", "an already-loaded agent just restarts");
            }
            other => panic!("expected launchd steps, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn stop_matches_how_it_runs() {
        assert_eq!(plan_stop(None).len(), 1, "not running: one note, no action");

        let via_host = plan_stop(Some(&Server::Systemd { via_host: true }));
        assert_eq!(
            via_host[1],
            Step::Run(vec!["distrobox-host-exec".into(), "systemctl".into(), "--user".into(), "stop".into(), SERVICE.into()])
        );
        assert_eq!(via_host[2], Step::WaitDown(STOP_TIMEOUT));

        let launchd = plan_stop(Some(&Server::Launchd { domain: "gui/501".into() }));
        assert_eq!(
            launchd[1],
            Step::Run(vec!["launchctl".into(), "bootout".into(), "gui/501/com.vicuna.server".into()])
        );

        let plain = plan_stop(Some(&Server::Process(proc_(10, &[]))));
        assert_eq!(plain[1], Step::Kill(10));
    }

    #[test]
    fn restart_is_detached_and_starts_when_nothing_runs() {
        let home = temp("restart-plan");
        let c = ctx(&home, &no_cmds, true);
        let steps = plan_restart(&c, Some(&Server::Systemd { via_host: false }), &[]);
        assert_eq!(steps[1], Step::Restart(Box::new(Server::Systemd { via_host: false })));
        let steps = plan_restart(&c, None, &[]);
        assert!(matches!(&steps[0], Step::Note(m) if m.contains("not running")));
        assert!(steps.iter().any(|s| matches!(s, Step::Serve(_))), "starts a plain serve when nothing was running");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn status_reports_how_it_runs() {
        let home = temp("status");
        let c = ctx(&home, &no_cmds, true);
        let procs = vec![proc_(4242, &["--tailscale"])];
        let lines = status_lines(&c, Some(&Server::Systemd { via_host: true }), &procs);
        let text = lines.join("\n");
        assert!(text.contains("running") && text.contains("pid: 4242"), "{text}");
        assert!(text.contains("systemd user unit") && text.contains("distrobox-host-exec"), "{text}");
        assert!(text.contains("args: --tailscale"), "{text}");
        assert!(text.contains(&format!("log: distrobox-host-exec journalctl --user -u {SERVICE} -f")), "{text}");

        let lines = status_lines(&c, None, &[]);
        let text = lines.join("\n");
        assert!(text.contains("not running") && text.contains("installed:"), "{text}");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn server_args_come_from_the_process_when_a_unit_hides_them() {
        let procs = vec![proc_(10, &["--tailscale", "--port", "7500"])];
        assert_eq!(serve_args_of_server(Some(&Server::Systemd { via_host: false }), &procs), procs[0].serve_args);
        assert_eq!(serve_args_of_server(Some(&Server::Process(procs[0].clone())), &[]), procs[0].serve_args);
        assert!(serve_args_of_server(None, &[]).is_empty());
    }
}
