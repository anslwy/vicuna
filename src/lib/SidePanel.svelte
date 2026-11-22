<script lang="ts">
  import GitPanel from "$lib/GitPanel.svelte";
  import type { WorkTreeEntry } from "$lib/work";
  import type { BrowserStatus, EngineDownloadProgress } from "$lib/browser";

  export interface SideTab {
    key: string;
    kind: "files" | "terminal" | "browser" | "git" | "plugin";
    title: string;
    pluginId?: string;
    panelId?: string;
    termId?: string;
    dead?: boolean;
    error?: string | null;
  }

  export interface PluginPanelEntry {
    key: string;
    pluginId: string;
    pluginName: string;
    title: string;
    rev: string;
    url: string | null;
    icon: string | null;
  }

  let {
    explorerOpen = $bindable(false),
    explorerWidth = $bindable(300),
    expResizing,
    explorerMinW,
    explorerDefaultW,
    sideTabs,
    activeSideTab,
    newTabMenu = $bindable(false),
    explorerRoot,
    explorerRootBase,
    explorerQuery = $bindable(""),
    explorerLoading,
    explorerError,
    explorerVisible,
    expRenaming,
    expRenameDraft = $bindable(""),
    collapsedDirs,
    pluginPanels,
    gitRefreshTick,
    onServer,
    browserTabVisible,
    browserStarting,
    browserKillArmed,
    browserDaemon,
    browserProxyUrl,
    browserFrameRev,
    browserFrameSrc,
    browserStartDisplay,
    browserDownload,
    browserStartSeconds,
    browserError,
    expNotice,
    displayWorkspace,
    formatSize,
    panelFrame,
    onExpResizeStart,
    onExpResizeKey,
    onSwitchSideTab,
    onCloseSideTab,
    onRestartBrowserFromButton,
    onRestartBrowserDaemon,
    onEnsureBrowserDaemon,
    onOpenSideExplorer,
    onOpenSideTerminal,
    onOpenSideBrowser,
    onOpenSideGit,
    onOpenSidePanel,
    onLoadExplorer,
    onCommitExpRename,
    onCancelExpRename,
    onToggleExplorerDir,
    onOpenExpCtx,
    onOpenInEditor,
    onRevealWorkPath
  }: {
    explorerOpen: boolean;
    explorerWidth: number;
    expResizing: boolean;
    explorerMinW: number;
    explorerDefaultW: number;
    sideTabs: SideTab[];
    activeSideTab: string;
    newTabMenu: boolean;
    explorerRoot: string;
    explorerRootBase: string;
    explorerQuery: string;
    explorerLoading: boolean;
    explorerError: string | null;
    explorerVisible: WorkTreeEntry[];
    expRenaming: string | null;
    expRenameDraft: string;
    collapsedDirs: string[];
    pluginPanels: PluginPanelEntry[];
    gitRefreshTick: number;
    onServer: boolean;
    browserTabVisible: boolean;
    browserStarting: boolean;
    browserKillArmed: boolean;
    browserDaemon: BrowserStatus | null;
    browserProxyUrl: string | null;
    browserFrameRev: number;
    browserFrameSrc: string;
    browserStartDisplay: string;
    browserDownload: EngineDownloadProgress | null;
    browserStartSeconds: number;
    browserError: string | null;
    expNotice: string | null;
    displayWorkspace: (p: string) => string;
    formatSize: (n: number) => string;
    panelFrame: (el: HTMLIFrameElement, arg: { key: string; pluginId: string }) => { destroy: () => void };
    onExpResizeStart: (e: PointerEvent) => void;
    onExpResizeKey: (e: KeyboardEvent) => void;
    onSwitchSideTab: (key: string) => void;
    onCloseSideTab: (key: string) => void;
    onRestartBrowserFromButton: () => Promise<void>;
    onRestartBrowserDaemon: () => Promise<void>;
    onEnsureBrowserDaemon: () => Promise<unknown>;
    onOpenSideExplorer: () => void;
    onOpenSideTerminal: () => Promise<void>;
    onOpenSideBrowser: () => void;
    onOpenSideGit: () => void;
    onOpenSidePanel: (key: string) => void;
    onLoadExplorer: () => Promise<void>;
    onCommitExpRename: () => Promise<void>;
    onCancelExpRename: () => void;
    onToggleExplorerDir: (path: string) => void;
    onOpenExpCtx: (ev: MouseEvent, path: string, isDir: boolean) => void;
    onOpenInEditor: (path: string) => Promise<void>;
    onRevealWorkPath: (path: string) => Promise<void>;
  } = $props();
