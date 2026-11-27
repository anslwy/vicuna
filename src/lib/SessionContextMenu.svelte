<script lang="ts">
  // Sidebar session right-click menu: rename/link/pin/archive, Work-session
  // permissions + lead + messaging bridges, Continue in Work (with its own
  // folder pill) and Delete. ChatApp owns the state and every action.
  import type { Chat } from "$lib/store";
  import { displayWorkspace, workspaceBase } from "$lib/chatFormat";
  import { isTauriRuntime } from "$lib/ollama";
  import { isWindows } from "$lib/chatDom";
  import { telegramConnLabel, telegramConnTitle, type TelegramConnState, type TelegramStatusSnapshot } from "$lib/telegram";
  import { bridgeConnLabel, bridgeConnState, bridgeConnTitle, bridgeDisplayName, bridgeIconBg, safeBridgeIconDataUrl, safeIconPath, type BridgeStatusSnapshot } from "$lib/bridgePlugin";
  import { safeColor, type InstalledPlugin } from "$lib/pluginTheme";

  let {
    menu,
    ctxChat,
    chats,
    serverPersist,
    onServer,
    telegramStatuses,
    telegramStateFor,
    bridgePlugins,
    bridgeStatuses,
    continueWorkspace,
    workspacePickerOpen,
    workspaceDraft = $bindable(),
    workspaceError,
    onClose,
    startRename,
    copySessionLink,
    togglePin,
    setArchived,
    toggleLead,
    openPermissions,
    openLeadPrompt,
    openTelegram,
    openBridge,
    continueInWork,
    pickContinueWorkspace,
    commitContinueWorkspace,
    closeWorkspacePicker,
    requestDelete
  }: {
    menu: { chatId: string; x: number; y: number };
    ctxChat: Chat | undefined;
    chats: Chat[];
    serverPersist: boolean;
    onServer: boolean;
    telegramStatuses: Record<string, TelegramStatusSnapshot>;
    telegramStateFor: (id: string | undefined) => TelegramConnState;
    bridgePlugins: InstalledPlugin[];
    bridgeStatuses: Record<string, Record<string, BridgeStatusSnapshot>>;
    continueWorkspace: string;
    workspacePickerOpen: boolean;
    workspaceDraft: string;
    workspaceError: string | null;
    onClose: () => void;
    startRename: (id: string) => void;
    copySessionLink: (id: string) => void;
    togglePin: (id: string) => void;
    setArchived: (id: string, archived: boolean) => void;
    toggleLead: (id: string) => void;
    openPermissions: (id: string) => void;
    openLeadPrompt: (id: string, prevTitle: string | null) => void;
    openTelegram: (id: string) => void;
    openBridge: (pluginId: string, sessionId: string) => void;
    continueInWork: (id: string) => void;
    pickContinueWorkspace: (e?: MouseEvent) => Promise<void>;
    commitContinueWorkspace: () => Promise<void>;
    closeWorkspacePicker: () => void;
    requestDelete: (id: string) => void;
  } = $props();
</script>

<button
  class="ctx-backdrop"
  data-testid="session-menu-backdrop"
  aria-label="Close menu"
  onclick={onClose}
  oncontextmenu={(e) => {
    e.preventDefault();
    onClose();
  }}
  tabindex="-1"
></button>
<div
  class="ctx-menu"
  data-testid="session-menu"
  style="left: {menu.x}px; top: {menu.y}px;"
