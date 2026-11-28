use serde::Serialize;

use crate::work_fs::{decode_base64, WorkFileContent};

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn upload_read_file(session_id: String, name: String) -> Result<WorkFileContent, String> {
    upload_read_file_logic(session_id, name).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn app_build_info(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let version = app.package_info().version.to_string();
    Ok(app_build_info_with_version(&version))
}

#[cfg(not(feature = "desktop"))]
pub(crate) async fn app_build_info() -> Result<serde_json::Value, String> {
    app_build_info_logic().await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn github_latest_release() -> Result<serde_json::Value, String> {
    github_latest_release_logic().await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn github_download_asset(url: String, file_name: String) -> Result<String, String> {
    github_download_asset_logic(url, file_name).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn upload_stage_file(session_id: String, file_name: String, base64: String) -> Result<String, String> {
    upload_stage_file_logic(session_id, file_name, base64).await
}

/// ---------- Session file uploads (composer attachments, desktop only) ----------
///
/// Dropped files are *copied* (never moved) into a per-session staging area
/// under the user's home: `~/vicuna/uploads/<session-id>/<file name>`.
/// This keeps project workspaces untouched (no git noise, no build or grep
/// pollution) and works for chat sessions that have no workspace at all.
/// The agent reaches staged files through `upload_read_file`, which is
/// scoped to the calling session — never arbitrary absolute paths.

pub(crate) fn uploads_home_dir() -> Result<std::path::PathBuf, String> {
    #[cfg(windows)]
    let home = std::env::var("USERPROFILE").map_err(|_| "cannot locate home folder".to_string())?;
    #[cfg(not(windows))]
    let home = std::env::var("HOME").map_err(|_| "cannot locate home folder".to_string())?;
    Ok(std::path::PathBuf::from(home))
}

/// The user's Downloads folder (installer landing zone for in-app updates).
pub fn downloads_dir() -> Result<std::path::PathBuf, String> {
    Ok(uploads_home_dir()?.join("Downloads"))
}

/// Open a downloaded installer with the system handler. Bridge-only path
/// (desktop uses the opener plugin). Restricted to installer payloads
/// inside ~/Downloads so the bridge can't be aimed at arbitrary files.
pub fn open_installer(path: &str) -> Result<(), String> {
    let full = std::path::PathBuf::from(path.trim());
    let canon = full.canonicalize().map_err(|_| "file not found".to_string())?;
    let dir = downloads_dir()?;
    let canon_dir = dir.canonicalize().unwrap_or(dir);
    if !canon.starts_with(&canon_dir) {
        return Err("refusing path outside Downloads".to_string());
    }
    let ext_ok = canon
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_lowercase().as_str(), "dmg" | "exe" | "appimage" | "deb"))
        .unwrap_or(false);
    if !ext_ok {
        return Err("not an installer file".to_string());
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&canon)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot open installer: {e}"))?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&canon)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot open installer: {e}"))?;
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let arg = canon.to_string_lossy().to_string();
        std::process::Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(arg)
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot open installer: {e}"))?;
    }
    Ok(())
}

/// ---------- In-app updates (Settings → App updates) ----------
///
/// The CI release workflow attaches installers to the rolling `latest`
/// prerelease on every main push with `gh release upload --clobber`, so the
/// release *tag* never changes and — crucially — neither does its
/// `published_at` (verified: the release keeps its first-publish date while
/// the assets are replaced). Staleness is therefore decided against the
/// *asset* timestamp (`updated_at`, created_at fallback), which GitHub
/// refreshes on every clobber upload, compared to this build's baked
/// timestamp (`VICUNA_BUILD_UNIX`, see build.rs). Downloads are allowlisted
/// to this repo's release attachments and land in ~/Downloads; the UI then
/// opens the installer (the user runs it — unsigned side-loaded builds
/// can't self-replace).

#[derive(Debug, Clone, Serialize)]
pub struct UpdateAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

