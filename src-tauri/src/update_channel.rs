// Desktop update channel (Stable / Dev). Stable reads `latest.json` from the
// newest versioned GitHub release; Dev reads the one the rolling `latest`
// prerelease carries (rebuilt on every push to main, version
// `<maj>.<min>.<patch+1>-dev.<YYYYMMDDHHMMSS>` so it sorts above the stable it
// came from and below the next stable). The channel lives in
// `~/vicuna/config/app-shell.json` beside `showTrayIcon`.
//
// The JS updater `check()` can't change endpoints, so checks/installs run here
// with `updater_builder().endpoints(..)`; the frontend keeps its own prompt,
// progress and relaunch flow (src/lib/appUpdateState.svelte.ts).

use std::path::Path;

pub const STABLE_ENDPOINT: &str = "https://github.com/anslwy/vicuna/releases/latest/download/latest.json";
pub const DEV_ENDPOINT: &str = "https://github.com/anslwy/vicuna/releases/download/latest/latest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateChannel {
    Stable,
    Dev,
}

impl UpdateChannel {
    /// Anything but `"dev"` is Stable, so a hand-edited typo never opts a
    /// user into prereleases.
    pub fn parse(s: Option<&str>) -> Self {
        match s.map(|v| v.trim().to_ascii_lowercase()) {
            Some(v) if v == "dev" => Self::Dev,
            _ => Self::Stable,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Dev => "dev",
        }
    }
    pub fn endpoint(self) -> &'static str {
        match self {
            Self::Stable => STABLE_ENDPOINT,
            Self::Dev => DEV_ENDPOINT,
        }
    }
}

/// Offer `remote` over `current`? Newer on either channel; on Stable also the
/// stable release while running a dev prerelease, so switching Dev → Stable
/// can reinstall it right away (`0.1.1` over `0.1.2-dev.<ts>`). Never an
/// older stable over a stable.
pub fn should_offer_update(channel: UpdateChannel, current: &str, remote: &str) -> bool {
    crate::version::should_offer_update(channel == UpdateChannel::Stable, current, remote)
}

pub(crate) fn read_channel_in(home: &Path) -> UpdateChannel {
    UpdateChannel::parse(crate::app_shell::read_app_shell_config_in(home).update_channel.as_deref())
}

pub(crate) fn write_channel_in(home: &Path, channel: UpdateChannel) -> Result<(), String> {
    let mut cfg = crate::app_shell::read_app_shell_config_in(home);
    cfg.update_channel = Some(channel.as_str().to_string());
    crate::app_shell::write_app_shell_config_in(home, &cfg)
}

#[cfg(feature = "desktop")]
pub(crate) use desktop::*;

#[cfg(feature = "desktop")]
mod desktop {
    use super::*;
    use std::sync::Mutex;
    use tauri::ipc::Channel;
    use tauri::{AppHandle, State};
    use tauri_plugin_updater::{Update, UpdaterExt};

    fn home() -> Result<std::path::PathBuf, String> {
        crate::opencode::home_dir().ok_or_else(|| "cannot locate home folder".to_string())
    }

    fn current_channel() -> UpdateChannel {
        home().map(|h| read_channel_in(&h)).unwrap_or(UpdateChannel::Stable)
    }

    /// The last update found by `app_check_update` (owns a native handle).
    #[derive(Default)]
    pub(crate) struct PendingUpdate(pub(crate) Mutex<Option<Update>>);

    #[tauri::command]
    pub(crate) async fn app_get_update_channel() -> Result<String, String> {
        Ok(current_channel().as_str().to_string())
    }

    #[tauri::command]
    pub(crate) async fn app_set_update_channel(
        pending: State<'_, PendingUpdate>,
        channel: String,
    ) -> Result<String, String> {
        let ch = match channel.trim() {
            "stable" => UpdateChannel::Stable,
            "dev" => UpdateChannel::Dev,
            other => return Err(format!("unknown update channel {other:?} (stable or dev)")),
        };
        write_channel_in(&home()?, ch)?;
        // an update found on the other channel is stale now
        if let Ok(mut g) = pending.0.lock() {
            *g = None;
        }
        Ok(ch.as_str().to_string())
    }

    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    pub(crate) struct UpdateInfo {
        version: String,
        current_version: String,
        body: Option<String>,
        date: Option<String>,
        target: String,
        channel: &'static str,
    }

