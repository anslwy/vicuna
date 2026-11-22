use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
#[cfg(feature = "desktop")]
use tauri::State;

use crate::cli_launcher::default_unix_shell;
use crate::ipc::Channel;
use crate::work_fs::{work_canonical_root, work_display_string};
#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

/// ---------- Interactive terminals (side-panel tabs, desktop only) ----------
///
/// Each terminal tab owns a portable-pty pair rooted at the workspace: a
/// reader thread streams output over the Channel, a waiter thread reports
/// the exit code. Master/writer stay in the map for write/resize; kills go
/// by pid so a stuck child is always reaped (best-effort, never fatal).

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum TermEvent {
    Output { text: String },
    Exit { code: Option<i32> },
}

pub(crate) struct TermHandle {
    master: Box<dyn portable_pty::MasterPty + Send>,
    pub(crate) writer: Mutex<Box<dyn Write + Send>>,
    pub(crate) pid: Option<u32>,
}

#[derive(Default)]
pub struct TermState {
    pub(crate) terms: Mutex<HashMap<String, TermHandle>>,
    seq: AtomicU64,
}

static TERM_SEQ: AtomicU64 = AtomicU64::new(1);

fn term_size(cols: Option<u16>, rows: Option<u16>) -> portable_pty::PtySize {
    portable_pty::PtySize {
        rows: rows.unwrap_or(24).clamp(5, 200),
        cols: cols.unwrap_or(80).clamp(20, 400),
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Kill by pid (unix kill -9 / windows taskkill). Best-effort.
/// Kill a background job: its whole process group on Unix (bg jobs lead
/// their own group), else just the pid.
pub(crate) fn kill_job(pid: u32) {
    #[cfg(unix)]
    {
        let _ = Command::new("kill")
            .args(["-9", "--"])
            .arg(format!("-{pid}"))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .output();
    }
    kill_pid(pid);
}

/// Kill every terminal pty in `state` (Quit; the embedded server's terminals).
pub fn term_kill_all(state: &TermState) {
    if let Ok(mut terms) = state.terms.lock() {
        for (_, h) in terms.drain() {
            if let Some(pid) = h.pid {
                kill_pid(pid);
            }
        }
    }
}

pub(crate) fn kill_pid(pid: u32) {
    #[cfg(not(windows))]
    let _ = Command::new("kill")
        .arg("-9")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output();
}

/// Spawn a login shell on a fresh pty rooted at `root`. The reader thread
/// streams output until EOF; the waiter thread reports the exit code.
/// Returns the handle (master + writer + pid for write/resize/kill).
pub(crate) fn spawn_pty<F, G>(
    root: &str,
    cols: Option<u16>,
    rows: Option<u16>,
    on_output: F,
    on_exit: G,
) -> Result<TermHandle, String>
where
    F: Fn(String) + Send + 'static,
    G: FnOnce(Option<i32>) + Send + 'static,
{
    let canon = work_canonical_root(root.trim())?;
    // `canonicalize()` returns extended-length paths on Windows
    // (`\\?\C:\...`); spawning the shell there makes PowerShell render its
    // prompt as `Microsoft.PowerShell.Core\FileSystem::\\?\C:\...`. The
    // stripped path points at the same folder and prompts cleanly.
    let cwd = work_display_string(&canon);
    let pty = portable_pty::native_pty_system();
    let pair = pty
        .openpty(term_size(cols, rows))
        .map_err(|e| format!("cannot open pty: {e}"))?;
    let mut cmd = {
        #[cfg(windows)]
        {
            // PowerShell from the start: nesting it under cmd garbles
            // PSReadLine styling. The Work agent has its own persistent
            // PowerShell pty (AgentShellState), separate from these tabs.
            let mut c = portable_pty::CommandBuilder::new("powershell.exe");
            c.args(["-NoLogo"]);
            c
        }
        #[cfg(not(windows))]
        {
            let shell = default_unix_shell();
            let mut c = portable_pty::CommandBuilder::new(shell);
            c.args(["-l"]);
            c
        }
    };
    cmd.cwd(&cwd);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("cannot spawn shell: {e}"))?;
    drop(pair.slave);
    let pid = child.process_id();
    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("pty failed: {e}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("pty failed: {e}"))?;
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    on_output(String::from_utf8_lossy(&buf[..n]).to_string());
                }
                Err(_) => break,
            }
        }
    });
    std::thread::spawn(move || {
        // signaled deaths (e.g. our kill -9) carry no status code
        let code = child
            .wait()
            .ok()
            .and_then(|s| {
                if s.signal().is_some() {
                    None
                } else {
                    Some(s.exit_code() as i32)
                }
            });
        on_exit(code);
    });
    Ok(TermHandle {
        master: pair.master,
        writer: Mutex::new(writer),
        pid,
    })
}

