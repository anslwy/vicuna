//! Usage and quota reporting for Work harnesses (/usage).
//!
//! Each Work engine exposes usage differently:
//! - Claude Code (`claude`): headless `claude -p "/usage"`, which prints the
//!   same numbers the TUI's Usage tab draws. Falls back to driving the
//!   interactive `/usage` view through `portable-pty` (older CLIs).
//! - Antigravity (`agy`): headless `agy -p /usage` (one TSV row per meter).
//!   Falls back to the interactive `/usage` view via `portable-pty`.
//!
//! Which route goes first depends on the platform (`HEADLESS_FIRST`): on
//! Windows the interactive route is unreliable (see that constant), elsewhere
//! the interactive view works and is the richer of the two.
//! - Codex (`codex`): `codex app-server` JSON-RPC `account/rateLimits/read`.
//! - OpenCode / `pi` / `fx`: no subscription quota; the popup shows no tab for them
//!   and the command returns an informational note if asked anyway.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::work_engine::{effective_engine, Engine};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageMeter {
    pub label: String,
    /// 0.0..=100.0
    pub percent: f64,
    /// "remaining" or "used"
    pub kind: String,
    pub value_text: String,
    pub reset_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageGroup {
    pub title: String,
    pub subtitle: Option<String>,
    pub meters: Vec<UsageMeter>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct UsageStat {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WorkUsageReport {
    pub engine: String,
    pub engine_label: String,
    pub account: Option<String>,
    pub plan: Option<String>,
    pub groups: Vec<UsageGroup>,
    pub stats: Vec<UsageStat>,
    pub note: Option<String>,
    pub raw: Option<String>,
    pub error: Option<String>,
}

impl WorkUsageReport {
    fn base(engine: Engine) -> Self {
        Self {
            engine: engine.id().to_string(),
            engine_label: engine.label().to_string(),
            account: None,
            plan: None,
            groups: Vec::new(),
            stats: Vec::new(),
            note: None,
            raw: None,
            error: None,
        }
    }
}

/// Drive an interactive CLI (`/usage`) inside a tall PTY so the full view fits without paging.
///
/// `usage_view_markers` names the strings that mean "the usage view is on
/// screen"; it is used only to decide whether to retype `/usage` when the TUI
/// swallowed it. Empty disables that retry (a CLI that loads slowly would
/// otherwise get stray keystrokes typed into its prompt while it works).
fn run_pty_slash_usage(
    bin: &Path,
    ready_markers: &[&str],
    done_markers: &[&str],
    usage_view_markers: &[&str],
    settle_after_done: Duration,
    total_timeout: Duration,
) -> Result<PtyCapture, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: PTY_ROWS,
            cols: PTY_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("failed to open PTY: {e}"))?;

    let mut cmd = CommandBuilder::new(bin);
    cmd.env("TERM", "xterm-256color");
    cmd.env("NO_COLOR", "1");
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    if let Some(home) = crate::opencode::home_dir() {
        cmd.cwd(home);
    } else if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("failed to start {}: {e}", bin.display()))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| format!("failed to read PTY: {e}"))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|e| format!("failed to write PTY: {e}"))?;

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            if tx.send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    let start = Instant::now();
    // Render what the CLI draws instead of concatenating the byte stream:
    // terminals repaint only the cells that changed (always on Windows'
    // ConPTY, and for Ink's partial redraws everywhere), so the stream read
    // as text dropped letters ("Current ses ion", losing that meter) and kept
    // stale values ("0% used" after a later repaint to 12%).
    let mut screen = vt100::Parser::new(PTY_ROWS, PTY_COLS, 0);
    // every distinct rendered frame, oldest first: some facts show only on a
    // passing frame (agy's "(Google AI Pro)" on its loading view), while the
    // parsers read the meters from the last frame
    let mut frames = String::new();
    let mut last_frame = String::new();
    // The last frame that showed the meters, and the last frame that drew
    // anything at all. Windows ConPTY can leave the final screen blank (the
    // CLI exits, or restores the primary buffer of the alternate screen),
    // which used to parse as "no usage data" even though the meters had been
    // on screen moments earlier.
    let mut last_usage = String::new();
    let mut last_drawn = String::new();
    let mut sent = false;
    let mut done_at: Option<Instant> = None;
    let mut trust_tries = 0u32;
    let mut trust_last: Option<Instant> = None;
    let mut usage_resends = 0u32;
    let mut usage_sent_at: Option<Instant> = None;

    while start.elapsed() < total_timeout {
        match rx.recv_timeout(Duration::from_millis(120)) {
            Ok(chunk) => screen.process(&chunk),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        let cleaned = screen.screen().contents();
        if cleaned != last_frame {
            if frames.len() < FRAMES_CAP {
                frames.push_str(&cleaned);
                frames.push('\n');
            }
            last_frame = cleaned.clone();
        }
        if !cleaned.trim().is_empty() {
            last_drawn = cleaned.clone();
            if done_markers.iter().any(|m| cleaned.contains(m)) {
                last_usage = cleaned.clone();
            }
        }

        // Claude Code's folder-trust dialog ("Quick safety check: Is this a
        // project you created or one you trust?"). The default row is
        // "No, exit", so a bare Enter quits the CLI; pick "Yes, I trust this
        // folder" (one row down) instead. Repeat, throttled: keys typed
        // before the TUI's input reader is up are dropped (verified on
        // Windows ConPTY), and the prompt redraws over several frames.
        if !sent && (cleaned.contains("Quick safety check") || cleaned.contains("Do you trust")) {
            let yes_selected = cleaned
                .lines()
                .any(|l| l.contains("Yes, I trust this folder") && l.contains('❯'));
            if trust_tries < 8 && trust_last.map_or(true, |t| t.elapsed() >= Duration::from_millis(800)) {
                let keys: &[u8] = if yes_selected { b"\r" } else { b"\x1b[B\r" };
                let _ = writer.write_all(keys);
                let _ = writer.flush();
                trust_last = Some(Instant::now());
                trust_tries += 1;
            }
            continue;
        }

        if !sent {
            let after_trust = cleaned.rsplit("enter Confirm").next().unwrap_or(&cleaned);
            let ready = ready_markers.iter().any(|m| after_trust.contains(m))
                || start.elapsed() >= Duration::from_millis(3500);
            if ready {
                let _ = writer.write_all(b"/usage\r");
                let _ = writer.flush();
                sent = true;
                usage_sent_at = Some(Instant::now());
            }
        } else {
            // the screen holds only the current view, so no earlier frame can
            // match (and the view's own "/usage-credits" hint must not hide it)
            if done_at.is_none() && done_markers.iter().any(|m| cleaned.contains(m)) {
                done_at = Some(Instant::now());
            }
            // the same dropped-keystroke window can swallow "/usage" itself:
            // retry while the TUI shows no usage view at all
            if done_at.is_none()
                && !usage_view_markers.is_empty()
                && usage_resends < 3
                && usage_sent_at.map_or(false, |t| t.elapsed() >= Duration::from_millis(1500))
                && !usage_view_markers.iter().any(|m| cleaned.contains(m))
            {
                let _ = writer.write_all(b"/usage\r");
                let _ = writer.flush();
                usage_resends += 1;
                usage_sent_at = Some(Instant::now());
            }
            if let Some(t) = done_at {
                if t.elapsed() >= settle_after_done {
                    break;
                }
            }
        }
    }

    if let Some(pid) = child.process_id() {
        kill_tree(pid);
    }
    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(pair.master);

    Ok(PtyCapture {
        screen: screen.screen().contents(),
        frames,
        best: if !last_usage.is_empty() { last_usage } else { last_drawn },
    })
}

