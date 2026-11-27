<script lang="ts">
  // One sidebar session row: title/lead badge, live elapsed, archived and
  // Work/workspace/bridge badges, inline rename. ChatApp owns all state.
  import type { Chat } from "$lib/store";
  import { formatLiveElapsed, displayWorkspace, workspaceBase } from "$lib/chatFormat";
  import { isNoWorkspacePath, showWorkspaceBadge } from "$lib/sidebar";
  import { telegramConnState, telegramConnTitle, type TelegramStatusSnapshot } from "$lib/telegram";
  import { bridgeConnState, bridgeConnTitle, bridgeDisplayName, safeIconPath, type BridgeStatusSnapshot } from "$lib/bridgePlugin";
  import { safeColor, type InstalledPlugin } from "$lib/pluginTheme";

  let {
    c,
    active,
    live,
    liveTitleText,
    liveMs,
    renaming,
    renameDraft = $bindable(),
    telegramSnap,
    bridgePlugins,
    bridgeStatuses,
    sessionFilter,
    onSelect,
    onPrefetch,
    onHoverEnter,
    onHoverLeave,
    onMenu,
    commitRename,
    cancelRename
  }: {
    c: Chat;
    active: boolean;
    live: boolean;
    liveTitleText: string | undefined;
    liveMs: number | null;
    renaming: boolean;
    renameDraft: string;
    telegramSnap: TelegramStatusSnapshot | undefined;
    bridgePlugins: InstalledPlugin[];
    bridgeStatuses: Record<string, Record<string, BridgeStatusSnapshot>>;
    sessionFilter: string;
    onSelect: (id: string) => void;
    onPrefetch: (c: Chat) => void;
    onHoverEnter: (c: Chat) => void;
    onHoverLeave: () => void;
    onMenu: (e: MouseEvent, id: string) => void;
    commitRename: () => void;
    cancelRename: () => void;
  } = $props();
</script>

<button
  class="chat-item {active ? 'active' : ''} {live ? 'streaming' : ''} {c.archived ? 'archived' : ''} {c.role === 'lead' ? 'lead-row' : ''}"
  data-testid="chat-item"
  data-archived={c.archived ? "true" : null}
  data-streaming={live ? "true" : null}
  title={live ? liveTitleText : c.role === "lead" ? c.title : undefined}
  onclick={() => onSelect(c.id)}
  onpointerdown={() => onPrefetch(c)}
  onpointerenter={() => onHoverEnter(c)}
  onpointerleave={onHoverLeave}
  oncontextmenu={(e) => onMenu(e, c.id)}