>
  <button class="ctx-item" data-testid="rename-item" onclick={() => startRename(menu.chatId)}>Rename</button>
  <button class="ctx-item" data-testid="copy-link-item" onclick={() => copySessionLink(menu.chatId)}>Copy Link</button>
  <button class="ctx-item" data-testid="pin-item" onclick={() => togglePin(menu.chatId)}>{ctxChat?.pinned ? "Unpin" : "Pin"}</button>
  <button class="ctx-item" data-testid="archive-item" onclick={() => setArchived(menu.chatId, !ctxChat?.archived)}>{ctxChat?.archived ? "Unarchive" : "Archive"}</button>
  <div class="ctx-sep" role="separator"></div>
  {#if (ctxChat?.mode ?? "chat") === "work"}
    {#if serverPersist}
      <button class="ctx-item" data-testid="permissions-item" onclick={() => openPermissions(menu.chatId)}>Permissions…</button>
    {/if}
    {@const tgSnap = ctxChat ? telegramStatuses[ctxChat.id] : null}
    {@const tgState = telegramStateFor(ctxChat?.id)}
    {#if ctxChat?.role === "lead"}
      <button class="ctx-item" data-testid="lead-item" onclick={() => toggleLead(menu.chatId)}>Remove lead</button>
    {:else}
      {@const hasLead = chats.some((x) => x.role === "lead" && x.id !== menu.chatId)}
      <button
        class="ctx-item"
        data-testid="lead-item"
        onclick={() => openLeadPrompt(menu.chatId, chats.find((x) => x.role === "lead" && x.id !== menu.chatId)?.title ?? null)}
      >{hasLead ? "Replace lead" : "Make lead"}</button>
    {/if}
    <button
      class="ctx-item ctx-telegram"
      data-testid="telegram-item"
      title={telegramConnTitle(tgSnap)}
      onclick={() => openTelegram(menu.chatId)}
    >
      <span class="ctx-tg-icon" aria-hidden="true"><svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg></span>
      <span class="ctx-tg-label">Telegram</span>
      {#if tgSnap?.botUsername}<span class="ctx-tg-bot">@{tgSnap.botUsername}</span>{/if}
      <span class="ctx-tg-dot {tgState}" data-testid="telegram-status-dot" data-status={tgState} aria-label={telegramConnLabel(tgState)}></span>
    </button>
    {#each bridgePlugins as bp (bp.id)}
      {@const bSnap = ctxChat ? bridgeStatuses[bp.id]?.[ctxChat.id] : null}
      {@const bState = bridgeConnState(bSnap)}
      {@const bName = bridgeDisplayName(bp)}
      {@const bIconUrl = safeBridgeIconDataUrl(bp.bridge?.iconDataUrl)}
      {@const bColor = bIconUrl ? bridgeIconBg(bp.bridge) : (safeColor(bp.bridge?.color) ? bp.bridge!.color! : "var(--accent)")}
      <button
        class="ctx-item ctx-telegram"
        data-testid="bridge-item-{bp.id}"
        title={bridgeConnTitle(bName, bSnap)}
        onclick={() => openBridge(bp.id, menu.chatId)}
      >
        <span class="ctx-tg-icon" class:has-img={!!bIconUrl} style:background={bColor} aria-hidden="true">
          {#if bIconUrl}
            <img src={bIconUrl} width="12" height="12" alt="" />
          {:else}
            <svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><path d={safeIconPath(bp.bridge?.iconPath)}/></svg>
          {/if}
        </span>
        <span class="ctx-tg-label">{bName}</span>
        {#if bSnap?.botUsername}<span class="ctx-tg-bot">@{bSnap.botUsername}</span>{/if}
        <span class="ctx-tg-dot {bState}" data-testid="bridge-status-dot-{bp.id}" data-status={bState} aria-label={bridgeConnLabel(bName, bState)}></span>
      </button>
    {/each}
  {/if}
  <div class="ctx-continue-row">
    <button class="ctx-item ctx-continue" data-testid="continue-work-item" onclick={() => continueInWork(menu.chatId)}>Continue in Work</button>
    <button
      class="ctx-ws-pill"
      data-testid="continue-work-workspace"
      title={continueWorkspace ? `Work folder: ${displayWorkspace(continueWorkspace)} — click to change` : "Pick a work folder"}
      aria-label="Change work folder"
      onclick={(e) => void pickContinueWorkspace(e)}
    >
      <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
      <span class="ctx-ws-name">{workspaceBase(continueWorkspace) || "Folder"}</span>
    </button>
  </div>
  {#if workspacePickerOpen && (!isTauriRuntime() || onServer)}
    <form
      class="ctx-ws-form"
      data-testid="continue-work-workspace-form"
      onsubmit={(e) => {
        e.preventDefault();
        void commitContinueWorkspace();
      }}
    >
      <input
        class="pull-input"
        data-testid="continue-work-workspace-input"
        placeholder={isWindows ? "C:\\path\\to\\workspace" : "/absolute/path/to/workspace"}
        bind:value={workspaceDraft}
        spellcheck={false}
        onkeydown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            closeWorkspacePicker();
          }
        }}
      />
      {#if workspaceError}
        <div class="goto-error" data-testid="continue-work-workspace-error">{workspaceError}</div>
      {/if}
    </form>
  {/if}
  <div class="ctx-sep" role="separator"></div>
  <button class="ctx-item danger" data-testid="delete-item" onclick={() => requestDelete(menu.chatId)}>Delete</button>
</div>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  .ctx-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 40; }
  .ctx-menu { position: fixed; z-index: 41; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; min-width: 150px; display: flex; flex-direction: column; }
  .ctx-item { border: none; background: transparent; text-align: left; font: inherit; font-size: 14.5px; padding: 8px 14px; border-radius: 10px; cursor: pointer; color: var(--text); }
  .ctx-item:hover { background: var(--hover-3); }
  .ctx-item.danger { color: var(--danger); }
  .ctx-item.ctx-telegram { display: flex; align-items: center; gap: 8px; width: 100%; box-sizing: border-box; }
  .ctx-sep { height: 1px; background: var(--border); margin: 4px 8px; }
  .ctx-tg-label { flex: 1; min-width: 0; }
  .ctx-tg-bot { font-size: 12px; color: var(--muted); max-width: 110px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctx-tg-icon { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; border-radius: 50%; background: #2aabee; color: #fff; line-height: 0; }
  .ctx-tg-icon.has-img { box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.08); }
  .ctx-tg-icon img { display: block; width: 12px; height: 12px; object-fit: contain; }
  .ctx-tg-dot { flex: none; margin-left: auto; width: 8px; height: 8px; border-radius: 50%; box-sizing: border-box; }
  .ctx-tg-dot.connected { background: #22c55e; }
  .ctx-tg-dot.disconnected { background: #ef4444; }
  .ctx-tg-dot.unconfigured { background: transparent; border: 1.5px solid var(--muted-2); }
  .ctx-continue-row { display: flex; align-items: center; gap: 4px; }
  .ctx-continue { flex: 1; min-width: 0; white-space: nowrap; }
  .ctx-ws-pill { display: inline-flex; align-items: center; gap: 5px; flex-shrink: 0; max-width: 130px; border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 999px; padding: 5px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .ctx-ws-pill:hover { background: var(--hover-3); color: var(--text); border-color: var(--border-3); }
  .ctx-ws-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ctx-ws-form { display: flex; flex-direction: column; gap: 6px; padding: 4px 8px 6px 8px; }
  .ctx-ws-form .pull-input { width: 100%; box-sizing: border-box; }
  .pull-input { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 13px; font-family: inherit; padding: 6px 14px; }
  .pull-input:focus { outline: none; border-color: var(--accent); }
  .pull-input:disabled { opacity: .5; }
</style>