/// What the PTY drew: the final screen (parse this), every distinct frame
/// before it (for facts only a passing frame shows), and the last frame that
/// showed the usage view (Windows ConPTY can blank the final screen).
struct PtyCapture {
    screen: String,
    frames: String,
    best: String,
}

const PTY_ROWS: u16 = 80;
const FRAMES_CAP: usize = 4 * 1024 * 1024;
const PTY_COLS: u16 = 200;

/// Windows: npm installs the CLIs as `.cmd` shims, so killing the child only
/// ends `cmd.exe` and the real CLI kept running after every usage check.
/// End the whole tree. (Unix: the plain kill reaches the CLI itself.)
#[cfg(windows)]
fn kill_tree(pid: u32) {
    use std::os::windows::process::CommandExt as _;
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000) // CREATE_NO_WINDOW
        .status();
}

#[cfg(not(windows))]
fn kill_tree(_pid: u32) {}

fn parse_percent_line(line: &str) -> Option<f64> {
    // Matches e.g. "[██████████░░] 86.63%" or "12% used"
    for tok in line.split_whitespace().rev() {
        let trimmed = tok.trim_end_matches('%');
        if let Ok(v) = trimmed.parse::<f64>() {
            if (0.0..=100.0).contains(&v) {
                return Some(v);
            }
        }
    }
    None
}

pub fn parse_agy_usage(cleaned: &str) -> WorkUsageReport {
    let mut report = WorkUsageReport::base(Engine::Agy);
    let body = cleaned
        .rsplit("Models & Quota")
        .next()
        .unwrap_or(cleaned)
        .replace("GEMINI MODELS", "\nGEMINI MODELS");

    let mut lines: Vec<&str> = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with("↑/↓ Scroll") || t.starts_with("? for shortcuts") || t.contains("Loading quota summary") {
            continue;
        }
        lines.push(t);
    }

    if let Some(m_start) = cleaned.find("(Google AI ") {
        if let Some(m_end) = cleaned[m_start..].find(')') {
            report.plan = Some(cleaned[m_start + 1..m_start + m_end].trim().to_string());
        }
    }

    let mut current_group: Option<UsageGroup> = None;
    let mut pending_label: Option<String> = None;

    for (idx, &line) in lines.iter().enumerate() {
        if line.is_empty() {
            continue;
        }
        if let Some(acct) = line.strip_prefix("Account:") {
            report.account = Some(acct.trim().to_string());
            continue;
        }
        if line.starts_with('│') {
            let note_part = line.trim_start_matches('│').trim();
            if !note_part.is_empty() {
                let n = report.note.get_or_insert_with(String::new);
                if !n.is_empty() {
                    n.push(' ');
                }
                n.push_str(note_part);
            }
            continue;
        }
        // Group header: ALL CAPS words/digits/spaces (e.g. "GEMINI MODELS", "CLAUDE AND GPT MODELS", "GEMINI 4 ARGON")
        let is_group_header = line.len() >= 4
            && !line.contains('%')
            && !line.starts_with('[')
            && line
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ');
        if is_group_header {
            if let Some(g) = current_group.take() {
                if !g.meters.is_empty() {
                    report.groups.push(g);
                }
            }
            current_group = Some(UsageGroup {
                title: line.to_string(),
                subtitle: None,
                meters: Vec::new(),
            });
            pending_label = None;
            continue;
        }

        if line.starts_with("Models within this group:") || line.starts_with("For paid ") {
            if let Some(g) = current_group.as_mut() {
                g.subtitle = Some(line.to_string());
            }
            continue;
        }
        // Handle wrapped subtitle continuation lines (e.g. if a long description wraps before the meter label)
        if let Some(g) = current_group.as_mut() {
            if g.subtitle.is_some()
                && g.meters.is_empty()
                && pending_label.is_none()
                && !line.contains("Limit")
                && !line.starts_with('[')
            {
                let next_is_meter = lines
                    .iter()
                    .skip(idx + 1)
                    .find(|l| !l.is_empty())
                    .is_some_and(|l| l.starts_with('[') && l.contains('%'));
                if !next_is_meter {
                    if let Some(sub) = g.subtitle.as_mut() {
                        sub.push(' ');
                        sub.push_str(line);
                    }
                    continue;
                }
            }
        }

        if line.starts_with('[') && line.contains(']') && line.contains('%') {
            if let Some(pct) = parse_percent_line(line) {
                let label = pending_label
                    .take()
                    .unwrap_or_else(|| "Limit Remaining".to_string());
                // Peek next non-empty line for "Refreshes in ..." or "Quota available"
                let mut reset_text = None;
                for &next in lines.iter().skip(idx + 1) {
                    if next.is_empty() {
                        continue;
                    }
                    if next.starts_with("Refreshes in") || next == "Quota available" {
                        reset_text = Some(next.to_string());
                    }
                    break;
                }
                let meter = UsageMeter {
                    label,
                    percent: pct,
                    kind: "remaining".to_string(),
                    value_text: format!("{pct:.2}% remaining"),
                    reset_text,
                };
                current_group
                    .get_or_insert_with(|| UsageGroup {
                        title: "Quota".to_string(),
                        subtitle: None,
                        meters: Vec::new(),
                    })
                    .meters
                    .push(meter);
            }
            continue;
        }

        if line.starts_with("Refreshes in") || line == "Quota available" {
            continue;
        }

        // Otherwise this is likely a meter label like "Weekly Limit Remaining"
        pending_label = Some(line.to_string());
    }

    if let Some(g) = current_group {
        if !g.meters.is_empty() {
            report.groups.push(g);
        }
    }

    let raw_trimmed = lines
        .into_iter()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if !raw_trimmed.is_empty() {
        report.raw = Some(raw_trimmed);
    }
    if report.groups.is_empty() && report.account.is_none() && cleaned.contains("not signed in") {
        report.error = Some("Antigravity CLI is not signed in. Run `agy` in a terminal to sign in.".to_string());
    }
    report
}