>
  {#if renaming}
    <input
      class="rename-input"
      data-testid="rename-input"
      bind:value={renameDraft}
      onclick={(e) => e.stopPropagation()}
      onkeydown={(e) => {
        e.stopPropagation();
        if (e.key === "Enter") commitRename();
        else if (e.key === "Escape") cancelRename();
      }}
      onblur={commitRename}
    />
  {:else}
    {#if live}<span class="stream-dot" aria-hidden="true"></span>{/if}
    {#if c.role === "lead"}
      <span class="work-badge lead-badge" data-testid="lead-badge" title="Lead session: can create, message, stop and remove other sessions">Lead</span>
    {:else}
      <span class="title {live ? 'shimmer' : ''}">{c.title}</span>
    {/if}
        {#if liveMs != null}<span class="live-elapsed" data-testid="live-elapsed">{formatLiveElapsed(liveMs)}</span>{/if}
    {#if c.archived}<span class="work-badge archived-badge" data-testid="archived-badge" title="Archived: hidden unless Show archived is on">Archived</span>{/if}
    {#if c.mode === "work"}
                        {@const tgSnap = telegramSnap}
      {@const tgState = telegramConnState(tgSnap)}
      {#if tgState !== "unconfigured"}
        <span
          class="tg-row-badge {tgState}"
          data-testid="telegram-row-badge"
          data-status={tgState}
          title={telegramConnTitle(tgSnap)}
          aria-label={telegramConnTitle(tgSnap)}
        >
          <svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg>
        </span>
      {/if}
      {#each bridgePlugins as bp (bp.id)}
        {@const bSnap = bridgeStatuses[bp.id]?.[c.id]}
        {@const bState = bridgeConnState(bSnap)}
        {#if bState !== "unconfigured"}
          {@const bName = bridgeDisplayName(bp)}
          {@const bColor = safeColor(bp.bridge?.color) ? bp.bridge!.color! : "var(--accent)"}
          <span
            class="tg-row-badge {bState}"
            style:background={bState === "connected" ? bColor : undefined}
            data-testid="bridge-row-badge-{bp.id}"
            data-status={bState}
            title={bridgeConnTitle(bName, bSnap)}
            aria-label={bridgeConnTitle(bName, bSnap)}
          >
            <svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d={safeIconPath(bp.bridge?.iconPath)}/></svg>
          </span>
        {/if}
      {/each}
      {#if showWorkspaceBadge(c, sessionFilter)}
        {@const wsIsNone = isNoWorkspacePath(c.workspace)}
        {@const wsBase = wsIsNone ? "" : workspaceBase(c.workspace ?? "")}
        {#if wsIsNone}
          <span class="work-badge ws-badge" data-testid="ws-badge" title="No workspace — uses a temporary empty scratch folder if needed">
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M5.6 5.6l12.8 12.8"/></svg>
            <span>No workspace</span>
          </span>
        {:else if wsBase}
          <span class="work-badge ws-badge" data-testid="ws-badge" title={displayWorkspace(c.workspace ?? "")}>
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
            <span>{wsBase}</span>
          </span>
        {:else}
          <span class="work-badge" title="Work session">Work</span>
        {/if}
      {/if}
    {/if}
  {/if}
</button>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .chat-item { display: flex; align-items: center; gap: 6px; width: 100%; box-sizing: border-box; min-width: 0; border: none; background: transparent; padding: 7px 8px; border-radius: 8px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text); user-select: none; -webkit-user-select: none; }
  .chat-item:hover { background: var(--hover-3); }
  .chat-item.active { background: var(--active-item); }
  .chat-item.archived .title, .chat-item.archived .lead-badge { opacity: .55; }
  .archived-badge { opacity: .75; }
  .chat-item .title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
  .chat-item .work-badge { margin-left: 0; }
  .chat-item.streaming { background: var(--hover-3); }
  .chat-item.streaming.active { background: var(--active-item); }
  .stream-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex-shrink: 0; animation: stream-pulse 1.2s ease-in-out infinite; }
  @keyframes stream-pulse { 0%, 100% { opacity: .35; transform: scale(.85); } 50% { opacity: 1; transform: scale(1.15); } }
  .live-elapsed { flex-shrink: 0; font-size: 11px; font-variant-numeric: tabular-nums; white-space: nowrap; color: var(--muted-2); }
  .rename-input { flex: 1; min-width: 0; border: 1px solid var(--border-strong); border-radius: 6px; outline: none; font: inherit; font-size: 14px; padding: 2px 6px; background: var(--card); color: var(--text); }
  .work-badge.lead-badge { background: var(--bg); color: var(--accent); border: 1px solid var(--accent); padding: 0 7px; }
  .chat-item .work-badge.lead-badge { margin-right: auto; }
  .tg-row-badge { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: 50%; color: #fff; line-height: 0; }
  .tg-row-badge.connected { background: #2aabee; }
  .tg-row-badge.disconnected { background: #ef4444; opacity: 0.85; }
  .shimmer { background: linear-gradient(90deg, var(--muted-2) 25%, var(--text) 50%, var(--muted-2) 75%); background-size: 200% 100%; -webkit-background-clip: text; background-clip: text; color: transparent; animation: shimmer-slide 1.4s linear infinite; }
  @keyframes shimmer-slide { to { background-position: -200% 0; } }
  @media (prefers-reduced-motion: reduce) { .shimmer { animation: none; } .stream-dot { animation: none; opacity: .8; } }
  .work-badge { flex-shrink: 0; font-size: 11px; font-weight: 700; color: var(--accent-fg); background: var(--accent); border-radius: 999px; padding: 1px 8px; margin-left: 8px; }
  .ws-badge { display: inline-flex; align-items: center; gap: 4px; max-width: 110px; min-width: 0; }
  .chat-item .work-badge.ws-badge { flex-shrink: 1; font-weight: 500; color: var(--accent); background: color-mix(in srgb, var(--accent) 13%, transparent); padding: 1px 7px; }
  .ws-badge svg { flex-shrink: 0; }
  .ws-badge span:last-child { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
</style>
