<script lang="ts">
  // Settings → Plugins: install from a GitHub repo (or manifest URL),
  // upgrade, uninstall. All work happens on the server (plugin_* actions),
  // so a remote server window manages that server's plugins.
  import { onMount } from "svelte";
  import { invokeOrBridge } from "./bridge";
  import { installBlocked, type InstalledPlugin } from "./pluginTheme";

  let { plugins, activeThemeKey, onChanged, onUseTheme, onOpenPanel }: {
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

  async function install() {
    const s = source.trim();
    if (installBlocked(s, busy)) return;
    busy = "install";
    error = notice = "";
    try {
      const p = await invokeOrBridge<InstalledPlugin>("plugin_install", { source: s });
      notice = `Installed ${p.name} ${p.version}.`;
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
      onChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
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
    } catch (e: any) {
      error = e?.message ?? String(e);
    } finally {
      busy = null;
    }
  }
</script>

<div class="pl" data-testid="plugins-panel">
  <p class="pl-muted">Install plugins from a GitHub repo (<code>owner/repo</code> or its URL — private repos use the server's <code>gh</code> sign-in) or a direct <code>vicuna-plugin.json</code> link. Plugins can add themes and side panels.</p>
  <form class="pl-row" onsubmit={(e) => { e.preventDefault(); void install(); }}>
    <input class="pl-input" data-testid="plugin-source" placeholder="owner/repo or https://github.com/owner/repo" bind:value={source} spellcheck="false" autocomplete="off" />
    <button class="pl-btn primary" data-testid="plugin-install" type="submit" disabled={installBlocked(source, busy)}>{busy === "install" ? "Installing…" : "Install"}</button>
  </form>
  {#if error}<div class="pl-err" data-testid="plugin-error">{error}</div>{/if}
  {#if notice}<div class="pl-note" data-testid="plugin-notice">{notice}</div>{/if}

  <div class="pl-head">Installed</div>
  {#if plugins.length === 0}
    <div class="pl-muted" data-testid="plugins-empty">{listing ? "Loading…" : "No plugins installed."}</div>
  {:else}
    {#each plugins as p (p.id)}
      <div class="pl-item" data-testid="plugin-row-{p.id}">
        <div class="pl-item-top">
          <span class="pl-name">{p.name}</span>
          <span class="pl-ver">v{p.version}</span>
          <span class="pl-spacer"></span>
          <button class="pl-btn" data-testid="plugin-upgrade-{p.id}" disabled={!!busy} onclick={() => upgrade(p)}>{busy === `upgrade:${p.id}` ? "Checking…" : "Upgrade"}</button>
          <button class="pl-btn {confirmRemove === p.id ? 'danger' : ''}" data-testid="plugin-uninstall-{p.id}" disabled={!!busy} onclick={() => uninstall(p)} onblur={() => { if (confirmRemove === p.id) confirmRemove = null; }}>{busy === `uninstall:${p.id}` ? "Removing…" : confirmRemove === p.id ? "Confirm?" : "Uninstall"}</button>
        </div>
        {#if p.description}<div class="pl-muted">{p.description}</div>{/if}
        <div class="pl-src" title={p.source}>{p.source}</div>
        <div class="pl-themes">
          {#each p.panels ?? [] as pn (pn.id)}
            <button class="pl-theme" data-testid="plugin-panel-{p.id}-{pn.id}" title="Open this panel in the side bar" onclick={() => onOpenPanel?.(`plugin:${p.id}:${pn.id}`)}>
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/></svg>
              {pn.title}
            </button>
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
</div>

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
  .pl-themes { display: flex; flex-wrap: wrap; gap: 6px; }
  .pl-theme { display: inline-flex; align-items: center; gap: 6px; border: 1px solid var(--border-2); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 10px; font: inherit; font-size: 12.5px; cursor: pointer; }
  .pl-theme:hover { background: var(--hover); }
  .pl-theme.sel { border-color: var(--accent); }
  .pl-dot { width: 12px; height: 12px; border-radius: 50%; border: 2px solid; box-sizing: border-box; }
</style>