pub fn parse_claude_usage(cleaned: &str) -> WorkUsageReport {
    let mut report = WorkUsageReport::base(Engine::Claude);

    // Extract plan from header banner if present (e.g. "Opus 5.5 · Claude Pro")
    for line in cleaned.lines() {
        let t = line.trim();
        if t.contains("Claude Pro") || t.contains("Claude Max") || t.contains("Claude Team") || t.contains("Claude Enterprise") {
            for part in t.split('·') {
                let p = part.trim();
                if p.starts_with("Claude ") {
                    report.plan = Some(p.to_string());
                    break;
                }
            }
            if report.plan.is_some() {
                break;
            }
        }
    }

    // Focus on the Usage view after the /usage command (avoid matching "/usage-credits" at the bottom)
    let tail = cleaned
        .find("❯ /usage")
        .or_else(|| cleaned.find("❯/usage"))
        .map(|i| &cleaned[i..])
        .unwrap_or(cleaned);
    let body = tail
        .rsplit("Usage   Stats")
        .next()
        .unwrap_or(tail);

    // Normalize glued tokens from TUI cursor moves and split before each "Current ..." / "Usage credits"
    let normalized = body
        .replace("Currentsession", "Current session")
        .replace("Currentweek", "Current week")
        .replace("(allmodels)", "(all models)")
        .replace("Usagecredits", "Usage credits")
        .replace("Current session", "\nCurrent session")
        .replace("Current week", "\nCurrent week")
        .replace("Usage credits", "\nUsage credits")
        .replace("Resets", "\nResets");

    let mut meters: Vec<UsageMeter> = Vec::new();
    let lines: Vec<&str> = normalized.lines().map(str::trim).collect();

    for (idx, &line) in lines.iter().enumerate() {
        if line.starts_with("Current session") || line.starts_with("Current week") {
            // Line may contain both label and "██████ 12% used" if they were on the same terminal row.
            let mut label = line.to_string();
            let mut pct: Option<f64> = None;
            let mut reset_text: Option<String> = None;

            if let Some(pos) = line.find("% used").or_else(|| line.find("%used")) {
                let before_pct = &line[..pos];
                if let Some(p) = before_pct
                    .split_whitespace()
                    .last()
                    .and_then(|s| s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse::<f64>().ok())
                {
                    pct = Some(p.clamp(0.0, 100.0));
                }
                // Trim bar/percent from label
                if let Some(end_lbl) = line.find("  ") {
                    label = line[..end_lbl].trim().to_string();
                } else if let Some(bar_pos) = line.find('█') {
                    label = line[..bar_pos].trim().to_string();
                }
            }

            for &next in lines.iter().skip(idx + 1).take(4) {
                if next.is_empty() {
                    continue;
                }
                if next.starts_with("Current ") || next.starts_with("Usage credits") {
                    break;
                }
                if pct.is_none() && (next.contains("% used") || next.contains("%used")) {
                    if let Some(p) = next
                        .split('%')
                        .next()
                        .and_then(|s| s.split_whitespace().last())
                        .and_then(|s| s.chars().filter(|c| c.is_ascii_digit() || *c == '.').collect::<String>().parse::<f64>().ok())
                    {
                        pct = Some(p.clamp(0.0, 100.0));
                    }
                } else if next.starts_with("Resets") {
                    reset_text = Some(next.split("  ").next().unwrap_or(next).trim().to_string());
                }
            }
            if let Some(p) = pct {
                // Deduplicate if TUI redrew the same meter multiple times (keep latest)
                meters.retain(|m| m.label != label);
                meters.push(UsageMeter {
                    label,
                    percent: p,
                    kind: "used".to_string(),
                    value_text: format!("{p:.0}% used"),
                    reset_text,
                });
            }
        } else if line.starts_with("Usage credits are") {
            report.note = Some(line.split('·').next().unwrap_or(line).trim().to_string());
        }
    }

    if !meters.is_empty() {
        report.groups.push(UsageGroup {
            title: "Rate Limits".to_string(),
            subtitle: None,
            meters,
        });
    }

    let raw_clean = lines
        .into_iter()
        .filter(|l| !l.is_empty() && !l.starts_with("Esc to cancel") && !l.starts_with("Esctocancel"))
        .collect::<Vec<_>>()
        .join("\n");
    if !raw_clean.is_empty() {
        report.raw = Some(raw_clean);
    }
    if report.groups.is_empty() && (cleaned.contains("Not logged in") || cleaned.contains("Please run /login")) {
        report.error = Some("Claude Code is not logged in. Run `claude` in a terminal to sign in.".to_string());
    }
    report
}

/// Windows: ConPTY hands Vicuna a blank screen for these TUIs (verified on
/// Windows 11 with Claude Code 2.1.322 and agy: both tabs reported "No usage
/// data returned" while the CLIs ran fine in a terminal), so the headless
/// `/usage` route goes first there. Elsewhere the interactive view works and
/// also names the plan/account, so it stays first with headless as fallback.
#[cfg(windows)]
const HEADLESS_FIRST: bool = true;
#[cfg(not(windows))]
const HEADLESS_FIRST: bool = false;