/// Spawn a login shell rooted at the workspace. Output/exit stream back
/// over the channel; returns the terminal id for write/resize/kill. Shared by
/// the Tauri command and the headless bridge (remote-server terminals).
pub fn term_spawn_logic(
    state: &TermState,
    root: &str,
    cols: Option<u16>,
    rows: Option<u16>,
    on_event: Channel<TermEvent>,
) -> Result<String, String> {
    let canon = work_canonical_root(root.trim())?;
    let id = format!("term-{}", TERM_SEQ.fetch_add(1, Ordering::Relaxed));
    let ev = on_event.clone();
    let ev2 = on_event;
    let handle = spawn_pty(
        &canon.to_string_lossy(),
        cols,
        rows,
        move |text| {
            if ev.send(TermEvent::Output { text }).is_err() {
                // frontend went away; the reader loop keeps draining the
                // pty until the shell exits (killed with the tab)
            }
        },
        move |code| {
            let _ = ev2.send(TermEvent::Exit { code });
        },
    )?;
    state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?
        .insert(id.clone(), handle);
    Ok(id)
}

/// Send keystrokes to a terminal.
pub fn term_write_logic(state: &TermState, id: &str, data: &str) -> Result<(), String> {
    let terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .get(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    let mut w = h
        .writer
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    w.write_all(data.as_bytes())
        .map_err(|e| format!("terminal write failed: {e}"))?;
    w.flush()
        .map_err(|e| format!("terminal write failed: {e}"))?;
    Ok(())
}

/// Resize a terminal (columns × rows from the xterm fit addon).
pub fn term_resize_logic(state: &TermState, id: &str, cols: u16, rows: u16) -> Result<(), String> {
    let terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .get(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    h.master
        .resize(term_size(Some(cols), Some(rows)))
        .map_err(|e| format!("terminal resize failed: {e}"))?;
    Ok(())
}

/// Kill a terminal's shell (by pid, best-effort) and drop the entry.
pub fn term_kill_logic(state: &TermState, id: &str) -> Result<(), String> {
    let mut terms = state
        .terms
        .lock()
        .map_err(|_| "terminal state poisoned".to_string())?;
    let h = terms
        .remove(id.trim())
        .ok_or_else(|| "terminal closed".to_string())?;
    drop(terms);
    if let Some(pid) = h.pid {
        kill_pid(pid);
    }
    Ok(())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn term_spawn(
    root: String,
    cols: Option<u16>,
    rows: Option<u16>,
    state: State<'_, TermState>,
    on_event: Channel<TermEvent>,
) -> Result<String, String> {
    term_spawn_logic(&state, &root, cols, rows, on_event)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn term_write(id: String, data: String, state: State<'_, TermState>) -> Result<(), String> {
    term_write_logic(&state, &id, &data)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn term_resize(
    id: String,
    cols: u16,
    rows: u16,
    state: State<'_, TermState>,
) -> Result<(), String> {
    term_resize_logic(&state, &id, cols, rows)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn term_kill(id: String, state: State<'_, TermState>) -> Result<(), String> {
    term_kill_logic(&state, &id)
}
