//! Usage and quota reporting for Work harnesses (/usage).
//!
//! Each Work engine exposes usage differently:
//! - Antigravity (`agy`) & Claude Code (`claude`): interactive `/usage` view driven via `portable-pty`.
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

/// Clean ANSI terminal output while preserving cursor-forward spacing (`\x1b[NC`).
pub fn clean_ansi(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b == 0x1b {
            // ESC
            if i + 1 < bytes.len() && bytes[i + 1] == b'[' {
                // CSI sequence: ESC [ <params> <final 0x40..=0x7E>
                let mut j = i + 2;
                while j < bytes.len() && !(0x40..=0x7e).contains(&bytes[j]) {
                    j += 1;
                }
                if j < bytes.len() {
                    let final_byte = bytes[j];
                    if final_byte == b'C' {
                        // Cursor forward: emit spaces so words don't glue together.
                        let param = std::str::from_utf8(&bytes[i + 2..j]).unwrap_or("1");
                        let n: usize = param.parse().unwrap_or(1).min(40);
                        for _ in 0..n {
                            out.push(' ');
                        }
                    } else if final_byte == b'G' {
                        if !out.ends_with('\n') && !out.ends_with(' ') {
                            out.push(' ');
                        }
                    } else if final_byte == b'B' || final_byte == b'H' || final_byte == b'f' {
                        if !out.ends_with('\n') {
                            out.push('\n');
                        }
                    }
                    i = j + 1;
                    continue;
                }
            } else if i + 1 < bytes.len() && bytes[i + 1] == b']' {
                // OSC sequence: ESC ] ... (BEL or ESC \)
                let mut j = i + 2;
                while j < bytes.len() {
                    if bytes[j] == 0x07 {
                        j += 1;
                        break;
                    }
                    if bytes[j] == 0x1b && j + 1 < bytes.len() && bytes[j + 1] == b'\\' {
                        j += 2;
                        break;
                    }
                    j += 1;
                }
                i = j;
                continue;
            } else if i + 1 < bytes.len() {
                i += 2;
                continue;
            }
        } else if b == b'\r' {
            i += 1;
            continue;
        }
        // Decode UTF-8 char safely
        let s = &input[i..];
        if let Some(ch) = s.chars().next() {
            if !ch.is_control() || ch == '\n' || ch == '\t' {
                out.push(ch);
            }
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }
    out
}

/// Drive an interactive CLI (`/usage`) inside a tall PTY so the full view fits without paging.
fn run_pty_slash_usage(
    bin: &Path,
    ready_markers: &[&str],
    done_markers: &[&str],
    settle_after_done: Duration,
    total_timeout: Duration,
) -> Result<String, String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 80,
            cols: 200,
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
    let mut raw = String::new();
    let mut trusted = false;
    let mut sent = false;
    let mut done_at: Option<Instant> = None;

    while start.elapsed() < total_timeout {
        match rx.recv_timeout(Duration::from_millis(120)) {
            Ok(chunk) => {
                raw.push_str(&String::from_utf8_lossy(&chunk));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        let cleaned = clean_ansi(&raw);
        if !trusted && !sent && cleaned.contains("Do you trust") {
            let _ = writer.write_all(b"\r");
            let _ = writer.flush();
            trusted = true;
            continue;
        }
        if !sent && !cleaned.contains("Do you trust") && !cleaned.contains("Quick safety check") {
            let after_trust = cleaned.rsplit("enter Confirm").next().unwrap_or(&cleaned);
            let ready = ready_markers.iter().any(|m| after_trust.contains(m))
                || start.elapsed() >= Duration::from_millis(3500);
            if ready {
                let _ = writer.write_all(b"/usage\r");
                let _ = writer.flush();
                sent = true;
            }
        } else if sent {
            let after_cmd = cleaned.rsplit("/usage").next().unwrap_or(&cleaned);
            if done_at.is_none() && done_markers.iter().any(|m| after_cmd.contains(m)) {
                done_at = Some(Instant::now());
            }
            if let Some(t) = done_at {
                if t.elapsed() >= settle_after_done {
                    break;
                }
            }
        }
    }

    let _ = child.kill();
    let _ = child.wait();
    drop(writer);
    drop(pair.master);

    Ok(clean_ansi(&raw))
}

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

fn fetch_codex_usage(bin: &Path) -> Result<WorkUsageReport, String> {
    let mut cmd = Command::new(bin);
    cmd.arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(p) = crate::exe_resolve::child_path(bin.parent()) {
        cmd.env("PATH", p);
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
                            result = Some(Err(msg.to_string()));
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

    let _ = child.kill();
    let _ = child.wait();
    result.unwrap_or_else(|| Err("Timed out waiting for Codex rate limits".to_string()))
}

pub async fn fetch_usage_logic(engine: Option<String>) -> Result<WorkUsageReport, String> {
    let eng = effective_engine(engine.as_deref());
    tokio::task::spawn_blocking(move || match eng {
        Engine::Agy => {
            let Some(bin) = crate::agy::find_agy() else {
                let mut r = WorkUsageReport::base(eng);
                r.error = Some(eng.missing_message());
                return Ok(r);
            };
            let cleaned = run_pty_slash_usage(
                &bin,
                &["for shortcuts"],
                &["Refreshes in", "Quota available"],
                Duration::from_millis(600),
                Duration::from_secs(12),
            )?;
            Ok(parse_agy_usage(&cleaned))
        }
        Engine::Claude => {
            let Some(bin) = crate::claude::find_claude() else {
                let mut r = WorkUsageReport::base(eng);
                r.error = Some(eng.missing_message());
                return Ok(r);
            };
            let cleaned = run_pty_slash_usage(
                &bin,
                &["Claude Code", "for agents"],
                &["Current week", "Current session"],
                Duration::from_millis(1400),
                Duration::from_secs(10),
            )?;
            Ok(parse_claude_usage(&cleaned))
        }
        Engine::Codex => {
            let Some(bin) = crate::codex::find_codex() else {
                let mut r = WorkUsageReport::base(eng);
                r.error = Some(eng.missing_message());
                return Ok(r);
            };
            fetch_codex_usage(&bin)
        }
        Engine::Opencode | Engine::Pi | Engine::Fx => {
            let mut r = WorkUsageReport::base(eng);
            r.note = Some(format!(
                "{} does not report subscription quota or usage.",
                eng.label()
            ));
            Ok(r)
        }
    })
    .await
    .map_err(|e| format!("usage task failed: {e}"))?
}

#[tauri::command]
pub async fn work_usage(engine: Option<String>) -> Result<WorkUsageReport, String> {
    fetch_usage_logic(engine).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_ansi_preserves_cursor_forward_spaces() {
        let s = "\x1b[32mCurrent\x1b[1Cweek\x1b[1C(all\x1b[1Cmodels)\x1b[0m\r\n";
        assert_eq!(clean_ansi(s), "Current week (all models)\n");
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
