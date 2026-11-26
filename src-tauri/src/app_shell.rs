use std::sync::Mutex;
#[cfg(feature = "desktop")]
use tauri::menu::{IsMenuItem, Menu, MenuItem, PredefinedMenuItem};
#[cfg(feature = "desktop")]
use tauri::State;
#[cfg(feature = "desktop")]
use tauri::{AppHandle, Emitter, Manager, Runtime};

#[cfg(feature = "desktop")]
use crate::{
    agent_shell::{kill_agent_shells, AgentShellState},
    bg_exec::{bg_kill_running, BgExecState},
    browser,
    ollama::OllamaServerState,
    server, servers,
    terminal::{kill_pid, TermState},
};

/// Bring the main window forward (CLI deep links arrive while hidden in the
/// tray; the OS activates the app but doesn't un-hide the window).
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn show_main_window(app: AppHandle) -> Result<(), String> {
    let w = app
        .get_webview_window("main")
        .ok_or_else(|| "no main window".to_string())?;
    w.show().map_err(|e| format!("cannot show window: {e}"))?;
    w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
    Ok(())
}

/// Open (or focus) the window for a remote server: each server gets its own
/// window — the frontend reads `?server=<id>` once at load and routes every
/// backend call there (src/lib/bridge.ts) — so switching servers never
/// replaces or interrupts what the main window (This computer) is doing.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn open_server_window(app: AppHandle, id: String, title: Option<String>) -> Result<(), String> {
    open_server_window_impl(&app, &id, title.as_deref(), None)
}

#[cfg(feature = "desktop")]
fn open_server_window_impl(app: &AppHandle, id: &str, title: Option<&str>, chat: Option<&str>) -> Result<(), String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("bad server id".into());
    }
    let resolved_name = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or_else(|| servers::load().into_iter().find(|s| s.id == id).map(|s| s.name))
        .filter(|n| !n.trim().is_empty());
    let label = format!("server-{id}");
    if let Some(w) = app.get_webview_window(&label) {
        if let Some(name) = resolved_name.as_deref() {
            let _ = w.set_title(&format!("{} — {name}", app_title()));
        }
        let _ = w.unminimize();
        w.show().map_err(|e| format!("cannot show window: {e}"))?;
        w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
        return Ok(());
    }
    let name = resolved_name.as_deref().unwrap_or(id);
    let mut url = format!("index.html?server={id}");
    if let Some(c) = chat.filter(|c| !c.is_empty()) {
        url.push_str(&format!("&chat={c}"));
    }
    tauri::WebviewWindowBuilder::new(app, &label, tauri::WebviewUrl::App(url.into()))
        .title(format!("{} — {name}", app_title()))
        .inner_size(1200.0, 800.0)
        .build()
        .map_err(|e| format!("cannot open window: {e}"))?;
    Ok(())
}

/// Open a session from the search palette on the host it lives on: raise that
/// host's window (a server's own window, or the main window for This computer)
/// and have it navigate to `chat_id`. A fresh server window gets `?chat=`, an
/// already-open window (whose URL must not reload mid-turn) an event instead.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn open_chat_window(app: AppHandle, server_id: Option<String>, chat_id: String, title: Option<String>) -> Result<(), String> {
    if chat_id.is_empty() || !chat_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("bad chat id".into());
    }
    let label = match server_id.as_deref() {
        Some(id) => {
            let label = format!("server-{id}");
            if app.get_webview_window(&label).is_none() {
                // fresh window: the URL carries the session, no event needed
                return open_server_window_impl(&app, id, title.as_deref(), Some(&chat_id));
            }
            label
        }
        None => "main".to_string(),
    };
    if let Some(w) = app.get_webview_window(&label) {
        let _ = w.unminimize();
        w.show().map_err(|e| format!("cannot show window: {e}"))?;
        w.set_focus().map_err(|e| format!("cannot focus window: {e}"))?;
    }
    app.emit_to(label.as_str(), "vicuna-open-chat", serde_json::json!({ "chatId": chat_id }))
        .map_err(|e| format!("cannot notify window: {e}"))?;
    Ok(())
}

/// Schemes a capture window may never claim: web, app-internal and the
/// dangerous ones. Plugins name their own custom scheme (e.g. `signalcaptcha`).
const CAPTURE_BLOCKED_SCHEMES: &[&str] = &[
    "http", "https", "file", "data", "blob", "javascript", "about", "ftp", "ws", "wss",
    "vicuna", "tauri", "ipc", "asset", "http+unix", "mailto", "tel",
];