</script>

<aside
  class="explorer {expResizing ? 'resizing' : ''}"
  class:hidden={!explorerOpen}
  data-testid="explorer"
  style="width: {explorerWidth}px"
  ondragover={(e) => e.preventDefault()}
  ondrop={(e) => e.preventDefault()}
>
  <div
    class="exp-resize"
    data-testid="explorer-resize"
    role="slider"
    aria-orientation="vertical"
    aria-label="Resize side panel"
    aria-valuenow={explorerWidth}
    aria-valuemin={explorerMinW}
    aria-valuemax={560}
    tabindex="0"
    title="Drag to resize · double-click to reset"
    onpointerdown={onExpResizeStart}
    ondblclick={() => (explorerWidth = explorerDefaultW)}
    onkeydown={onExpResizeKey}
  ></div>
  <div class="side-tabs" data-testid="side-tabs" role="tablist" aria-label="Side panel tabs">
    {#each sideTabs as t (t.key)}
      {@const tabTitle = t.kind === "files" || t.kind === "git" || t.kind === "plugin" ? explorerRootBase || t.title : t.title}
      <!-- Files / Git follow the workspace: titled by its folder name, like terminal tabs -->
      <div
        class="side-tab {t.key === activeSideTab ? 'sel' : ''}"
        data-testid="side-tab"
        role="tab"
        aria-selected={t.key === activeSideTab}
        tabindex="0"
        title={displayWorkspace(explorerRoot.trim()) || t.title}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onSwitchSideTab(t.key);
          } else if (e.key === "Delete" || e.key === "Backspace") {
            e.preventDefault();
            onCloseSideTab(t.key);
          }
        }}
        onmousedown={(e) => {
          if (e.button === 1) e.preventDefault();
        }}
        onauxclick={(e) => {
          if (e.button === 1) {
            e.preventDefault();
            onCloseSideTab(t.key);
          }
        }}
      >
        <button
          class="side-tab-label"
          aria-label={t.kind === "files" ? "Files tab" : t.kind === "git" ? "Git tab" : t.kind === "plugin" ? `${t.title} tab` : `${t.title} tab`}
          title={t.kind === "plugin" ? t.title : undefined}
          onclick={() => onSwitchSideTab(t.key)}
        >
          {#if t.kind === "files"}
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
          {:else if t.kind === "plugin"}
            {@const pn = pluginPanels.find((x) => x.key === t.key)}
            {#if pn?.icon}
              <img class="plugin-tab-icon" src={pn.icon} width="13" height="13" alt="" referrerpolicy="no-referrer" />
            {:else}
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 3v4M15 3v4M6 7h12v4a6 6 0 0 1-12 0z"/><path d="M12 17v4"/></svg>
            {/if}
          {:else if t.kind === "git"}
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="5" r="2.5"/><circle cx="18" cy="5" r="2.5"/><circle cx="12" cy="19" r="2.5"/><path d="M6 7.5v1.5a3 3 0 0 0 3 3h6a3 3 0 0 0 3-3V7.5"/><path d="M12 12v4.5"/></svg>
          {:else if t.kind === "browser"}
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M2 12h20"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>
          {:else}
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>
          {/if}
          <span class="side-tab-title">{tabTitle}</span>
        </button>
        <button
          class="side-tab-x"
          data-testid="side-tab-close"
          aria-label="Close {tabTitle} tab"
          title="Close tab"
          onclick={(e) => {
            e.stopPropagation();
            onCloseSideTab(t.key);
          }}
        >×</button>
      </div>
    {/each}
    <button
      class="icon-btn side-add"
      data-testid="side-add"
      aria-label="New panel tab"
      title="New tab: Explorer, Terminal, Browser or Git Diff"
      onclick={() => (newTabMenu = !newTabMenu)}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>
    </button>
    <div class="spacer"></div>
    {#if browserTabVisible}
      <button
        class="icon-btn exp-btn"
        class:browser-restarting={browserStarting}
        class:browser-kill-armed={browserKillArmed}
        data-testid="browser-restart-tab-btn"
        aria-label={browserKillArmed ? "Confirm browser takeover" : "Start, restart, or take over the browser engine"}
        title={browserKillArmed
          ? "Click again to confirm: force-stop the foreign browser and start Vicuna's own"
          : "Start the browser when down · restart when owned · force-stop + start when foreign (asks first)"}
        disabled={browserStarting}
        onclick={() => void onRestartBrowserFromButton()}
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.64-6.36"/><polyline points="21 3 21 9 15 9"/></svg>
      </button>
    {/if}
    <button
      class="icon-btn exp-btn"
      data-testid="explorer-close"
      aria-label="Close side panel"
      title="Close panel"
      onclick={() => (explorerOpen = false)}
    >
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
    </button>
    {#if newTabMenu}
      <button
        class="ctx-backdrop"
        data-testid="new-tab-backdrop"
        aria-label="Close new tab menu"
        onclick={() => (newTabMenu = false)}
        tabindex="-1"
      ></button>
      <div class="ctx-menu new-tab-menu" data-testid="new-tab-menu">
        <button class="ctx-item" data-testid="new-tab-explorer" onclick={onOpenSideExplorer}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
          <span>Explorer</span>
        </button>
        <button class="ctx-item" data-testid="new-tab-terminal" onclick={() => void onOpenSideTerminal()}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>
          <span>Terminal</span>
        </button>
        <button class="ctx-item" data-testid="new-tab-browser" onclick={onOpenSideBrowser}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M2 12h20"/><path d="M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10z"/></svg>
          <span>Browser</span>
        </button>
        <button class="ctx-item" data-testid="new-tab-git" onclick={onOpenSideGit}>
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="5" r="2.5"/><circle cx="18" cy="5" r="2.5"/><circle cx="12" cy="19" r="2.5"/><path d="M6 7.5v1.5a3 3 0 0 0 3 3h6a3 3 0 0 0 3-3V7.5"/><path d="M12 12v4.5"/></svg>
          <span>Git Diff</span>
        </button>
        {#each pluginPanels as pn (pn.key)}
          <button class="ctx-item" data-testid="new-tab-plugin-{pn.key.replaceAll(':', '-')}" title="From the {pn.pluginName} plugin" onclick={() => onOpenSidePanel(pn.key)}>
            {#if pn.icon}
              <img class="plugin-tab-icon" src={pn.icon} width="14" height="14" alt="" referrerpolicy="no-referrer" />
            {:else}
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 3v4M15 3v4M6 7h12v4a6 6 0 0 1-12 0z"/><path d="M12 17v4"/></svg>
            {/if}
            <span>{pn.title}</span>
          </button>
        {/each}
      </div>
    {/if}
  </div>
  {#if sideTabs.length === 0}
    <div class="exp-empty">No tabs open — press + to add an Explorer, Terminal, Browser or Git Diff.</div>
  {/if}
  {#each sideTabs as t (t.key)}
    <div class="side-view" class:hidden={t.key !== activeSideTab}>
      {#if t.kind === "files"}
        <div class="explorer-search">
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
          <input
            class="explorer-search-input"
            data-testid="explorer-search"
            placeholder="Search files"
            bind:value={explorerQuery}
            spellcheck={false}
          />
          <button
            class="icon-btn exp-btn"
            data-testid="explorer-refresh"
            aria-label="Refresh files"
            title="Refresh"
            disabled={explorerLoading}
            onclick={() => void onLoadExplorer()}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.64-6.36"/><polyline points="21 3 21 9 15 9"/></svg>
          </button>
        </div>
        <div class="explorer-list" data-testid="explorer-list">
          {#if explorerLoading}
            <div class="exp-empty">Loading…</div>
          {:else if explorerError}
            <div class="exp-empty">{explorerError}</div>
          {:else if explorerVisible.length === 0}
            <div class="exp-empty">{explorerQuery.trim() ? "No matching files." : "Empty folder."}</div>
          {:else}
            {#each explorerVisible as e (e.path)}
              {#if expRenaming === e.path}
                <div class="exp-row exp-renaming" style="padding-left: {10 + e.depth * 14}px">
                  {#if e.is_dir}
                    <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                  {:else}
                    <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                  {/if}
                  <input
                    class="exp-rename"
                    data-testid="explorer-rename"
                    bind:value={expRenameDraft}
                    aria-label="New name for {e.name}"
                    onclick={(ev) => ev.stopPropagation()}
                    onkeydown={(ev) => {
                      if (ev.key === "Enter") {
                        ev.preventDefault();
                        void onCommitExpRename();
                      } else if (ev.key === "Escape") {
                        ev.preventDefault();
                        ev.stopPropagation();
                        onCancelExpRename();
                      }
                    }}
                    onblur={() => void onCommitExpRename()}
                  />
                </div>
              {:else if e.is_dir}
                {@const collapsed = collapsedDirs.includes(e.path)}
                <button
                  class="exp-row exp-dir"
                  style="padding-left: {10 + e.depth * 14}px"
                  data-testid="explorer-dir"
                  title={e.path}
                  onclick={() => onToggleExplorerDir(e.path)}
                  oncontextmenu={(ev) => onOpenExpCtx(ev, e.path, true)}
                >
                  <span class="exp-chev">{collapsed ? "▸" : "▾"}</span>
                  <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                  <span class="exp-name">{e.name}</span>
                </button>
              {:else}
                <button
                  class="exp-row exp-file"
                  style="padding-left: {10 + e.depth * 14}px"
                  data-testid="explorer-file"
                  title={e.path}
                  onclick={() => void onOpenInEditor(e.path)}
                  oncontextmenu={(ev) => onOpenExpCtx(ev, e.path, false)}
                >
                  <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                  <span class="exp-name">{e.name}</span>
                  {#if e.size != null}
                    <span class="exp-size">{formatSize(e.size)}</span>
                  {/if}
                </button>
              {/if}
            {/each}
          {/if}
        </div>
      {:else if t.kind === "plugin"}
        {@const pn = pluginPanels.find((x) => x.key === t.key)}
        {#if pn?.url}
          {#key pn.rev}
            <!-- opaque origin: NO allow-same-origin; the server adds a CSP sandbox too -->
            <iframe
              class="plugin-frame"
              data-testid="plugin-frame-{t.key.replaceAll(':', '-')}"
              title="{pn.title} ({pn.pluginName} plugin)"
              src={pn.url}
              sandbox="allow-scripts"
              referrerpolicy="no-referrer"
              allow=""
              use:panelFrame={{ key: t.key, pluginId: pn.pluginId }}
            ></iframe>
          {/key}
        {:else}
          <div class="exp-empty">This panel needs a Vicuna server to load from (desktop app, a connected server, or a bridged browser).</div>
        {/if}
      {:else if t.kind === "git"}
        <GitPanel
          root={explorerRoot.trim()}
          active={explorerOpen && t.key === activeSideTab}
          refreshTick={gitRefreshTick}
          onopenfile={(p) => void onOpenInEditor(p)}
          onreveal={(p) => void onRevealWorkPath(p)}
        />
      {:else if t.kind === "browser"}
        {#if browserDaemon?.running && (!onServer || browserProxyUrl)}
          {#if browserDaemon.health?.ui_ok === false}
            <div class="exp-empty browser-error" data-testid="browser-broken-ui">
              <span>Browser engine files changed while it was running (usually an app update) — the view can't load. Restart the browser to pick up the new files.{#if !browserDaemon.owned} Vicuna didn't start this daemon; taking it over asks first and loses its tabs.{/if}</span>
              {#if browserDaemon.owned}
                <button class="mini-btn" data-testid="browser-broken-restart-btn" onclick={() => void onRestartBrowserDaemon()}>Restart browser</button>
              {:else}
                <button class="mini-btn" data-testid="browser-broken-takeover-btn" onclick={() => void onRestartBrowserFromButton()}>Take over &amp; restart</button>
              {/if}
            </div>
          {:else}
            {#if browserDaemon.engine_stale}
              <div class="browser-stale" data-testid="browser-stale" role="status">
                <span>{browserDaemon.warning ?? "Browser engine is outdated."}</span>
                {#if browserDaemon.owned}
                  <button class="mini-btn" data-testid="browser-restart-btn" onclick={() => void onRestartBrowserDaemon()}>Restart browser</button>
                {:else}
                  <button class="mini-btn" data-testid="browser-stale-takeover-btn" onclick={() => void onRestartBrowserFromButton()}>Take over &amp; restart</button>
                {/if}
              </div>
            {:else if browserDaemon.warning}
              <div class="browser-stale" data-testid="browser-warning" role="status">
                <span>{browserDaemon.warning}</span>
                {#if browserDaemon.owned}
                  <button class="mini-btn" data-testid="browser-fallback-retry" onclick={() => void onRestartBrowserDaemon()}>Retry built-in engine</button>
                {/if}
              </div>
            {/if}
            {#key browserFrameRev}
              <iframe
                class="browser-frame"
                data-testid="browser-frame"
                title="Shared browser"
                src={browserFrameSrc}
                sandbox="allow-scripts allow-same-origin allow-forms allow-popups allow-downloads allow-modals"
              ></iframe>
            {/key}
          {/if}
        {:else if browserStarting}
          <div class="exp-empty" data-testid="browser-starting">
            <div>{browserStartDisplay}</div>
            {#if browserDownload}
              {#if browserDownload.percent != null}
                <progress class="browser-dl-bar" max="100" value={browserDownload.percent}></progress>
              {/if}
              <div class="browser-start-hint">
                One-time download for the built-in browser engine.
              </div>
            {:else if browserStartSeconds >= 20}
              <div class="browser-start-hint">
                First launch can take a minute while antivirus scans the engine. If this never finishes, the start
                path falls back to an installed Chrome/Edge automatically — retry from the restart button above.
              </div>
            {/if}
          </div>
        {:else if browserError}
          <div class="exp-empty browser-error" data-testid="browser-error">
            <span>{browserError}</span>
            <button class="mini-btn" data-testid="browser-retry" onclick={() => void onEnsureBrowserDaemon()}>Retry</button>
          </div>
        {:else}
          <div class="exp-empty browser-error" data-testid="browser-start">
            <span>Browser daemon not running.</span>
            <button class="mini-btn" data-testid="browser-start-btn" onclick={() => void onEnsureBrowserDaemon()}>Start browser</button>
          </div>
        {/if}
      {:else}
        {#if t.error}
          <div class="exp-empty">{t.error}</div>
        {:else}
          <div class="term-wrap" data-term={t.key} data-testid="terminal-view"></div>
        {/if}
      {/if}
    </div>
  {/each}
  {#if expNotice}
    <div class="explorer-toast" data-testid="explorer-notice" role="status">{expNotice}</div>
  {/if}
</aside>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .icon-btn { background: transparent; border: none; cursor: pointer; color: var(--icon); padding: 4px; border-radius: 6px; }
  .icon-btn:hover { background: var(--hover); }
  .spacer { flex: 1; }
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .ctx-backdrop { position: fixed; inset: 0; z-index: 40; border: none; background: transparent; cursor: default; padding: 0; }
  .ctx-menu { position: fixed; z-index: 41; min-width: 150px; background: var(--card); border: 1px solid var(--border); border-radius: 14px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 5px; display: flex; flex-direction: column; }
  .ctx-item { border: none; background: transparent; text-align: left; font: inherit; font-size: 14px; padding: 8px 12px; border-radius: 9px; cursor: pointer; color: var(--text); }
  .ctx-item:hover { background: var(--hover-3); }
  .explorer { width: 300px; flex: 0 0 auto; border-left: 1px solid var(--border); background: var(--bg); display: flex; flex-direction: column; min-height: 0; min-width: 0; position: relative; }
  .explorer.hidden { display: none; }
  .explorer.resizing { user-select: none; }
  .exp-resize { position: absolute; left: -4px; top: 0; bottom: 0; width: 9px; cursor: ew-resize; z-index: 5; touch-action: none; }
  .exp-resize::after { content: ""; position: absolute; left: 4px; top: 0; bottom: 0; width: 1px; background: transparent; transition: background .15s; }
  .exp-resize:hover::after, .explorer.resizing .exp-resize::after { background: var(--accent); }
  .exp-resize:focus-visible { outline: 1.5px solid var(--accent); outline-offset: -1.5px; }
  .exp-btn { width: 26px; height: 26px; }
  .side-tabs { position: relative; display: flex; align-items: center; gap: 4px; padding: 10px 8px 8px 10px; border-bottom: 1px solid var(--border); }
  .side-tab { display: flex; align-items: center; gap: 2px; min-width: 0; max-width: 150px; border: none; background: transparent; border-radius: 9px; padding: 3px 3px 3px 8px; cursor: pointer; color: var(--muted-2); }
  .side-tab:hover { background: var(--hover-3); }
  .side-tab.sel { background: var(--hover-3); color: var(--text); }
  .side-tab-label { display: flex; align-items: center; gap: 6px; min-width: 0; border: none; background: transparent; color: inherit; font: inherit; font-size: 13px; padding: 2px 0; cursor: pointer; }
  .plugin-frame { flex: 1; width: 100%; height: 100%; border: 0; display: block; background: var(--bg); }
  .plugin-tab-icon { display: block; flex-shrink: 0; }
  .side-tab-title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .side-tab-x { flex-shrink: 0; border: none; background: transparent; color: var(--faint); font-size: 14px; line-height: 1; width: 20px; height: 20px; border-radius: 6px; cursor: pointer; padding: 0; }
  .side-tab-x:hover { background: var(--hover); color: var(--text); }
  .side-add { width: 26px; height: 26px; flex-shrink: 0; }
  .new-tab-menu { position: absolute; left: 8px; top: calc(100% - 2px); z-index: 42; min-width: 160px; }
  .new-tab-menu .ctx-item { display: flex; align-items: center; gap: 9px; }
  .side-view { flex: 1; display: flex; flex-direction: column; min-height: 0; min-width: 0; }
  .side-view.hidden { display: none; }
  .term-wrap { flex: 1; min-height: 0; padding: 8px 4px 8px 10px; background: var(--bg); }
  .browser-frame { flex: 1; min-height: 0; width: 100%; border: none; background: var(--bg); }
  .browser-stale { display: flex; align-items: center; gap: 10px; padding: 8px 12px; font-size: 12.5px; color: var(--text); background: color-mix(in srgb, var(--accent) 12%, var(--card)); border-bottom: 1px solid var(--border); }
  .browser-stale span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .browser-start-hint { margin-top: 8px; font-size: 12px; line-height: 1.45; color: var(--muted); opacity: 0.85; }
  .browser-dl-bar { margin-top: 10px; width: min(240px, 70%); height: 6px; accent-color: var(--accent); }
  @keyframes browser-restart-spin { to { transform: rotate(360deg); } }
  .browser-restarting svg { animation: browser-restart-spin 1s linear infinite; }
  .browser-kill-armed { color: var(--danger); }
  .exp-empty.browser-error { display: flex; flex-direction: column; align-items: center; gap: 10px; }
  .exp-empty.browser-error span { overflow-wrap: anywhere; }
  .explorer-search { display: flex; align-items: center; gap: 7px; margin: 10px 10px 8px 10px; padding: 7px 6px 7px 10px; border: 1px solid var(--border); border-radius: 10px; background: var(--card); color: var(--muted); }
  .explorer-search-input { flex: 1; min-width: 0; border: none; outline: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding: 0; }
  .explorer-list { flex: 1; overflow-y: auto; overflow-x: hidden; padding: 0 6px 10px 6px; min-height: 0; }
  .exp-row { display: flex; align-items: center; gap: 7px; width: 100%; box-sizing: border-box; border: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding-top: 5px; padding-bottom: 5px; padding-right: 8px; border-radius: 8px; cursor: pointer; text-align: left; min-width: 0; }
  .exp-row:hover { background: var(--hover-3); }
  .exp-rename { flex: 1; min-width: 0; font: inherit; font-size: 12.5px; color: var(--text); background: var(--bg); border: 1.5px solid var(--accent); border-radius: 6px; padding: 2px 6px; outline: none; }
  .exp-chev { flex-shrink: 0; width: 12px; font-size: 10px; color: var(--muted); }
  .exp-icon { flex-shrink: 0; color: var(--muted-2); }
  .exp-dir .exp-icon { color: var(--accent); }
  .exp-name { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .exp-file .exp-name { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .exp-size { flex-shrink: 0; font-size: 11px; color: var(--faint); }
  .exp-empty { padding: 18px 14px; font-size: 13px; color: var(--muted); text-align: center; }
  .explorer-toast { position: absolute; left: 12px; right: 12px; bottom: 12px; z-index: 20; background: var(--card); border: 1px solid var(--border-2); border-radius: 10px; box-shadow: 0 8px 24px rgba(0,0,0,.18); padding: 8px 12px; font-size: 12.5px; color: var(--text); pointer-events: none; text-align: center; animation: notice-in .14s ease-out; }
  @keyframes notice-in { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) { .explorer-toast { animation: none; } }
</style>
