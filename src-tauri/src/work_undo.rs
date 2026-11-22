use std::collections::HashMap;
use std::sync::Mutex;
#[cfg(feature = "desktop")]
use tauri::State;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;

use crate::{work_canonical_root, work_exec_logic, work_join};
#[cfg(windows)]
use crate::CREATE_NO_WINDOW;

/// ---------- Work undo/redo (/undo, /redo: revert last turn's file changes) ----------
///
/// The agent mutates the workspace through `write` (direct) and `exec`
/// (arbitrary shell). Before each `write`, dispatch snapshots the previous
/// bytes and records the new ones, so the change is invertible in both
/// directions; `exec` commands are recorded as notes since shell side
/// effects can't be reliably inverted. A turn pushes one frame at start
/// (`undo_begin_turn`); `/undo` pops the last frame and inverts it (delete
/// created files, restore modified ones), pushing it onto a redo stack;
/// `/redo` pops that and re-applies it.
#[derive(Debug, Clone)]
pub enum UndoOp {
    /// File did not exist before the turn: undo deletes it, redo rewrites it.
    Created { path: String, content: Vec<u8> },
    /// File existed: undo restores `prev`, redo restores `next`.
    Modified { path: String, prev: Vec<u8>, next: Vec<u8> },
    ExecNote { command: String },
}

#[derive(Debug, Clone)]
pub struct UndoTurn {
    pub workspace: String,
    pub ops: Vec<UndoOp>,
}

#[derive(Default)]
pub struct UndoState {
    turns: Mutex<HashMap<String, Vec<UndoTurn>>>,
    /// Frames popped by /undo, newest last; cleared by a new agent turn.
    redo: Mutex<HashMap<String, Vec<UndoTurn>>>,
    /// `git status --porcelain` captured at the previous turn start, per session.
    /// Used to tell the agent what was already dirty before it touched anything
    /// (and what changed since its last turn) so it cannot honestly blame
    /// itself for the user's edits — or claim it changed nothing.
    git_baseline: Mutex<HashMap<String, String>>,
}

/// Session key for the undo stack: the session id when present, else the
/// workspace (agent turns always carry a session, the fallback keeps
/// headless callers grouped sensibly).
pub fn undo_key(session_id: Option<&str>, workspace: &str) -> String {
    match session_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => s.to_string(),
        None => workspace.trim().to_string(),
    }
}

const GIT_NOTE_MAX_LINES: usize = 40;

fn git_note_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

fn git_note_block(lines: &[String]) -> String {
    let shown = lines.len().min(GIT_NOTE_MAX_LINES);
    let mut out = lines[..shown].join("\n");
    if lines.len() > shown {
        out.push_str(&format!("\n…(+{} more)", lines.len() - shown));
    }
    out
}