/// Run a CLI's headless usage command and return its stdout. Same launch
/// contract as the engine processes (terminal PATH, no console window).
fn run_usage_command(bin: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    if let Some(home) = crate::opencode::home_dir() {
        cmd.current_dir(home);
    }
    // a hover/refresh must never flash a console window
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start {}: {e}", bin.display()))?;
    let stdout = child.stdout.take().ok_or("stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("stderr unavailable")?;

    // stdout and stderr are different types, so each gets its own reader
    // (a shared array of them would not typecheck). Lossy decoding: one odd
    // byte must not throw away a whole usage report.
    fn drain<R: Read + Send + 'static>(mut pipe: R, is_err: bool, tx: mpsc::Sender<(bool, String)>) {
        let mut buf = Vec::new();
        let _ = pipe.read_to_end(&mut buf);
        let _ = tx.send((is_err, String::from_utf8_lossy(&buf).into_owned()));
    }
    let (tx, rx) = mpsc::channel::<(bool, String)>();
    drain(stdout, false, tx.clone());
    drain(stderr, true, tx.clone());
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut out = String::new();
    let mut err = String::new();
    let mut finished = 0;
    while finished < 2 {
        if Instant::now() >= deadline {
            kill_tree(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "{} did not finish within {}s",
                bin.display(),
                timeout.as_secs()
            ));
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok((is_err, s)) => {
                finished += 1;
                if is_err {
                    err.push_str(&s);
                } else {
                    out.push_str(&s);
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = child.wait();
    if out.trim().is_empty() {
        let msg = err.trim();
        return Err(if msg.is_empty() {
            format!("{} returned no output", bin.display())
        } else {
            msg.to_string()
        });
    }
    Ok(out)
}

/// Percent from "100% used" / "32%used" (the headless Claude output and the
/// TUI's rendered meter rows).
fn percent_before_used(s: &str) -> Option<f64> {
    let idx = s.find("% used").or_else(|| s.find("%used"))?;
    let num: String = s[..idx]
        .trim()
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    num.parse::<f64>().ok().map(|v| v.clamp(0.0, 100.0))
}

fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `claude -p "/usage"` prints the same numbers the TUI's Usage tab draws,
/// one meter per line (verified on Windows, where the interactive view never
/// got past the folder-trust dialog):
///
/// ```text
/// Current session: 100% used · resets Nov 19, 12:49pm (America/Toronto)
/// Current week (all models): 32% used · resets Nov 23, 8:59pm (America/Toronto)
/// Last 7d · 266 requests · 12 sessions
/// ```
pub fn parse_claude_usage_text(text: &str) -> WorkUsageReport {
    let mut report = WorkUsageReport::base(Engine::Claude);
    let mut meters: Vec<UsageMeter> = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if let Some((label, rest)) = t.split_once(':') {
            let label = label.trim();
            if label.starts_with("Current session") || label.starts_with("Current week") {
                if let Some(pct) = percent_before_used(rest) {
                    let reset_text = rest
                        .split_once('·')
                        .map(|(_, r)| capitalize_first(r.trim()))
                        .filter(|r| !r.is_empty());
                    meters.retain(|m| m.label != label);
                    meters.push(UsageMeter {
                        label: label.to_string(),
                        percent: pct,
                        kind: "used".to_string(),
                        value_text: format!("{pct:.0}% used"),
                        reset_text,
                    });
                    continue;
                }
            }
        }
        if t.starts_with("Usage credits are ") {
            report.note = Some(t.split('·').next().unwrap_or(t).trim().to_string());
            continue;
        }
        // "Last 7d · 266 requests · 12 sessions"
        if let Some(rest) = t.strip_prefix("Last ") {
            if let Some((window, tail)) = rest.split_once('·') {
                let window = window.trim();
                for part in tail.split('·').map(str::trim) {
                    if let Some(n) = part.strip_suffix(" requests") {
                        report.stats.push(UsageStat {
                            label: format!("Requests ({window})"),
                            value: n.trim().to_string(),
                        });
                    } else if let Some(n) = part.strip_suffix(" sessions") {
                        report.stats.push(UsageStat {
                            label: format!("Sessions ({window})"),
                            value: n.trim().to_string(),
                        });
                    }
                }
            }
        }
    }

    if !meters.is_empty() {
        report.groups.push(UsageGroup {
            title: "Rate Limits".to_string(),
            subtitle: None,
            meters,
        });
    }
    let raw = text.trim();
    if !raw.is_empty() {
        report.raw = Some(raw.to_string());
    }
    if report.groups.is_empty() && (text.contains("Not logged in") || text.contains("Please run /login")) {
        report.error = Some("Claude Code is not logged in. Run `claude` in a terminal to sign in.".to_string());
    }
    report
}

/// `agy -p "/usage"` prints one tab-separated line per meter:
/// `<group>\t<label>\t<percent>%\t<ISO reset>`.
pub fn parse_agy_usage_text(text: &str) -> WorkUsageReport {
    let mut report = WorkUsageReport::base(Engine::Agy);
    let mut groups: Vec<UsageGroup> = Vec::new();

    for line in text.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let parts: Vec<&str> = t.split('\t').map(str::trim).collect();
        if parts.len() < 3 {
            continue;
        }
        let Some(pct) = parts[2]
            .trim_end_matches('%')
            .trim()
            .parse::<f64>()
            .ok()
            .map(|v| v.clamp(0.0, 100.0))
        else {
            continue;
        };
        let reset_text = parts
            .get(3)
            .and_then(|s| parse_rfc3339_utc(s))
            .map(|e| format_reset_epoch(e).replacen("Resets", "Refreshes", 1));
        let meter = UsageMeter {
            label: parts[1].to_string(),
            percent: pct,
            kind: "remaining".to_string(),
            value_text: format!("{pct:.2}% remaining"),
            reset_text,
        };
        match groups.iter_mut().find(|g| g.title == parts[0]) {
            Some(g) => g.meters.push(meter),
            None => groups.push(UsageGroup {
                title: parts[0].to_string(),
                subtitle: None,
                meters: vec![meter],
            }),
        }
    }

    report.groups = groups;
    let raw = text.trim();
    if !raw.is_empty() {
        report.raw = Some(raw.to_string());
    }
    if report.groups.is_empty() && text.contains("not signed in") {
        report.error = Some("Antigravity CLI is not signed in. Run `agy` in a terminal to sign in.".to_string());
    }
    report
}

/// Epoch seconds for the UTC timestamps agy prints (`2026-11-26T15:14:32Z`).
fn parse_rfc3339_utc(s: &str) -> Option<i64> {
    let s = s.trim();
    let core = s
        .strip_suffix('Z')
        .or_else(|| s.strip_suffix("+00:00"))
        .unwrap_or(s);
    let (date, time) = core.split_once('T').or_else(|| core.split_once(' '))?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let mut t = time.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let sec: i64 = t.next()?.split('.').next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }
    // days since 1970-01-01 (Howard Hinnant's civil-days algorithm)
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = (if y_adj >= 0 { y_adj } else { y_adj - 399 }) / 400;
    let yoe = y_adj - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + h * 3600 + mi * 60 + sec)
}

