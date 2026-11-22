use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(feature = "desktop")]
use tauri::State;

use crate::{
    bg_exec::bg_now_secs, full_disk_scan_error, kill_pid, refuse_full_disk_scan, trunc_out,
    work_canonical_root, work_display_string,
};
#[cfg(not(windows))]
use crate::work_exec_path;

/// ---------- Persistent interactive agent shell ----------
///
/// Work `exec` no longer spawns a fresh `sh -c` per call. One long-lived
/// shell per workspace keeps cwd, exported vars, `source venv/bin/activate`,
/// `nvm use`, shell functions and aliases alive between calls (the same
/// stateful model Claude Code and Codex use). The shell runs on a PTY so
/// interactive programs work: stdin can be fed with `exec_input`, and
/// `exec_kill` sends Ctrl-C. Windows runs PowerShell — the shell the user
/// actually sees — never `cmd`, which is what made Windows tasks fail when
/// the model wrote PowerShell syntax.
///
/// Completion is detected with a unique prompt marker (PS1 / PowerShell's
/// `prompt` function), so no sentinel command has to be injected into stdin
/// (a sentinel line would be eaten by any program reading stdin).

#[derive(Debug, Clone, Serialize)]
pub struct ShellRun {
    pub output: String,
    pub exit_code: Option<i32>,
    pub running: bool,
    pub truncated: bool,
    /// Working directory after the command (best effort, from the exit probe).
    pub cwd: Option<String>,
    /// Wall-clock duration of the exec call in milliseconds.
    pub duration_ms: Option<u64>,
}

struct AgentShell {
    pid: Option<u32>,
    /// Kept alive so the pty (and therefore the shell) stays open. Wrapped in
    /// a Mutex because `MasterPty` is Send but not Sync.
    _master: Mutex<Box<dyn portable_pty::MasterPty + Send>>,
    writer: Mutex<Box<dyn Write + Send>>,
    buf: Arc<Mutex<String>>,
    marker: String,
    /// Exit-probe marker, also built from an env var so a re-enabled terminal
    /// echo of the probe line can never be mistaken for its output.
    exit_marker: String,
    alive: Arc<AtomicBool>,
    busy: AtomicBool,
    /// Epoch seconds when the current command started (0 while idle). Used to
    /// tell the model how long a blocked shell has been stuck.
    busy_since: AtomicU64,
    last_used: AtomicU64,
}

impl AgentShell {
    fn touch(&self) {
        self.last_used.store(bg_now_secs(), Ordering::Relaxed);
    }

    fn mark_busy(&self) {
        self.busy.store(true, Ordering::Relaxed);
        self.busy_since.store(bg_now_secs(), Ordering::Relaxed);
    }

    fn mark_idle(&self) {
        self.busy.store(false, Ordering::Relaxed);
        self.busy_since.store(0, Ordering::Relaxed);
    }

    fn busy_secs(&self) -> u64 {
        match self.busy_since.load(Ordering::Relaxed) {
            0 => 0,
            start => bg_now_secs().saturating_sub(start),
        }
    }

    fn write_raw(&self, data: &str) -> Result<(), String> {
        let mut w = self
            .writer
            .lock()
            .map_err(|_| "agent shell state poisoned".to_string())?;
        w.write_all(data.as_bytes())
            .map_err(|e| format!("agent shell write failed: {e}"))?;
        w.flush()
            .map_err(|e| format!("agent shell write failed: {e}"))
    }

    fn write_line(&self, line: &str) -> Result<(), String> {
        self.write_raw(&format!("{line}\n"))
    }

    fn take_buf(&self) -> String {
        self.buf
            .lock()
            .map(|mut g| std::mem::take(&mut *g))
            .unwrap_or_default()
    }

    fn drain(&self) {
        let _ = self.take_buf();
    }
}

/// Take buffered output and, when the prompt marker is present, clear the
/// busy flag (the command finished). Returns (text before the marker, done).
fn take_output(shell: &AgentShell) -> (String, bool) {
    let raw = shell.take_buf();
    if let Some(idx) = raw.find(&shell.marker) {
        shell.mark_idle();
        (raw[..idx].to_string(), true)
    } else {
        (raw, false)
    }
}