/// Summarize `git status --porcelain` at the start of a Work turn. The agent
/// is told what was already dirty before it acted, and (after the first turn)
/// what changed since its previous turn, so it can distinguish the user's
/// edits from its own instead of making false attribution claims.
/// Returns `None` outside a git work tree (or when git cannot run).
pub async fn workspace_git_note(state: &UndoState, key: &str, workspace: &str) -> Option<String> {
    let res = work_exec_logic(
        workspace.to_string(),
        "git status --porcelain".to_string(),
        Some(5000),
    )
    .await
    .ok()?;
    if res.timed_out || res.exit_code != Some(0) {
        return None;
    }
    let current = git_note_lines(&res.stdout);
    let previous = match state.git_baseline.lock() {
        Ok(mut m) => m.insert(key.to_string(), current.join("\n")),
        Err(_) => None,
    };

    if current.is_empty() {
        return Some(match previous {
            None => "Working tree is clean at turn start (git status --porcelain).".to_string(),
            Some(prev) if prev.trim().is_empty() => {
                "Working tree is clean at turn start (unchanged since your previous turn).".to_string()
            }
            Some(_) => "Working tree is clean at turn start — changes from your previous turn are gone (committed, reverted, or undone).".to_string(),
        });
    }

    let Some(prev) = previous else {
        return Some(format!(
            "Working tree BEFORE this turn (git status --porcelain). These changes already existed when the turn started — they may be the user's or from an earlier turn, not necessarily yours:\n{}",
            git_note_block(&current)
        ));
    };
    let prev_lines = git_note_lines(&prev);
    let prev_set: std::collections::HashSet<&str> =
        prev_lines.iter().map(String::as_str).collect();
    let current_set: std::collections::HashSet<&str> =
        current.iter().map(String::as_str).collect();
    let added: Vec<String> = current
        .iter()
        .filter(|line| !prev_set.contains(line.as_str()))
        .map(|line| {
            if line.starts_with("?? ") {
                format!(
                    "+ {line}  (appeared during your previous turn — if you created this, move scratch to the system temp dir)"
                )
            } else {
                format!("+ {line}")
            }
        })
        .collect();
    let removed: Vec<String> = prev_lines
        .iter()
        .filter(|line| !current_set.contains(line.as_str()))
        .map(|line| format!("- {line}"))
        .collect();
    if added.is_empty() && removed.is_empty() {
        return Some(format!(
            "Working tree unchanged since your previous turn (still dirty):\n{}",
            git_note_block(&current)
        ));
    }
    let mut delta = added;
    delta.extend(removed);
    Some(format!(
        "Working tree CHANGED since your previous turn (sampled before this turn; includes your own last-turn writes and the user's edits):\n{}",
        git_note_block(&delta)
    ))
}

/// Snapshot a file before the agent overwrites it. `Some((existed, bytes))`
/// means tracked; `None` means untrackable (directory, >5MB, unreadable) and
/// the write is then left out of the undo history.
pub fn undo_capture_prev(root: &str, path: &str) -> Option<(bool, Vec<u8>)> {
    let canon = work_canonical_root(root).ok()?;
    let full = work_join(&canon, path).ok()?;
    if !full.exists() {
        return Some((false, Vec::new()));
    }
    if !full.is_file() {
        return None;
    }
    const CAP: u64 = 5 * 1024 * 1024;
    if std::fs::metadata(&full).map(|m| m.len() > CAP).unwrap_or(true) {
        return None;
    }
    std::fs::read(&full).ok().map(|b| (true, b))
}

/// Push a fresh frame for a starting turn (cap 20 per session). A new turn
/// also invalidates the redo history, like an editor after a new edit.
pub fn undo_begin_turn(state: &UndoState, key: &str, workspace: &str) {
    if key.trim().is_empty() {
        return;
    }
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key.to_string()).or_default();
        stack.push(UndoTurn { workspace: workspace.to_string(), ops: Vec::new() });
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if let Ok(mut r) = state.redo.lock() {
        r.remove(key);
    }
}

/// Record one op onto the session's live frame (creating one if the turn
/// started before this backend rolled, e.g. older app versions).
pub fn undo_record(state: &UndoState, key: &str, op: UndoOp) {
    if key.trim().is_empty() {
        return;
    }
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key.to_string()).or_default();
        if stack.is_empty() {
            stack.push(UndoTurn { workspace: String::new(), ops: Vec::new() });
        }
        if let Some(top) = stack.last_mut() {
            top.ops.push(op);
        }
    }
}

/// Record a completed `write` (prev captured before it ran) so both undo
/// and redo can restore the exact bytes.
pub fn undo_record_write(state: &UndoState, key: &str, path: &str, existed: bool, prev: Vec<u8>, next: &[u8]) {
    let op = if existed {
        UndoOp::Modified { path: path.to_string(), prev, next: next.to_vec() }
    } else {
        UndoOp::Created { path: path.to_string(), content: next.to_vec() }
    };
    undo_record(state, key, op);
}