/// Claude Code does not name the plan (or the account) in its headless
/// output; its own config records both under `oauthAccount`.
fn claude_account_info() -> (Option<String>, Option<String>) {
    let Some(home) = crate::opencode::home_dir() else {
        return (None, None);
    };
    let Ok(text) = std::fs::read_to_string(home.join(".claude.json")) else {
        return (None, None);
    };
    let Ok(v) = serde_json::from_str::<Value>(&text) else {
        return (None, None);
    };
    let Some(acct) = v.get("oauthAccount") else {
        return (None, None);
    };
    let email = acct.get("emailAddress").and_then(Value::as_str).map(str::to_string);
    let plan = acct
        .get("organizationType")
        .and_then(Value::as_str)
        .and_then(|t| {
            let t = t.to_ascii_lowercase();
            let label = if t.contains("enterprise") {
                "Claude Enterprise"
            } else if t.contains("team") {
                "Claude Team"
            } else if t.contains("max") {
                "Claude Max"
            } else if t.contains("pro") {
                "Claude Pro"
            } else {
                return None;
            };
            Some(label.to_string())
        });
    (email, plan)
}

fn format_reset_epoch(epoch_secs: i64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let diff = epoch_secs - now;
    if diff <= 0 {
        return "Resets soon".to_string();
    }
    let mins = diff / 60;
    let hours = mins / 60;
    let rem_mins = mins % 60;
    if hours >= 24 {
        let days = hours / 24;
        let rem_hours = hours % 24;
        format!("Resets in {days}d {rem_hours}h")
    } else if hours > 0 {
        format!("Resets in {hours}h {rem_mins}m")
    } else {
        format!("Resets in {rem_mins}m")
    }
}

pub fn parse_codex_rate_limits(result: &Value) -> WorkUsageReport {
    let mut report = WorkUsageReport::base(Engine::Codex);
    let rl = result.get("rateLimits").unwrap_or(result);

    if let Some(plan) = rl.get("planType").and_then(Value::as_str) {
        let mut chars = plan.chars();
        let cap = match chars.next() {
            Some(c) => format!("{}{}", c.to_uppercase(), chars.as_str()),
            None => plan.to_string(),
        };
        report.plan = Some(format!("ChatGPT {cap}"));
    }

    let mut meters = Vec::new();
    for (key, default_label) in [("primary", "5-Hour Limit"), ("secondary", "Weekly Limit")] {
        if let Some(win) = rl.get(key).filter(|v| !v.is_null()) {
            let used = win.get("usedPercent").and_then(Value::as_f64).unwrap_or(0.0);
            let mins = win.get("windowDurationMins").and_then(Value::as_i64);
            let label = match mins {
                Some(300) => "5-Hour Limit".to_string(),
                Some(10080) => "Weekly Limit".to_string(),
                Some(m) if m >= 60 => format!("{}-Hour Limit", m / 60),
                _ => default_label.to_string(),
            };
            let reset_text = win
                .get("resetsAt")
                .and_then(Value::as_i64)
                .map(format_reset_epoch);
            meters.push(UsageMeter {
                label,
                percent: used.clamp(0.0, 100.0),
                kind: "used".to_string(),
                value_text: format!("{used:.0}% used"),
                reset_text,
            });
        }
    }

    if !meters.is_empty() {
        report.groups.push(UsageGroup {
            title: "Rate Limits".to_string(),
            subtitle: None,
            meters,
        });
    }

    if let Some(credits) = rl.get("credits").filter(|v| !v.is_null()) {
        if credits.get("unlimited").and_then(Value::as_bool) == Some(true) {
            report.stats.push(UsageStat {
                label: "Credits".to_string(),
                value: "Unlimited".to_string(),
            });
        } else if let Some(bal) = credits.get("balance").and_then(Value::as_str) {
            if let Ok(v) = bal.parse::<f64>() {
                report.stats.push(UsageStat {
                    label: "Credits Balance".to_string(),
                    value: format!("${v:.2}"),
                });
            }
        }
    }
    if let Some(cnt) = result
        .pointer("/rateLimitResetCredits/availableCount")
        .and_then(Value::as_i64)
        .filter(|&c| c > 0)
    {
        report.stats.push(UsageStat {
            label: "Reset Credits".to_string(),
            value: format!("{cnt} available"),
        });
    }

    report
}

/// Codex's rate-limit call answers with the CLI's own auth error when its
/// ChatGPT token is expired (verified on Windows: `invalid_jwt`,
/// "Could not find the appropriate key in your authentication token"), and
/// `codex exec` says the same thing. Name the fix instead of pasting the HTTP
/// dump into the popup.
fn codex_error_hint(msg: &str) -> Option<&'static str> {
    let m = msg.to_ascii_lowercase();
    if m.contains("401")
        || m.contains("unauthorized")
        || m.contains("invalid_jwt")
        || m.contains("could not be refreshed")
        || m.contains("not signed in")
    {
        return Some(
            "Codex CLI is not signed in (its ChatGPT token expired). Run `codex login` in a terminal.",
        );
    }
    None
}