#[derive(Default)]
pub struct AgentShellState {
    shells: Mutex<HashMap<String, Arc<AgentShell>>>,
}

impl AgentShellState {
    fn all(&self) -> Vec<Arc<AgentShell>> {
        self.shells
            .lock()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }
}

/// Kill every agent shell (Quit path).
pub fn kill_agent_shells(state: &AgentShellState) {
    let shells = state.all();
    if let Ok(mut m) = state.shells.lock() {
        m.clear();
    }
    for s in shells {
        if let Some(pid) = s.pid {
            kill_pid(pid);
        }
    }
}

fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// Strip ANSI escape sequences (colours, cursor moves) and carriage returns
/// from PTY output so the model sees plain text.
fn strip_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() && !(0x40..=0x7e).contains(&bytes[i]) {
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else if i < bytes.len() {
                i += 1;
            }
        } else if bytes[i] == b'\r' {
            i += 1;
        } else {
            let end = (i + utf8_char_len(bytes[i])).min(bytes.len());
            out.push_str(&input[i..end]);
            i = end;
        }
    }
    out
}

/// The PTY echoes what we type (Windows always; Unix is silenced with
/// `stty -echo`, but be safe). Drop a leading copy of the command.
fn strip_leading_echo(out: &str, command: &str) -> String {
    let trimmed = out.trim_start_matches([' ', '\t', '\n']);
    let cmd = command.trim_end();
    if !cmd.is_empty() && trimmed.starts_with(cmd) {
        return trimmed[cmd.len()..].trim_start_matches('\n').to_string();
    }
    out.to_string()
}

fn clean_shell_output(raw: &str, command: Option<&str>) -> (String, bool) {
    let plain = strip_ansi(raw);
    let stripped = match command {
        Some(c) => strip_leading_echo(&plain, c),
        None => plain,
    };
    trunc_out(stripped.trim_end_matches('\n'))
}

/// Environment that keeps agent-run commands non-interactive. The persistent
/// shell runs on a real PTY, so `git diff`, `git log`, `man`, etc. would
/// otherwise launch a pager, take over the TTY and swallow every subsequent
/// command as keystrokes. Disabling pagers/editors up front is the only
/// reliable fix; a stuck pager cannot be trusted to exit on Ctrl-C.
pub(crate) const AGENT_NONINTERACTIVE_ENV: &[(&str, &str)] = &[
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("MANPAGER", "cat"),
    ("SYSTEMD_PAGER", "cat"),
    ("LESS", "FRX"),
    ("GIT_TERMINAL_PROMPT", "0"),
    ("GIT_EDITOR", "true"),
    ("GIT_SEQUENCE_EDITOR", "true"),
    ("GIT_MERGE_AUTOEDIT", "no"),
];

