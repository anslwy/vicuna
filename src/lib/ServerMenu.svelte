<script lang="ts">
  // Sidebar server switcher popup: recents, server version/upgrade, rename,
  // and "Connect to new server…". State lives in ChatApp; this only renders.
  import { defaultServerName } from "$lib/chatFormat";
  import type { ServerInfo } from "$lib/bridge";

  let {
    srvList,
    onServer,
    activeServer,
    srvVersion,
    clientVersion,
    srvVersionSkew,
    srvUpgradeAvailable,
    srvCanUpgrade,
    srvUpgrading,
    srvUpgradeStage,
    srvUpgradePct,
    srvUpgradeDetail,
    srvLatest,
    srvCurrent,
    srvMenuRenaming = $bindable(),
    srvEditDraft = $bindable(),
    srvRenaming,
    onClose,
    requestServerSwitch,
    upgradeServer,
    saveMenuRenameServer,
    startMenuRenameServer,
    openServerDialog
  }: {
    srvList: ServerInfo[];
    onServer: boolean;
    activeServer: string | null;
    srvVersion: string | null;
    clientVersion: string;
    srvVersionSkew: boolean;
    srvUpgradeAvailable: boolean;
    srvCanUpgrade: boolean;
    srvUpgrading: boolean;
    srvUpgradeStage: string | null;
    srvUpgradePct: number | null;
    srvUpgradeDetail: string | null;
    srvLatest: string | null;
    srvCurrent: ServerInfo | null;
    srvMenuRenaming: boolean;
    srvEditDraft: string;
    srvRenaming: boolean;
    onClose: () => void;
    requestServerSwitch: (id: string | null) => void;
    upgradeServer: () => void;
    saveMenuRenameServer: () => Promise<void>;
    startMenuRenameServer: () => void;
    openServerDialog: () => void;
  } = $props();
</script>

