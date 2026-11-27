use serde::Deserialize;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter, Manager, State};
#[cfg(feature = "desktop")]
use tauri_plugin_sql::{DbInstances, DbPool};

use crate::{app_title, browser, downloads_dir, git_panel, mcp, opencode, servers, skills, work_engine};

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// ---- chat storage: atomic full rewrite (see store.ts saveChats) ----
// The JS rewrite used to run DELETE + per-message INSERTs as separate plugin
// statements. A page unload (right-click → Refresh on Windows) kills the
// awaiting JS loop right after the DELETEs land, leaving the tables empty —
// history was lost exactly this way (live, 2026-11-11). The plugin pool runs
// each statement on a possibly different connection, so BEGIN/COMMIT from JS
// is unsafe; one sqlx transaction on that pool makes the rewrite
// all-or-nothing: a save killed mid-flight rolls back instead of truncating.

const CHATS_DB_KEY: &str = "sqlite:vicuna.db";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct MsgSaveRow {
    pub(crate) role: String,
    pub(crate) content: String,
    pub(crate) thinking: String,
    pub(crate) model: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) images: Option<Vec<String>>,
    pub(crate) gen_ms: Option<f64>,
    pub(crate) gen_tps: Option<f64>,
    pub(crate) gen_cost: Option<f64>,
    pub(crate) summary: Option<bool>,
    pub(crate) context_cleared: Option<bool>,
}