async fn spawn_agent_shell(root: &str) -> Result<AgentShell, String> {
    let canon = work_canonical_root(root.trim())?;
    let cwd = work_display_string(&canon);
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(portable_pty::PtySize {
            // Wide, tall pty: less line wrapping/scrollback churn in output.
            rows: 40,
            cols: 240,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("cannot open agent shell pty: {e}"))?;
    #[cfg(windows)]
    let mut cmd = {
        let mut c = portable_pty::CommandBuilder::new("powershell.exe");
        c.args(["-NoLogo", "-NoProfile", "-NoExit"]);
        c
    };
    #[cfg(not(windows))]
    let is_fish = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        shell
            .rsplit('/')
            .next()
            .map(|name| name.starts_with("fish"))
            .unwrap_or(false)
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let mut c = portable_pty::CommandBuilder::new(shell);
        c.args(["-i"]);
        if let Some(path) = work_exec_path().await {
            c.env("PATH", path);
        }
        c
    };
    cmd.cwd(&cwd);
    // The marker is built from this env var, never written literally: the
    // PTY echoes our init line, and a literal marker in that echo would be
    // mistaken for the shell becoming ready.
    let token = format!(
        "{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    );
    cmd.env("VICUNA_READY_TOKEN", &token);
    // Same anti-echo trick for the exit probe: the typed line only ever
    // contains the env-var name, while the real output expands to the marker.
    let exit_token = format!("{token}x");
    cmd.env("VICUNA_EXIT_TOKEN", &exit_token);
    // Point the agent shell at the same temp root the file tools accept, so
    // `mktemp -d "$TMPDIR/..."` always yields a writable scratch path.
    cmd.env("TMPDIR", std::env::temp_dir());
    cmd.env("TERM", "dumb");
    cmd.env("NO_COLOR", "1");
    cmd.env("CLICOLOR", "0");
    for (k, v) in AGENT_NONINTERACTIVE_ENV {
        cmd.env(k, v);
    }
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("cannot spawn agent shell: {e}"))?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("agent shell pty failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("agent shell pty failed: {e}"))?;
    let buf = Arc::new(Mutex::new(String::new()));
    let alive = Arc::new(AtomicBool::new(true));
    let b = buf.clone();
    let a = alive.clone();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut g) = b.lock() {
                        g.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    }
                }
                Err(_) => break,
            }
        }
        a.store(false, Ordering::Relaxed);
    });
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    let marker = format!("__VICUNA_READY_{token}__");
    let exit_marker = format!("__VICUNA_EXIT_{exit_token}__");
    let shell = AgentShell {
        pid,
        _master: Mutex::new(pair.master),
        writer: Mutex::new(writer),
        buf,
        marker: marker.clone(),
        exit_marker,
        alive,
        busy: AtomicBool::new(false),
        busy_since: AtomicU64::new(0),
        last_used: AtomicU64::new(bg_now_secs()),
    };
    #[cfg(windows)]
    let init = "function prompt { \"__VICUNA_READY_$($env:VICUNA_READY_TOKEN)__\" }\n$OutputEncoding = [Console]::OutputEncoding = [Text.Encoding]::UTF8\n"
        .to_string();
    #[cfg(not(windows))]
    let init = if is_fish {
        "stty -echo 2>/dev/null; function fish_prompt; printf '%s' \"__VICUNA_READY_${VICUNA_READY_TOKEN}__\"; end; function fish_right_prompt; end\n"
            .to_string()
    } else {
        "stty -echo 2>/dev/null; PS1=\"__VICUNA_READY_${VICUNA_READY_TOKEN}__\"; PS2=''; PROMPT=$PS1; RPROMPT=''; PROMPT_COMMAND=''; export PS1 PROMPT\n"
            .to_string()
    };
    shell.write_raw(&init)?;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    loop {
        let got = shell.take_buf();
        if got.contains(&marker) {
            break;
        }
        if !shell.alive.load(Ordering::Relaxed) {
            return Err("agent shell exited during startup".to_string());
        }
        if std::time::Instant::now() >= deadline {
            let _ = shell.write_raw("\u{3}");
            return Err("agent shell did not become ready (timed out)".to_string());
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    shell.drain();
    Ok(shell)
}

/// Get the workspace's shell, respawning it if it died.
async fn shell_for(state: &AgentShellState, root: &str, key: &str) -> Result<Arc<AgentShell>, String> {
    let key = key.to_string();
    if let Some(existing) = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
    {
        if existing.alive.load(Ordering::Relaxed) {
            existing.touch();
            return Ok(existing);
        }
    }
    let fresh = Arc::new(spawn_agent_shell(root).await?);
    let mut map = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?;
    // another call may have created one while we were spawning; keep theirs
    if let Some(existing) = map.get(&key) {
        if existing.alive.load(Ordering::Relaxed) {
            if let Some(pid) = fresh.pid {
                kill_pid(pid);
            }
            existing.touch();
            return Ok(existing.clone());
        }
    }
    // cap live shells (one per workspace): evict the least recently used
    if map.len() >= 8 {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, s)| s.last_used.load(Ordering::Relaxed))
            .map(|(k, _)| k.clone())
        {
            if let Some(victim) = map.remove(&oldest) {
                if let Some(pid) = victim.pid {
                    kill_pid(pid);
                }
            }
        }
    }
    map.insert(key, fresh.clone());
    Ok(fresh)
}