/// Write `bytes` to a workspace-relative file, creating parent folders.
fn undo_write_file(canon: &std::path::Path, path: &str, bytes: &[u8]) -> Result<(), ()> {
    let full = work_join(canon, path).map_err(|_| ())?;
    if full.is_dir() {
        return Err(());
    }
    if let Some(parent) = full.parent() {
        if !parent.starts_with(canon) {
            return Err(());
        }
        std::fs::create_dir_all(parent).map_err(|_| ())?;
    }
    std::fs::write(&full, bytes).map_err(|_| ())
}

/// Invert one frame: created files are deleted, modified ones restored.
/// `exec` notes are reported, never auto-reverted (shell side effects are
/// not reliably invertible).
fn undo_apply_reverse(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
    let mut restored = 0usize;
    let mut removed = 0usize;
    let mut execs: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for op in ops.iter().rev() {
        match op {
            UndoOp::Created { path, .. } => match work_join(canon, path) {
                Ok(full) => {
                    if full.is_file() && !full.is_symlink() {
                        match std::fs::remove_file(&full) {
                            Ok(_) => removed += 1,
                            Err(_) => skipped.push(path.clone()),
                        }
                    } else if !full.exists() {
                        removed += 1; // already gone counts as reverted
                    } else {
                        skipped.push(path.clone());
                    }
                }
                Err(_) => skipped.push(path.clone()),
            },
            UndoOp::Modified { path, prev, .. } => {
                if undo_write_file(canon, path, prev).is_ok() {
                    restored += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::ExecNote { command } => {
                execs.push(command.chars().take(80).collect());
            }
        }
    }
    (restored, removed, execs, skipped)
}

/// Re-apply one frame: created files are rewritten, modified ones get the
/// turn's bytes back. Exec notes still can't be replayed.
fn undo_apply_forward(canon: &std::path::Path, ops: &[UndoOp]) -> (usize, usize, Vec<String>, Vec<String>) {
    let mut restored = 0usize;
    let mut recreated = 0usize;
    let mut execs: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for op in ops.iter() {
        match op {
            UndoOp::Created { path, content } => {
                if undo_write_file(canon, path, content).is_ok() {
                    recreated += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::Modified { path, next, .. } => {
                if undo_write_file(canon, path, next).is_ok() {
                    restored += 1;
                } else {
                    skipped.push(path.clone());
                }
            }
            UndoOp::ExecNote { command } => {
                execs.push(command.chars().take(80).collect());
            }
        }
    }
    (restored, recreated, execs, skipped)
}

/// Shared tail for the undo/redo notices ("restored 1 file, removed …").
fn undo_message(verb: &str, restored: usize, other: usize, other_verb: &str, other_noun: &str, execs: &[String], skipped: &[String]) -> String {
    let mut parts: Vec<String> = Vec::new();
    if restored > 0 {
        parts.push(format!("restored {} file{}", restored, if restored == 1 { "" } else { "s" }));
    }
    if other > 0 {
        parts.push(format!(
            "{other_verb} {other} {other_noun}{}",
            if other == 1 { "" } else { "s" }
        ));
    }
    let mut msg = if parts.is_empty() {
        format!("{verb}: no file writes to change.")
    } else {
        format!("{verb}: {}.", parts.join(", "))
    };
    if !execs.is_empty() {
        msg.push_str(&format!(
            " ({} shell command{} ran — not auto-replayed, check manually).",
            execs.len(),
            if execs.len() == 1 { "" } else { "s" }
        ));
    }
    if !skipped.is_empty() {
        let names: Vec<String> = skipped.iter().take(3).cloned().collect();
        msg.push_str(&format!(" Skipped: {}.", names.join(", ")));
    }
    msg
}

/// Pop the last turn for a session, invert its file ops, and park it on the
/// redo stack. Returns a human summary for the chat notice.
pub async fn work_undo_logic(state: &UndoState, session_id: String) -> Result<String, String> {
    let key = session_id.trim().to_string();
    if key.is_empty() {
        return Err("no session for undo".to_string());
    }
    let turn = {
        let mut m = state.turns.lock().map_err(|_| "undo state poisoned".to_string())?;
        let stack = m.get_mut(&key).ok_or_else(|| "Nothing to undo — no assistant turn was recorded for this session.".to_string())?;
        stack.pop().ok_or_else(|| "Nothing to undo — no assistant turn was recorded for this session.".to_string())?
    };
    // every popped frame is redoable, even an empty one (frontend still
    // restores the message pair)
    if let Ok(mut r) = state.redo.lock() {
        let stack = r.entry(key).or_default();
        stack.push(turn.clone());
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if turn.ops.is_empty() || turn.workspace.trim().is_empty() {
        let execs = turn.ops.iter().filter(|o| matches!(o, UndoOp::ExecNote { .. })).count();
        if execs > 0 {
            return Ok(format!("Undid last turn: no file changes to revert ({} shell command{} ran — check manually).", execs, if execs == 1 { "" } else { "s" }));
        }
        return Ok("Undid last turn: no file changes to revert.".to_string());
    }
    let canon = work_canonical_root(turn.workspace.trim())
        .map_err(|e| format!("cannot undo into workspace: {e}"))?;
    let (restored, removed, execs, skipped) = undo_apply_reverse(&canon, &turn.ops);
    if restored == 0 && removed == 0 && execs.is_empty() && skipped.is_empty() {
        return Ok("Undid last turn: no file changes to revert.".to_string());
    }
    Ok(undo_message("Undid last turn", restored, removed, "removed", "created file", &execs, &skipped))
}

/// Re-apply the last undo frame for a session (the bubbles come back from
/// the frontend's stash). Pushes the frame back onto the undo stack so the
/// pair can alternate.
pub async fn work_redo_logic(state: &UndoState, session_id: String) -> Result<String, String> {
    let key = session_id.trim().to_string();
    if key.is_empty() {
        return Err("no session for redo".to_string());
    }
    let turn = {
        let mut r = state.redo.lock().map_err(|_| "undo state poisoned".to_string())?;
        let stack = r.get_mut(&key).ok_or_else(|| "Nothing to redo — /undo first.".to_string())?;
        stack.pop().ok_or_else(|| "Nothing to redo — /undo first.".to_string())?
    };
    if let Ok(mut m) = state.turns.lock() {
        let stack = m.entry(key).or_default();
        stack.push(turn.clone());
        while stack.len() > 20 {
            stack.remove(0);
        }
    }
    if turn.ops.is_empty() || turn.workspace.trim().is_empty() {
        let execs = turn.ops.iter().filter(|o| matches!(o, UndoOp::ExecNote { .. })).count();
        if execs > 0 {
            return Ok(format!("Redid last turn: no file changes to replay ({} shell command{} — run manually).", execs, if execs == 1 { "" } else { "s" }));
        }
        return Ok("Redid last turn: no file changes to replay.".to_string());
    }
    let canon = work_canonical_root(turn.workspace.trim())
        .map_err(|e| format!("cannot redo into workspace: {e}"))?;
    let (restored, recreated, execs, skipped) = undo_apply_forward(&canon, &turn.ops);
    if restored == 0 && recreated == 0 && execs.is_empty() && skipped.is_empty() {
        return Ok("Redid last turn: no file changes to replay.".to_string());
    }
    Ok(undo_message("Redid last turn", restored, recreated, "recreated", "file", &execs, &skipped))
}

/// Revert the last recorded Work turn's file changes for a session.
/// Pops one undo frame; chat bubbles are removed frontend-side.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_undo(session_id: String, state: State<'_, UndoState>) -> Result<String, String> {
    work_undo_logic(&state, session_id).await
}

/// Re-apply the turn most recently reverted by /undo. Chat bubbles are
/// restored frontend-side.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn work_redo(session_id: String, state: State<'_, UndoState>) -> Result<String, String> {
    work_redo_logic(&state, session_id).await
}