/// Validate a capture request: an https page plus a plain custom scheme.
pub(crate) fn capture_target_ok(url: &str, scheme: &str) -> Result<(), String> {
    let scheme_ok = (2..=32).contains(&scheme.len())
        && scheme.starts_with(|c: char| c.is_ascii_lowercase())
        && scheme.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '+' | '-' | '.'))
        && !CAPTURE_BLOCKED_SCHEMES.contains(&scheme);
    if !scheme_ok {
        return Err("bad capture scheme".into());
    }
    if url.len() > 2048 || !url.starts_with("https://") || url.chars().any(|c| c.is_control() || c == ' ') {
        return Err("capture page must be an https URL".into());
    }
    Ok(())
}

/// Open `url` in a separate, permission-less window and wait until the page
/// navigates to `<scheme>:…` (e.g. Signal's CAPTCHA page redirecting to
/// `signalcaptcha://…`, which the OS would otherwise hand to another app).
/// The navigation is blocked, the window closes and the full captured URL is
/// returned. Closing the window cancels. One capture window at a time.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn capture_scheme_url(app: AppHandle, url: String, scheme: String, title: Option<String>) -> Result<String, String> {
    use std::sync::Arc;
    capture_target_ok(&url, &scheme)?;
    let parsed: tauri::Url = url.parse().map_err(|e| format!("bad capture URL: {e}"))?;
    const LABEL: &str = "capture";
    if let Some(w) = app.get_webview_window(LABEL) {
        let _ = w.destroy();
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<Result<String, String>>();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let send = {
        let tx = tx.clone();
        move |r: Result<String, String>| {
            if let Some(s) = tx.lock().ok().and_then(|mut g| g.take()) {
                let _ = s.send(r);
            }
        }
    };
    let nav_send = send.clone();
    let nav_app = app.clone();
    let window = tauri::WebviewWindowBuilder::new(&app, LABEL, tauri::WebviewUrl::External(parsed))
        .title(title.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| app_title().to_string()))
        .inner_size(520.0, 720.0)
        .on_navigation(move |u| {
            if u.scheme() == scheme {
                nav_send(Ok(u.to_string()));
                let app = nav_app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(w) = app.get_webview_window(LABEL) {
                        let _ = w.destroy();
                    }
                });
                return false;
            }
            // stay on the web; never hand other schemes to the OS
            matches!(u.scheme(), "https" | "about" | "blob" | "data")
        })
        .build()
        .map_err(|e| format!("cannot open window: {e}"))?;
    let close_send = send.clone();
    window.on_window_event(move |e| {
        if matches!(e, tauri::WindowEvent::Destroyed) {
            close_send(Err("closed".into()));
        }
    });
    rx.await.unwrap_or_else(|_| Err("closed".into()))
}

#[cfg(test)]
mod capture_tests {
    use super::capture_target_ok;

    #[test]
    fn capture_requests_are_validated() {
        assert!(capture_target_ok("https://signalcaptchas.org/registration/generate.html", "signalcaptcha").is_ok());
        assert!(capture_target_ok("http://x.org", "signalcaptcha").is_err());
        assert!(capture_target_ok("https://x.org/a b", "signalcaptcha").is_err());
        for bad in ["https", "file", "javascript", "vicuna", "tauri", "Upper", "a", "1abc", "x/y"] {
            assert!(capture_target_ok("https://x.org", bad).is_err(), "{bad}");
        }
    }
}

/// Window zoom level for Cmd/Ctrl +/−/0. Desktop shells don't zoom webviews
/// on those keys the way browsers do natively, so the frontend catches the
/// chord (Tauri only) and drives the webview zoom from here.
pub(crate) struct ZoomState {    pub(crate) factor: Mutex<f64>,
}

fn clamp_zoom(f: f64) -> f64 {
    f.clamp(0.5, 3.0)
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn zoom_apply(
    window: tauri::WebviewWindow,
    state: State<'_, ZoomState>,
    factor: f64,
) -> Result<f64, String> {
    let factor = clamp_zoom(factor);
    window
        .set_zoom(factor)
        .map_err(|e| format!("zoom failed: {e}"))?;
    state
        .factor
        .lock()
        .map(|mut f| {
            *f = factor;
            *f
        })
        .map_err(|_| "zoom state poisoned".to_string())
}

#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn zoom_step(
    window: tauri::WebviewWindow,
    state: State<'_, ZoomState>,
    direction: String,
) -> Result<f64, String> {
    let current = *state
        .factor
        .lock()
        .map_err(|_| "zoom state poisoned".to_string())?;
    let base = if current <= 0.0 { 1.0 } else { current };
    let next = match direction.as_str() {
        "in" => base * 1.15,
        "out" => base / 1.15,
        _ => 1.0,
    };
    zoom_apply(window, state, next).await
}

