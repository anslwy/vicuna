<script lang="ts">
  import { isTauriRuntime, isCloudModel, type RunningModel } from "$lib/ollama";
  import { setNotifyEnabled, setNotifySound, setNotifySoundChoice } from "$lib/notify";
  import { playChime, CHIME_OPTIONS } from "$lib/chime";
  import type { CatalogCat, CatalogEntry } from "$lib/catalog";
  import { engineId, type WorkEngineInfo } from "$lib/work";
  import { isRemoteServer } from "$lib/bridge";
  import {
    isNewerRelease,
    pickAssetForPlatform,
    releaseTimestamp,
    currentPlatform,
    formatAssetSize,
    formatBuildDate,
    type ReleaseInfo
  } from "$lib/update";
  import type { ActivePluginTheme } from "$lib/pluginTheme";
  import { APP_UPDATE_RELEASES_URL, type AppUpdateStatus } from "$lib/appUpdate";
  import { CONTINUE_MODES, type ContinueMode } from "$lib/continue";
  import type { CliStatus } from "$lib/cli";
  import type { BrowserStatus } from "$lib/browser";
  import {
    OPENROUTER_FILTERS,
    formatContextLength,
    formatModelPricing,
    formatCacheAge,
    isFreeModel,
    supportsVision,
    supportsTools,
    maskKey,
    displayOpenRouterId,
    type OpenRouterFilter,
    type OpenRouterKeyInfo,
    type OpenRouterModel
  } from "$lib/openrouter";
  import { OLLAMA_ICON_PATH, OPENROUTER_ICON_PATH } from "$lib/modelIcons";

  export type SettingsTab = "general" | "work" | "models" | "openrouter" | "chat";

  let {
    settingsOpen = $bindable(false),
    settingsTab = $bindable<SettingsTab>("general"),
    // General / updates
    buildInfo,
    appUpdateStatus = $bindable<AppUpdateStatus>("idle"),
    appUpdateError = $bindable<string | null>(null),
    appUpdateLabel,
    showUpdatePrompt,
    autoCheckAppUpdatesEnabled,
    updateOp,
    updateRelease,
    updateNote,
    updateError = $bindable<string | null>(null),
    onOpenAppUpdate,
    onSetAutoCheckAppUpdates,
    onCheckForUpdates,
    onApplyUpdate,
    onOpenExternal,
    // Appearance
    theme,
    themeBase,
    pluginTheme,
    pluginThemeChoices,
    pluginAccent,
    accentSel,
    accents,
    fontSize,
    onSetTheme,
    onSetPluginTheme,
    onSetAccent,
    onSetFontSize,
    // Notifications
    notifyTurns = $bindable(true),
    notifySound = $bindable(true),
    notifySoundChoice = $bindable(""),
    // Desktop & CLI
    cliInstallCmd,
    cliCopied,
    cliState,
    onCopyCliInstall,
    onQuitVicuna,
    // Work
    workEngineInfo,
    workEngineLabel,
    browserDaemon,
    onRefreshWorkEngine,
    onPickWorkEngine,
    onToggleWorkApproval,
    onToggleWorkSendNow,
    // Chat / continue
    continueMode,
    stopWordsText = $bindable(""),
    onSetContinueMode,
    onSetStopWords,
    // Ollama models
    ollamaVersion,
    serverChecking,
    serverOp,
    serverError,
    serverChatTurns,
    ollamaBase = $bindable(""),
    browseView = $bindable<"installed" | "discover">("installed"),
    runnersError,
    sortedManageModels,
    runners,
    selectedModel,
    modelOp,
    pulling,
    confirmDeleteModel = $bindable<string | null>(null),
    pullName = $bindable(""),
    pullStatus,
    pullDone,
    pullTotal,
    pullRate,
    browseQuery = $bindable(""),
    browseCat = $bindable<"all" | CatalogCat | "small" | "mlx">("all"),
    browseCats,
    filteredCatalog,
    catalog,
    catalogUpdated,
    fetching,
    catalogError,
    models,
    variantPick = $bindable<Record<string, string>>({}),
    onOpenModelsTab,
    onStartServerFlow,
    onStopServerFlow,
    onSetOllamaHost,
    onToggleModelLoaded,
    onRequestDeleteModel,
    onSubmitPull,
    onSubmitPullName,
    onCancelPull,
    onFetchCatalog,
    formatPullRate,
    isMlxVariant,
    pickedVariant,
    // OpenRouter
    openrouterKey,
    openrouterKeyBusy,
    openrouterKeyInfo,
    openrouterKeyEditing = $bindable(false),
    openrouterKeyVisible = $bindable(false),
    openrouterKeyDraft = $bindable(""),
    openrouterKeyDirty,
    openrouterKeyError,
    openrouterQuery = $bindable(""),
    openrouterCat = $bindable<OpenRouterFilter>("all"),
    openrouterLoading,
    openrouterModels,
    openrouterBrowsing,
    filteredOpenRouterModels,
    openrouterUpdated,
    openrouterError,
    openrouterAddedSet,
    openRouterUsageText,
    onOpenOpenRouterTab,
    onVerifyOpenRouterKey,
    onClearOpenRouterKey,
    onSaveOpenRouterKey,
    onFetchOpenRouterCatalog,
    onToggleOpenRouterAdded
  }: {
    settingsOpen: boolean;
    settingsTab: SettingsTab;
    buildInfo: { version: string; builtAtUnix: number } | null;
    appUpdateStatus: AppUpdateStatus;
    appUpdateError: string | null;
    appUpdateLabel: string;
    showUpdatePrompt: boolean;
    autoCheckAppUpdatesEnabled: boolean;
    updateOp: "check" | "download" | null;
    updateRelease: ReleaseInfo | null;
    updateNote: string | null;
    updateError: string | null;
    onOpenAppUpdate: () => Promise<void>;
    onSetAutoCheckAppUpdates: (enabled: boolean) => void;
    onCheckForUpdates: () => Promise<void>;
    onApplyUpdate: () => Promise<void>;
    onOpenExternal: (url: string) => Promise<void>;
    theme: "light" | "dark";
    themeBase: "light" | "dark";
    pluginTheme: ActivePluginTheme | null;
    pluginThemeChoices: ActivePluginTheme[];
    pluginAccent: { bg: string; fg: string } | null;
    accentSel: string;
    accents: Record<string, { label: string; light: { bg: string; hover: string; fg: string }; dark: { bg: string; hover: string; fg: string } }>;
    fontSize: number;
    onSetTheme: (t: "light" | "dark") => void;
    onSetPluginTheme: (t: ActivePluginTheme | null) => void;
    onSetAccent: (a: string) => void;
    onSetFontSize: (n: number) => void;
    notifyTurns: boolean;
    notifySound: boolean;
    notifySoundChoice: string;
    cliInstallCmd: string;
    cliCopied: boolean;
    cliState: CliStatus | null;
    onCopyCliInstall: () => Promise<void>;
    onQuitVicuna: (force?: boolean) => Promise<void>;
    workEngineInfo: WorkEngineInfo | null;
    workEngineLabel: string;
    browserDaemon: BrowserStatus | null;
    onRefreshWorkEngine: () => Promise<void>;
    onPickWorkEngine: (id: string) => Promise<void>;
    onToggleWorkApproval: (ask: boolean) => Promise<void>;
    onToggleWorkSendNow?: (sendNow: boolean) => Promise<void>;
    continueMode: ContinueMode;
    stopWordsText: string;
    onSetContinueMode: (m: ContinueMode) => void;
    onSetStopWords: (text: string) => void;
    ollamaVersion: string | null;
    serverChecking: boolean;
    serverOp: "start" | "stop" | null;
    serverError: string | null;
    serverChatTurns: boolean;
    ollamaBase: string;
    browseView: "installed" | "discover";
    runnersError: string | null;
    sortedManageModels: string[];
    runners: RunningModel[];
    selectedModel: string;
    modelOp: string | null;
    pulling: string | null;
    confirmDeleteModel: string | null;
    pullName: string;
    pullStatus: string;
    pullDone: number;
    pullTotal: number;
    pullRate: number;
    browseQuery: string;
    browseCat: "all" | CatalogCat | "small" | "mlx";
    browseCats: readonly ("all" | CatalogCat | "small" | "mlx")[];
    filteredCatalog: CatalogEntry[];
    catalog: CatalogEntry[];
    catalogUpdated: string;
    fetching: { done: number; total: number } | null;
    catalogError: string | null;
    models: string[];
    variantPick: Record<string, string>;
    onOpenModelsTab: () => Promise<void>;
    onStartServerFlow: () => Promise<void>;
    onStopServerFlow: () => Promise<void>;
    onSetOllamaHost: (text: string) => void;
    onToggleModelLoaded: (name: string, loaded: boolean) => Promise<void>;
    onRequestDeleteModel: (name: string) => Promise<void>;
    onSubmitPull: () => Promise<void>;
    onSubmitPullName: (name: string) => Promise<void>;
    onCancelPull: () => void;
    onFetchCatalog: () => Promise<void>;
    formatPullRate: (bps: number) => string;
    isMlxVariant: (tag: string) => boolean;
    pickedVariant: (entryName: string, fallback: string) => string;
    openrouterKey: string;
    openrouterKeyBusy: boolean;
    openrouterKeyInfo: OpenRouterKeyInfo | null;
    openrouterKeyEditing: boolean;
    openrouterKeyVisible: boolean;
    openrouterKeyDraft: string;
    openrouterKeyDirty: boolean;
    openrouterKeyError: string | null;
    openrouterQuery: string;
    openrouterCat: OpenRouterFilter;
    openrouterLoading: boolean;
    openrouterModels: OpenRouterModel[];
    openrouterBrowsing: boolean;
    filteredOpenRouterModels: OpenRouterModel[];
    openrouterUpdated: string;
    openrouterError: string | null;
    openrouterAddedSet: Set<string>;
    openRouterUsageText: (info: OpenRouterKeyInfo) => string;
    onOpenOpenRouterTab: () => void;
    onVerifyOpenRouterKey: () => Promise<void>;
    onClearOpenRouterKey: () => void;
    onSaveOpenRouterKey: () => Promise<void>;
    onFetchOpenRouterCatalog: () => Promise<void>;
    onToggleOpenRouterAdded: (id: string) => void;
  } = $props();
