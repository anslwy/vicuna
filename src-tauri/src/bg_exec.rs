use serde::Serialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(feature = "desktop")]
use tauri::State;
use tokio::io::AsyncReadExt as _;

use crate::{
    full_disk_scan_error, kill_job, refuse_full_disk_scan, work_canonical_root, work_exec_path,
    AGENT_NONINTERACTIVE_ENV,
};
#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

/// ---------- Background exec jobs (work mode, desktop only) ----------
///
/// Long commands (test suites, builds) run detached: `work_exec_start`
/// returns immediately with a job id (`bg-N`), the model keeps working on
/// other tools, and `work_exec_status` / `work_exec_list` / `work_exec_kill`
/// report back. Output streams are captured incrementally (32KB cap per
/// stream, like foreground exec) so polling mid-run shows progress.

#[derive(Debug, Clone, Serialize)]
pub struct BgJobStatus {
    pub id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BgJobSummary {
    pub id: String,
    pub command: String,
    pub running: bool,
    pub exit_code: Option<i32>,
}

struct BgJobRecord {
    command: String,
    pid: Option<u32>,
    running: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    truncated: bool,
    started_at: u64,
}

#[derive(Default)]
pub struct BgExecState {
    jobs: Mutex<HashMap<String, Arc<Mutex<BgJobRecord>>>>,
    seq: AtomicU64,
}

pub(crate) fn bg_now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Cap for a captured background stream (32KB like foreground exec).
const BG_STREAM_CAP: usize = 32768;

/// Body of a detached job: spawn `sh -c` / `cmd /C` rooted at `canon`,
/// stream both pipes into the shared record, then mark completion.
/// Spawn failures are recorded as an immediately-finished job so polling
/// the id still returns something explainable.
async fn bg_run_job(id: String, canon: std::path::PathBuf, cmd: String, rec: Arc<Mutex<BgJobRecord>>) {
    #[cfg(windows)]
    let mut builder = {
        let mut c = tokio::process::Command::new("powershell");
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", &cmd]);
        c
    };
    #[cfg(not(windows))]
    let mut builder = {
        let mut c = tokio::process::Command::new("sh");
        c.arg("-c").arg(&cmd);
        c
    };
    builder
        .current_dir(&canon)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (k, v) in AGENT_NONINTERACTIVE_ENV {
        builder.env(k, v);
    }
    builder.env("TMPDIR", std::env::temp_dir());
    #[cfg(not(windows))]
    if let Some(path) = work_exec_path().await {
        builder.env("PATH", path);
    }
    #[cfg(windows)]
    builder.creation_flags(CREATE_NO_WINDOW);
    // own process group: a kill reaches the command's children too (killing
    // only `sh` left e.g. `npm test`'s node running and holding the pipes)
    #[cfg(unix)]
    builder.process_group(0);
    let mut child = match builder.spawn() {
        Ok(c) => c,
        Err(e) => {
            if let Ok(mut g) = rec.lock() {
                g.running = false;
                g.stderr = format!("cannot run command: {e}");
            }
            return;
        }
    };
    if let Ok(mut g) = rec.lock() {
        g.pid = child.id();
    }
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let rec_out = rec.clone();
    let out_h = tokio::spawn(async move {
        if let Some(mut pipe) = out_pipe.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if let Ok(mut g) = rec_out.lock() {
                            // sequential field touches only: the guard's
                            // DerefMut can't prove two &mut field borrows
                            // disjoint, so never hold both at once
                            if !g.truncated {
                                let room = BG_STREAM_CAP.saturating_sub(g.stdout.len());
                                if chunk.len() > room {
                                    g.stdout.push_str(&chunk[..chunk.len().min(room)]);
                                    g.stdout.push_str("\n…[truncated]");
                                    g.truncated = true;
                                } else {
                                    g.stdout.push_str(&chunk);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let rec_err = rec.clone();
    let err_h = tokio::spawn(async move {
        if let Some(mut pipe) = err_pipe.take() {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let chunk = String::from_utf8_lossy(&buf[..n]).to_string();
                        if let Ok(mut g) = rec_err.lock() {
                            if !g.truncated {
                                let room = BG_STREAM_CAP.saturating_sub(g.stderr.len());
                                if chunk.len() > room {
                                    g.stderr.push_str(&chunk[..chunk.len().min(room)]);
                                    g.stderr.push_str("\n…[truncated]");
                                    g.truncated = true;
                                } else {
                                    g.stderr.push_str(&chunk);
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });
    let status = child.wait().await.ok();
    // The pid is reaped now and may be reused by any new process: forget it
    // at once, so a later kill can never hit an unrelated process.
    if let Ok(mut g) = rec.lock() {
        g.pid = None;
    }
    // A child the command left behind (`cmd &`, a daemon) inherits the pipes
    // and keeps them open: the job finished when the shell did. Waiting for
    // EOF made a 30 s job report "after 4h 49m" and let Stop "kill" a job that
    // was already done, losing its exit status.
    let _ = tokio::time::timeout(BG_PIPE_DRAIN, async { tokio::join!(out_h, err_h) }).await;
    bg_finish(&rec, status.and_then(|s| s.code()));
    let _ = id;
}

/// How long a finished job's pipes may keep draining before it is reported.
const BG_PIPE_DRAIN: std::time::Duration = std::time::Duration::from_secs(2);

fn bg_finish(rec: &Mutex<BgJobRecord>, exit_code: Option<i32>) {
    if let Ok(mut g) = rec.lock() {
        g.running = false;
        g.pid = None;
        g.exit_code = exit_code;
    }
}

/// Plain (non-Tauri-State) start so unit tests can drive it directly.
pub async fn bg_start(state: &BgExecState, root: String, command: String) -> Result<String, String> {
    let cmd = command.trim();
    if cmd.is_empty() {
        return Err("empty command".to_string());
    }
    if cmd.len() > 8000 {
        return Err("command too long".to_string());
    }
    if let Some(frag) = refuse_full_disk_scan(cmd) {
        return Err(full_disk_scan_error(&frag));
    }
    let canon = work_canonical_root(root.trim())?;
    let n = state.seq.fetch_add(1, Ordering::Relaxed);
    let id = format!("bg-{}", n + 1);
    let rec = Arc::new(Mutex::new(BgJobRecord {
        command: cmd.to_string(),
        pid: None,
        running: true,
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        truncated: false,
        started_at: bg_now_secs(),
    }));
    {
        let mut jobs = state.jobs.lock().map_err(|_| "exec state poisoned".to_string())?;
        jobs.insert(id.clone(), rec.clone());
        // cap memory: keep running jobs plus the newest ~100 finished ones
        if jobs.len() > 120 {
            let mut done: Vec<(String, u64)> = jobs
                .iter()
                .filter_map(|(k, r)| {
                    r.lock()
                        .ok()
                        .filter(|g| !g.running)
                        .map(|g| (k.clone(), g.started_at))
                })
                .collect();
            done.sort_by_key(|(_, t)| *t);
            let drop_n = jobs.len().saturating_sub(120);
            for (k, _) in done.into_iter().take(drop_n) {
                jobs.remove(&k);
            }
        }
    }
    tokio::spawn(bg_run_job(id.clone(), canon, cmd.to_string(), rec));
    Ok(id)
}

pub fn bg_snapshot(state: &BgExecState, id: &str) -> Result<BgJobStatus, String> {
    let rec = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "exec state poisoned".to_string())?;
        jobs
            .get(id.trim())
            .cloned()
            .ok_or_else(|| "unknown background job".to_string())?
    };
    let g = rec.lock().map_err(|_| "exec state poisoned".to_string())?;
    Ok(BgJobStatus {
        id: id.trim().to_string(),
        command: g.command.clone(),
        running: g.running,
        exit_code: g.exit_code,
        stdout: g.stdout.clone(),
        stderr: g.stderr.clone(),
        truncated: g.truncated,
    })
}

pub fn bg_list(state: &BgExecState) -> Result<Vec<BgJobSummary>, String> {
    let jobs = state
        .jobs
        .lock()
        .map_err(|_| "exec state poisoned".to_string())?;
    let mut out: Vec<BgJobSummary> = Vec::new();
    for (id, rec) in jobs.iter() {
        if let Ok(g) = rec.lock() {
            out.push(BgJobSummary {
                id: id.clone(),
                command: g.command.clone(),
                running: g.running,
                exit_code: g.exit_code,
            });
        }
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

pub fn bg_kill(state: &BgExecState, id: &str) -> Result<BgJobStatus, String> {
    let pid = {
        let jobs = state
            .jobs
            .lock()
            .map_err(|_| "exec state poisoned".to_string())?;
        let rec = jobs
            .get(id.trim())
            .cloned()
            .ok_or_else(|| "unknown background job".to_string())?;
        rec.lock()
            .map_err(|_| "exec state poisoned".to_string())?
            .pid
    };
    match pid {
        Some(p) => kill_job(p),
        None => {
            // already finished (or spawn failed): nothing to kill, just report
        }
    }
    bg_snapshot(state, id)
}

/// Kill every still-running job (Quit).
pub fn bg_kill_running(state: &BgExecState) {
    if let Ok(jobs) = state.jobs.lock() {
        for (_, rec) in jobs.iter() {
            if let Ok(g) = rec.lock() {
                if g.running {
                    if let Some(pid) = g.pid {
                        kill_job(pid);
                    }
                }
            }
        }
    }
}

/// Start a detached shell command; returns the job id immediately.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_exec_start(root: String, command: String, state: State<'_, BgExecState>) -> Result<String, String> {
    bg_start(&state, root, command).await
}

/// Poll one background job (full captured output so far + running flag).
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_exec_status(id: String, state: State<'_, BgExecState>) -> Result<BgJobStatus, String> {
    bg_snapshot(&state, &id)
}

/// List background jobs (newest last, no output payload).
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_exec_list(state: State<'_, BgExecState>) -> Result<Vec<BgJobSummary>, String> {
    bg_list(&state)
}

/// Kill a background job (best-effort); returns the latest snapshot.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_exec_kill(id: String, state: State<'_, BgExecState>) -> Result<BgJobStatus, String> {
    bg_kill(&state, &id)
}