/// Whether a window close is a real quit (tray / Settings) or just a hide.
/// Closing the main window hides to the tray so streams keep running;
/// only an explicit Quit sets this and lets the app (and everything it
/// started) exit.
#[derive(Default)]
pub(crate) struct AppQuitState {
    pub(crate) allow: Mutex<bool>,
}

/// Kill everything Vicuna started: a self-launched `ollama serve` plus all
/// terminal ptys. Best-effort — never fails the quit path.
#[cfg(feature = "desktop")]
pub(crate) fn shutdown_children(app: &AppHandle) {
    server::embedded::shutdown();
    if let Some(s) = app.try_state::<std::sync::Arc<browser::BrowserState>>() {
        browser::kill_owned(&s);
    }
    if let Some(s) = app.try_state::<OllamaServerState>() {
        if let Ok(mut g) = s.child.lock() {
            if let Some(mut child) = g.take() {
                let _ = child.kill();
            }
        }
    }
    if let Some(s) = app.try_state::<TermState>() {
        if let Ok(mut terms) = s.terms.lock() {
            for (_, h) in terms.drain() {
                if let Some(pid) = h.pid {
                    kill_pid(pid);
                }
            }
        }
    }
    if let Some(s) = app.try_state::<BgExecState>() {
        bg_kill_running(&s);
    }
    if let Some(s) = app.try_state::<AgentShellState>() {
        kill_agent_shells(&s);
    }
}

#[cfg(feature = "desktop")]
pub(crate) fn allow_quit(app: &AppHandle) {
    if let Some(s) = app.try_state::<AppQuitState>() {
        if let Ok(mut g) = s.allow.lock() {
            *g = true;
        }
    }
}

/// What the frontend last reported about scheduled sends. They only go out
/// while the app runs, so a quit request with some pending asks first.
#[derive(Default)]
pub(crate) struct ScheduleState {
    count: Mutex<usize>,
}

#[cfg(feature = "desktop")]
pub(crate) fn quit_now(app: &AppHandle) {
    allow_quit(app);
    shutdown_children(app);
    app.exit(0);
}

/// Quit asked from the tray menu or the app menu (Cmd+Q). With scheduled
/// messages pending, the window comes up and the frontend shows the list
/// with a warning ("Quit anyway" then calls `quit_app`); otherwise quit.
#[cfg(feature = "desktop")]
pub(crate) fn request_quit(app: &AppHandle) {
    let pending = app
        .try_state::<ScheduleState>()
        .and_then(|s| s.count.lock().ok().map(|g| *g))
        .unwrap_or(0);
    if pending > 0 {
        if let Some(w) = app.get_webview_window("main") {
            let _ = w.unminimize();
            let _ = w.show();
            let _ = w.set_focus();
            if app.emit("vicuna-quit-requested", pending).is_ok() {
                return;
            }
        }
    }
    quit_now(app);
}

/// macOS app menu: the stock Quit item terminates the process directly, so
/// it is swapped for one that goes through `request_quit` (same id as the
/// tray's Quit; the tray's menu handler receives every menu event).
#[cfg(feature = "desktop")]
#[cfg(target_os = "macos")]
pub(crate) fn install_app_menu(app: &AppHandle) -> tauri::Result<()> {
    use tauri::menu::MenuItemKind;
    let menu = Menu::default(app)?;
    if let Some(MenuItemKind::Submenu(app_sub)) = menu.items()?.into_iter().next() {
        for item in app_sub.items()? {
            if let MenuItemKind::Predefined(p) = item {
                // muda labels it "&Quit" (the app name is added at draw time)
                if p
                    .text()
                    .map(|t| t.trim_start_matches('&').starts_with("Quit"))
                    .unwrap_or(false)
                {
                    app_sub.remove(&p)?;
                    let quit =
                        MenuItem::with_id(app, "quit", "Quit Vicuna", true, Some("CmdOrCtrl+Q"))?;
                    app_sub.append(&quit)?;
                    break;
                }
            }
        }
    }
    app.set_menu(menu)?;
    Ok(())
}

/// Explicit quit (Settings button, or "Quit anyway" in the scheduled-messages
/// prompt): kill everything Vicuna started, then exit. Window-close hides to the tray instead (see
/// `on_window_event` below) so background streams survive.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn quit_app(
    app: AppHandle,
    _ollama: State<'_, OllamaServerState>,
    _terms: State<'_, TermState>,
) -> Result<(), String> {
    quit_now(&app);
    Ok(())
}

