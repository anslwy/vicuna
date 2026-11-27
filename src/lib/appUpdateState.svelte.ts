// Signed in-app desktop updates (tauri-plugin-updater): automatic and manual
// checks, the update prompt, skip/remind preferences, download progress and
// relaunch. Created once by ChatApp.
import { type AppUpdateInfo, type AppUpdateStatus, loadAppUpdatePreferenceFromStorage, persistAppUpdatePreference, APP_UPDATE_RELEASES_URL, normalizeUpdateErrorMessage, formatAppUpdateInstallError } from "./appUpdate";
import { isTauriRuntime } from "./ollama";
import { openExternal } from "./chatDom";
import { resolveAppUpdateReleaseNotes } from "./chatAppData";

export interface AppUpdateStateDeps {
  readonly flashNotice: (text: string, ttl?: number) => void;
  readonly updateError: string | null;
}

export function createAppUpdateState(deps: AppUpdateStateDeps) {
  // --- Signed in-app updates (Tauri updater; same flow as cojudge's
  // src/routes/+page.svelte: auto-check on startup + manual check +
  // download/install + relaunch). The manifest is latest.json on the newest
  // stable GitHub release (tauri.conf.json → plugins.updater.endpoints).
  // Plain-browser/bridge mode has no updater and keeps the download flow above.
  let showUpdatePrompt = $state(false);
  let availableAppUpdate = $state<AppUpdateInfo | null>(null);
  let appUpdateStatus = $state<AppUpdateStatus>("idle");
  let appUpdateError = $state<string | null>(null);
  let appUpdateProgress = $state<{ downloadedBytes: number; totalBytes: number | null } | null>(null);
  let skippedAppUpdateVersion = $state<string | null>(null);
  let autoCheckAppUpdatesEnabled = $state(true);
  let suppressAutoUpdatePromptUntilNextCheck = false;
  /** the plugin's Update handle (not reactive: it owns a native resource) */
  let updateObject: {
    version: string;
    currentVersion?: string;
    downloadAndInstall: (cb?: (event: unknown) => void) => Promise<void>;
  } | null = null;
  const appUpdateLabel = $derived(
    appUpdateStatus === "checking"
      ? "Checking for updates…"
      : appUpdateStatus === "installing"
        ? "Installing update…"
        : appUpdateStatus === "installed"
          ? "Restart to apply update"
          : appUpdateStatus === "available" && availableAppUpdate
            ? `Update to v${availableAppUpdate.version}`
            : "Check for updates"
  );
  function initializeAppUpdatePreference() {
    const stored = loadAppUpdatePreferenceFromStorage();
    skippedAppUpdateVersion = stored.skippedVersion;
    autoCheckAppUpdatesEnabled = stored.autoCheckEnabled;
  }
  function setAutoCheckAppUpdates(enabled: boolean) {
    autoCheckAppUpdatesEnabled = enabled;
    try {
      persistAppUpdatePreference({ skippedVersion: skippedAppUpdateVersion, autoCheckEnabled: enabled });
    } catch {
      /* storage unavailable: keep the in-memory choice */
    }
  }
  function closeUpdatePrompt() {
    showUpdatePrompt = false;
  }
  function skipAvailableAppUpdateVersion() {
    const version = availableAppUpdate?.version?.trim();
    if (version) {
      skippedAppUpdateVersion = version;
      try {
        persistAppUpdatePreference({ skippedVersion: version, autoCheckEnabled: autoCheckAppUpdatesEnabled });
      } catch {
        /* storage unavailable */
      }
    }
    suppressAutoUpdatePromptUntilNextCheck = false;
    closeUpdatePrompt();
  }
  function remindAboutAppUpdateLater() {
    suppressAutoUpdatePromptUntilNextCheck = true;
    closeUpdatePrompt();
  }
  async function triggerAutomaticAppUpdateCheck() {
    if (typeof navigator !== "undefined" && !navigator.onLine) return;
    if (!autoCheckAppUpdatesEnabled) return;
    if (appUpdateStatus === "checking" || appUpdateStatus === "installing" || appUpdateStatus === "installed") return;
    const suppressPrompt = suppressAutoUpdatePromptUntilNextCheck;
    suppressAutoUpdatePromptUntilNextCheck = false;
    await checkForAppUpdates({ source: "automatic", suppressAutoPrompt: suppressPrompt });
  }
  async function checkForAppUpdates(
    options: { source?: "manual" | "automatic"; suppressAutoPrompt?: boolean } = {}
  ) {
    const source = options.source ?? "manual";
    const suppressAutoPrompt = options.suppressAutoPrompt ?? false;
    if (!isTauriRuntime()) {
      void openExternal(APP_UPDATE_RELEASES_URL);
      return;
    }
    if (appUpdateStatus === "checking" || appUpdateStatus === "installing") return;

    const previousAvailable = availableAppUpdate;
    const previousStatus = appUpdateStatus;
    const previousError = appUpdateError;
    const previousUpdateObject = updateObject;

    appUpdateStatus = "checking";
    appUpdateError = null;
    appUpdateProgress = null;
    availableAppUpdate = null;
    updateObject = null;

    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        const rawTarget = (update as unknown as { rawJson?: { target?: unknown } }).rawJson?.target;
        const releaseNotes = await resolveAppUpdateReleaseNotes({
          body: (update as unknown as { body?: string | null }).body ?? null
        });
        availableAppUpdate = {
          version: update.version,
          currentVersion: update.currentVersion || "",
          notes: releaseNotes.notesPreview,
          publishedAt: (update as unknown as { date?: string | null }).date ?? null,
          target: typeof rawTarget === "string" ? rawTarget : "",
          releaseNotesUrl: releaseNotes.releaseNotesUrl
        };
        updateObject = update as unknown as typeof updateObject;
        appUpdateStatus = "available";
        if (source === "manual") {
          showUpdatePrompt = true;
        } else if (!suppressAutoPrompt && update.version !== skippedAppUpdateVersion) {
          showUpdatePrompt = true;
        } else {
          closeUpdatePrompt();
        }
      } else {
        appUpdateStatus = "up_to_date";
        closeUpdatePrompt();
        if (source === "manual") deps.flashNotice("Vicuna is already up to date.");
      }
    } catch (err) {
      console.error("Failed to check for app updates:", err);
      if (source === "automatic") {
        // background checks never surface errors (offline, no release yet…)
        availableAppUpdate = previousAvailable;
        appUpdateStatus = previousStatus;
        appUpdateError = previousError;
        updateObject = previousUpdateObject;
        return;
      }
      availableAppUpdate = null;
      updateObject = null;
      appUpdateStatus = "error";
      appUpdateError = normalizeUpdateErrorMessage(err);
      showUpdatePrompt = false;
    }
  }
  async function installAppUpdate() {
    if (appUpdateStatus === "installing" || appUpdateStatus === "installed") return;
    // The modal can outlive the in-memory updater handle: re-check instead
    // of silently doing nothing.
    if (!updateObject) {
      appUpdateError = null;
      await checkForAppUpdates({ source: "manual" });
      if (!updateObject || appUpdateStatus !== "available") return;
    }
    if (appUpdateStatus !== "available" && appUpdateStatus !== "error") return;
    const handle = updateObject;
    if (!handle) return;
    appUpdateStatus = "installing";
    appUpdateError = null;
    appUpdateProgress = { downloadedBytes: 0, totalBytes: null };
    // Keep the dialog visible so progress is seen; it stays dismissible and
    // the install continues in the background if hidden.
    showUpdatePrompt = true;
    try {
      await handle.downloadAndInstall((event) => {
        const name = (event as { event?: string })?.event;
        const data = (event as { data?: { chunkLength?: unknown; contentLength?: unknown } })?.data ?? {};
        if (name === "Started") {
          const total = typeof data.contentLength === "number" ? data.contentLength : null;
          appUpdateProgress = { downloadedBytes: 0, totalBytes: total };
        } else if (name === "Progress") {
          const chunk = typeof data.chunkLength === "number" ? data.chunkLength : 0;
          const prev = appUpdateProgress ?? { downloadedBytes: 0, totalBytes: null };
          appUpdateProgress = { downloadedBytes: prev.downloadedBytes + chunk, totalBytes: prev.totalBytes };
        }
      });
      appUpdateProgress = null;
      appUpdateStatus = "installed";
    } catch (err) {
      console.error("Failed to install app update:", err);
      appUpdateProgress = null;
      appUpdateStatus = "error";
      appUpdateError = formatAppUpdateInstallError(err);
    }
  }
  async function restartToApplyUpdate() {
    try {
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (err) {
      console.error("Failed to restart after installing update:", err);
      appUpdateError = normalizeUpdateErrorMessage(err);
    }
  }
  /** Settings → App updates button: reopen a known update (incl. one that
   *  is installing/installed/failed) instead of starting a conflicting check. */
  async function openAppUpdate() {
    if (
      availableAppUpdate &&
      (appUpdateStatus === "available" ||
        appUpdateStatus === "installing" ||
        appUpdateStatus === "installed" ||
        appUpdateStatus === "error")
    ) {
      showUpdatePrompt = true;
      return;
    }
    await checkForAppUpdates({ source: "manual" });
  }

  return {
    get showUpdatePrompt() { return showUpdatePrompt; },
    set showUpdatePrompt(v) { showUpdatePrompt = v; },
    get availableAppUpdate() { return availableAppUpdate; },
    set availableAppUpdate(v) { availableAppUpdate = v; },
    get appUpdateStatus() { return appUpdateStatus; },
    set appUpdateStatus(v) { appUpdateStatus = v; },
    get appUpdateError() { return appUpdateError; },
    set appUpdateError(v) { appUpdateError = v; },
    get appUpdateProgress() { return appUpdateProgress; },
    set appUpdateProgress(v) { appUpdateProgress = v; },
    get skippedAppUpdateVersion() { return skippedAppUpdateVersion; },
    set skippedAppUpdateVersion(v) { skippedAppUpdateVersion = v; },
    get autoCheckAppUpdatesEnabled() { return autoCheckAppUpdatesEnabled; },
    set autoCheckAppUpdatesEnabled(v) { autoCheckAppUpdatesEnabled = v; },
    get suppressAutoUpdatePromptUntilNextCheck() { return suppressAutoUpdatePromptUntilNextCheck; },
    set suppressAutoUpdatePromptUntilNextCheck(v) { suppressAutoUpdatePromptUntilNextCheck = v; },
    get updateObject() { return updateObject; },
    set updateObject(v) { updateObject = v; },
    get appUpdateLabel() { return appUpdateLabel; },
    initializeAppUpdatePreference,
    setAutoCheckAppUpdates,
    closeUpdatePrompt,
    skipAvailableAppUpdateVersion,
    remindAboutAppUpdateLater,
    triggerAutomaticAppUpdateCheck,
    checkForAppUpdates,
    installAppUpdate,
    restartToApplyUpdate,
    openAppUpdate
  };
}

export type AppUpdateState = ReturnType<typeof createAppUpdateState>;