</script>

<button
  class="confirm-backdrop"
  data-testid="settings-backdrop"
  aria-label="Close settings"
  onclick={() => (settingsOpen = false)}
  tabindex="-1"
></button>
<div
  class="confirm-card settings-card settings-split"
  data-testid="settings-modal"
  role="dialog"
  aria-modal="true"
  aria-label="Settings"
>
  <button class="modal-x settings-close" data-testid="close-settings-btn" aria-label="Close settings" onclick={() => (settingsOpen = false)}>×</button>
  <div class="settings-layout">
    <div class="settings-nav" role="tablist" aria-label="Settings sections" aria-orientation="vertical">
      <p class="settings-nav-title">Settings</p>

      <div class="settings-nav-group">
        <span class="settings-nav-label">App</span>
        <button
          class="settings-nav-btn {settingsTab === 'general' ? 'sel' : ''}"
          data-testid="settings-tab-general"
          role="tab"
          aria-selected={settingsTab === "general"}
          onclick={() => (settingsTab = "general")}
        >
          <span class="nav-ico" aria-hidden="true"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9c.26.6.85 1 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg></span>
          <span>General</span>
          {#if appUpdateStatus === "available" || appUpdateStatus === "installed"}
            <span class="nav-dot" title="Update available"></span>
          {/if}
        </button>
      </div>

      <div class="settings-nav-group">
        <span class="settings-nav-label">Work mode</span>
        <button
          class="settings-nav-btn {settingsTab === 'work' ? 'sel' : ''}"
          data-testid="settings-tab-work"
          role="tab"
          aria-selected={settingsTab === "work"}
          onclick={() => (settingsTab = "work")}
        >
          <span class="nav-ico" aria-hidden="true"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg></span>
          <span>Work</span>
        </button>
      </div>

      <div class="settings-nav-group" data-testid="settings-nav-chat">
        <span class="settings-nav-label">Chat mode</span>
        <button
          class="settings-nav-btn {settingsTab === 'models' ? 'sel' : ''}"
          data-testid="settings-tab-ollama"
          role="tab"
          aria-selected={settingsTab === "models"}
          onclick={() => void onOpenModelsTab()}
        >
          <span class="nav-ico" aria-hidden="true"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OLLAMA_ICON_PATH}/></svg></span>
          <span>Ollama</span>
        </button>
        <button
          class="settings-nav-btn {settingsTab === 'openrouter' ? 'sel' : ''}"
          data-testid="settings-tab-openrouter"
          role="tab"
          aria-selected={settingsTab === "openrouter"}
          onclick={onOpenOpenRouterTab}
        >
          <span class="nav-ico" aria-hidden="true"><svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OPENROUTER_ICON_PATH}/></svg></span>
          <span>OpenRouter</span>
        </button>
        <button
          class="settings-nav-btn {settingsTab === 'chat' ? 'sel' : ''}"
          data-testid="settings-tab-chat"
          role="tab"
          aria-selected={settingsTab === "chat"}
          onclick={() => (settingsTab = "chat")}
        >
          <span class="nav-ico" aria-hidden="true"><svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg></span>
          <span>Continue &amp; stops</span>
        </button>
      </div>
    </div>

    <div class="settings-main">
      <div class="settings-pane-head">
        <div class="settings-pane-title-row">
          <h2 class="settings-pane-title">
            {#if settingsTab === "general"}General
            {:else if settingsTab === "work"}Work
            {:else if settingsTab === "models"}Ollama
            {:else if settingsTab === "openrouter"}OpenRouter
            {:else}Continue &amp; stops{/if}
          </h2>
          {#if settingsTab === "models" || settingsTab === "openrouter" || settingsTab === "chat"}
            <span class="settings-scope-badge" data-testid="settings-scope-chat">Chat mode only</span>
          {:else if settingsTab === "work"}
            <span class="settings-scope-badge" data-testid="settings-scope-work">Work mode</span>
          {/if}
        </div>
        {#if settingsTab === "models"}
          <p class="settings-pane-sub">Local models via Ollama for Chat mode. Work mode uses its own CLI engines.</p>
        {:else if settingsTab === "openrouter"}
          <p class="settings-pane-sub">Cloud models via OpenRouter for Chat mode. Work mode uses its own CLI engines.</p>
        {:else if settingsTab === "chat"}
          <p class="settings-pane-sub">Controls how Chat mode continues partial replies.</p>
        {:else if settingsTab === "work"}
          <p class="settings-pane-sub">Coding agents, tool permissions, and the built-in browser engine.</p>
        {/if}
      </div>

      <div class="settings-pane-body {settingsTab === 'models' || settingsTab === 'openrouter' ? 'flex-pane' : ''}">
        {#if settingsTab === "general"}
          <section class="settings-group" data-testid="settings-updates-section">
            <div class="settings-group-head">
              <div class="settings-group-head-text">
                <span class="setting-label">App updates</span>
                {#if buildInfo}
                  <span class="setting-note" data-testid="app-version">v{buildInfo.version} · built {formatBuildDate(buildInfo.builtAtUnix) || "date unknown"}</span>
                {:else}
                  <span class="setting-note">Current version unknown.</span>
                {/if}
              </div>
              {#if isTauriRuntime()}
                <button class="mini-btn {appUpdateStatus === 'available' || appUpdateStatus === 'installed' ? 'primary' : ''}" data-testid="update-check-btn" disabled={appUpdateStatus === "checking"} onclick={() => void onOpenAppUpdate()}>{appUpdateLabel}</button>
              {:else}
                <button class="mini-btn" data-testid="update-check-btn" disabled={updateOp !== null} onclick={() => void onCheckForUpdates()}>{updateOp === "check" ? "Checking…" : "Check for updates"}</button>
              {/if}
            </div>
            {#if isTauriRuntime()}
              {#if appUpdateStatus === "up_to_date"}
                <span class="setting-note" data-testid="update-uptodate">You're up to date.</span>
              {/if}
              {#if appUpdateStatus === "error" && appUpdateError && !showUpdatePrompt}
                <p class="goto-error" data-testid="update-error">Update check failed: {appUpdateError} <button type="button" class="err-x" data-testid="update-error-dismiss" aria-label="Dismiss update error" title="Dismiss" onclick={() => { appUpdateError = null; appUpdateStatus = "idle"; }}>×</button></p>
              {/if}
              <label class="setting-note approval-row" data-testid="update-autocheck">
                <input type="checkbox" data-testid="update-autocheck-toggle" checked={autoCheckAppUpdatesEnabled} onchange={(e) => onSetAutoCheckAppUpdates((e.currentTarget as HTMLInputElement).checked)} />
                Check for updates automatically on startup.
              </label>
              <span class="setting-note">Signed updates from the newest <a href={APP_UPDATE_RELEASES_URL} onclick={(e) => { e.preventDefault(); void onOpenExternal(APP_UPDATE_RELEASES_URL); }}>GitHub release</a> — install in-app, then restart.</span>
            {:else}
              {#if updateRelease}
                {@const platform = currentPlatform()}
                {@const stamp = releaseTimestamp(updateRelease, platform)}
                {@const behind = !buildInfo || isNewerRelease(stamp, buildInfo.builtAtUnix)}
                {@const asset = pickAssetForPlatform(updateRelease.assets, platform)}
                <span class="setting-note" data-testid="update-latest">Latest: {updateRelease.tag} · installer built {formatBuildDate(Math.floor(Date.parse(stamp) / 1000)) || "date unknown"}</span>
                {#if behind && asset}
                  <div class="browse-actions">
                    <button class="mini-btn primary" data-testid="update-install-btn" disabled={updateOp !== null} onclick={() => void onApplyUpdate()}>{updateOp === "download" ? "Downloading…" : `Download ${asset.name} (${formatAssetSize(asset.size)})`}</button>
                    <button class="mini-btn" data-testid="update-notes-btn" onclick={() => void onOpenExternal(updateRelease?.htmlUrl ?? "https://github.com/anslwy/vicuna/releases/latest")}>Release notes</button>
                  </div>
                {:else if behind}
                  <span class="setting-note">No installer for this platform in {updateRelease.tag} — see the release notes.</span>
                  <div class="browse-actions">
                    <button class="mini-btn" data-testid="update-notes-btn" onclick={() => void onOpenExternal(updateRelease?.htmlUrl ?? "https://github.com/anslwy/vicuna/releases/latest")}>Release notes</button>
                  </div>
                {:else}
                  <span class="setting-note" data-testid="update-uptodate">You're up to date.</span>
                {/if}
              {/if}
              {#if updateNote}<span class="setting-note" data-testid="update-note">{updateNote}</span>{/if}
              {#if updateError}<p class="goto-error" data-testid="update-error">{updateError} <button type="button" class="err-x" data-testid="update-error-dismiss" aria-label="Dismiss update error" title="Dismiss" onclick={() => (updateError = null)}>×</button></p>{/if}
              <span class="setting-note">Compares your build date against the rolling <code>latest</code> prerelease.</span>
            {/if}
          </section>

          <section class="settings-group" data-testid="settings-appearance-section">
            <span class="settings-group-kicker">Appearance</span>
            <div class="setting-row setting-row-wrap">
              <span class="setting-label">Theme</span>
              <div class="seg theme-seg">
                <button class="seg-btn {!pluginTheme && theme === 'light' ? 'sel' : ''}" data-testid="theme-light-btn" onclick={() => onSetTheme("light")}>Light</button>
                <button class="seg-btn {!pluginTheme && theme === 'dark' ? 'sel' : ''}" data-testid="theme-dark-btn" onclick={() => onSetTheme("dark")}>Dark</button>
                {#each pluginThemeChoices as t (t.key)}
                  <button class="seg-btn {pluginTheme?.key === t.key ? 'sel' : ''}" data-testid="theme-plugin-{t.key.replace('/', '-')}" title="From the {t.pluginName} plugin" onclick={() => onSetPluginTheme(t)}>{t.name}</button>
                {/each}
              </div>
            </div>
            <div class="setting-row">
              <span class="setting-label">Accent color</span>
              <div class="swatches">
                {#if pluginTheme && pluginAccent}
                  <button
                    class="swatch {accentSel === 'theme' ? 'sel' : ''}"
                    data-testid="accent-theme"
                    aria-label="{pluginTheme.name} accent"
                    title="{pluginTheme.name} accent (from the theme)"
                    style="background: {pluginAccent.bg}"
                    onclick={() => onSetAccent("theme")}
                  >{#if accentSel === "theme"}<span class="swatch-check" style="color: {pluginAccent.fg}">✓</span>{/if}</button>
                {/if}
                {#each Object.entries(accents) as [key, a]}
                  <button
                    class="swatch {accentSel === key ? 'sel' : ''}"
                    data-testid="accent-{key}"
                    aria-label={a.label}
                    title={a.label}
                    style="background: {themeBase === 'dark' ? a.dark.bg : a.light.bg}"
                    onclick={() => onSetAccent(key)}
                  >{#if accentSel === key}<span class="swatch-check">✓</span>{/if}</button>
                {/each}
              </div>
            </div>
            <div class="setting-row">
              <span class="setting-label">Font size</span>
              <div class="seg">
                <button class="seg-btn" data-testid="font-decrease" aria-label="Decrease font size" onclick={() => onSetFontSize(fontSize - 1)}>−</button>
                <span class="font-val" data-testid="font-size-value">{fontSize}px</span>
                <button class="seg-btn" data-testid="font-increase" aria-label="Increase font size" onclick={() => onSetFontSize(fontSize + 1)}>+</button>
              </div>
            </div>
          </section>

          <section class="settings-group">
            <span class="settings-group-kicker">Notifications</span>
            <label class="setting-note approval-row" data-testid="notify-turns">
              <input type="checkbox" data-testid="notify-turns-toggle" checked={notifyTurns} onchange={(e) => { notifyTurns = (e.currentTarget as HTMLInputElement).checked; setNotifyEnabled(notifyTurns); }} />
              Notify when a Chat or Work turn finishes while Vicuna isn't in front (not when you press Stop).
            </label>
            <label class="setting-note approval-row" data-testid="notify-sound">
              <input type="checkbox" data-testid="notify-sound-toggle" checked={notifySound} disabled={!notifyTurns} onchange={(e) => { notifySound = (e.currentTarget as HTMLInputElement).checked; setNotifySound(notifySound); if (notifySound) void playChime(notifySoundChoice); }} />
              Play a sound with the notification.
            </label>
            <div class="browse-actions notify-sound-pick">
              <select
                class="browse-variant"
                data-testid="notify-sound-select"
                aria-label="Notification sound"
                value={notifySoundChoice}
                disabled={!notifyTurns || !notifySound}
                onchange={(e) => { notifySoundChoice = e.currentTarget.value; setNotifySoundChoice(notifySoundChoice); void playChime(notifySoundChoice); }}
              >
                {#each CHIME_OPTIONS as s}
                  <option value={s.id}>{s.label}</option>
                {/each}
              </select>
              <button class="mini-btn" data-testid="notify-sound-preview" disabled={!notifyTurns || !notifySound} onclick={() => void playChime(notifySoundChoice)}>▶ Preview</button>
            </div>
          </section>

          {#if isTauriRuntime()}
            <section class="settings-group">
              <span class="settings-group-kicker">Desktop &amp; CLI</span>
              <div class="setting-stacked">
                <span class="setting-label">Vicuna CLI</span>
                <!-- the `vicuna` binary (bridge/ crate) replaced the old launcher script:
                     one command for `vicuna open [path]` and `vicuna serve` (server) -->
                <div class="browse-actions">
                  <code class="cli-oneliner" data-testid="cli-oneliner">{cliInstallCmd}</code>
                  <button class="mini-btn" data-testid="cli-copy-btn" onclick={() => void onCopyCliInstall()}>{cliCopied ? "Copied" : "Copy"}</button>
                </div>
                {#if cliState?.found}
                  <span class="setting-note" data-testid="cli-status">
                    {#if cliState.managed}Found the old launcher at {cliState.found}: the command above replaces it.{:else}Found `{cliState.exe_name}` at {cliState.found}.{/if}
                  </span>
                {/if}
                <span class="setting-note">Run in a terminal (macOS/Linux, no sudo). Then <code>vicuna [path]</code> opens a folder here, and <code>vicuna serve --tailscale</code> exposes this machine as a server.</span>
              </div>
              <div class="setting-row quit-row">
                <div class="settings-group-head-text">
                  <span class="setting-label">Quit Vicuna</span>
                  <span class="setting-note">Closing the window keeps Vicuna in the tray so streams continue. Quit stops everything.</span>
                </div>
                <button class="mini-btn danger" data-testid="quit-app-btn" onclick={() => void onQuitVicuna()}>Quit Vicuna</button>
              </div>
            </section>
          {/if}

        {:else if settingsTab === "work"}
          <section class="settings-group">
            <div class="settings-group-head">
              <span class="setting-label">Work engine</span>
              <button class="mini-btn" data-testid="work-engine-refresh" onclick={() => void onRefreshWorkEngine()}>Refresh</button>
            </div>
            {#if (workEngineInfo?.engines ?? []).length > 0}
              <div class="engine-switch settings-engines" data-testid="settings-engine-picker" role="radiogroup" aria-label="Work engine">
                {#each workEngineInfo?.engines ?? [] as eng (eng.id)}
                  <button class="engine-opt {eng.id === engineId(workEngineInfo) ? 'sel' : ''}" role="radio" aria-checked={eng.id === engineId(workEngineInfo)} data-testid="settings-engine-opt" data-engine={eng.id} disabled={!eng.installed} title={eng.installed ? `${eng.label} ${eng.version ?? ""}` : (eng.missing ?? "not installed")} onclick={() => void onPickWorkEngine(eng.id)}>{eng.label}{eng.installed ? "" : " (not installed)"}</button>
                {/each}
              </div>
            {/if}
            <span class="setting-note" data-testid="work-engine-status">
              {#if workEngineInfo?.installed}
                {workEngineLabel} {workEngineInfo.version ?? ""} · {workEngineInfo.path}
                {#if workEngineInfo.warning}
                  <br /><span data-testid="work-engine-warning">⚠ {workEngineInfo.warning}</span>
                {/if}
                {#if (workEngineInfo.others ?? []).length > 0}
                  <br /><span data-testid="work-engine-others">Also found (not used; the newest version wins): {(workEngineInfo.others ?? []).join(", ")}</span>
                {/if}
              {:else if workEngineInfo}
                {workEngineInfo.missing ?? `${workEngineLabel} not found, install it: ${workEngineInfo.installUrl}`}
              {:else}
                Checking…
              {/if}
            </span>
            {#each (workEngineInfo?.engines ?? []).filter((e) => !e.installed && e.id !== engineId(workEngineInfo)) as eng (eng.id)}
              <span class="setting-note" data-testid="settings-engine-missing" data-engine={eng.id}>{eng.missing}</span>
            {/each}
            <label class="setting-note approval-row settings-divider-top" data-testid="work-approval">
              <input type="checkbox" data-testid="work-approval-toggle" checked={workEngineInfo?.askPermission ?? false} disabled={!workEngineInfo} onchange={(e) => void onToggleWorkApproval((e.currentTarget as HTMLInputElement).checked)} />
              Ask before running tools (edits, shell commands, web fetches): each request shows Allow / Reject in the chat. Off = approve automatically and note it in the transcript.
            </label>
            <label class="setting-note approval-row" data-testid="work-send-now">
              <input type="checkbox" data-testid="work-send-now-toggle" checked={workEngineInfo?.sendNow ?? true} disabled={!workEngineInfo} onchange={(e) => void onToggleWorkSendNow?.((e.currentTarget as HTMLInputElement).checked)} />
              Send follow-ups immediately while the agent works: a message sent mid-turn (also from Telegram and bridge plugins) interrupts the current step and goes now. Off = it queues until the agent finishes (↑ or <code>/send_now</code> sends it now).
            </label>
            <details class="setting-details">
              <summary>How engines &amp; binary overrides work</summary>
              <p class="setting-note">Work mode runs your installed CLI (<code>opencode acp</code>, <code>claude -p</code>, <code>codex app-server</code>, <code>agy acp</code>, <code>fx acp</code>, or <code>pi --mode rpc</code>) with your own login and config. Vicuna searches your login-shell PATH and common install folders, picking the newest version. Models and credentials come from the engine.</p>
              <p class="setting-note">Override binaries with <code>VICUNA_OPENCODE_BIN</code>, <code>VICUNA_CLAUDE_BIN</code>, <code>VICUNA_CODEX_BIN</code>, <code>VICUNA_AGY_BIN</code>, <code>VICUNA_FX_BIN</code>, or <code>VICUNA_PI_BIN</code>. <code>fx</code> exposes <code>provider/model</code> from its logins and <code>~/.fx/settings.json</code>; <code>pi</code> loads extensions from <code>~/vicuna/config/pi/extensions</code>.</p>
            </details>
          </section>

          <section class="settings-group" data-testid="browser-engine">
            <span class="setting-label">Browser engine</span>
            {#if browserDaemon?.lb_version}
              <span class="setting-note" data-testid="browser-engine-version">lb {browserDaemon.lb_version} · {browserDaemon.source === "bundled" ? "bundled with Vicuna" : "system PATH"} · port {new URL(browserDaemon.url).port}</span>
              <span class="setting-note work-path" data-testid="browser-engine-path" title={browserDaemon.bin ?? ""}>{browserDaemon.bin ?? "path unknown"}</span>
            {:else}
              <span class="setting-note">Not found: no bundled lb and none on PATH.</span>
            {/if}
          </section>

        {:else if settingsTab === "chat"}
          <section class="settings-group">
            <span class="setting-label">Continue method</span>
            <div class="seg" data-testid="continue-mode">
              {#each CONTINUE_MODES as m}
                <button
                  class="seg-btn {continueMode === m.id ? 'sel' : ''}"
                  data-testid="continue-mode-{m.id}"
                  onclick={() => onSetContinueMode(m.id)}
                >{m.label}</button>
              {/each}
            </div>
            <span class="setting-note">Raw prompt (default) works best for Gemma families and falls back to native chat for other models. Cloud models cannot continue.</span>
            <details class="setting-details">
              <summary>How each method works</summary>
              <p class="setting-note"><strong>Raw prompt</strong> prefills the model's own prompt through <code>/api/generate</code> with <code>raw:true</code> — works for gemma4/gemma2/3 and falls back to native chat for other families. <strong>Auto</strong> uses the raw prompt for gemma4 only (its chat template closes the assistant turn, so native Continue starts a fresh greeting) and native chat otherwise. <strong>Native chat</strong> always resends the partial through <code>/api/chat</code>. Threads with images always use native chat, and raw continuations are cut off if the build degenerates into repetition.</p>
            </details>
          </section>

          <section class="settings-group">
            <span class="setting-label">Stop words <span class="setting-hint">· applied when continuing a reply</span></span>
            <textarea
              class="stopwords-input"
              data-testid="stopwords-input"
              rows="4"
              spellcheck={false}
              placeholder={"\\nUser:\\nAssistant:"}
              bind:value={stopWordsText}
              onblur={() => onSetStopWords(stopWordsText)}
            ></textarea>
            <span class="setting-note">One per line. Use <code>\n</code> for newlines. Generation stops when the model outputs one of these.</span>
          </section>

        {:else if settingsTab === "models"}
          <div class="model-manage-list" data-testid="model-manage-list">
            <div class="server-row" data-testid="ollama-server-row">
              <div class="model-manage-info">
                <span class="model-manage-name">Ollama server</span>
                <span class="model-manage-sub">
                  {#if ollamaVersion}
                    <span class="dot-live"></span> Running · v{ollamaVersion}
                  {:else if serverChecking || serverOp}
                    <span class="dot-idle"></span> {serverOp === "start" ? "Starting…" : serverOp === "stop" ? "Stopping…" : "Checking…"}
                  {:else}
                    <span class="dot-idle"></span> Not reachable{serverError ? ` — ${serverError}` : ""}
                  {/if}
                  {#if serverChatTurns}
                    <span>· http://localhost:11434 on connected server</span>
                  {/if}
                </span>
              </div>
              <div class="model-manage-actions">
                {#if isTauriRuntime() && !(serverChatTurns && isRemoteServer())}
                  {#if ollamaVersion}
                    <button class="mini-btn" data-testid="stop-server-btn" disabled={serverOp !== null} onclick={() => void onStopServerFlow()}>{serverOp === "stop" ? "…" : "Stop"}</button>
                  {:else}
                    <button class="mini-btn primary" data-testid="start-server-btn" disabled={serverOp !== null} onclick={() => void onStartServerFlow()}>{serverOp === "start" ? "…" : "Start"}</button>
                  {/if}
                {/if}
              </div>
            </div>
            {#if !serverChatTurns}
              <div class="ollama-host-box">
                <span class="setting-label">Ollama host <span class="setting-hint">· browser mode only</span></span>
                <input
                  class="pull-input"
                  data-testid="ollama-host-input"
                  placeholder="http://localhost:11434"
                  bind:value={ollamaBase}
                  onblur={() => onSetOllamaHost(ollamaBase)}
                  onkeydown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
                  spellcheck={false}
                />
                <span class="setting-note">Where this browser reaches Ollama. Empty = this machine. The desktop app always uses its local Ollama.</span>
              </div>
            {/if}
            {#if serverError}
              <p class="goto-error" data-testid="server-error">{serverError}</p>
            {/if}
            <div class="seg browse-tabs" role="tablist" aria-label="Model library sections">
              <button class="seg-btn {browseView === 'installed' ? 'sel' : ''}" data-testid="browse-installed-btn" role="tab" onclick={() => (browseView = "installed")}>Installed</button>
              <button class="seg-btn {browseView === 'discover' ? 'sel' : ''}" data-testid="browse-discover-btn" role="tab" onclick={() => (browseView = "discover")}>Discover</button>
            </div>
            {#if browseView === "installed"}
            {#if runnersError}
              <p class="goto-error" data-testid="models-error">{runnersError}</p>
            {/if}
            {#each sortedManageModels as name}
              {@const running = runners.find((r) => r.name === name)}
              <div class="model-manage-row" data-testid="model-row" data-name={name}>
                <div class="model-manage-info">
                  <span class="model-manage-name">{name}{#if name === selectedModel} <span class="model-inuse" title="Used by current chat">• in use</span>{/if}</span>
                  <span class="model-manage-sub">
                    {#if running}
                      <span class="dot-live"></span> Loaded · {(running.size_vram / 1e9).toFixed(1)} GB VRAM
                    {:else if isCloudModel(name)}
                      <span class="dot-live"></span> Cloud · ready, no load needed
                    {:else}
                      <span class="dot-idle"></span> Unloaded
                    {/if}
                  </span>
                </div>
                <div class="model-manage-actions">
                  {#if !isCloudModel(name)}
                    {#if running}
                      <button class="mini-btn" data-testid="unload-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => void onToggleModelLoaded(name, true)}>{modelOp === name ? "…" : "Unload"}</button>
                    {:else}
                      <button class="mini-btn primary" data-testid="load-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => void onToggleModelLoaded(name, false)}>{modelOp === name ? "…" : "Load"}</button>
                    {/if}
                  {/if}
                  {#if confirmDeleteModel === name}
                    <button class="mini-btn danger" data-testid="confirm-delete-model-btn" data-name={name} disabled={modelOp !== null} onclick={() => void onRequestDeleteModel(name)}>{modelOp === name ? "…" : "Confirm?"}</button>
                    <button class="mini-btn" data-testid="keep-model-btn" data-name={name} disabled={modelOp !== null} onclick={() => (confirmDeleteModel = null)}>Keep</button>
                  {:else}
                    <button class="mini-btn" data-testid="delete-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => void onRequestDeleteModel(name)}>Delete</button>
                  {/if}
                </div>
              </div>
            {:else}
              <p class="no-match">No models installed.</p>
            {/each}
            {:else}
            <div class="pull-row" data-testid="model-pull-row">
              <input
                class="pull-input"
                data-testid="model-pull-input"
                placeholder="Pull by exact name, e.g. gemma4:e4b"
                bind:value={pullName}
                disabled={pulling !== null}
                onkeydown={(e) => { if (e.key === "Enter") void onSubmitPull(); }}
              />
              {#if pulling !== null}
                <button class="mini-btn" data-testid="cancel-pull-btn" onclick={onCancelPull}>Cancel</button>
              {:else}
                <button class="mini-btn primary" data-testid="pull-model-btn" disabled={pullName.trim() === "" || modelOp !== null} onclick={() => void onSubmitPull()}>Pull</button>
              {/if}
            </div>
            {#if pulling !== null}
              <div class="pull-progress" data-testid="model-pull-progress">
                <span class="pull-status">{pulling} · {pullStatus}</span>
                {#if pullTotal > 0}
                  <div class="pull-bar"><div class="pull-fill" style="width: {Math.min(100, (pullDone / pullTotal) * 100).toFixed(1)}%"></div></div>
                  <span class="pull-pct">{((pullDone / pullTotal) * 100).toFixed(0)}% · {(pullDone / 1e9).toFixed(1)} / {(pullTotal / 1e9).toFixed(1)} GB{#if pullRate > 0} · {formatPullRate(pullRate)}{/if}</span>
                {/if}
              </div>
            {/if}
            <div class="pull-row" data-testid="model-search-row">
              <input
                class="pull-input"
                data-testid="discover-search-input"
                placeholder="Search the library — try llama, code, vision…"
                bind:value={browseQuery}
              />
            </div>
            <div class="chip-row" data-testid="discover-chips" role="group" aria-label="Filter by capability">
              {#each browseCats as c}
                <button class="chip {browseCat === c ? 'sel' : ''}" data-testid="discover-chip" data-cat={c} onclick={() => (browseCat = c)}>{c === "all" ? "All" : c === "mlx" ? "MLX" : c[0].toUpperCase() + c.slice(1)}</button>
              {/each}
            </div>
            <p class="browse-count" data-testid="discover-count">{filteredCatalog.length} of {catalog.length} models · catalog {catalogUpdated}
              {#if fetching}
                <span class="fetch-progress" data-testid="catalog-fetch-progress">Fetching {fetching.done}/{fetching.total}…</span>
              {:else}
                <button class="mini-btn" data-testid="catalog-fetch-btn" onclick={() => void onFetchCatalog()}>Fetch</button>
              {/if}
            </p>
            {#if catalogError}
              <p class="goto-error" data-testid="catalog-fetch-error">{catalogError}</p>
            {/if}
            {#each filteredCatalog.filter((e) => e.variants.length > 0) as entry}
              {@const mlxFirst = entry.variants.find((v) => isMlxVariant(v.tag))?.tag}
              {@const picked = pickedVariant(entry.name, browseCat === "mlx" && mlxFirst ? mlxFirst : entry.variants[0].tag)}
              {@const fullName = `${entry.name}:${picked}`}
              {@const installed = entry.variants.some((v) => models.includes(`${entry.name}:${v.tag}`))}
              <div class="browse-card" data-testid="browse-card" data-name={entry.name}>
                <div class="browse-head">
                  <span class="browse-name">{entry.name}</span>
                  {#if installed}<span class="browse-installed" title="A variant of this model is in your library">In library</span>{/if}
                </div>
                <p class="browse-blurb">{entry.blurb}</p>
                <div class="browse-meta">
                  {#each entry.cats as c}<span class="browse-cat">{c}</span>{/each}
                  <span class="browse-params">{entry.params}</span>
                </div>
                <div class="browse-actions">
                  <select
                    class="browse-variant"
                    data-testid="variant-select"
                    aria-label="Variant of {entry.name}"
                    value={picked}
                    onchange={(e) => (variantPick = { ...variantPick, [entry.name]: e.currentTarget.value })}
                  >
                    {#each entry.variants as v}
                      <option value={v.tag}>{v.tag} · {v.size}</option>
                    {/each}
                  </select>
                  <button class="mini-btn primary" data-testid="browse-pull-btn" data-name={fullName} disabled={pulling !== null || modelOp !== null} onclick={() => void onSubmitPullName(fullName)}>{pulling === fullName ? "…" : "Pull"}</button>
                </div>
              </div>
            {:else}
              <p class="no-match">No matches — try another search, or pull by exact name above.</p>
            {/each}
            {/if}
          </div>

        {:else}
          <div class="or-panel" data-testid="openrouter-panel">
            <div class="server-row" data-testid="openrouter-key-row">
              <div class="model-manage-info">
                <span class="model-manage-name">OpenRouter API key</span>
                <span class="model-manage-sub" data-testid="openrouter-key-status">
                  {#if openrouterKeyBusy}
                    <span class="dot-idle"></span> Checking key…
                  {:else if openrouterKeyInfo}
                    <span class="dot-live"></span> Connected{openrouterKeyInfo.isFreeTier ? " · free tier" : ""}{#if openrouterKey}{" · "}{maskKey(openrouterKey)}{/if}
                  {:else if openrouterKey}
                    <span class="dot-idle"></span> Saved · not verified
                  {:else}
                    <span class="dot-idle"></span> Not configured
                  {/if}
                </span>
                {#if openrouterKeyInfo && openRouterUsageText(openrouterKeyInfo)}
                  <span class="model-manage-sub or-usage">{openRouterUsageText(openrouterKeyInfo)}</span>
                {/if}
              </div>
              <div class="model-manage-actions">
                <button class="mini-btn" data-testid="openrouter-key-test" disabled={openrouterKeyBusy || !openrouterKey} onclick={() => void onVerifyOpenRouterKey()}>{openrouterKeyBusy ? "…" : "Test"}</button>
                {#if openrouterKey && !openrouterKeyEditing}
                  <button class="mini-btn" data-testid="openrouter-key-change" disabled={openrouterKeyBusy} onclick={() => (openrouterKeyEditing = true)}>Change</button>
                {/if}
                {#if openrouterKey}
                  <button class="mini-btn" data-testid="openrouter-key-clear" disabled={openrouterKeyBusy} onclick={onClearOpenRouterKey}>Clear</button>
                {/if}
              </div>
            </div>
            {#if !openrouterKey || openrouterKeyEditing}
            <div class="pull-row" data-testid="openrouter-key-form">
              <input
                class="pull-input"
                data-testid="openrouter-key-input"
                type={openrouterKeyVisible ? "text" : "password"}
                placeholder="sk-or-v1-…"
                bind:value={openrouterKeyDraft}
                spellcheck={false}
                autocomplete="off"
                disabled={openrouterKeyBusy}
                onkeydown={(e) => { if (e.key === "Enter") void onSaveOpenRouterKey(); }}
              />
              <button class="mini-btn" data-testid="openrouter-key-reveal" disabled={!openrouterKeyDraft} onclick={() => (openrouterKeyVisible = !openrouterKeyVisible)}>{openrouterKeyVisible ? "Hide" : "Show"}</button>
              <button class="mini-btn primary" data-testid="openrouter-key-save" disabled={openrouterKeyBusy || (!openrouterKeyDirty && !openrouterKeyEditing)} onclick={() => void onSaveOpenRouterKey()}>Save</button>
            </div>
            <div class="or-note-row">
              <span class="setting-note">Stored locally with your settings and only sent to openrouter.ai. The public catalog fetches without a key; a key verifies your account and usage.</span>
              <button class="mini-btn" data-testid="openrouter-get-key" onclick={() => void onOpenExternal("https://openrouter.ai/keys")}>Get a key</button>
            </div>
            {/if}
            {#if openrouterKeyError}
              <p class="goto-error" data-testid="openrouter-key-error">{openrouterKeyError}</p>
            {/if}
            <div class="pull-row" data-testid="openrouter-search-row">
              <input
                class="pull-input"
                data-testid="openrouter-search-input"
                placeholder="Search models — try claude, free, vision…"
                bind:value={openrouterQuery}
                disabled={openrouterModels.length === 0}
              />
            </div>
            <div class="chip-row" data-testid="openrouter-chips" role="group" aria-label="Filter OpenRouter models">
              {#each OPENROUTER_FILTERS as c}
                <button class="chip {openrouterCat === c ? 'sel' : ''}" data-testid="openrouter-chip" data-cat={c} onclick={() => (openrouterCat = c)}>{c === "all" ? "All" : c[0].toUpperCase() + c.slice(1)}</button>
              {/each}
            </div>
            <p class="browse-count" data-testid="openrouter-count">
              {#if openrouterLoading}
                <span class="fetch-progress">Fetching the live OpenRouter catalog…</span>
              {:else if openrouterModels.length > 0}
                {#if openrouterBrowsing}
                  {filteredOpenRouterModels.length} of {openrouterModels.length} models{openrouterUpdated ? ` · updated ${formatCacheAge(openrouterUpdated)}` : ""}
                {:else}
                  {filteredOpenRouterModels.length} added{openrouterUpdated ? ` · catalog updated ${formatCacheAge(openrouterUpdated)}` : ""}
                {/if}
              {:else}
                No catalog loaded yet.
              {/if}
              <button class="mini-btn" data-testid="openrouter-fetch-btn" disabled={openrouterLoading} onclick={() => void onFetchOpenRouterCatalog()}>{openrouterLoading ? "…" : "Fetch"}</button>
            </p>
            {#if openrouterError}
              <p class="goto-error" data-testid="openrouter-error">{openrouterError}</p>
            {/if}
            <div class="model-manage-list or-models" data-testid="openrouter-models">
              {#each filteredOpenRouterModels as m (m.id)}
                <div class="model-manage-row" data-testid="openrouter-model-row" data-id={m.id} title={m.description}>
                  <div class="model-manage-info">
                    <span class="model-manage-name">{displayOpenRouterId(m.id)}</span>
                    <span class="model-manage-sub or-meta">
                      <span>{formatContextLength(m.contextLength)} ctx</span>
                      <span>·</span>
                      <span>{formatModelPricing(m)}</span>
                      {#if isFreeModel(m)}<span class="browse-cat">free</span>{/if}
                      {#if supportsVision(m)}<span class="browse-cat">vision</span>{/if}
                      {#if supportsTools(m)}<span class="browse-cat">tools</span>{/if}
                      {#if m.reasoning}<span class="browse-cat">reasoning</span>{/if}
                    </span>
                  </div>
                  <div class="model-manage-actions">
                    {#if openrouterAddedSet.has(m.id)}
                      <button class="mini-btn" data-testid="openrouter-remove-btn" data-id={m.id} title="Remove from your models" onclick={() => onToggleOpenRouterAdded(m.id)}>Remove</button>
                    {:else}
                      <button class="mini-btn primary" data-testid="openrouter-add-btn" data-id={m.id} title="Add to your models" onclick={() => onToggleOpenRouterAdded(m.id)}>Add</button>
                    {/if}
                  </div>
                </div>
              {:else}
                <p class="no-match">{openrouterModels.length === 0 ? "Press Fetch to load the OpenRouter catalog." : openrouterBrowsing ? "No models match this search." : "No models added yet — search above to find models to add."}</p>
              {/each}
            </div>
          </div>
        {/if}
      </div>
    </div>
  </div>
</div>

<style>
  textarea, input { -webkit-user-select: text; user-select: text; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  .err-x { border: none; background: transparent; color: inherit; cursor: pointer; font: inherit; padding: 0 4px; opacity: 0.7; }
  .err-x:hover { opacity: 1; }
  .no-match { padding: 12px 18px; font-size: 14px; color: var(--muted); }
  .cli-oneliner { flex: 1; min-width: 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; padding: 6px 8px; border: 1px solid var(--border-2); border-radius: 8px; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
  .work-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .settings-card { width: 80vw; max-width: 900px; height: 80vh; max-height: 820px; top: 50%; display: flex; flex-direction: column; overflow-y: auto; }
  .settings-card.settings-split { padding: 0; overflow: hidden; }
  .settings-close { top: 14px; right: 14px; z-index: 2; }
  .settings-layout { display: flex; flex: 1; min-height: 0; }
  .settings-nav { width: 208px; flex-shrink: 0; padding: 20px 12px 16px 12px; border-right: 1px solid var(--border); background: color-mix(in srgb, var(--hover) 38%, var(--card)); display: flex; flex-direction: column; gap: 16px; overflow-y: auto; box-sizing: border-box; }
  .settings-nav-title { font-size: 16px; font-weight: 700; color: var(--text); margin: 0 0 2px 8px; }
  .settings-nav-group { display: flex; flex-direction: column; gap: 2px; }
  .settings-nav-label { font-size: 10.5px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.06em; color: var(--muted-2); padding: 0 10px 4px 10px; }
  .settings-nav-btn { display: flex; align-items: center; gap: 9px; width: 100%; border: none; background: transparent; color: var(--text-dim); font: inherit; font-size: 13.5px; padding: 7px 10px; border-radius: 9px; cursor: pointer; text-align: left; }
  .settings-nav-btn:hover { background: var(--hover); color: var(--text); }
  .settings-nav-btn.sel { background: var(--card); color: var(--text); font-weight: 600; box-shadow: 0 1px 4px rgba(0,0,0,.1); }
  .nav-ico { display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; color: var(--muted-2); flex-shrink: 0; }
  .settings-nav-btn.sel .nav-ico { color: var(--accent); }
  .nav-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); margin-left: auto; flex-shrink: 0; }
  .settings-main { flex: 1; min-width: 0; display: flex; flex-direction: column; overflow: hidden; }
  .settings-pane-head { padding: 18px 52px 12px 24px; border-bottom: 1px solid var(--border); flex-shrink: 0; }
  .settings-pane-title-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .settings-pane-title { font-size: 17px; font-weight: 700; color: var(--text); margin: 0; }
  .settings-scope-badge { font-size: 11px; font-weight: 600; color: var(--accent); background: color-mix(in srgb, var(--accent) 13%, transparent); border: 1px solid color-mix(in srgb, var(--accent) 28%, transparent); border-radius: 999px; padding: 2px 9px; }
  .settings-pane-sub { font-size: 12.5px; color: var(--muted); margin: 4px 0 0 0; line-height: 1.4; }
  .settings-pane-body { flex: 1; min-height: 0; overflow-y: auto; padding: 18px 24px 24px 24px; display: flex; flex-direction: column; gap: 14px; }
  .settings-pane-body.flex-pane { overflow: hidden; padding-bottom: 16px; }
  .settings-group { border: 1px solid var(--border); border-radius: 14px; padding: 14px 16px; background: color-mix(in srgb, var(--hover) 24%, transparent); display: flex; flex-direction: column; gap: 10px; }
  .settings-group-kicker { font-size: 11px; font-weight: 700; text-transform: uppercase; letter-spacing: 0.05em; color: var(--muted-2); margin-bottom: 2px; }
  .settings-group-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .settings-group-head-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .settings-group .setting-row { margin: 0; padding: 8px 0; border-top: 1px solid var(--border); }
  .settings-group .setting-row:first-of-type { border-top: none; padding-top: 0; }
  .settings-group .setting-row:last-child { padding-bottom: 0; }
  .settings-group .quit-row { margin-top: 4px; padding-top: 12px; border-top: 1px solid var(--border); }
  .setting-row-wrap { flex-wrap: wrap; }
  .theme-seg { flex-wrap: wrap; max-width: 100%; }
  .settings-divider-top { padding-top: 10px; border-top: 1px solid var(--border); }
  .setting-details { margin-top: 2px; font-size: 12px; color: var(--muted); }
  .setting-details summary { cursor: pointer; color: var(--muted-2); user-select: none; font-weight: 500; }
  .setting-details summary:hover { color: var(--text); }
  .setting-details p { margin: 8px 0 0 0; line-height: 1.5; }
  .ollama-host-box { display: flex; flex-direction: column; gap: 6px; padding: 10px 12px; margin-bottom: 8px; border: 1px solid var(--border); border-radius: 14px; }
  .font-val { font-size: 14px; min-width: 48px; text-align: center; color: var(--text); font-variant-numeric: tabular-nums; margin-top: 5px; }
  .model-manage-list { display: flex; flex-direction: column; gap: 4px; flex: 1; min-height: 0; overflow-y: auto; margin-bottom: 4px; }
  .server-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 12px; margin-bottom: 8px; background: var(--composer); border-radius: 16px; }
  .model-manage-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 8px 4px; border-bottom: 1px solid var(--border); }
  .model-manage-row:last-child { border-bottom: none; }
  .model-manage-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .model-manage-name { font-size: 13.5px; font-weight: 600; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .model-inuse { color: var(--accent); font-weight: 500; }
  .model-manage-sub { font-size: 12px; color: var(--muted); display: flex; align-items: center; gap: 6px; }
  .dot-live { width: 7px; height: 7px; border-radius: 50%; background: #22c55e; display: inline-block; }
  .dot-idle { width: 7px; height: 7px; border-radius: 50%; background: var(--muted); display: inline-block; }
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .mini-btn.primary { background: var(--accent); border-color: transparent; color: var(--accent-fg); }
  .mini-btn.primary:hover:not(:disabled) { background: var(--accent-hover); }
  .mini-btn.danger { border-color: var(--danger); color: var(--danger); }
  .mini-btn.danger:hover:not(:disabled) { background: var(--err-bg); }
  .model-manage-actions { display: flex; gap: 6px; flex-shrink: 0; }
  .pull-row { display: flex; gap: 8px; padding: 4px 4px 10px 4px; }
  .pull-input { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 13px; font-family: inherit; padding: 6px 14px; }
  .pull-input:focus { outline: none; border-color: var(--accent); }
  .pull-input:disabled { opacity: .5; }
  .pull-progress { display: flex; flex-direction: column; gap: 6px; padding: 0 4px 10px 4px; font-size: 12px; color: var(--muted); }
  .pull-status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pull-bar { height: 6px; border-radius: 999px; background: var(--hover); overflow: hidden; }
  .pull-fill { height: 100%; background: var(--accent); border-radius: 999px; transition: width .2s; }
  .pull-pct { font-variant-numeric: tabular-nums; }
  .browse-tabs { margin-bottom: 8px; }
  .chip-row { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 4px 8px 4px; }
  .chip { border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 999px; padding: 3px 12px; font-size: 12px; font-family: inherit; cursor: pointer; }
  .chip:hover { background: var(--hover); color: var(--text); }
  .chip.sel { background: var(--text); border-color: transparent; color: var(--bg); }
  .browse-count { font-size: 12px; color: var(--muted); margin: 0; padding: 0 4px 8px 4px; display: flex; align-items: center; gap: 8px; }
  .fetch-progress { font-variant-numeric: tabular-nums; }
  .browse-card { border: 1px solid var(--border); border-radius: 14px; padding: 10px 12px; margin: 0 0 8px 0; display: flex; flex-direction: column; gap: 6px; }
  .browse-head { display: flex; align-items: center; gap: 8px; }
  .browse-name { font-size: 13.5px; font-weight: 700; color: var(--text); }
  .browse-installed { font-size: 11px; font-weight: 600; color: var(--ok); border: 1px solid var(--ok); border-radius: 999px; padding: 1px 8px; }
  .browse-blurb { font-size: 12.5px; color: var(--text-dim); margin: 0; line-height: 1.4; }
  .browse-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .browse-cat { font-size: 11px; color: var(--muted); background: var(--hover); border-radius: 999px; padding: 1px 8px; }
  .browse-params { font-size: 11.5px; color: var(--muted); }
  .browse-actions { display: flex; gap: 8px; align-items: center; }
  .browse-variant { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 12.5px; font-family: inherit; padding: 5px 10px; }
  .browse-variant:focus { outline: none; border-color: var(--accent); }
  .setting-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0 0 16px 0; }
  .quit-row { margin-top: 20px; margin-bottom: 6px; padding-top: 16px; border-top: 1px solid var(--border); }
  .setting-label { font-size: 14px; color: var(--text); }
  .seg { display: flex; background: var(--hover); border-radius: 12px; padding: 3px; gap: 2px; }
  .seg-btn { border: none; background: transparent; cursor: pointer; font: inherit; font-size: 13.5px; color: var(--text-dim); padding: 6px 16px; border-radius: 9px; }
  .seg-btn.sel { background: var(--card); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
  .swatches { display: flex; gap: 10px; }
  .swatch { width: 32px; height: 32px; border-radius: 50%; border: 2px solid transparent; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 14px; color: #fff; padding: 0; }
  .swatch.sel { border-color: var(--text); }
  .swatch-check { line-height: 1; text-shadow: 0 1px 2px rgba(0,0,0,.4); }
  .setting-stacked { display: flex; flex-direction: column; gap: 8px; margin: 0 0 4px 0; }
  .setting-hint { color: var(--muted); font-weight: 400; }
  .stopwords-input { width: 100%; box-sizing: border-box; border: 1px solid var(--border-strong); border-radius: 12px; background: var(--card); color: var(--text); font: inherit; font-size: 13px; line-height: 1.6; padding: 10px 12px; resize: vertical; outline: none; }
  .stopwords-input:focus { border-color: var(--accent); }
  .setting-note { font-size: 12px; color: var(--muted); }
  .or-note-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 0 4px 10px 4px; }
  .or-panel { display: flex; flex-direction: column; gap: 4px; flex: 1; min-height: 0; }
  .or-models { flex: 1; min-height: 120px; }
  .or-note-row .setting-note { flex: 1; min-width: 0; }
  .or-meta { flex-wrap: wrap; }
  .or-usage { color: var(--muted-2); font-variant-numeric: tabular-nums; }
  .engine-switch { display: flex; gap: 4px; padding: 8px 8px 0; }
  .engine-switch.settings-engines { padding: 2px 0; flex-wrap: wrap; }
  .engine-opt { display: inline-flex; align-items: center; justify-content: center; gap: 6px; flex: 1; border: 1px solid var(--border); background: transparent; color: var(--muted); border-radius: 10px; padding: 5px 10px; font-size: 12.5px; cursor: pointer; }
  .engine-opt.sel { background: var(--hover); color: var(--text); border-color: var(--border-2); }
  .engine-opt:disabled { opacity: .5; cursor: default; }
  .approval-row { display: flex; gap: 8px; align-items: flex-start; cursor: pointer; }
  .approval-row input { margin-top: 2px; }
  .notify-sound-pick { max-width: 320px; padding-left: 21px; }
  .notify-sound-pick :disabled { opacity: 0.5; cursor: default; }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
    .settings-card { width: calc(100vw - 24px); height: 86vh; }
    .settings-layout { flex-direction: column; }
    .settings-nav { width: 100%; border-right: none; border-bottom: 1px solid var(--border); padding: 12px; gap: 8px; flex-direction: row; flex-wrap: wrap; align-items: center; }
    .settings-nav-title { width: 100%; margin: 0 0 2px 4px; }
    .settings-nav-group { flex-direction: row; flex-wrap: wrap; align-items: center; gap: 4px; }
    .settings-nav-label { display: none; }
    .settings-nav-btn { width: auto; padding: 5px 10px; }
  }
</style>