/// Build the tray menu: a disabled live-session status line ("No running
/// sessions" / "N running work session(s)"), a separator, Show / Quit —
/// plus, while sessions run, a second separator and a disabled elapsed line
/// ("32m 25s") under Quit. With scheduled messages pending, a disabled
/// section ("2 scheduled messages" + one row each) sits under the status
/// line. Rebuilt on every frontend report.
#[cfg(feature = "desktop")]
pub(crate) fn tray_menu<R: Runtime>(
    manager: &impl Manager<R>,
    status: &str,
    elapsed: Option<&str>,
    scheduled: &[String],
) -> tauri::Result<Menu<R>> {
    let status_item = MenuItem::with_id(manager, "status", status, false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(manager)?;
    let show = MenuItem::with_id(manager, "show", "Show Vicuna", true, None::<&str>)?;
    let quit = MenuItem::with_id(manager, "quit", "Quit Vicuna", true, None::<&str>)?;
    let sep2 = PredefinedMenuItem::separator(manager)?;
    let elapsed_item = match elapsed {
        Some(e) => Some(MenuItem::with_id(manager, "elapsed", e, false, None::<&str>)?),
        None => None,
    };
    let sched_sep = PredefinedMenuItem::separator(manager)?;
    let mut sched_items = Vec::new();
    for (i, line) in scheduled.iter().enumerate() {
        sched_items.push(MenuItem::with_id(
            manager,
            format!("scheduled-{i}"),
            line,
            false,
            None::<&str>,
        )?);
    }
    let mut items: Vec<&dyn IsMenuItem<R>> = vec![&status_item as &dyn IsMenuItem<R>];
    if !sched_items.is_empty() {
        items.push(&sched_sep as &dyn IsMenuItem<R>);
        for it in &sched_items {
            items.push(it as &dyn IsMenuItem<R>);
        }
    }
    items.extend([
        &sep as &dyn IsMenuItem<R>,
        &show as &dyn IsMenuItem<R>,
        &quit as &dyn IsMenuItem<R>,
    ]);
    if let Some(ref e) = elapsed_item {
        items.push(&sep2 as &dyn IsMenuItem<R>);
        items.push(e as &dyn IsMenuItem<R>);
    }
    Menu::with_items(manager, &items)
}

/// Refresh the tray menu (no-op when the tray is unavailable).
#[cfg(feature = "desktop")]
pub(crate) fn refresh_tray_menu(
    app: &AppHandle,
    status: &str,
    elapsed: Option<&str>,
    scheduled: &[String],
) -> Result<(), String> {
    if let Some(tray) = app.tray_by_id("main") {
        let menu = tray_menu(app, status, elapsed, scheduled)
            .map_err(|e| format!("tray menu failed: {e}"))?;
        tray.set_menu(Some(menu))
            .map_err(|e| format!("tray menu failed: {e}"))?;
    }
    Ok(())
}

/// Update the tray menu: status line plus, while sessions run, an elapsed
/// line under Quit. The frontend owns streaming state, so it reports both
/// whenever relevant. Best-effort: never fails a turn.
#[cfg(feature = "desktop")]
#[tauri::command]
pub(crate) async fn tray_set_status(
    app: AppHandle,
    status: String,
    elapsed: Option<String>,
    scheduled: Option<Vec<String>>,
    scheduled_count: Option<usize>,
) -> Result<(), String> {
    // header row first ("2 scheduled messages"), then one row per message
    let scheduled: Vec<String> = scheduled
        .unwrap_or_default()
        .into_iter()
        .map(|l| l.trim().chars().take(80).collect::<String>())
        .filter(|l| !l.is_empty())
        .take(8)
        .collect();
    // the count gates the quit prompt, so it is stored even without a tray
    if let Some(s) = app.try_state::<ScheduleState>() {
        if let Ok(mut g) = s.count.lock() {
            *g = scheduled_count.unwrap_or(0);
        }
    }
    let short: String = status.trim().chars().take(80).collect();
    let elapsed: Option<String> = elapsed
        .map(|e| e.trim().chars().take(20).collect::<String>())
        .filter(|e| !e.is_empty());
    refresh_tray_menu(
        &app,
        if short.is_empty() {
            "No running sessions"
        } else {
            &short
        },
        elapsed.as_deref(),
        &scheduled,
    )
}

/// Window/tray title of the running shell. `tauri dev` builds with
/// `debug_assertions`, and so does a local `cargo build`, so debug builds are
/// labelled "Vicuna (Dev)": on macOS a dev window is otherwise
/// indistinguishable from the installed Vicuna.App. The frontend `<title>` in
/// `app.html` stays plain "Vicuna" on purpose — `scripts/desktop-dev.sh`
/// probes for it to tell our own dev server from a foreign :1420 occupant.
pub fn app_title() -> &'static str {
    if cfg!(debug_assertions) {
        "Vicuna (Dev)"
    } else {
        "Vicuna"
    }
}