fn fetch_codex_usage(bin: &Path) -> Result<WorkUsageReport, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
    }
    // every hover/refresh of the usage meter runs this: no console flash
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        cmd.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start codex app-server: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("codex stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("codex stdout unavailable")?;

    let init_req = json!({
        "id": 1,
        "method": "initialize",
        "params": { "clientInfo": { "name": "vicuna", "title": "Vicuna", "version": "0.1.0" } }
    });
    writeln!(stdin, "{init_req}").map_err(|e| format!("codex write failed: {e}"))?;
    let _ = stdin.flush();

    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(12);
    let mut initialized = false;
    let mut account_email: Option<String> = None;
    let mut account_done = false;
    let mut rate_done = false;
    let mut result: Option<Result<WorkUsageReport, String>> = None;
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(line) => {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if !initialized && v.get("id").and_then(Value::as_i64) == Some(1) {
                        initialized = true;
                        let init_notif = json!({ "method": "initialized", "params": {} });
                        let rate_req = json!({ "id": 2, "method": "account/rateLimits/read", "params": {} });
                        let acct_req = json!({ "id": 3, "method": "account/read", "params": {} });
                        let _ = writeln!(stdin, "{init_notif}");
                        let _ = writeln!(stdin, "{rate_req}");
                        let _ = writeln!(stdin, "{acct_req}");
                        let _ = stdin.flush();
                    } else if v.get("id").and_then(Value::as_i64) == Some(3) {
                        account_done = true;
                        account_email = v
                            .pointer("/result/account/email")
                            .and_then(Value::as_str)
                            .map(str::to_string);
                        if let Some(Ok(r)) = result.as_mut() {
                            if r.account.is_none() {
                                r.account = account_email.clone();
                            }
                        }
                        if rate_done {
                            break;
                        }
                    } else if v.get("id").and_then(Value::as_i64) == Some(2) {
                        rate_done = true;
                        if let Some(err) = v.get("error") {
                            let msg = err
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("Codex rate limits request failed");
                            result = Some(Err(codex_error_hint(msg)
                                .map(str::to_string)
                                .unwrap_or_else(|| msg.to_string())));
                            break;
                        } else if let Some(res) = v.get("result") {
                            let mut rep = parse_codex_rate_limits(res);
                            if rep.account.is_none() {
                                rep.account = account_email.clone();
                            }
                            result = Some(Ok(rep));
                        }
                        if account_done {
                            break;
                        }
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    kill_tree(child.id());
    let _ = child.kill();
    let _ = child.wait();
    result.unwrap_or_else(|| Err("Timed out waiting for Codex rate limits".to_string()))
}

/// Interactive `/usage` view for Claude Code (older CLIs, and the fallback on
/// platforms where ConPTY hands back nothing).
fn claude_via_pty(bin: &Path) -> WorkUsageReport {
    let cap = match run_pty_slash_usage(
        bin,
        &["Claude Code", "for agents"],
        &["Current week", "Current session"],
        &["Current session", "Current week", "Usage   Stats"],
        Duration::from_millis(1400),
        Duration::from_secs(10),
    ) {
        Ok(cap) => cap,
        Err(e) => {
            let mut r = WorkUsageReport::base(Engine::Claude);
            r.error = Some(e);
            return r;
        }
    };
    let mut r = parse_claude_usage(&cap.screen);
    if r.groups.is_empty() && !cap.best.is_empty() {
        // the final screen can be blank even though the meters were drawn
        r = parse_claude_usage(&cap.best);
    }
    r
}

fn agy_via_pty(bin: &Path) -> WorkUsageReport {
    let cap = match run_pty_slash_usage(
        bin,
        &["for shortcuts"],
        &["Refreshes in", "Quota available"],
        // no retry: agy can spend ~12s loading quota, and a retyped "/usage"
        // would land in its prompt meanwhile
        &[],
        Duration::from_millis(600),
        // agy sometimes takes over 12 s to load quota; a normal run
        // still returns as soon as the meters are on screen
        Duration::from_secs(20),
    ) {
        Ok(cap) => cap,
        Err(e) => {
            let mut r = WorkUsageReport::base(Engine::Agy);
            r.error = Some(e);
            return r;
        }
    };
    let mut r = parse_agy_usage(&cap.screen);
    if r.groups.is_empty() && !cap.best.is_empty() {
        r = parse_agy_usage(&cap.best);
    }
    if r.plan.is_none() {
        // agy names the plan only on its loading view
        r.plan = parse_agy_usage(&cap.frames).plan;
    }
    r
}

/// Headless `/usage` for a CLI that supports it. A failure becomes a report
/// carrying the CLI's own message, so the caller can decide to fall back.
fn usage_via_command(engine: Engine, bin: &Path, parse: fn(&str) -> WorkUsageReport) -> WorkUsageReport {
    match run_usage_command(bin, &["-p", "/usage"], Duration::from_secs(45)) {
        Ok(text) => parse(&text),
        Err(e) => {
            let mut r = WorkUsageReport::base(engine);
            r.error = Some(e);
            r
        }
    }
}

/// Run the platform-preferred route, and the other one only when it found no
/// meters (each route launches a CLI, so never run both eagerly). What the
/// first route did capture (raw screen, error) is kept when the second also
/// comes back empty.
fn prefer_usage(
    first: impl FnOnce() -> WorkUsageReport,
    alt: impl FnOnce() -> WorkUsageReport,
) -> WorkUsageReport {
    let first = first();
    if !first.groups.is_empty() {
        return first;
    }
    let mut alt = alt();
    if alt.groups.is_empty() {
        if alt.raw.is_none() {
            alt.raw = first.raw;
        }
        if alt.error.is_none() {
            alt.error = first.error;
        }
        if alt.note.is_none() {
            alt.note = first.note;
        }
    }
    alt
}

pub async fn fetch_usage_logic(engine: Option<String>) -> Result<WorkUsageReport, String> {
    let eng = effective_engine(engine.as_deref());
    tokio::task::spawn_blocking(move || {
        let report = match eng {
            Engine::Agy => {
                let Some(bin) = crate::agy::find_agy() else {
                    let mut r = WorkUsageReport::base(eng);
                    r.error = Some(eng.missing_message());
                    return Ok(r);
                };
                let headless = || usage_via_command(Engine::Agy, &bin, parse_agy_usage_text);
                if HEADLESS_FIRST {
                    prefer_usage(headless, || agy_via_pty(&bin))
                } else {
                    prefer_usage(|| agy_via_pty(&bin), headless)
                }
            }
            Engine::Claude => {
                let Some(bin) = crate::claude::find_claude() else {
                    let mut r = WorkUsageReport::base(eng);
                    r.error = Some(eng.missing_message());
                    return Ok(r);
                };
                let headless = || usage_via_command(Engine::Claude, &bin, parse_claude_usage_text);
                let mut r = if HEADLESS_FIRST {
                    prefer_usage(headless, || claude_via_pty(&bin))
                } else {
                    prefer_usage(|| claude_via_pty(&bin), headless)
                };
                // the headless text names neither the plan nor the account;
                // Claude Code records both in its own config
                let (account, plan) = claude_account_info();
                if r.account.is_none() {
                    r.account = account;
                }
                if r.plan.is_none() {
                    r.plan = plan;
                }
                r
            }
            Engine::Codex => {
                let Some(bin) = crate::codex::find_codex() else {
                    let mut r = WorkUsageReport::base(eng);
                    r.error = Some(eng.missing_message());
                    return Ok(r);
                };
                fetch_codex_usage(&bin)?
            }
            Engine::Opencode | Engine::Pi | Engine::Fx => {
                let mut r = WorkUsageReport::base(eng);
                r.note = Some(format!(
                    "{} does not report subscription quota or usage.",
                    eng.label()
                ));
                r
            }
        };
        // Nothing at all: say so here, with the CLI named, instead of the
        // popup's generic "No usage data returned".
        let mut report = report;
        if report.groups.is_empty()
            && report.stats.is_empty()
            && report.note.is_none()
            && report.raw.is_none()
            && report.error.is_none()
        {
            report.note = Some(format!(
                "{} printed no usage data (its /usage view could not be read).",
                eng.label()
            ));
        }
        Ok(report)
    })
    .await
    .map_err(|e| format!("usage task failed: {e}"))?
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub async fn work_usage(engine: Option<String>) -> Result<WorkUsageReport, String> {
    fetch_usage_logic(engine).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the PTY loop does with the CLI's output: render it to a screen.
    fn render(stream: &str) -> String {
        let mut p = vt100::Parser::new(PTY_ROWS, PTY_COLS, 0);
        p.process(stream.as_bytes());
        p.screen().contents()
    }

    fn meters(r: &WorkUsageReport) -> Vec<(String, f64)> {
        r.groups.iter().flat_map(|g| g.meters.iter().map(|m| (m.label.clone(), m.percent))).collect()
    }

    #[test]
    fn conpty_repaints_render_like_the_screen() {
        // Windows ConPTY: absolute cursor moves, blank runs as ECH/CUF, and a
        // later repaint of only the changed cell (0% -> 12%). Read as a byte
        // stream this parsed as "0% used" with a stray "12%" line.
        let s = render("\x1b[2J\x1b[1;1H> /usage\x1b[5;3HCurrent\x1b[1Csession\x1b[12X\x1b[12C\u{2588}\x1b[1C0%\x1b[1Cused\x1b[6;3HResets\x1b[1C5pm\x1b[8;3HCurrent\x1b[1Cweek\x1b[1C(all\x1b[1Cmodels)\x1b[2C\u{2588}\x1b[1C40%\x1b[1Cused\x1b[5;32H12% used");
        assert!(s.contains("12% used") && !s.contains(" 0% used"), "{s}");
        assert_eq!(meters(&parse_claude_usage(&s)), [("Current session".into(), 12.0), ("Current week (all models)".into(), 40.0)]);
    }

    #[test]
    fn partially_redrawn_labels_keep_every_letter() {
        // Ink redraws a frame by rewriting only changed cells; the stream
        // then lacked letters ("Current ses ion") and the meter was lost
        // (seen live on Linux: the 49% session meter never parsed).
        let s = render("Current ses ion\r\n\u{2588}\u{2588} 49% used\r\nRese s 11:29pm\x1b[1;12Hs\x1b[3;5Ht");
        assert_eq!(meters(&parse_claude_usage(&s)), [("Current session".into(), 49.0)]);
    }

    #[test]
    fn parses_agy_quota_output() {
        let sample = r#"
└ Models & Quota

  Account: user@example.com

GEMINI MODELS
  Models within this group: Gemini 4 Hydrogen, Gemini 3.8 Flash

  Weekly Limit Remaining
[███████████████████████████████████████████░░░░░░░] 86.63%
Refreshes in 145h 21m

  Five Hour Limit Remaining
[█████████████████████████████████████████████████░] 98.64%
Refreshes in 3h 18m

CLAUDE AND GPT MODELS
  Models within this group: Claude Opus 5.5, Claude Sonnet 5.5

  Weekly Limit Remaining
[█████████████████████████░░░░░░░░░░░░░░░░░░░░░░░░░] 49.30%
Refreshes in 40h 34m

  Five Hour Limit Remaining
[██████████████████████████████████████████████████] 100.00%
Quota available
  ↑/↓ Scroll · esc Close
"#;
        let r = parse_agy_usage(sample);
        assert_eq!(r.account.as_deref(), Some("user@example.com"));
        assert_eq!(r.groups.len(), 2);
        assert_eq!(r.groups[0].title, "GEMINI MODELS");
        assert_eq!(r.groups[0].meters.len(), 2);
        assert_eq!(r.groups[0].meters[0].percent, 86.63);
        assert_eq!(r.groups[0].meters[0].reset_text.as_deref(), Some("Refreshes in 145h 21m"));
        assert_eq!(r.groups[1].meters[1].reset_text.as_deref(), Some("Quota available"));
    }

    #[test]
    fn parses_claude_usage_output() {
        let sample = r#"
Opus 5.5 · Claude Pro
❯ /usage
  Settings  Status   Config   Usage   Stats
Current session
0% used
Resets 9:40pm (America/Toronto)

Current week (all models)
██████                12% used
Resets Nov 16 at 9pm (America/Toronto)

Current week (Fable)
█                             2% used
Resets Nov 16 at 9pm (America/Toronto)

Usage credits
Usage credits are off · /usage-credits to turn them on
"#;
        let r = parse_claude_usage(sample);
        assert_eq!(r.plan.as_deref(), Some("Claude Pro"));
        assert_eq!(r.groups.len(), 1);
        assert_eq!(r.groups[0].meters.len(), 3);
        assert_eq!(r.groups[0].meters[1].label, "Current week (all models)");
        assert_eq!(r.groups[0].meters[1].percent, 12.0);
        assert_eq!(r.groups[0].meters[1].kind, "used");
        assert_eq!(r.note.as_deref(), Some("Usage credits are off"));
    }

    /// `claude -p "/usage"` on Windows (the interactive view never got past
    /// the folder-trust dialog there). Captured live; the numbers are the same
    /// ones the TUI's Usage tab draws.
    #[test]
    fn parses_claude_headless_usage_text() {
        let sample = "You are currently using your subscription to power your Claude Code usage\n\
\n\
Current session: 100% used \u{b7} resets Nov 19, 12:49pm (America/Toronto)\n\
Current week (all models): 32% used \u{b7} resets Nov 23, 8:59pm (America/Toronto)\n\
\n\
What's contributing to your usage?\n\
Approximate \u{b7} sessions on this machine only \u{b7} shares overlap\n\
\n\
Last 7d \u{b7} 266 requests \u{b7} 12 sessions\n\
  18%  Start fresh when you switch tasks \u{b7} run /clear\n\
  Top MCP servers: bugtracker 2%, vicuna-mcp 1%\n";
        let r = parse_claude_usage_text(sample);
        assert!(r.error.is_none(), "{:?}", r.error);
        assert_eq!(
            meters(&r),
            [
                ("Current session".to_string(), 100.0),
                ("Current week (all models)".to_string(), 32.0),
            ]
        );
        assert_eq!(r.groups[0].meters[0].kind, "used");
        assert_eq!(r.groups[0].meters[0].value_text, "100% used");
        assert_eq!(
            r.groups[0].meters[0].reset_text.as_deref(),
            Some("Resets Nov 19, 12:49pm (America/Toronto)")
        );
        assert_eq!(r.stats.len(), 2);
        assert_eq!(r.stats[0].label, "Requests (7d)");
        assert_eq!(r.stats[0].value, "266");
        assert_eq!(r.stats[1].label, "Sessions (7d)");
        assert_eq!(r.stats[1].value, "12");
    }

    /// `agy -p /usage`: one tab-separated row per meter, the reset as
    /// ISO-8601 UTC (captured live on Windows).
    #[test]
    fn parses_agy_headless_usage_text() {
        let sample = "Gemini Models\tWeekly Limit Remaining\t73%\t2026-11-26T15:14:32Z\n\
Claude and GPT models\tWeekly Limit Remaining\t100%\t2026-11-26T16:11:35Z\n";
        let r = parse_agy_usage_text(sample);
        assert_eq!(r.groups.len(), 2);
        assert_eq!(r.groups[0].title, "Gemini Models");
        assert_eq!(r.groups[0].meters[0].label, "Weekly Limit Remaining");
        assert_eq!(r.groups[0].meters[0].percent, 73.0);
        assert_eq!(r.groups[0].meters[0].kind, "remaining");
        assert_eq!(r.groups[0].meters[0].value_text, "73.00% remaining");
        let reset = r.groups[0].meters[0].reset_text.as_deref().unwrap_or("");
        assert!(reset.starts_with("Refreshes in "), "{reset}");
        assert_eq!(r.groups[1].title, "Claude and GPT models");
        assert_eq!(r.groups[1].meters[0].percent, 100.0);
    }

    #[test]
    fn parses_utc_timestamps() {
        assert_eq!(parse_rfc3339_utc("2026-11-26T15:14:32Z"), Some(1795706072));
        assert_eq!(parse_rfc3339_utc("2026-11-26T16:11:35Z"), Some(1795709495));
        assert_eq!(parse_rfc3339_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_utc("not a date"), None);
    }

    /// A failed first route must not lose its message when the second one is
    /// empty too (that message is what the popup shows).
    #[test]
    fn empty_second_route_keeps_the_first_error() {
        let mut first = WorkUsageReport::base(Engine::Claude);
        first.error = Some("claude printed no output".to_string());
        let alt = WorkUsageReport::base(Engine::Claude);
        let r = prefer_usage(|| first, || alt);
        assert_eq!(r.error.as_deref(), Some("claude printed no output"));
    }

    /// The real Windows error from `codex app-server` with an expired token
    /// (captured live) becomes a sign-in instruction, not an HTTP dump.
    #[test]
    fn codex_auth_errors_name_the_fix() {
        let live = "failed to fetch codex rate limits: GET https://chatgpt.com/backend-api/wham/usage failed: 401 Unauthorized; content-type=text/plain; body={\n  \"error\": {\n    \"message\": \"Could not find the appropriate key in your authentication token. Please try signing in again.\",\n    \"type\": \"invalid_request_error\",\n    \"code\": \"invalid_jwt\"\n  }\n}";
        assert!(codex_error_hint(live)
            .unwrap_or("")
            .contains("codex login"));
        assert!(codex_error_hint("Your access token could not be refreshed. Please log out and sign in again.")
            .is_some());
        assert_eq!(codex_error_hint("workspace routing discovery failed: timeout"), None);
    }

    #[test]
    fn parses_codex_rate_limits() {
        let sample = json!({
            "rateLimits": {
                "planType": "plus",
                "primary": { "usedPercent": 5, "windowDurationMins": 300, "resetsAt": 9999999999_i64 },
                "secondary": { "usedPercent": 100, "windowDurationMins": 10080, "resetsAt": 9999999999_i64 },
                "credits": { "hasCredits": true, "unlimited": false, "balance": "188.5536" }
            },
            "rateLimitResetCredits": { "availableCount": 1 }
        });
        let r = parse_codex_rate_limits(&sample);
        assert_eq!(r.plan.as_deref(), Some("ChatGPT Plus"));
        assert_eq!(r.groups[0].meters.len(), 2);
        assert_eq!(r.groups[0].meters[0].label, "5-Hour Limit");
        assert_eq!(r.groups[0].meters[0].percent, 5.0);
        assert_eq!(r.stats[0].value, "$188.55");
        assert_eq!(r.stats[1].value, "1 available");
    }

}
