//! Git panel backend (side panel "Git" tab): status, per-file diffs, and
//! stage / unstage / discard at file, hunk and line level.
//!
//! The workspace may be a subfolder of a repository, so every command works
//! from the repository top level with repo-relative paths internally, and
//! reports workspace-relative paths to the frontend. Paths coming back from
//! the frontend are joined under the workspace prefix and must stay inside
//! it; patches are checked the same way before `git apply` sees them.
//!
//! Hunk / line staging is done by the frontend building a reduced unified
//! diff (`src/lib/gitDiff.ts`) that is applied here with `git apply
//! [--cached] [--reverse] --recount`.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

const GIT_TIMEOUT: Duration = Duration::from_secs(20);
/// Diffs larger than this are not rendered (the panel shows a notice).
const DIFF_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Status entries beyond this are dropped (`truncated` is set).
const STATUS_MAX_FILES: usize = 5000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitEntry {
    /// Workspace-relative path (forward slashes).
    pub path: String,
    /// Rename/copy source, workspace-relative (None when outside the workspace).
    pub orig_path: Option<String>,
    /// Index status letter (`.` = unchanged), porcelain v2 `X`.
    pub index: String,
    /// Worktree status letter (`.` = unchanged), porcelain v2 `Y`.
    pub worktree: String,
    /// "changed" | "untracked" | "conflict"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatus {
    pub repo: bool,
    pub branch: Option<String>,
    pub files: Vec<GitEntry>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiff {
    pub text: String,
    pub too_large: bool,
}

struct Repo {
    top: PathBuf,
    /// Workspace position inside the repo, `""` or `"sub/dir/"`.
    prefix: String,
}

struct GitOut {
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

async fn run_git(cwd: &Path, args: &[&str], stdin: Option<&[u8]>) -> Result<GitOut, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.current_dir(cwd)
        // config the user may have that would change the output we parse
        .args([
            "-c", "core.quotepath=false",
            "-c", "color.ui=false",
            "-c", "diff.noprefix=false",
            "-c", "diff.mnemonicPrefix=false",
            "-c", "diff.external=",
        ])
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_LITERAL_PATHSPECS", "1")
        .env("GIT_PAGER", "cat")
        .env("LC_ALL", "C")
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "git is not installed (or not on PATH)".to_string()
        } else {
            format!("cannot run git: {e}")
        }
    })?;
    if let Some(bytes) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(bytes).await.map_err(|e| format!("git stdin: {e}"))?;
            drop(pipe);
        }
    }
    let out = tokio::time::timeout(GIT_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| "git timed out".to_string())?
        .map_err(|e| format!("git failed: {e}"))?;
    Ok(GitOut {
        code: out.status.code(),
        stdout: out.stdout,
        stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

fn git_err(out: &GitOut, what: &str) -> String {
    if out.stderr.is_empty() {
        format!("{what} failed (exit {})", out.code.map(|c| c.to_string()).unwrap_or_else(|| "?".into()))
    } else {
        out.stderr.clone()
    }
}

/// Locate the repository for a workspace. `Ok(None)` = not a git work tree.
async fn open_repo(root: &str) -> Result<Option<Repo>, String> {
    let canon = crate::work_canonical_root(root.trim())?;
    let out = run_git(&canon, &["rev-parse", "--show-toplevel", "--show-prefix"], None).await?;
    if out.code != Some(0) {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let mut lines = text.lines();
    let top = lines.next().unwrap_or("").trim();
    if top.is_empty() {
        return Ok(None);
    }
    let prefix = lines.next().unwrap_or("").trim().replace('\\', "/");
    Ok(Some(Repo { top: PathBuf::from(top), prefix }))
}

async fn require_repo(root: &str) -> Result<Repo, String> {
    open_repo(root).await?.ok_or_else(|| "not a git repository".to_string())
}

/// Workspace-relative → repo-relative, rejecting anything that leaves the workspace.
fn repo_path(repo: &Repo, rel: &str) -> Result<String, String> {
    let rel = rel.trim().replace('\\', "/");
    // "." = the whole workspace (Stage All / Unstage All): the repo-relative
    // workspace folder, or the repo top when the workspace is the repo itself
    if rel == "." {
        let scope = repo.prefix.trim_end_matches('/');
        return Ok(if scope.is_empty() { ".".to_string() } else { scope.to_string() });
    }
    let rel = rel.trim_start_matches("./");
    if rel.is_empty() || rel.starts_with('/') || rel.contains(":/") {
        return Err(format!("invalid path: {rel}"));
    }
    if rel.split('/').any(|seg| seg == ".." || seg == ".git") {
        return Err(format!("path escapes the workspace: {rel}"));
    }
    Ok(format!("{}{}", repo.prefix, rel))
}

/// Repo-relative → workspace-relative (None when outside the workspace).
fn ws_path(repo: &Repo, repo_rel: &str) -> Option<String> {
    repo_rel.strip_prefix(repo.prefix.as_str()).map(str::to_string)
}

/// Parse `git status --porcelain=v2 -z --branch` output into entries
/// (repo-relative paths) plus the branch name.
fn parse_status(raw: &[u8]) -> (Option<String>, Vec<(String, Option<String>, String, String, String)>) {
    let text = String::from_utf8_lossy(raw);
    let mut recs = text.split('\0');
    let mut branch = None;
    let mut out = Vec::new();
    while let Some(rec) = recs.next() {
        if rec.is_empty() {
            continue;
        }
        if let Some(rest) = rec.strip_prefix("# branch.head ") {
            branch = Some(rest.to_string());
            continue;
        }
        if rec.starts_with('#') {
            continue;
        }
        let kind = rec.as_bytes()[0];
        match kind {
            b'1' => {
                // 1 XY sub mH mI mW hH hI path
                let f: Vec<&str> = rec.splitn(9, ' ').collect();
                if f.len() == 9 {
                    let xy = f[1].as_bytes();
                    out.push((f[8].to_string(), None, (xy[0] as char).to_string(), (xy[1] as char).to_string(), "changed".to_string()));
                }
            }
            b'2' => {
                // 2 XY sub mH mI mW hH hI Xscore path \0 origPath
                let f: Vec<&str> = rec.splitn(10, ' ').collect();
                let orig = recs.next().map(str::to_string);
                if f.len() == 10 {
                    let xy = f[1].as_bytes();
                    out.push((f[9].to_string(), orig, (xy[0] as char).to_string(), (xy[1] as char).to_string(), "changed".to_string()));
                }
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let f: Vec<&str> = rec.splitn(11, ' ').collect();
                if f.len() == 11 {
                    let xy = f[1].as_bytes();
                    out.push((f[10].to_string(), None, (xy[0] as char).to_string(), (xy[1] as char).to_string(), "conflict".to_string()));
                }
            }
            b'?' => {
                if let Some(p) = rec.get(2..) {
                    out.push((p.to_string(), None, ".".to_string(), "?".to_string(), "untracked".to_string()));
                }
            }
            _ => {}
        }
    }
    (branch, out)
}

pub async fn git_status_logic(root: String) -> Result<GitStatus, String> {
    let Some(repo) = open_repo(&root).await? else {
        return Ok(GitStatus { repo: false, branch: None, files: vec![], truncated: false });
    };
    let scope = if repo.prefix.is_empty() { ".".to_string() } else { repo.prefix.clone() };
    let out = run_git(
        &repo.top,
        &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all", "--", &scope],
        None,
    )
    .await?;
    if out.code != Some(0) {
        return Err(git_err(&out, "git status"));
    }
    let (branch, raw) = parse_status(&out.stdout);
    let mut files = Vec::new();
    let mut truncated = false;
    for (path, orig, x, y, kind) in raw {
        let Some(wp) = ws_path(&repo, &path) else { continue };
        if files.len() >= STATUS_MAX_FILES {
            truncated = true;
            break;
        }
        files.push(GitEntry {
            path: wp,
            orig_path: orig.as_deref().and_then(|o| ws_path(&repo, o)),
            index: x,
            worktree: y,
            kind,
        });
    }
    Ok(GitStatus { repo: true, branch, files, truncated })
}

/// Unified diff of one file. `staged` = index vs HEAD, else worktree vs
/// index; `untracked` diffs the new file against /dev/null.
pub async fn git_diff_logic(root: String, path: String, staged: bool, untracked: bool) -> Result<GitDiff, String> {
    let repo = require_repo(&root).await?;
    let rp = repo_path(&repo, &path)?;
    let out = if untracked {
        run_git(
            &repo.top,
            &["diff", "--no-index", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", "--", "/dev/null", &rp],
            None,
        )
        .await?
    } else {
        let mut args = vec!["diff", "--no-ext-diff", "--no-renames", "--src-prefix=a/", "--dst-prefix=b/"];
        if staged {
            args.push("--cached");
        }
        args.push("--");
        args.push(&rp);
        run_git(&repo.top, &args, None).await?
    };
    // --no-index exits 1 when the files differ
    if !matches!(out.code, Some(0) | Some(1)) {
        return Err(git_err(&out, "git diff"));
    }
    if out.stdout.len() > DIFF_MAX_BYTES {
        return Ok(GitDiff { text: String::new(), too_large: true });
    }
    Ok(GitDiff { text: String::from_utf8_lossy(&out.stdout).into_owned(), too_large: false })
}

fn repo_paths(repo: &Repo, paths: &[String]) -> Result<Vec<String>, String> {
    if paths.is_empty() {
        return Err("no files given".to_string());
    }
    paths.iter().map(|p| repo_path(repo, p)).collect()
}

async fn git_with_paths(repo: &Repo, base: &[&str], paths: &[String], what: &str) -> Result<(), String> {
    let mut args: Vec<&str> = base.to_vec();
    args.push("--");
    args.extend(paths.iter().map(String::as_str));
    let out = run_git(&repo.top, &args, None).await?;
    if out.code != Some(0) {
        return Err(git_err(&out, what));
    }
    Ok(())
}

pub async fn git_stage_logic(root: String, paths: Vec<String>) -> Result<(), String> {
    let repo = require_repo(&root).await?;
    let rps = repo_paths(&repo, &paths)?;
    git_with_paths(&repo, &["add", "-A"], &rps, "git add").await
}

async fn has_head(repo: &Repo) -> bool {
    run_git(&repo.top, &["rev-parse", "--verify", "-q", "HEAD"], None)
        .await
        .map(|o| o.code == Some(0))
        .unwrap_or(false)
}

pub async fn git_unstage_logic(root: String, paths: Vec<String>) -> Result<(), String> {
    let repo = require_repo(&root).await?;
    let rps = repo_paths(&repo, &paths)?;
    if has_head(&repo).await {
        git_with_paths(&repo, &["restore", "--staged"], &rps, "git restore").await
    } else {
        // no commits yet: nothing to restore from, just drop the index entries
        git_with_paths(&repo, &["rm", "--cached", "-r", "-q", "--ignore-unmatch"], &rps, "git rm --cached").await
    }
}

/// Discard worktree changes. `untracked` files (and intent-to-add entries)
/// are deleted; tracked files are restored from the index, so staged work
/// is kept (the same as Fork's "Discard" on the unstaged list).
pub async fn git_discard_logic(root: String, paths: Vec<String>, untracked: Vec<String>) -> Result<(), String> {
    let repo = require_repo(&root).await?;
    let canon = crate::work_canonical_root(root.trim())?;
    if !paths.is_empty() {
        let rps = repo_paths(&repo, &paths)?;
        git_with_paths(&repo, &["restore", "--worktree"], &rps, "git restore").await?;
    }
    if !untracked.is_empty() {
        let rps = repo_paths(&repo, &untracked)?;
        // intent-to-add entries live in the index: drop them first (no-op otherwise)
        git_with_paths(&repo, &["rm", "--cached", "-q", "--ignore-unmatch"], &rps, "git rm --cached").await?;
        for rel in &untracked {
            let full = crate::work_join(&canon, rel)?;
            let meta = std::fs::symlink_metadata(&full).map_err(|e| format!("{rel}: {e}"))?;
            if meta.is_dir() {
                return Err(format!("{rel} is a folder; discard its files instead"));
            }
            std::fs::remove_file(&full).map_err(|e| format!("cannot delete {rel}: {e}"))?;
        }
    }
    Ok(())
}

fn unquote_patch_path(raw: &str) -> String {
    let raw = raw.trim_end_matches('\t').trim();
    if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
        raw[1..raw.len() - 1].replace("\\\"", "\"").replace("\\\\", "\\")
    } else {
        raw.to_string()
    }
}

/// Every file a patch touches must be inside the workspace; renames/copies
/// and binary payloads are refused (the panel never builds them).
fn check_patch_paths(repo: &Repo, patch: &str) -> Result<(), String> {
    let mut touched = 0;
    for line in patch.lines() {
        if line.starts_with("rename ") || line.starts_with("copy ") || line.starts_with("GIT binary patch") {
            return Err("patch contains a rename, copy or binary change".to_string());
        }
        let target = if let Some(rest) = line.strip_prefix("--- ") {
            rest
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            rest
        } else {
            continue;
        };
        let p = unquote_patch_path(target);
        if p == "/dev/null" {
            continue;
        }
        let rel = p
            .strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .ok_or_else(|| format!("unexpected patch path: {p}"))?;
        if rel.split('/').any(|seg| seg == ".." || seg == ".git") || rel.starts_with('/') {
            return Err(format!("patch path escapes the workspace: {rel}"));
        }
        if !rel.starts_with(repo.prefix.as_str()) {
            return Err(format!("patch path is outside the workspace: {rel}"));
        }
        touched += 1;
    }
    if touched == 0 {
        return Err("patch touches no files".to_string());
    }
    Ok(())
}

/// Apply a (reduced) patch: `cached` → index, else worktree; `reverse`
/// un-applies it. `intent_to_add` (workspace-relative) are untracked files
/// that must get an index entry first so their lines can be staged; the
/// entry is rolled back if the apply fails.
pub async fn git_apply_logic(
    root: String,
    patch: String,
    cached: bool,
    reverse: bool,
    intent_to_add: Vec<String>,
) -> Result<(), String> {
    let repo = require_repo(&root).await?;
    check_patch_paths(&repo, &patch)?;
    let ita = if intent_to_add.is_empty() { vec![] } else { repo_paths(&repo, &intent_to_add)? };
    if !ita.is_empty() {
        git_with_paths(&repo, &["add", "-N"], &ita, "git add -N").await?;
    }
    let mut args = vec!["apply", "--recount", "--whitespace=nowarn"];
    if cached {
        args.push("--cached");
    }
    if reverse {
        args.push("--reverse");
    }
    args.push("-");
    let mut body = patch;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let out = run_git(&repo.top, &args, Some(body.as_bytes())).await?;
    if out.code != Some(0) {
        if !ita.is_empty() {
            let _ = git_with_paths(&repo, &["rm", "--cached", "-q", "--ignore-unmatch"], &ita, "git rm --cached").await;
        }
        return Err(git_err(&out, "git apply"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .expect("git runs")
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    }

    fn temp_repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vicuna-git-panel-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("ws")).unwrap();
        sh(&dir, &["init", "-q"]);
        sh(&dir, &["config", "user.email", "t@t"]);
        sh(&dir, &["config", "user.name", "t"]);
        std::fs::write(dir.join("ws/f.txt"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.join("outside.txt"), "o\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "init"]);
        dir.canonicalize().unwrap()
    }

    fn show_index(dir: &Path, path: &str) -> String {
        let out = std::process::Command::new("git").current_dir(dir).args(["show", &format!(":{path}")]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    #[test]
    fn parse_status_v2_records() {
        let raw = b"# branch.oid abc\0# branch.head main\0\
1 .M N... 100644 100644 100644 aaa aaa ws/f.txt\0\
2 R. N... 100644 100644 100644 aaa aaa R100 ws/new name.txt\0ws/old.txt\0\
u UU N... 100644 100644 100644 100644 a b c ws/c.txt\0\
? ws/u.txt\0";
        let (branch, e) = parse_status(raw);
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(e.len(), 4);
        assert_eq!((e[0].0.as_str(), e[0].2.as_str(), e[0].3.as_str()), ("ws/f.txt", ".", "M"));
        assert_eq!(e[1].0, "ws/new name.txt");
        assert_eq!(e[1].1.as_deref(), Some("ws/old.txt"));
        assert_eq!(e[2].4, "conflict");
        assert_eq!((e[3].0.as_str(), e[3].4.as_str()), ("ws/u.txt", "untracked"));
    }

    #[tokio::test]
    async fn stage_all_and_unstage_all_stay_in_workspace() {
        let dir = temp_repo("all");
        let ws = dir.join("ws").to_string_lossy().to_string();
        std::fs::write(dir.join("ws/f.txt"), "a\nB\nc\n").unwrap();
        std::fs::create_dir_all(dir.join("ws/deep")).unwrap();
        std::fs::write(dir.join("ws/deep/new.txt"), "n\n").unwrap();
        std::fs::write(dir.join("outside.txt"), "changed\n").unwrap();

        git_stage_logic(ws.clone(), vec![".".into()]).await.unwrap();
        let st = git_status_logic(ws.clone()).await.unwrap();
        assert!(st.files.iter().all(|f| f.worktree == "." && f.index != "."), "everything in ws staged: {:?}", st.files);
        assert_eq!(st.files.len(), 2);
        assert_eq!(show_index(&dir, "outside.txt"), "o\n", "outside the workspace stays unstaged");

        git_unstage_logic(ws.clone(), vec![".".into()]).await.unwrap();
        let st = git_status_logic(ws).await.unwrap();
        assert!(st.files.iter().all(|f| f.index == "." || f.kind == "untracked"), "nothing staged: {:?}", st.files);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn status_stage_unstage_discard_in_subfolder_workspace() {
        let dir = temp_repo("files");
        let ws = dir.join("ws").to_string_lossy().to_string();
        std::fs::write(dir.join("ws/f.txt"), "a\nB\nc\n").unwrap();
        std::fs::write(dir.join("ws/new.txt"), "n\n").unwrap();
        std::fs::write(dir.join("outside.txt"), "changed\n").unwrap();

        let st = git_status_logic(ws.clone()).await.unwrap();
        assert!(st.repo);
        let paths: Vec<&str> = st.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["f.txt", "new.txt"], "outside.txt is outside the workspace");

        let d = git_diff_logic(ws.clone(), "f.txt".into(), false, false).await.unwrap();
        assert!(d.text.contains("--- a/ws/f.txt") && d.text.contains("+B"));
        let nd = git_diff_logic(ws.clone(), "new.txt".into(), false, true).await.unwrap();
        assert!(nd.text.contains("+++ b/ws/new.txt") && nd.text.contains("+n"));

        git_stage_logic(ws.clone(), vec!["f.txt".into(), "new.txt".into()]).await.unwrap();
        let st = git_status_logic(ws.clone()).await.unwrap();
        assert!(st.files.iter().all(|f| f.index != "." && f.worktree == "."));

        git_unstage_logic(ws.clone(), vec!["new.txt".into()]).await.unwrap();
        let st = git_status_logic(ws.clone()).await.unwrap();
        assert_eq!(st.files.iter().find(|f| f.path == "new.txt").unwrap().kind, "untracked");

        git_discard_logic(ws.clone(), vec![], vec!["new.txt".into()]).await.unwrap();
        assert!(!dir.join("ws/new.txt").exists());

        git_unstage_logic(ws.clone(), vec!["f.txt".into()]).await.unwrap();
        git_discard_logic(ws.clone(), vec!["f.txt".into()], vec![]).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("ws/f.txt")).unwrap(), "a\nb\nc\n");
        assert_eq!(std::fs::read_to_string(dir.join("outside.txt")).unwrap(), "changed\n", "never touched");

        assert!(git_stage_logic(ws.clone(), vec!["../outside.txt".into()]).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn apply_partial_patches_and_guards() {
        let dir = temp_repo("apply");
        let ws = dir.join("ws").to_string_lossy().to_string();
        std::fs::write(dir.join("ws/f.txt"), "a\nB\nc\nd\n").unwrap();

        // stage only the "+d" line: "-b" becomes context, "+B" is dropped
        let stage_d = "diff --git a/ws/f.txt b/ws/f.txt\n--- a/ws/f.txt\n+++ b/ws/f.txt\n@@ -1,3 +1,4 @@\n a\n b\n c\n+d\n";
        git_apply_logic(ws.clone(), stage_d.into(), true, false, vec![]).await.unwrap();
        assert_eq!(show_index(&dir, "ws/f.txt"), "a\nb\nc\nd\n");

        // unstage it again (reverse onto the index)
        git_apply_logic(ws.clone(), stage_d.into(), true, true, vec![]).await.unwrap();
        assert_eq!(show_index(&dir, "ws/f.txt"), "a\nb\nc\n");

        // discard only "B" in the worktree (reverse of b→B), keeping "+d"
        let discard_b = "diff --git a/ws/f.txt b/ws/f.txt\n--- a/ws/f.txt\n+++ b/ws/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n";
        git_apply_logic(ws.clone(), discard_b.into(), false, true, vec![]).await.unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("ws/f.txt")).unwrap(), "a\nb\nc\nd\n");

        // first line of an untracked file, via intent-to-add
        std::fs::write(dir.join("ws/n.txt"), "x\ny\n").unwrap();
        let new_x = "diff --git a/ws/n.txt b/ws/n.txt\n--- a/ws/n.txt\n+++ b/ws/n.txt\n@@ -0,0 +1,1 @@\n+x\n";
        git_apply_logic(ws.clone(), new_x.into(), true, false, vec!["n.txt".into()]).await.unwrap();
        assert_eq!(show_index(&dir, "ws/n.txt"), "x\n");

        // a failing apply rolls the intent-to-add entry back
        std::fs::write(dir.join("ws/m.txt"), "m\n").unwrap();
        let bad = "diff --git a/ws/m.txt b/ws/m.txt\n--- a/ws/m.txt\n+++ b/ws/m.txt\n@@ -1,1 +1,1 @@\n-nope\n+m\n";
        assert!(git_apply_logic(ws.clone(), bad.into(), true, false, vec!["m.txt".into()]).await.is_err());
        let st = git_status_logic(ws.clone()).await.unwrap();
        assert_eq!(st.files.iter().find(|f| f.path == "m.txt").unwrap().kind, "untracked");

        // patches may only touch files inside the workspace
        let outside = "--- a/outside.txt\n+++ b/outside.txt\n@@ -1 +1 @@\n-o\n+p\n";
        assert!(git_apply_logic(ws.clone(), outside.into(), true, false, vec![]).await.unwrap_err().contains("outside"));
        let escape = "--- a/ws/../outside.txt\n+++ b/ws/../outside.txt\n@@ -1 +1 @@\n-o\n+p\n";
        assert!(git_apply_logic(ws.clone(), escape.into(), true, false, vec![]).await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn non_repo_reports_repo_false() {
        let dir = std::env::temp_dir().join(format!("vicuna-git-panel-norepo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let st = git_status_logic(dir.to_string_lossy().to_string()).await.unwrap();
        assert!(!st.repo);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
