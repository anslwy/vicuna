<script lang="ts">
  // Plugins modal: install from a GitHub repo (or manifest URL),
  // upgrade, uninstall. All work happens on the server (plugin_* actions),
  // so a remote server window manages that server's plugins.
  import { onMount } from "svelte";
  import { invokeOrBridge } from "./bridge";
  import { installBlocked, type InstalledPlugin } from "./pluginTheme";
  import { accountSummary, commandLine, consentPayload, pendingConsent, type PluginHarness } from "./pluginHarness";
  import { cardAction, filterCatalog, githubRepoUrl, KIND_LABEL, safeImageUrl, type Catalog, type CatalogEntry } from "./pluginCatalog";

  let { plugins, activeThemeKey, onChanged, onUseTheme, onOpenPanel, onHarnessesChanged }: {
    onHarnessesChanged?: () => void;
    plugins: InstalledPlugin[];
    activeThemeKey: string | null;
    onChanged: (list: InstalledPlugin[]) => void;
    onUseTheme: (pluginId: string, themeId: string) => void;
    onOpenPanel?: (key: string) => void;
  } = $props();

  let source = $state("");
  let busy = $state<string | null>(null); // "install" | "upgrade:<id>" | "uninstall:<id>"
  let error = $state("");
  let notice = $state("");
  let confirmRemove = $state<string | null>(null);

  // Harness consent: shown after install/upgrade (or "Enable…") for every
  // harness whose exact command the user hasn't approved. The server enforces
  // it; declining leaves the harness disabled.
  let consent = $state<{ plugin: InstalledPlugin; harnesses: PluginHarness[] } | null>(null);
  let consentBusy = $state(false);
  let preview = $state<{ name: string; url: string } | null>(null);

  $effect(() => {
    if (!preview) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      preview = null;
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  /** Move the dialog to <body>: the Plugins modal card is transformed, which
   *  would trap position:fixed inside it (and clip the dialog). */
  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  function askConsent(p: InstalledPlugin, list = pendingConsent(p)) {
    if (list.length) consent = { plugin: p, harnesses: list };
  }

  async function acceptConsent() {
    if (!consent || consentBusy) return;
    consentBusy = true;
    try {
      await invokeOrBridge("plugin_harness_consent", consentPayload(consent.plugin.id, consent.harnesses));
      notice = `Enabled ${consent.harnesses.map((h) => h.label).join(", ")}.`;
      consent = null;
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
      onHarnessesChanged?.();
    } catch (e: any) {
      error = e?.message ?? String(e);
      consent = null;
    } finally {
      consentBusy = false;
    }
  }

  function declineConsent() {
    if (!consent) return;
    notice = `${consent.harnesses.map((h) => h.label).join(", ")} left disabled. Enable it later from Installed.`;
    consent = null;
  }

  async function disableHarness(p: InstalledPlugin, h: PluginHarness) {
    try {
      await invokeOrBridge("plugin_harness_revoke", { id: p.id, harness: h.id });
      notice = `Disabled ${h.label}.`;
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
      onHarnessesChanged?.();
    } catch (e: any) {
      error = e?.message ?? String(e);
    }
  }

  /** the first list load: shown as "Loading…", never blocks Install */
  let listing = $state(false);

  async function refresh() {
    listing = true;
    try {
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      listing = false;
    }
  }

  onMount(() => {
    void refresh();
  });

  // Explore: the official index, fetched + validated by the server.
  let view = $state<"installed" | "explore">("installed");
  let catalog = $state<Catalog | null>(null);
  let catalogLoading = $state(false);
  let catalogError = $state("");
  let query = $state("");
  const shown = $derived(catalog ? filterCatalog(catalog.plugins, query) : []);
  const installedById = $derived(new Map(plugins.map((p) => [p.id, p])));

  async function loadCatalog(refresh = false) {
    catalogLoading = true;
    catalogError = "";
    try {
      catalog = await invokeOrBridge<Catalog>("plugin_catalog", { refresh });
    } catch (e: any) {
      catalogError = e?.message ?? String(e);
    } finally {
      catalogLoading = false;
    }
  }

  function openExplore() {
    view = "explore";
    if (!catalog && !catalogLoading) void loadCatalog();
  }

  async function installEntry(e: CatalogEntry) {
    if (busy) return;
    busy = `install:${e.id}`;
    error = notice = "";
    try {
      const p = await invokeOrBridge<InstalledPlugin>("plugin_install", { source: e.source });
      notice = `Installed ${p.name} ${p.version}.`;
      askConsent(p);
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
      await loadCatalog();
    } catch (err: any) {
      error = err?.message ?? String(err);
    } finally {
      busy = null;
    }
  }

  async function install() {
    const s = source.trim();
    if (installBlocked(s, busy)) return;
    busy = "install";
    error = notice = "";
    try {
      const p = await invokeOrBridge<InstalledPlugin>("plugin_install", { source: s });
      notice = `Installed ${p.name} ${p.version}.`;
      askConsent(p);
      source = "";
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      busy = null;
    }
  }

  async function upgrade(p: InstalledPlugin) {
    busy = `upgrade:${p.id}`;
    error = notice = "";
    try {
      const r = await invokeOrBridge<InstalledPlugin & { changed: boolean; previousVersion: string }>("plugin_upgrade", { id: p.id });
      notice = r.changed
        ? r.previousVersion === r.version ? `${r.name} updated (still ${r.version}).` : `${r.name} upgraded ${r.previousVersion} → ${r.version}.`
        : `${r.name} is up to date (${r.version}).`;
      // a changed command loses its consent server-side: ask again
      askConsent(r);
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
      if (catalog) void loadCatalog();
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      busy = null;
    }
  }

  async function uninstall(p: InstalledPlugin) {
    if (confirmRemove !== p.id) {
      confirmRemove = p.id;
      return;
    }
    confirmRemove = null;
    busy = `uninstall:${p.id}`;
    error = notice = "";
    try {
      await invokeOrBridge("plugin_uninstall", { id: p.id });
      notice = `Uninstalled ${p.name}.`;
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
      if (p.harnesses?.length) onHarnessesChanged?.();
      if (catalog) void loadCatalog();
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      busy = null;
    }
  }
</script>

<div class="pl" data-testid="plugins-panel">
  <div class="pl-seg" role="tablist">
    <button class="pl-seg-btn {view === 'installed' ? 'sel' : ''}" role="tab" aria-selected={view === "installed"} data-testid="plugins-tab-installed" onclick={() => (view = "installed")}>Installed{#if plugins.length}<span class="pl-count">{plugins.length}</span>{/if}</button>
    <button class="pl-seg-btn {view === 'explore' ? 'sel' : ''}" role="tab" aria-selected={view === "explore"} data-testid="plugins-tab-explore" onclick={openExplore}>Explore</button>
  </div>
  {#if view === "explore"}
    <div class="pl-row">
      <input class="pl-input pl-search" data-testid="plugin-search" placeholder="Search plugins" bind:value={query} spellcheck="false" autocomplete="off" />
      <button class="pl-btn" data-testid="plugin-catalog-refresh" disabled={catalogLoading} onclick={() => loadCatalog(true)} title="Fetch the index again">{catalogLoading ? "Loading…" : "Refresh"}</button>
    </div>
    {#if error}<div class="pl-err" data-testid="plugin-error">{error}</div>{/if}
    {#if notice}<div class="pl-note" data-testid="plugin-notice">{notice}</div>{/if}
    {#if catalogError}
      <div class="pl-err" data-testid="plugin-catalog-error">Couldn't load the plugin index: {catalogError}</div>
    {:else if !catalog}
      <div class="pl-muted">Loading plugins…</div>
    {:else if shown.length === 0}
      <div class="pl-muted" data-testid="plugin-catalog-empty">{query.trim() ? "No plugins match your search." : "The index has no plugins yet."}</div>
    {:else}
      <div class="pl-grid">
        {#each shown as e (e.id)}
          {@const act = cardAction(e)}
          {@const shot = safeImageUrl(e.screenshot)}
          {@const icon = safeImageUrl(e.icon)}
          {@const inst = installedById.get(e.id)}
          {@const repoUrl = githubRepoUrl(e.source)}
          {@const hasQuickActions = !!(inst && act === "upgrade" && ((inst.panels?.length ?? 0) > 0 || inst.themes.length > 0))}
          <div class="pl-card" data-testid="plugin-card-{e.id}">
            <div class="pl-card-body">
              <div class="pl-item-top">
                {#if icon}<img class="pl-icon" src={icon} alt="" loading="lazy" referrerpolicy="no-referrer" />{/if}
                <div class="pl-card-title">
                  <span class="pl-name">{e.name}</span>
                  <span class="pl-ver">by {e.author}</span>
                </div>
                <span class="pl-spacer"></span>
                {#if act === "install"}
                  <button class="pl-btn primary" data-testid="plugin-card-install-{e.id}" disabled={!!busy} onclick={() => installEntry(e)}>{busy === `install:${e.id}` ? "Installing…" : "Install"}</button>
                {:else if act === "upgrade" && inst}
                  <span class="pl-badge ok" data-testid="plugin-card-installed-{e.id}">Installed · v{e.installed}</span>
                  <button class="pl-btn" data-testid="plugin-card-upgrade-{e.id}" disabled={!!busy} onclick={() => upgrade(inst)}>{busy === `upgrade:${e.id}` ? "Checking…" : "Upgrade"}</button>
                {:else if act === "conflict"}
                  <span class="pl-badge" title="A plugin with this id is installed from another source">Installed from elsewhere</span>
                {:else if act === "incompatible"}
                  <span class="pl-badge" title="Update Vicuna to install this plugin">Needs Vicuna {e.minVicuna}</span>
                {/if}
              </div>
              <div class="pl-muted pl-card-desc" title={e.description}>{e.description}</div>
              <div class="pl-card-foot">
                <div class="pl-tags">
                  {#each e.kinds as k (k)}<span class="pl-kind">{KIND_LABEL[k] ?? k}</span>{/each}
                  {#each e.tags as t (t)}<span class="pl-tag">{t}</span>{/each}
                  <span class="pl-src" title={e.source}>
                    {#if repoUrl}
                      <a class="pl-src-link" href={repoUrl} target="_blank" rel="noopener noreferrer" title="Open {e.source} on GitHub" data-testid="plugin-card-repo-{e.id}">
                        <svg class="pl-gh" width="13" height="13" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>
                        <span class="pl-src-text">{e.source}</span>
                      </a>
                    {:else}
                      {e.source}
                    {/if}
                  </span>
                </div>
                {#if hasQuickActions || shot}
                  <div class="pl-themes">
                    {#if inst && act === "upgrade"}
                      {#each inst.panels ?? [] as pn (pn.id)}
                        <button class="pl-theme" data-testid="plugin-card-panel-{e.id}-{pn.id}" title="Open this panel in the side bar" onclick={() => onOpenPanel?.(`plugin:${inst.id}:${pn.id}`)}>Open {pn.title}</button>
                      {/each}
                      {#each inst.themes as t (t.id)}
                        {@const key = `${inst.id}/${t.id}`}
                        <button class="pl-theme {activeThemeKey === key ? 'sel' : ''}" data-testid="plugin-card-theme-{e.id}-{t.id}" onclick={() => onUseTheme(inst.id, t.id)} title="Use this theme">
                          <span class="pl-dot" style="background: {t.vars['--bg'] ?? (t.base === 'dark' ? '#3d3332' : '#ffffff')}; border-color: {t.accent?.bg ?? 'var(--border-2)'}"></span>
                          {t.name}{#if activeThemeKey === key}{" "}✓{/if}
                        </button>
                      {/each}
                    {/if}
                    {#if shot}
                      <button class="pl-theme pl-preview-btn" data-testid="plugin-card-preview-{e.id}" title="Preview {e.name} screenshot" onclick={() => (preview = { name: e.name, url: shot })}>
                        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/></svg>
                        Preview
                      </button>
                    {/if}
                  </div>
                {/if}
              </div>
            </div>
          </div>
        {/each}
      </div>
      <p class="pl-muted pl-foot">From the <a href="https://github.com/anslwy/vicuna-plugins" target="_blank" rel="noopener noreferrer">Vicuna plugin index</a>{#if catalog.skipped} · {catalog.skipped} invalid {catalog.skipped === 1 ? "entry" : "entries"} hidden{/if}. Plugins are made by their authors; install only what you trust.</p>
    {/if}
  {:else}
  <p class="pl-muted">Install plugins from a GitHub repo (<code>owner/repo</code> or its URL — private repos use the server's <code>gh</code> sign-in), a local folder path, or a direct <code>vicuna-plugin.json</code> link. Plugins can add themes, side panels, Work harnesses, and messaging bridges.</p>
  <form class="pl-row" onsubmit={(e) => { e.preventDefault(); void install(); }}>
    <input class="pl-input" data-testid="plugin-source" placeholder="owner/repo, /path/to/plugin, or https://github.com/owner/repo" bind:value={source} spellcheck="false" autocomplete="off" />
    <button class="pl-btn primary" data-testid="plugin-install" type="submit" disabled={installBlocked(source, busy)}>{busy === "install" ? "Installing…" : "Install"}</button>
  </form>
  {#if error}<div class="pl-err" data-testid="plugin-error">{error}</div>{/if}
  {#if notice}<div class="pl-note" data-testid="plugin-notice">{notice}</div>{/if}

  <div class="pl-head">Installed</div>
  {#if plugins.length === 0}
    <div class="pl-muted" data-testid="plugins-empty">{listing ? "Loading…" : "No plugins installed."}</div>
  {:else}
    {#each plugins as p (p.id)}
      {@const repoUrl = githubRepoUrl(p.source)}
      <div class="pl-item" data-testid="plugin-row-{p.id}">
        <div class="pl-item-top">
          <span class="pl-name">{p.name}</span>
          <span class="pl-ver">v{p.version}</span>
          <span class="pl-spacer"></span>
          <button class="pl-btn" data-testid="plugin-upgrade-{p.id}" disabled={!!busy} onclick={() => upgrade(p)}>{busy === `upgrade:${p.id}` ? "Checking…" : "Upgrade"}</button>
          <button class="pl-btn {confirmRemove === p.id ? 'danger' : ''}" data-testid="plugin-uninstall-{p.id}" disabled={!!busy} onclick={() => uninstall(p)} onblur={() => { if (confirmRemove === p.id) confirmRemove = null; }}>{busy === `uninstall:${p.id}` ? "Removing…" : confirmRemove === p.id ? "Confirm?" : "Uninstall"}</button>
        </div>
        {#if p.description}<div class="pl-muted">{p.description}</div>{/if}
        <div class="pl-src" title={p.source}>
          {#if repoUrl}
            <a class="pl-src-link" href={repoUrl} target="_blank" rel="noopener noreferrer" title="Open {p.source} on GitHub" data-testid="plugin-repo-{p.id}">
              <svg class="pl-gh" width="13" height="13" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8z"/></svg>
              <span class="pl-src-text">{p.source}</span>
            </a>
          {:else}
            {p.source}
          {/if}
        </div>
        <div class="pl-themes">
          {#if p.bridge}
            <span class="pl-theme" data-testid="plugin-bridge-{p.id}">Bridge · {p.bridge.name || p.name} (right-click a Work session to configure)</span>
          {/if}
          {#each p.panels ?? [] as pn (pn.id)}
            <button class="pl-theme" data-testid="plugin-panel-{p.id}-{pn.id}" title="Open this panel in the side bar" onclick={() => onOpenPanel?.(`plugin:${p.id}:${pn.id}`)}>
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/></svg>
              {pn.title}
            </button>
          {/each}
          {#each p.harnesses ?? [] as h (h.id)}
            <div class="pl-harness" data-testid="plugin-harness-{p.id}-{h.id}">
              <span class="pl-name">{h.label}</span>
              <code class="pl-cmd" title="The command this harness runs">{commandLine(h)}</code>
              {#if h.usage}<code class="pl-cmd" title="The command that reads its usage">{commandLine(h.usage)}</code>{/if}
              {#if h.enabled}<span class="pl-badge ok">Enabled</span>{:else}<span class="pl-badge">Disabled</span>{/if}
              {#if !h.installed}<span class="pl-badge" title={h.installHint ?? ""}>`{h.command}` not found</span>{/if}
              <span class="pl-spacer"></span>
              {#if h.enabled}
                <button class="pl-btn" data-testid="plugin-harness-disable-{p.id}-{h.id}" onclick={() => disableHarness(p, h)}>Disable</button>
              {:else}
                <button class="pl-btn primary" data-testid="plugin-harness-enable-{p.id}-{h.id}" onclick={() => askConsent(p, [h])}>Enable…</button>
              {/if}
            </div>
          {/each}
          {#each p.themes as t (t.id)}
            {@const key = `${p.id}/${t.id}`}
            <button class="pl-theme {activeThemeKey === key ? 'sel' : ''}" data-testid="plugin-theme-{p.id}-{t.id}" onclick={() => onUseTheme(p.id, t.id)} title="Use this theme">
              <span class="pl-dot" style="background: {t.vars['--bg'] ?? (t.base === 'dark' ? '#3d3332' : '#ffffff')}; border-color: {t.accent?.bg ?? 'var(--border-2)'}"></span>
              {t.name}{#if activeThemeKey === key}{" "}✓{/if}
            </button>
          {/each}
        </div>
      </div>
    {/each}
  {/if}
  {/if}
</div>

{#if consent}
  <div use:portal class="pl-portal">
  <div class="pl-backdrop" role="presentation" onclick={declineConsent}></div>
  <div class="pl-consent" role="dialog" aria-modal="true" aria-labelledby="pl-consent-title" data-testid="harness-consent">
    <h3 id="pl-consent-title">Allow {consent.plugin.name} to run a coding agent?</h3>
    <p class="pl-muted">This plugin adds {consent.harnesses.length === 1 ? "a Work harness" : "Work harnesses"}. When you pick {consent.harnesses.length === 1 ? "it" : "one"} for a session, Vicuna will run exactly:</p>
    {#each consent.harnesses as h (h.id)}
      <div class="pl-consent-h">
        <div class="pl-name">{h.label}</div>
        <code class="pl-cmd big" data-testid="harness-consent-command-{h.id}">{commandLine(h)}</code>
        {#if h.usage}
          <div class="pl-muted">and, to read its subscription usage for the Usage view:</div>
          <code class="pl-cmd big" data-testid="harness-consent-usage-{h.id}">{commandLine(h.usage)}</code>
        {/if}
        {#if accountSummary(h.accounts)}
          {@const acct = accountSummary(h.accounts)!}
          <div class="pl-muted" data-testid="harness-consent-accounts-{h.id}">Named accounts: each one runs with its own folders ({acct.folders.join(", ")}){acct.clear.length ? `, without ${acct.clear.join(", ")}` : ""}. Signing an account in means running:</div>
          {#each acct.login as line (line)}<code class="pl-cmd big">{line}</code>{/each}
        {/if}
        <div class="pl-muted">{h.installed ? `Found \`${h.command}\` on this server's PATH.` : `\`${h.command}\` is not installed on this server yet. ${h.installHint ?? ""}`}</div>
      </div>
    {/each}
    <p class="pl-warn">The agent works in the session's workspace folder with the same powers as the built-in harnesses: it can read and change files and run commands as you. The program comes from your own install, not from the plugin. Only allow plugins you trust. If an upgrade changes these commands, Vicuna asks again.</p>
    <div class="pl-row pl-consent-actions">
      <button class="pl-btn" data-testid="harness-consent-decline" onclick={declineConsent}>Not now</button>
      <button class="pl-btn primary" data-testid="harness-consent-accept" disabled={consentBusy} onclick={acceptConsent}>{consentBusy ? "Enabling…" : "Allow and enable"}</button>
    </div>
  </div>
  </div>
{/if}

{#if preview}
  <div use:portal class="pl-portal">
    <div class="pl-backdrop" role="presentation" data-testid="plugin-preview-backdrop" onclick={() => (preview = null)}></div>
    <div class="pl-preview" role="dialog" aria-modal="true" aria-labelledby="pl-preview-title" data-testid="plugin-preview-modal">
      <div class="pl-preview-head">
        <h3 id="pl-preview-title">{preview.name}</h3>
        <button class="pl-btn" data-testid="plugin-preview-close" onclick={() => (preview = null)}>Close</button>
      </div>
      <img class="pl-preview-img" src={preview.url} alt="{preview.name} preview" referrerpolicy="no-referrer" />
    </div>
  </div>
{/if}

<style>
  .pl { display: flex; flex-direction: column; gap: 10px; font-size: 14px; color: var(--text); }
  .pl-muted { color: var(--muted); font-size: 12.5px; margin: 0; line-height: 1.5; }
  .pl-muted code, .pl-src { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; }
  .pl-row { display: flex; gap: 8px; }
  .pl-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 10px; font: inherit; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; background: transparent; color: var(--text); }
  .pl-input:focus { outline: none; border-color: var(--accent); }
  .pl-btn { border: 1px solid var(--border-2); background: transparent; color: var(--text); border-radius: 999px; padding: 5px 12px; font: inherit; font-size: 12.5px; cursor: pointer; white-space: nowrap; }
  .pl-btn:hover:not(:disabled) { background: var(--hover); }
  .pl-btn:disabled { opacity: 0.5; cursor: default; }
  .pl-btn.primary { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
  .pl-btn.primary:hover:not(:disabled) { background: var(--accent-hover, var(--accent)); border-color: var(--accent-hover, var(--accent)); }
  .pl-btn.danger { color: var(--danger); border-color: var(--danger); }
  .pl-err { color: var(--err-text); background: var(--err-bg); border-radius: 10px; padding: 8px 10px; font-size: 12.5px; word-break: break-word; }
  .pl-note { color: var(--text); background: var(--hover); border-radius: 10px; padding: 8px 10px; font-size: 12.5px; }
  .pl-head { font-weight: 600; font-size: 13px; margin-top: 4px; }
  .pl-item { border: 1px solid var(--border); border-radius: 12px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .pl-item-top { display: flex; align-items: center; gap: 8px; }
  .pl-name { font-weight: 600; }
  .pl-ver { color: var(--muted); font-size: 12px; }
  .pl-spacer { flex: 1; }
  .pl-src { color: var(--muted-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pl-src-link { display: inline-flex; align-items: center; gap: 5px; max-width: 100%; min-width: 0; color: inherit; text-decoration: none; vertical-align: middle; }
  .pl-src-link:hover { color: var(--text); }
  .pl-src-link:hover .pl-src-text { text-decoration: underline; }
  .pl-src-text { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .pl-gh { flex: none; opacity: 0.85; }
  .pl-themes { display: flex; flex-wrap: wrap; gap: 6px; }
  .pl-theme { display: inline-flex; align-items: center; gap: 6px; border: 1px solid var(--border-2); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 10px; font: inherit; font-size: 12.5px; cursor: pointer; }
  .pl-theme:hover { background: var(--hover); }
  .pl-theme.sel { border-color: var(--accent); }
  .pl-dot { width: 12px; height: 12px; border-radius: 50%; border: 2px solid; box-sizing: border-box; }
  .pl-seg { display: flex; gap: 4px; background: var(--hover); border-radius: 999px; padding: 3px; align-self: flex-start; }
  .pl-seg-btn { border: 0; background: transparent; color: var(--muted); border-radius: 999px; padding: 5px 14px; font: inherit; font-size: 12.5px; cursor: pointer; display: inline-flex; align-items: center; gap: 6px; }
  .pl-seg-btn.sel { background: var(--bg); color: var(--text); box-shadow: 0 1px 2px rgba(0, 0, 0, 0.12); }
  .pl-count { font-size: 11px; color: var(--muted); }
  .pl-search { font-family: inherit; font-size: 13px; }
  .pl-grid { display: grid; grid-template-columns: repeat(auto-fill, minmax(300px, 1fr)); gap: 10px; }
  .pl-card { border: 1px solid var(--border); border-radius: 12px; overflow: hidden; display: flex; flex-direction: column; }
  .pl-card-body { flex: 1; padding: 10px 12px; display: flex; flex-direction: column; gap: 8px; }
  .pl-card-desc { display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; min-height: 3em; }
  .pl-card-foot { margin-top: auto; display: flex; flex-direction: column; gap: 8px; }
  .pl-card-title { display: flex; flex-direction: column; min-width: 0; }
  .pl-card-title .pl-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pl-icon { width: 40px; height: 40px; border-radius: 7px; flex: none; object-fit: contain; }
  .pl-badge { font-size: 11.5px; color: var(--muted); border: 1px solid var(--border-2); border-radius: 999px; padding: 3px 8px; white-space: nowrap; }
  .pl-badge.ok { color: var(--accent); border-color: var(--accent); }
  .pl-tags { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; min-width: 0; }
  .pl-kind { font-size: 11px; font-weight: 600; color: var(--accent-fg); background: var(--accent); border-radius: 6px; padding: 1px 7px; }
  .pl-tag { font-size: 11px; color: var(--muted); background: var(--hover); border-radius: 6px; padding: 1px 7px; }
  .pl-tags .pl-src { flex: 1; min-width: 0; display: flex; justify-content: flex-end; }
  .pl-foot a { color: var(--accent); }
  .pl-harness { display: flex; flex-wrap: wrap; align-items: center; gap: 8px; width: 100%; border: 1px dashed var(--border-2); border-radius: 10px; padding: 6px 10px; }
  .pl-cmd { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; background: var(--hover); border-radius: 6px; padding: 2px 6px; word-break: break-all; }
  .pl-cmd.big { display: block; font-size: 13px; padding: 8px 10px; margin: 4px 0; }
  .pl-backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.35); z-index: 1000; }
  .pl-consent { position: fixed; z-index: 1001; max-height: 90vh; overflow: auto; box-sizing: border-box; left: 50%; top: 50%; transform: translate(-50%, -50%); width: min(520px, 92vw); background: var(--bg); color: var(--text); border: 1px solid var(--border-2); border-radius: 14px; padding: 18px 20px; display: flex; flex-direction: column; gap: 10px; box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3); }
  .pl-consent h3 { margin: 0; font-size: 16px; }
  .pl-consent-h { display: flex; flex-direction: column; gap: 2px; }
  .pl-warn { margin: 0; font-size: 12.5px; line-height: 1.5; color: var(--text); background: var(--err-bg); border-radius: 10px; padding: 8px 10px; }
  .pl-consent-actions { justify-content: flex-end; }
  .pl-preview { position: fixed; z-index: 1001; left: 50%; top: 50%; transform: translate(-50%, -50%); max-width: min(952px, 92vw); max-height: 92vh; box-sizing: border-box; background: var(--bg); color: var(--text); border: 1px solid var(--border-2); border-radius: 14px; padding: 14px 16px; display: flex; flex-direction: column; gap: 10px; box-shadow: 0 12px 40px rgba(0, 0, 0, 0.3); }
  .pl-preview-head { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .pl-preview-head h3 { margin: 0; font-size: 15px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pl-preview-img { display: block; max-width: min(920px, 100%); max-height: 80vh; object-fit: contain; border-radius: 8px; border: 1px solid var(--border); background: var(--hover); }
</style>
