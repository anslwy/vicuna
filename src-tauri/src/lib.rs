// Headless server builds (`--no-default-features`, see Cargo.toml) keep every
// Tauri command wrapper compiled but uncalled; don't drown real warnings.
#![cfg_attr(not(feature = "desktop"), allow(dead_code, unused_imports))]
// Vicuna backend: all Ollama inference traffic goes through here.
// The frontend (JS) is display-only: it invokes these commands and renders
// the streamed events. Direct `fetch("http://localhost:11434/...")` from JS
// is only a web-dev fallback (see src/lib/ollama.ts).

#[cfg(feature = "desktop")]
use std::sync::Mutex;
#[cfg(feature = "desktop")]
use tauri::tray::TrayIconBuilder;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Manager, WindowEvent};

pub mod ipc;
pub mod acp;
pub mod agy;
pub mod agy_accounts;
pub mod cli_accounts;
pub mod browser;
pub mod mcp;
pub mod opencode;
pub mod exe_resolve;
pub mod claude;
pub mod claude_cli;
pub mod codex;
pub mod codex_cli;
pub mod fx;
pub mod pi;
pub mod work_engine;
pub mod work_agent;
pub mod work_usage;
pub mod vicuna_mcp;
pub mod session_insight;
pub mod git_panel;
pub mod server_store;
pub mod servers;
pub mod server;
pub mod whisper;
pub mod plugins;
pub mod plugin_harness;
pub mod plugin_icon;
pub mod skills;

pub mod ollama;
pub mod web_fetch;
pub mod work_fs;
pub mod bg_exec;
pub mod agent_shell;
pub mod work_undo;
pub mod cli_launcher;
pub mod terminal;
pub mod updates_uploads;
pub mod app_shell;
pub mod tauri_commands;

#[cfg(test)]
mod work_tests;

pub const OLLAMA_BASE: &str = "http://localhost:11434";

/// Windows: keep background children from flashing a console window on the
/// user's screen (agent `exec`, its timeout killer, self-started
/// `ollama serve`, pid kills). No-op on other platforms.
#[cfg(windows)]
pub(crate) const CREATE_NO_WINDOW: u32 = 0x08000000;

// Re-exports preserving every crate::* and vicuna_lib::* path used by
// server/, bridge/, and internal modules.
pub use ollama::{ollama_fetch_text_logic, parse_show_context_length, ChatMessage};
pub(crate) use ollama::{
    ollama_chat_stream, ollama_delete_model, ollama_generate_stream, ollama_list_models,
    ollama_list_running, ollama_model_show, ollama_pull_model, ollama_server_version,
    ollama_warm_model, OllamaServerState,
};
pub use web_fetch::{work_fetch_url_logic, WorkFetchContent};
pub use work_fs::{
    fs_list_dirs_logic, is_scratch_workspace, work_canonical_root, work_delete_logic,
    work_ensure_scratch_workspace_logic, work_exec_logic, work_grep_logic, work_import_file_logic,
    work_join, work_join_allow_temp, work_list_dir_logic, work_read_chunk_logic,
    work_read_file_logic, work_read_image_logic, work_rename_logic, work_tree_logic,
    work_validate_root_logic, work_write_file_logic, WorkDirEntry, WorkExecResult, WorkFileContent,
    WorkGrepHit, WorkImageContent, WorkTreeEntry,
};
pub(crate) use work_fs::{
    full_disk_scan_error, refuse_full_disk_scan, trunc_out, work_display_string,
};
#[cfg(not(windows))]
pub(crate) use work_fs::work_exec_path;
pub use bg_exec::{
    bg_kill, bg_kill_running, bg_list, bg_snapshot, bg_start, BgExecState, BgJobStatus,
    BgJobSummary,
};
pub use agent_shell::{
    agent_shell_exec, agent_shell_input, agent_shell_interrupt, agent_shell_status,
    kill_agent_shells, AgentShellState, ShellRun,
};
pub(crate) use agent_shell::AGENT_NONINTERACTIVE_ENV;
pub use work_undo::{
    undo_begin_turn, undo_capture_prev, undo_key, undo_record, undo_record_write, work_redo_logic,
    work_undo_logic, workspace_git_note, UndoOp, UndoState, UndoTurn,
};
pub use terminal::{
    term_kill_all, term_kill_logic, term_resize_logic, term_spawn_logic, term_write_logic,
    TermEvent, TermState,
};
pub(crate) use terminal::{kill_job, kill_pid};
pub use updates_uploads::{
    app_build_info_logic, downloads_dir, github_download_asset_logic, github_latest_release_logic,
    open_installer, parse_release_info, upload_read_file_logic, upload_stage_file_logic,
    UpdateAsset,
};
pub(crate) use tauri_commands::{ChatSaveRow, MsgSaveRow};
pub use app_shell::app_title;
#[cfg(all(feature = "desktop", target_os = "macos"))]
pub(crate) use app_shell::install_app_menu;
#[cfg(feature = "desktop")]
pub(crate) use app_shell::{request_quit, tray_menu, AppQuitState, ScheduleState, ZoomState};