<button class="ctx-backdrop" data-testid="server-menu-backdrop" aria-label="Close menu" tabindex="-1" onclick={onClose}></button>
<div class="srv-menu" data-testid="server-menu" role="menu">
  <div class="srv-menu-label">Recents</div>
  <button class="srv-item" data-testid="server-item-local" role="menuitem" onclick={() => requestServerSwitch(null)}>
    <span class="srv-check">{#if !onServer}✓{/if}</span><span class="srv-item-name">This computer</span>
  </button>
  {#each srvList as sv (sv.id)}
    <button class="srv-item" data-testid="server-item" role="menuitem" title={sv.url} onclick={() => requestServerSwitch(sv.id)}>
      <span class="srv-check">{#if sv.id === activeServer}✓{/if}</span><span class="srv-item-name">{sv.name}</span>
    </button>
  {/each}
  <div class="srv-sep"></div>
  {#if onServer && srvVersion}
    <div class="srv-version" data-testid="server-version">
      <span>Server v{srvVersion} · App v{clientVersion}</span>
      {#if srvVersionSkew && !srvUpgradeAvailable}<span class="srv-skew" title="The server and this app run different versions">differs</span>{/if}
    </div>
    {#if srvUpgradeAvailable && srvCanUpgrade}
      <button class="srv-item srv-upgrade" data-testid="server-upgrade" role="menuitem" disabled={srvUpgrading} onclick={upgradeServer}>
        {#if srvUpgrading}
          <span class="srv-check"><span class="srv-spin" aria-hidden="true"></span></span>
          <span class="srv-item-name srv-upgrade-prog" data-testid="server-upgrade-progress">
            <span class="shimmer">{srvUpgradeStage ?? "Upgrading server…"}</span>
            {#if srvUpgradePct != null}<span class="srv-upgrade-bar"><span style="width:{srvUpgradePct}%"></span></span>{/if}
            {#if srvUpgradeDetail}<span class="srv-upgrade-detail">{srvUpgradeDetail}</span>{/if}
          </span>
        {:else}
          <span class="srv-check">↑</span><span class="srv-item-name">Upgrade server to v{srvLatest}</span>
        {/if}
      </button>
    {/if}
    <div class="srv-sep"></div>
  {/if}
  {#if onServer && srvCurrent}
    {#if srvMenuRenaming}
      <div class="srv-menu-rename-row" data-testid="server-menu-rename-form">
        <input class="srv-menu-rename-input" data-testid="server-menu-rename-input" placeholder={defaultServerName(srvCurrent.url)} autocomplete="off" spellcheck="false"
          bind:value={srvEditDraft} disabled={srvRenaming}
          onkeydown={(e) => { if (e.key === "Enter") void saveMenuRenameServer(); else if (e.key === "Escape") srvMenuRenaming = false; }} />
        <button class="srv-menu-rename-ok" data-testid="server-menu-rename-save" aria-label="Save name" title="Save" disabled={srvRenaming} onclick={() => void saveMenuRenameServer()}>✓</button>
      </div>
    {:else}
      <button class="srv-item" data-testid="server-menu-rename" role="menuitem" onclick={startMenuRenameServer}>
        <span class="srv-check">✎</span><span class="srv-item-name">Rename server…</span>
      </button>
    {/if}
  {/if}
  <button class="srv-item srv-connect" data-testid="server-connect-open" role="menuitem" onclick={openServerDialog}>
    <span class="srv-check">+</span><span class="srv-item-name">Connect to new server…</span>
  </button>
</div>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .srv-menu { position: absolute; left: 0; right: 0; bottom: calc(100% + 6px); z-index: 41; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; display: flex; flex-direction: column; min-width: 220px; }
  .srv-menu-label { font-size: 12px; color: var(--muted); padding: 6px 10px 4px; }
  .srv-item { display: flex; align-items: center; gap: 6px; border: none; background: transparent; text-align: left; font: inherit; font-size: 14px; padding: 8px 10px; border-radius: 10px; cursor: pointer; color: var(--text); }
  .srv-item:hover { background: var(--hover-3); }
  .srv-check { width: 16px; flex: none; color: var(--muted); text-align: center; }
  .srv-item-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .srv-sep { height: 1px; background: var(--border); margin: 4px 6px; }
  .srv-version { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--muted); padding: 4px 10px; }
  .srv-skew { font-size: 10.5px; font-weight: 600; color: var(--accent); border: 1px solid currentColor; border-radius: 999px; padding: 0 6px; }
  .srv-upgrade { color: var(--accent); }
  .srv-spin { display: inline-block; width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: srv-spin 0.8s linear infinite; vertical-align: -1px; }
  @keyframes srv-spin { to { transform: rotate(360deg); } }
  .srv-upgrade-prog { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .srv-upgrade-bar { display: block; height: 3px; border-radius: 2px; background: var(--hover-3, rgba(127,127,127,0.25)); overflow: hidden; }
  .srv-upgrade-bar > span { display: block; height: 100%; background: var(--accent); transition: width 0.4s ease; }
  .srv-upgrade-detail { font-size: 11px; color: var(--muted, inherit); opacity: 0.8; }
  @media (prefers-reduced-motion: reduce) { .srv-spin { animation-duration: 2.4s; } }
  .srv-menu-rename-row { display: flex; align-items: center; gap: 4px; padding: 4px 6px; }
  .srv-menu-rename-input { flex: 1; min-width: 0; border: 1px solid var(--accent); border-radius: 8px; padding: 5px 8px; font: inherit; font-size: 13px; background: transparent; color: var(--text); }
  .srv-menu-rename-input:focus { outline: none; }
  .srv-menu-rename-ok { flex: none; width: 28px; height: 28px; border: 1px solid var(--border-2); border-radius: 8px; background: transparent; color: var(--text); cursor: pointer; font-size: 13px; display: flex; align-items: center; justify-content: center; }
  .srv-menu-rename-ok:hover { background: var(--hover-3); }
  .ctx-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 40; }
  .shimmer { background: linear-gradient(90deg, var(--muted-2) 25%, var(--text) 50%, var(--muted-2) 75%); background-size: 200% 100%; -webkit-background-clip: text; background-clip: text; color: transparent; animation: shimmer-slide 1.4s linear infinite; }
  @keyframes shimmer-slide { to { background-position: -200% 0; } }
  @media (prefers-reduced-motion: reduce) { .shimmer { animation: none; } }
</style>