/// Build the `app_build_info` JSON payload for a given version string.
/// Desktop passes `app.package_info().version` (which includes `--config`
/// overrides such as `0.1.2-dev.20261128151100`); headless/server falls back
/// to `vicuna_version()` (the baked dev version on rolling builds).
pub(crate) fn app_build_info_with_version(version: &str) -> serde_json::Value {
    serde_json::json!({
        "version": version,
        "builtAtUnix": option_env!("VICUNA_BUILD_UNIX").and_then(|s| s.parse::<u64>().ok()).unwrap_or(0),
    })
}

/// This build's version + baked timestamp (seconds since epoch, 0 when the
/// build script couldn't read the clock).
pub async fn app_build_info_logic() -> Result<serde_json::Value, String> {
    Ok(app_build_info_with_version(crate::version::vicuna_version()))
}

/// Pick the release-attachment fields the frontend needs out of the GitHub
/// `/releases/tags/latest` payload. Pure (unit-tested) so the mapping stays
/// honest when GitHub reshapes the response.
pub fn parse_release_info(v: &serde_json::Value) -> Result<serde_json::Value, String> {
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "release has no tag_name".to_string())?;
    let published_at = v
        .get("published_at")
        .and_then(|t| t.as_str())
        .ok_or_else(|| "release has no published_at".to_string())?;
    let html_url = v
        .get("html_url")
        .and_then(|t| t.as_str())
        .unwrap_or("https://github.com/anslwy/vicuna/releases/latest");
    let assets = v
        .get("assets")
        .and_then(|a| a.as_array())
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for a in assets {
        let (Some(name), Some(url)) = (
            a.get("name").and_then(|n| n.as_str()),
            a.get("browser_download_url").and_then(|u| u.as_str()),
        ) else {
            continue;
        };
        // clobber uploads refresh updated_at (created_at on the very first
        // upload); the release-level published_at is frozen, so this is the
        // only reliable "when was this installer built" signal.
        let uploaded = a
            .get("updated_at")
            .and_then(|t| t.as_str())
            .or_else(|| a.get("created_at").and_then(|t| t.as_str()));
        out.push(serde_json::json!({
            "name": name,
            "url": url,
            "size": a.get("size").and_then(|s| s.as_u64()),
            "uploadedAt": uploaded,
        }));
    }
    let updated_at = v.get("updated_at").and_then(|t| t.as_str());
    Ok(serde_json::json!({
        "tag": tag,
        "publishedAt": published_at,
        "updatedAt": updated_at,
        "htmlUrl": html_url,
        "assets": out,
    }))
}

/// Latest published info for the rolling `latest` prerelease. GitHub's
/// `/releases/latest` endpoint skips prereleases, so address the tag
/// directly (GitHub API needs an explicit User-Agent).
pub async fn github_latest_release_logic() -> Result<serde_json::Value, String> {
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| format!("update check failed: {e}"))?
        .get("https://api.github.com/repos/anslwy/vicuna/releases/tags/latest")
        .header("User-Agent", "vicuna-update-check/1.0")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    if !res.status().is_success() {
        let status = res.status();
        return Err(format!("update check failed: GitHub answered {status}"));
    }
    let data: serde_json::Value = res
        .json()
        .await
        .map_err(|e| format!("update check failed: {e}"))?;
    parse_release_info(&data)
}