/// A string field that may also arrive as a JSON value: strings pass through,
/// null stays None, anything else is re-serialized to JSON text.
fn de_json_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(d)? {
        serde_json::Value::Null => None,
        serde_json::Value::String(s) => Some(s),
        other => Some(other.to_string()),
    })
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ChatSaveRow {
    pub(crate) id: String,
    pub(crate) title: String,
    pub(crate) created_at: i64,
    pub(crate) updated_at: Option<i64>,
    /// JSON text. The frontend sends it stringified, but `load_chats` (and so
    /// the desktop→server import) yields the parsed object: accept both, or
    /// one forked chat fails the whole batch ("invalid type: map").
    #[serde(deserialize_with = "de_json_text")]
    pub(crate) forked_from: Option<String>,
    pub(crate) fork_from_index: Option<i64>,
    pub(crate) mode: Option<String>,
    pub(crate) workspace: Option<String>,
    pub(crate) goal: Option<String>,
    /// per-session agent fields (engine/model/lead/pin) as JSON
    pub(crate) extra: Option<String>,
    pub(crate) messages: Vec<MsgSaveRow>,
    /// server merges only: the chat revision this client last saw. Older
    /// clients without a revision cannot overwrite server-recorded turns.
    pub(crate) base_rev: Option<i64>,
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn chats_save(
    instances: State<'_, DbInstances>,
    chats: Vec<ChatSaveRow>,
) -> Result<(), String> {
    let instances = instances.0.read().await;
    let pool = match instances.get(CHATS_DB_KEY) {
        Some(DbPool::Sqlite(pool)) => pool,
        None => return Err("chats_save: vicuna.db is not loaded".into()),
    };
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| format!("chats_save begin failed: {e}"))?;
    sqlx::query("DELETE FROM messages")
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
    sqlx::query("DELETE FROM chats")
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
    for chat in chats {
        sqlx::query(
            "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(chat.id.clone())
        .bind(chat.title.clone())
        .bind(chat.created_at)
        .bind(chat.forked_from)
        .bind(chat.fork_from_index)
        .bind(chat.mode.unwrap_or_else(|| "chat".into()))
        .bind(chat.workspace)
        .bind(chat.goal)
        .bind(chat.updated_at.unwrap_or(chat.created_at))
        .bind(chat.extra)
        .execute(&mut *tx)
        .await
        .map_err(|e| format!("chats_save: {e}"))?;
        for (i, m) in chat.messages.iter().enumerate() {
            let images = m
                .images
                .as_ref()
                .map(|v| serde_json::to_string(v).unwrap_or_default());
            sqlx::query(
                "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost, workspace, context_cleared) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            )
            .bind(chat.id.clone())
            .bind(m.role.clone())
            .bind(m.content.clone())
            .bind(m.thinking.clone())
            .bind(m.model.clone())
            .bind(i as i64)
            .bind(images)
            .bind(m.gen_ms)
            .bind(m.gen_tps)
            .bind(m.summary.map(|b| if b { 1 } else { 0 }))
            .bind(m.gen_cost)
            .bind(m.workspace.clone())
            .bind(m.context_cleared.map(i64::from))
            .execute(&mut *tx)
            .await
            .map_err(|e| format!("chats_save: {e}"))?;
        }
    }
    tx.commit()
        .await
        .map_err(|e| format!("chats_save commit failed: {e}"))?;
    Ok(())
}


// ---- Git panel (side panel Git tab; logic in git_panel.rs, shared with the bridge) ----

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_status(root: String) -> Result<git_panel::GitStatus, String> {
    git_panel::git_status_logic(root).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_diff(root: String, path: String, staged: bool, untracked: bool) -> Result<git_panel::GitDiff, String> {
    git_panel::git_diff_logic(root, path, staged, untracked).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_stage(root: String, paths: Vec<String>) -> Result<(), String> {
    git_panel::git_stage_logic(root, paths).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_unstage(root: String, paths: Vec<String>) -> Result<(), String> {
    git_panel::git_unstage_logic(root, paths).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_discard(root: String, paths: Vec<String>, untracked: Vec<String>) -> Result<(), String> {
    git_panel::git_discard_logic(root, paths, untracked).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn git_apply(root: String, patch: String, cached: bool, reverse: bool, intent_to_add: Vec<String>) -> Result<(), String> {
    git_panel::git_apply_logic(root, patch, cached, reverse, intent_to_add).await
}

// ---- remote servers (see src/servers.rs) ----

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) fn server_list() -> Vec<servers::ServerInfo> {
    servers::list()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn server_connect(url: String, token: Option<String>, name: Option<String>) -> Result<servers::ServerInfo, String> {
    servers::connect(&url, token, name).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) fn server_touch(id: String) -> Result<(), String> {
    servers::touch(&id)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) fn server_remove(id: String) -> Result<(), String> {
    servers::remove(&id)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) fn server_rename(app: AppHandle, id: String, name: Option<String>) -> Result<servers::ServerInfo, String> {
    let sv = servers::rename(&id, name)?;
    let label = format!("server-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.set_title(&format!("{} — {}", app_title(), sv.name));
    }
    let _ = app.emit("vicuna-servers-changed", ());
    Ok(sv)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn server_call(
    id: String,
    path: String,
    body: Option<serde_json::Value>,
    timeout_ms: Option<u64>,
) -> Result<serde_json::Value, String> {
    servers::call(&id, &path, body, timeout_ms).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn server_download_file(id: String, root: String, path: String) -> Result<String, String> {
    servers::download_file(&id, &root, &path, &downloads_dir()?).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn server_lb_proxy_url(id: String) -> Result<String, String> {
    servers::lb_proxy_url(&id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn server_base_url(id: String) -> Result<String, String> {
    servers::base_url(&id).await
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn work_engine_info() -> Result<opencode::WorkEngineInfo, String> {
    // re-detects on every call: the UI's Refresh / window focus picks up an
    // engine installed while Vicuna is running. Returns the current engine
    // (opencode or Claude Code) plus a summary of every engine.
    Ok(work_engine::engines_info().await)
}

/// ---------- MCP servers (Work mode only) ----------
///
/// Configs live in `~/vicuna/config/vicuna.json` (`mcp` map, opencode.json
/// shape) — the file is the source of truth, the UI edits it. Connections
/// are runtime-only in the McpManager (stdio children die on disconnect).

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_list_servers(app: AppHandle) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::session_servers(app.path().resource_dir().ok().as_deref())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_set_defaults(app: AppHandle, enabled: mcp::EnabledMap) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::set_defaults(&enabled)?;
    mcp_list_servers(app).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_save_server(
    app: AppHandle,
    config: mcp::McpServerConfig,
    previous_id: Option<String>,
) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::save_file_server(&config, previous_id.as_deref())?;
    mcp_list_servers(app).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_delete_server(app: AppHandle, id: String) -> Result<Vec<mcp::McpServerConfig>, String> {
    mcp::delete_file_server(&id)?;
    mcp_list_servers(app).await
}

// ---------- Skills (~/vicuna/skills/<slug>/SKILL.md) ----------

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn skills_list() -> Result<Vec<skills::SkillSummary>, String> {
    skills::list_skills()
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn skills_read(slug: String) -> Result<skills::SkillDetail, String> {
    skills::read_skill(&slug)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn skills_save(
    slug: String,
    content: String,
) -> Result<skills::SkillSummary, String> {
    skills::save_skill(&slug, &content)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn skills_set_defaults(skills_enabled: skills::EnabledMap) -> Result<(), String> {
    skills::set_defaults(&skills_enabled)
}

#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn skills_remove(slug: String) -> Result<(), String> {
    skills::remove_skill(&slug)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_connect(
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    state.connect(&config).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_disconnect(id: String, state: State<'_, mcp::McpManager>) -> Result<(), String> {
    state.disconnect(&id).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_list_tools(
    id: String,
    state: State<'_, mcp::McpManager>,
) -> Result<Vec<mcp::McpToolInfo>, String> {
    state.list_tools(&id).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_call_tool(
    server_id: String,
    tool: String,
    args: serde_json::Value,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpCallResult, String> {
    state.call_tool(&server_id, &tool, args).await
}

/// Browser sign-in for an OAuth-protected remote server: runs the PKCE
/// authorize flow (system browser + loopback callback), persists tokens to
/// auth.json, then connects. Errors when the server needs no auth.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_signin(
    app: AppHandle,
    config: mcp::McpServerConfig,
    state: State<'_, mcp::McpManager>,
) -> Result<mcp::McpServerInfo, String> {
    use tauri_plugin_opener::OpenerExt;
    mcp::oauth_login(&config, |url| async move {
        app.opener()
            .open_url(url, None::<&str>)
            .map_err(|e| format!("mcp: cannot open browser for sign-in: {e}"))
    })
    .await?;
    state.connect(&config).await
}

/// Forget the stored OAuth login and drop the connection, if any.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn mcp_signout(id: String, state: State<'_, mcp::McpManager>) -> Result<(), String> {
    // server names key the token store; resolve the id first
    let name = mcp::list_file_servers()?
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.name)
        .unwrap_or(id.clone());
    mcp::delete_auth(&name);
    let _ = state.disconnect(&id).await;
    Ok(())
}

/// Whether an OAuth login is stored for the server (drives the UI's
/// "Signed in / Sign out" affordance).
#[cfg_attr(feature = "desktop", tauri::command)]
pub(crate) async fn mcp_login_present(id: String) -> Result<bool, String> {
    let name = mcp::list_file_servers()?
        .into_iter()
        .find(|s| s.id == id)
        .map(|s| s.name)
        .unwrap_or(id);
    Ok(mcp::auth_present(&name))
}

/// ---------- Browser tab: `lb` (live-browser) sidecar daemon ----------
///
/// The daemon (Chrome via CDP + JSON API + Svelte UI) is the shared engine:
/// the side-panel tab iframes its web UI while the Work agent drives the
/// same tabs through `lb mcp`. Adopt-or-spawn + owned-only stop, exactly
/// like `ollama serve` above. The headless bridge reuses the `*_logic`
/// fns in `browser.rs` with no resource dir (PATH resolution only).

#[cfg(feature = "desktop")]
fn browser_resource_dir(app: &AppHandle) -> Option<std::path::PathBuf> {
    app.path().resource_dir().ok()
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_status(
    app: AppHandle,
    state: State<'_, std::sync::Arc<browser::BrowserState>>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_status_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_start(
    app: AppHandle,
    state: State<'_, std::sync::Arc<browser::BrowserState>>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_start_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_stop(state: State<'_, std::sync::Arc<browser::BrowserState>>) -> Result<browser::BrowserStatus, String> {
    browser::browser_stop_logic(&state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_restart(
    app: AppHandle,
    state: State<'_, std::sync::Arc<browser::BrowserState>>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_restart_logic(browser_resource_dir(&app), &state).await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_ensure_engine(
    app: AppHandle,
    background: Option<bool>,
) -> Result<browser::EngineInfo, String> {
    browser::browser_ensure_engine_logic(browser_resource_dir(&app), background.unwrap_or(false))
        .await
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_takeover(
    app: AppHandle,
    state: State<'_, std::sync::Arc<browser::BrowserState>>,
) -> Result<browser::BrowserStatus, String> {
    browser::browser_takeover_logic(browser_resource_dir(&app), &state).await
}

/// Absolute `lb mcp` argv for the default `browser` MCP server entry.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn browser_mcp_command(app: AppHandle) -> Result<Vec<String>, String> {
    let dir = browser_resource_dir(&app);
    browser::browser_mcp_command(dir.as_deref())
}