#[cfg(feature = "desktop")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Finder/Dock launches get launchd's minimal PATH: read the user's shell
    // PATH in the background so engine detection sees what a terminal sees.
    exe_resolve::warm_up();
    let builder = tauri::Builder::default();
    // Must be the first plugin: a second launch exits before anything else
    // starts and hands its args (incl. vicuna:// links, via the deep-link
    // feature) to this instance, which un-hides (tray) / restores / focuses
    // the existing window.
    #[cfg(any(target_os = "windows", target_os = "linux"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
        }
    }));
    builder
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_sql::Builder::default().build())
        .plugin(tauri_plugin_deep_link::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        // Signed in-app updates (same flow as cojudge): the frontend checks
        // `plugins.updater.endpoints` (latest.json on the newest GitHub
        // release), installs, then relaunches via the process plugin.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .manage(OllamaServerState::default())
        .manage(TermState::default())
        .manage(BgExecState::default())
        .manage(AgentShellState::default())
        .manage(UndoState::default())
        .manage(work_agent::WorkAgentState::default())
        .manage(acp::AcpState::default())
        .manage(AppQuitState::default())
        .manage(ScheduleState::default())
        .manage(browser::shared())
        .manage(mcp::McpManager::default())
        .manage(ZoomState {
            factor: Mutex::new(1.0),
        })
        .invoke_handler(tauri::generate_handler![
            tauri_commands::greet,
            tauri_commands::chats_save,
            ollama::ollama_list_models,
            ollama::ollama_list_running,
            ollama::ollama_warm_model,
            ollama::ollama_chat_stream,
            ollama::ollama_generate_stream,
            ollama::ollama_pull_model,
            ollama::ollama_delete_model,
            ollama::ollama_model_show,
            ollama::ollama_fetch_text,
            ollama::ollama_server_version,
            ollama::ollama_start_server,
            ollama::ollama_stop_server,
            work_fs::work_validate_root,
            work_fs::work_ensure_scratch_workspace,
            work_fs::work_list_dir,
            work_fs::work_tree,
            work_fs::work_import_file,
            work_fs::work_rename,
            work_fs::work_delete,
            updates_uploads::upload_stage_file,
            updates_uploads::upload_read_file,
            updates_uploads::app_build_info,
            updates_uploads::github_latest_release,
            updates_uploads::github_download_asset,
            work_fs::work_read_file,
            work_fs::work_read_image,
            work_fs::work_grep,
            work_fs::work_write_file,
            work_fs::work_open_path,
            tauri_commands::git_status,
            tauri_commands::git_diff,
            tauri_commands::git_stage,
            tauri_commands::git_unstage,
            tauri_commands::git_discard,
            tauri_commands::git_apply,
            work_fs::fs_list_dirs,
            app_shell::open_server_window,
            app_shell::open_chat_window,
            app_shell::capture_scheme_url,
            tauri_commands::server_list,
            tauri_commands::server_connect,
            tauri_commands::server_touch,
            tauri_commands::server_remove,
            tauri_commands::server_rename,
            tauri_commands::server_call,
            tauri_commands::server_download_file,
            tauri_commands::server_lb_proxy_url,
            tauri_commands::server_base_url,
            work_fs::work_exec,
            web_fetch::work_fetch_url,
            bg_exec::work_exec_start,
            bg_exec::work_exec_status,
            bg_exec::work_exec_list,
            bg_exec::work_exec_kill,
            agent_shell::work_shell_exec,
            agent_shell::work_shell_status,
            agent_shell::work_shell_input,
            agent_shell::work_shell_interrupt,
            work_undo::work_undo,
            work_undo::work_redo,
            cli_launcher::cli_status,
            cli_launcher::cli_install,
            cli_launcher::cli_uninstall,
            app_shell::show_main_window,
            terminal::term_spawn,
            terminal::term_write,
            terminal::term_resize,
            terminal::term_kill,
            app_shell::zoom_apply,
            app_shell::zoom_step,
            app_shell::quit_app,
            app_shell::tray_set_status,
            tauri_commands::mcp_list_servers,
            tauri_commands::mcp_set_defaults,
            tauri_commands::mcp_save_server,
            tauri_commands::mcp_delete_server,
            tauri_commands::mcp_connect,
            tauri_commands::mcp_disconnect,
            tauri_commands::mcp_list_tools,
            tauri_commands::mcp_call_tool,
            tauri_commands::mcp_signin,
            tauri_commands::mcp_signout,
            tauri_commands::mcp_login_present,
            tauri_commands::skills_list,
            tauri_commands::skills_read,
            tauri_commands::skills_save,
            tauri_commands::skills_set_defaults,
            tauri_commands::skills_remove,
            tauri_commands::browser_status,
            tauri_commands::browser_start,
            tauri_commands::browser_stop,
            tauri_commands::browser_restart,
            tauri_commands::browser_takeover,
            tauri_commands::browser_ensure_engine,
            tauri_commands::browser_mcp_command,
            work_agent::work_agent_start,
            work_agent::work_agent_stop,
            work_agent::work_agent_turn_live,
            work_agent::work_agent_queue,
            work_agent::work_agent_remove,
            work_agent::work_agent_answer,
            tauri_commands::work_engine_info,
            opencode::opencode_models,
            opencode::opencode_set_model,
            work_engine::work_engine_select,
            work_engine::work_models,
            work_engine::work_set_model,
            work_engine::work_set_approval,
            work_engine::work_set_send_now,
            work_usage::work_usage
        ])
        .setup(|app| {
            // The main window's backend: the same server `vicuna serve` runs,
            // embedded here (or a `vicuna serve` already running on this
            // machine). The window reaches it as server "local"; the CLI's
            // default target is the same server. Then, once, the chats this
            // app kept in its own sqlite move over (read-only on that file).
            {
                let resources = app.path().resource_dir().ok();
                let desktop_db = app.path().app_config_dir().ok().map(|d| d.join("vicuna.db"));
                tauri::async_runtime::spawn(server::embedded::launch(resources, desktop_db));
                let opener = app.handle().clone();
                server::set_url_opener(move |url| {
                    use tauri_plugin_opener::OpenerExt;
                    opener.opener().open_url(url, None::<&str>).map_err(|e| format!("mcp: cannot open browser for sign-in: {e}"))
                });
            }
            // Warm the long-lived `opencode acp` process so the first Work
            // turn skips its cold start (no-op when opencode is missing).
            {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    handle.state::<acp::AcpState>().prewarm().await;
                });
            }
            // Debug builds say so in the window title (`npm run desktop:dev`
            // → `tauri dev`), so a dev window can't be mistaken for the
            // installed Vicuna.App on the Mac. Release builds keep "Vicuna".
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_title(app_title());
            }
            // System tray: closing the window hides here (streams keep
            // running); only Quit exits. The menu opens on either click;
            // Show reopens the window.
            #[cfg(target_os = "macos")]
            let _ = install_app_menu(app.handle());
            let menu = tray_menu(app, "No running sessions", None, &[])?;
            if let Some(icon) = app.default_window_icon().cloned() {
                TrayIconBuilder::with_id("main")
                    .icon(icon)
                    .tooltip(app_title())
                    .menu(&menu)
                    .show_menu_on_left_click(true)
                    .on_menu_event(|app, event| match event.id.as_ref() {
                        "show" => {
                            if let Some(w) = app.get_webview_window("main") {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                        "quit" => request_quit(app),
                        _ => {}
                    })
                    .build(app)?;
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Hide to the tray instead of quitting so the backend (streams,
            // terminals, self-started ollama) survives a window close.
            // Only the main window hides; server windows (open_server_window)
            // are just views of a remote server and close for real.
            if let WindowEvent::CloseRequested { api, .. } = event {
                if window.label() != "main" {
                    return;
                }
                let allow = window
                    .app_handle()
                    .try_state::<AppQuitState>()
                    .and_then(|s| s.allow.lock().ok().map(|g| *g))
                    .unwrap_or(false);
                if !allow {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .build(tauri::generate_context!())
        .expect("error while running tauri application")
        .run(on_run_event);
}

/// App event handler:
/// - `RunEvent::Exit`: tear down owned children (browser sidecar, embedded
///   server, terminals, etc.) even when the app exits without going through
///   `quit_now` (e.g., macOS Dock Quit, OS logout/shutdown, or `app.exit`).
/// - `RunEvent::Reopen` (macOS only): dock click with the window hidden reopens it.
#[cfg(feature = "desktop")]
#[allow(unused_variables)]
fn on_run_event(app: &AppHandle, event: tauri::RunEvent) {
    match event {
        tauri::RunEvent::Exit => {
            app_shell::shutdown_children(app);
        }
        #[cfg(target_os = "macos")]
        tauri::RunEvent::Reopen { .. } => {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.show();
                let _ = w.set_focus();
            }
        }
        _ => {}
    }
}