/// Download a release attachment into ~/Downloads. The URL is allowlisted
/// to this repo's release attachments; the file name is flattened to its
/// final component. Returns the absolute path for the UI to open.
pub async fn github_download_asset_logic(url: String, file_name: String) -> Result<String, String> {
    const PREFIX: &str = "https://github.com/anslwy/vicuna/releases/download/";
    if !url.starts_with(PREFIX) {
        return Err("refusing non-release URL".to_string());
    }
    let name = std::path::Path::new(file_name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != ".." && n.len() <= 255)
        .ok_or_else(|| "invalid file name".to_string())?;
    let dir = downloads_dir()?;
    let dest = dir.join(name);
    if !dest.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    let res = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(600))
        .build()
        .map_err(|e| format!("download failed: {e}"))?
        .get(&url)
        .header("User-Agent", "vicuna-update-check/1.0")
        .send()
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    if !res.status().is_success() {
        return Err(format!("download failed: GitHub answered {}", res.status()));
    }
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt as _;
    let mut stream = res.bytes_stream();
    let mut file = tokio::fs::File::create(&dest)
        .await
        .map_err(|e| format!("download failed: {e}"))?;
    // installers are tens of MB; 1GB cap aborts runaway responses
    let mut total: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("download failed: {e}"))?;
        total += chunk.len() as u64;
        if total > 1024 * 1024 * 1024 {
            return Err("download failed: file too large".to_string());
        }
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("download failed: {e}"))?;
    }
    file.flush().await.map_err(|e| format!("download failed: {e}"))?;
    drop(file);
    // AppImages must be executable to run — browsers/KDE etc. set +x on
    // download, a plain HTTPS stream does not.
    #[cfg(unix)]
    if dest
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("appimage"))
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    Ok(dest.to_string_lossy().to_string())
}

pub(crate) fn upload_session_dir(session_id: &str) -> Result<std::path::PathBuf, String> {
    let id = session_id.trim();
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid session".to_string());
    }
    Ok(uploads_home_dir()?
        .join("vicuna")
        .join("uploads")
        .join(id))
}

/// Copy an uploaded file into the session staging area. The name is flattened
/// to its final component (no subdirectories, no escapes); existing files
/// with the same name are overwritten, like `cp`. Returns the absolute path
/// for the prompt note sent with the message.
pub async fn upload_stage_file_logic(session_id: String, file_name: String, base64: String) -> Result<String, String> {
    const CAP: usize = 16 * 1024 * 1024;
    let bytes = decode_base64(&base64).map_err(|_| "invalid file data".to_string())?;
    if bytes.len() > CAP {
        return Err("file too large (max 16MB)".to_string());
    }
    let name = std::path::Path::new(file_name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != ".." && n.len() <= 255)
        .ok_or_else(|| "invalid file name".to_string())?;
    let dir = upload_session_dir(&session_id)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot stage upload: {e}"))?;
    let dest = dir.join(name);
    if !dest.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    std::fs::write(&dest, &bytes).map_err(|e| format!("cannot stage upload: {e}"))?;
    Ok(dest.to_string_lossy().to_string())
}

/// Read a staged session file as text (cap ~64KB; binaries rejected).
/// Scoped to the calling session's upload dir — no absolute paths.
pub async fn upload_read_file_logic(session_id: String, name: String) -> Result<WorkFileContent, String> {
    let dir = upload_session_dir(&session_id)?;
    let clean = std::path::Path::new(name.trim())
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != "." && *n != "..")
        .ok_or_else(|| "invalid file name".to_string())?;
    let full = dir.join(clean);
    if !full.starts_with(&dir) {
        return Err("invalid file name".to_string());
    }
    let meta = std::fs::metadata(&full).map_err(|_| "file not found".to_string())?;
    if !meta.is_file() {
        return Err("not a file".to_string());
    }
    let size = meta.len();
    let bytes = std::fs::read(&full).map_err(|e| format!("cannot read file: {e}"))?;
    if bytes.iter().take(4096).any(|&b| b == 0) {
        return Err("binary file — refusing to dump as text".to_string());
    }
    const CAP: usize = 65536;
    let truncated = bytes.len() > CAP;
    let slice = &bytes[..bytes.len().min(CAP)];
    Ok(WorkFileContent {
        path: clean.to_string(),
        content: String::from_utf8_lossy(slice).to_string(),
        truncated,
        size,
    })
}