enum ShellWait {
    Done(String),
    TimedOut(String),
    Dead(String),
}

async fn wait_for_marker(shell: &AgentShell, deadline: std::time::Instant) -> ShellWait {
    let mut acc = String::new();
    loop {
        acc.push_str(&shell.take_buf());
        if let Some(idx) = acc.find(&shell.marker) {
            shell.mark_idle();
            return ShellWait::Done(acc[..idx].to_string());
        }
        if !shell.alive.load(Ordering::Relaxed) {
            shell.mark_idle();
            return ShellWait::Dead(acc);
        }
        if std::time::Instant::now() >= deadline {
            return ShellWait::TimedOut(acc);
        }
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

/// Read `$?` and the cwd right after a command finished (before any other
/// input). Returns (exit code, cwd).
async fn probe_exit_status(shell: &AgentShell) -> (Option<i32>, Option<String>) {
    if !shell.alive.load(Ordering::Relaxed) {
        return (None, None);
    }
    shell.drain();
    #[cfg(windows)]
    let probe = "Write-Output ('__VICUNA_EXIT_' + $env:VICUNA_EXIT_TOKEN + '__' + $(if ($?) {0} else {1}) + '|' + (Get-Location).Path)";
    #[cfg(not(windows))]
    let probe = "printf '__VICUNA_EXIT_%s__%s|%s\\n' \"$VICUNA_EXIT_TOKEN\" \"$?\" \"$PWD\"";
    if shell.write_line(probe).is_err() {
        return (None, None);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    match wait_for_marker(shell, deadline).await {
        ShellWait::Done(raw) => {
            let plain = strip_ansi(&raw);
            // Last occurrence: a re-enabled echo of the probe line contains the
            // literal prefix but not the expanded token.
            if !plain.contains(&shell.exit_marker) {
                return (None, None);
            }
            let tail = plain.rsplit(&shell.exit_marker).next().unwrap_or("").trim();
            let mut parts = tail.splitn(2, '|');
            let code = parts.next().and_then(|s| s.trim().parse::<i32>().ok());
            let cwd = parts
                .next()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
            (code, cwd)
        }
        _ => (None, None),
    }
}

/// Commands are typed into an interactive PTY. A multi-line payload (heredoc,
/// pasted script) is re-parsed line by line and mangled, so route it through
/// the `write` tool instead of pretending it ran.
fn reject_multiline_command(cmd: &str) -> Option<String> {
    if cmd.contains('\n') || cmd.contains('\r') {
        Some(
            "exec takes one command line — this one contains a newline or heredoc, which the \
             interactive shell corrupts. Put the content in a file with the write tool (or \
             write a script with write), then run it, e.g. exec {\"command\": \"sh script.sh\"}."
                .to_string(),
        )
    } else {
        None
    }
}

fn format_elapsed_secs(secs: u64) -> String {
    if secs >= 3600 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

/// Best-effort name of the pty's foreground process (Unix). macOS/Linux `ps`
/// exposes the terminal foreground pgid via `tpgid`.
#[cfg(not(windows))]
fn foreground_process_name(shell_pid: Option<u32>) -> Option<String> {
    let pid = shell_pid?;
    let out = std::process::Command::new("ps")
        .args(["-o", "tpgid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let pgid = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok()?;
    if pgid == 0 {
        return None;
    }
    let out = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pgid.to_string()])
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

#[cfg(windows)]
fn foreground_process_name(_shell_pid: Option<u32>) -> Option<String> {
    None
}

fn shell_busy_error(shell: &AgentShell) -> String {
    let mut msg = String::from("the shell is still running a command");
    let secs = shell.busy_secs();
    if secs > 0 {
        msg.push_str(&format!(" (busy {})", format_elapsed_secs(secs)));
    }
    if let Some(fg) = foreground_process_name(shell.pid) {
        let base = fg.rsplit(['/', '\\']).next().unwrap_or(&fg).to_lowercase();
        let hint = match base.as_str() {
            "less" | "more" | "most" | "man" | "vi" | "vim" | "nvim" | "top" | "htop" => {
                " — a pager/TUI owns the terminal: quit it with exec_input {\"text\": \"q\"}"
            }
            _ => "",
        };
        msg.push_str(&format!("; foreground={fg}{hint}"));
    }
    msg.push_str(
        ". Poll with exec_status, answer a prompt with exec_input, or interrupt with exec_kill first.",
    );
    msg
}

/// Run a command in the workspace's persistent shell.
pub async fn agent_shell_exec(
    state: &AgentShellState,
    root: String,
    key: String,
    command: String,
    timeout_ms: Option<u64>,
) -> Result<ShellRun, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    if cmd.len() > 8000 {
        return Err("command too long".to_string());
    }
    if let Some(err) = reject_multiline_command(cmd) {
        return Err(err);
    }
    if let Some(frag) = refuse_full_disk_scan(cmd) {
        return Err(full_disk_scan_error(&frag));
    }
    let shell = shell_for(state, &root, &key).await?;
    let (_pending, finished) = take_output(&shell);
    if shell.busy.load(Ordering::Relaxed) && !finished {
        return Err(shell_busy_error(&shell));
    }
    shell.mark_busy();
    let started = std::time::Instant::now();
    shell.write_line(cmd)?;
    let timeout = Duration::from_millis(timeout_ms.unwrap_or(30000).clamp(1000, 120000));
    match wait_for_marker(&shell, std::time::Instant::now() + timeout).await {
        ShellWait::Done(raw) => {
            shell.mark_idle();
            shell.touch();
            let (exit_code, cwd) = probe_exit_status(&shell).await;
            let (output, truncated) = clean_shell_output(&raw, Some(cmd));
            Ok(ShellRun {
                output,
                exit_code,
                running: false,
                truncated,
                cwd,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            })
        }
        ShellWait::TimedOut(raw) => {
            // Leave the command running: the model may answer a prompt with
            // exec_input, poll with exec_status, or interrupt with exec_kill.
            let (output, truncated) = clean_shell_output(&raw, Some(cmd));
            Ok(ShellRun {
                output,
                exit_code: None,
                running: true,
                truncated,
                cwd: None,
                duration_ms: Some(started.elapsed().as_millis() as u64),
            })
        }
        ShellWait::Dead(raw) => {
            shell.mark_idle();
            let (output, _) = clean_shell_output(&raw, Some(cmd));
            Err(format!("agent shell exited unexpectedly.\n{output}"))
        }
    }
}

/// Poll the workspace shell (or report it idle when no command is running).
pub async fn agent_shell_status(
    state: &AgentShellState,
    _root: String,
    key: String,
) -> Result<ShellRun, String> {
    let shell = match state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
    {
        Some(s) if s.alive.load(Ordering::Relaxed) => s,
        _ => {
            return Ok(ShellRun {
                output: "Shell idle (no command running).".to_string(),
                exit_code: None,
                running: false,
                truncated: false,
                cwd: None,
                duration_ms: None,
            })
        }
    };
    shell.touch();
    if !shell.busy.load(Ordering::Relaxed) {
        let (out, truncated) = clean_shell_output(&shell.take_buf(), None);
        return Ok(ShellRun {
            output: out,
            exit_code: None,
            running: false,
            truncated,
            cwd: None,
            duration_ms: None,
        });
    }
    let deadline = std::time::Instant::now() + Duration::from_millis(250);
    match wait_for_marker(&shell, deadline).await {
        ShellWait::Done(raw) => {
            shell.mark_idle();
            let (exit_code, cwd) = probe_exit_status(&shell).await;
            let (output, truncated) = clean_shell_output(&raw, None);
            Ok(ShellRun {
                output,
                exit_code,
                running: false,
                truncated,
                cwd,
                duration_ms: None,
            })
        }
        ShellWait::TimedOut(raw) | ShellWait::Dead(raw) => {
            let (output, truncated) = clean_shell_output(&raw, None);
            Ok(ShellRun {
                output,
                exit_code: None,
                running: true,
                truncated,
                cwd: None,
                duration_ms: None,
            })
        }
    }
}

/// Feed raw input to the running command (answers prompts, REPLs, …).
pub async fn agent_shell_input(
    state: &AgentShellState,
    _root: String,
    key: String,
    text: String,
) -> Result<String, String> {
    let shell = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
        .ok_or_else(|| "no shell is running for this workspace yet — run exec first".to_string())?;
    shell.touch();
    shell.write_line(&text)?;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (raw, finished) = take_output(&shell);
    let (out, _) = clean_shell_output(&raw, None);
    let mut parts = Vec::new();
    if !out.trim().is_empty() {
        parts.push(out);
    }
    if finished {
        parts.push("[prompt ready]".to_string());
    }
    Ok(if parts.is_empty() { "(input sent)".to_string() } else { parts.join("\n") })
}

/// Ctrl-C the running command and wait for the prompt to come back.
pub async fn agent_shell_interrupt(
    state: &AgentShellState,
    _root: String,
    key: String,
) -> Result<ShellRun, String> {
    let shell = state
        .shells
        .lock()
        .map_err(|_| "agent shell state poisoned".to_string())?
        .get(&key)
        .cloned()
        .ok_or_else(|| "no shell is running for this workspace".to_string())?;
    shell.touch();
    if !shell.busy.load(Ordering::Relaxed) {
        return Ok(ShellRun {
            output: "Nothing to interrupt — the shell is idle.".to_string(),
            exit_code: None,
            running: false,
            truncated: false,
            cwd: None,
            duration_ms: None,
        });
    }
    // Normal foreground jobs die on Ctrl-C. Full-screen pagers do not: less
    // treats it as "interrupt" and may even open its help screen, so after a
    // failed interrupt try quitting a pager explicitly before giving up.
    shell.write_raw("\u{3}")?;
    let mut acc = String::new();
    match wait_for_marker(&shell, std::time::Instant::now() + Duration::from_millis(1500)).await {
        ShellWait::Done(raw) | ShellWait::Dead(raw) => {
            acc.push_str(&raw);
            let (output, truncated) = clean_shell_output(&acc, None);
            return Ok(ShellRun {
                output,
                exit_code: None,
                running: false,
                truncated,
                cwd: None,
                duration_ms: None,
            });
        }
        ShellWait::TimedOut(raw) => acc.push_str(&raw),
    }
    for _ in 0..3 {
        shell.write_raw("q")?;
        match wait_for_marker(&shell, std::time::Instant::now() + Duration::from_millis(800)).await {
            ShellWait::Done(raw) | ShellWait::Dead(raw) => {
                acc.push_str(&raw);
                let (output, truncated) = clean_shell_output(&acc, None);
                return Ok(ShellRun {
                    output,
                    exit_code: None,
                    running: false,
                    truncated,
                    cwd: None,
                    duration_ms: None,
                });
            }
            ShellWait::TimedOut(raw) => acc.push_str(&raw),
        }
    }
    // Still owned by a TUI: report the truth so the next exec refuses instead
    // of typing a command into it.
    let (mut output, truncated) = clean_shell_output(&acc, None);
    output.push_str("\n[interrupt did not return the prompt — a pager/TUI may still be open; try exec_input {\"text\": \"q\"}]");
    Ok(ShellRun {
        output,
        exit_code: None,
        running: true,
        truncated,
        cwd: None,
        duration_ms: None,
    })
}

/// Persistent-shell command wrappers (same IPC naming pattern as the rest).
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_shell_exec(
    root: String,
    key: Option<String>,
    command: String,
    timeout_ms: Option<u64>,
    state: State<'_, AgentShellState>,
) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_exec(&state, root, key, command, timeout_ms).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_shell_status(root: String, key: Option<String>, state: State<'_, AgentShellState>) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_status(&state, root, key).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_shell_input(
    root: String,
    key: Option<String>,
    text: String,
    state: State<'_, AgentShellState>,
) -> Result<String, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_input(&state, root, key, text).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_shell_interrupt(root: String, key: Option<String>, state: State<'_, AgentShellState>) -> Result<ShellRun, String> {
    let key = key.unwrap_or_else(|| root.clone());
    agent_shell_interrupt(&state, root, key).await
}