    /// Check the saved channel's manifest. `None` = up to date.
    #[tauri::command]
    pub(crate) async fn app_check_update(
        app: AppHandle,
        pending: State<'_, PendingUpdate>,
    ) -> Result<Option<UpdateInfo>, String> {
        let ch = current_channel();
        let url = ch.endpoint().parse().map_err(|e| format!("bad endpoint: {e}"))?;
        let updater = app
            .updater_builder()
            .endpoints(vec![url])
            .map_err(|e| e.to_string())?
            .version_comparator(move |current, release| {
                should_offer_update(ch, &current.to_string(), &release.version.to_string())
            })
            .build()
            .map_err(|e| e.to_string())?;
        let found = updater.check().await.map_err(|e| e.to_string())?;
        let info = found.as_ref().map(|u| UpdateInfo {
            version: u.version.clone(),
            current_version: u.current_version.clone(),
            body: u.body.clone(),
            date: u.date.map(|d| d.to_string()),
            target: u.target.clone(),
            channel: ch.as_str(),
        });
        *pending.0.lock().map_err(|_| "update state poisoned".to_string())? = found;
        Ok(info)
    }

    /// Download + install the pending update, streaming the same
    /// `Started`/`Progress`/`Finished` events the JS plugin emits.
    #[tauri::command]
    pub(crate) async fn app_install_update(
        pending: State<'_, PendingUpdate>,
        on_event: Channel<serde_json::Value>,
    ) -> Result<(), String> {
        let update = pending
            .0
            .lock()
            .map_err(|_| "update state poisoned".to_string())?
            .take()
            .ok_or_else(|| "no update pending: check for updates first".to_string())?;
        let mut started = false;
        let ev = on_event.clone();
        let res = update
            .download_and_install(
                |chunk, total| {
                    if !started {
                        started = true;
                        let _ = ev.send(serde_json::json!({ "event": "Started", "data": { "contentLength": total } }));
                    }
                    let _ = ev.send(serde_json::json!({ "event": "Progress", "data": { "chunkLength": chunk } }));
                },
                || {
                    let _ = on_event.send(serde_json::json!({ "event": "Finished" }));
                },
            )
            .await;
        if let Err(e) = res {
            // keep the handle so Retry works without a new check
            if let Ok(mut g) = pending.0.lock() {
                *g = Some(update);
            }
            return Err(e.to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("vicuna-upd-ch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn dev_to_stable_switch_is_offered_but_never_a_stable_downgrade() {
        use UpdateChannel::*;
        let cases = [
            (Stable, "0.1.2-dev.20261128155414", "0.1.1", true),
            (Stable, "0.1.1", "0.1.1", false),
            (Stable, "0.1.1", "0.1.0", false),
            (Stable, "0.1.1", "0.1.2", true),
            (Dev, "0.1.1", "0.1.2-dev.20261128155414", true),
            (Dev, "0.1.2-dev.20261128155414", "0.1.2-dev.20261128151100", false),
            (Dev, "0.1.2-dev.20261128151100", "0.1.2-dev.20261128155414", true),
            (Dev, "0.1.2-dev.20261128155414", "0.1.1", false),
        ];
        for (ch, cur, remote, want) in cases {
            assert_eq!(should_offer_update(ch, cur, remote), want, "{ch:?} {cur} -> {remote}");
        }
    }

    #[test]
    fn channel_parse_and_endpoints() {
        assert_eq!(UpdateChannel::parse(None), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::parse(Some("dev")), UpdateChannel::Dev);
        assert_eq!(UpdateChannel::parse(Some(" DEV ")), UpdateChannel::Dev);
        assert_eq!(UpdateChannel::parse(Some("devv")), UpdateChannel::Stable);
        assert_eq!(UpdateChannel::Stable.endpoint(), "https://github.com/anslwy/vicuna/releases/latest/download/latest.json");
        assert_eq!(UpdateChannel::Dev.endpoint(), "https://github.com/anslwy/vicuna/releases/download/latest/latest.json");
    }

    #[test]
    fn channel_defaults_to_stable_and_round_trips_without_touching_tray() {
        let home = scratch("rt");
        assert_eq!(read_channel_in(&home), UpdateChannel::Stable);
        crate::app_shell::write_tray_enabled_in(&home, true).unwrap();
        write_channel_in(&home, UpdateChannel::Dev).unwrap();
        assert_eq!(read_channel_in(&home), UpdateChannel::Dev);
        assert!(crate::app_shell::read_tray_enabled_in(&home), "tray pref kept");
        let raw = std::fs::read_to_string(home.join("vicuna/config/app-shell.json")).unwrap();
        assert!(raw.contains("\"updateChannel\": \"dev\""), "{raw}");
        write_channel_in(&home, UpdateChannel::Stable).unwrap();
        assert_eq!(read_channel_in(&home), UpdateChannel::Stable);
        let _ = std::fs::remove_dir_all(&home);
    }
}
