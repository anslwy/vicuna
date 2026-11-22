<script lang="ts">
  import type { Chat } from "$lib/store";
  import type { ServerChatHit } from "$lib/bridge";

  export type RemoteHit = ServerChatHit & { serverId: string; serverName: string };
  export type SearchItem =
    | { kind: "new" }
    | { kind: "chat"; chat: Chat; snippet?: string }
    | { kind: "remote"; hit: RemoteHit };

  let {
    gotoInput = $bindable(""),
    gotoSel = $bindable(0),
    gotoError,
    searchQuery,
    searchItems,
    searchShowHosts,
    srvLabel,
    remoteSearching,
    localServerSearching,
    remoteFailures,
    remoteNotices,
    searchSnippet,
    highlightHtml,
    onClose,
    onKeyDown,
    onOpenItem,
    onPrefetchChat
  }: {
    gotoInput: string;
    gotoSel: number;
    gotoError: string | null;
    searchQuery: string;
    searchItems: SearchItem[];
    searchShowHosts: boolean;
    srvLabel: string;
    remoteSearching: boolean;
    localServerSearching: boolean;
    remoteFailures: { serverName: string; message: string }[];
    remoteNotices: string[];
    searchSnippet: (c: Chat, q: string) => string;
    highlightHtml: (text: string, q: string) => string;
    onClose: () => void;
    onKeyDown: (e: KeyboardEvent) => void;
    onOpenItem: (it: SearchItem) => void;
    onPrefetchChat: (chat: Chat) => void;
  } = $props();
</script>

<button
  class="confirm-backdrop"
  data-testid="goto-backdrop"
  aria-label="Cancel"
  onclick={onClose}
  tabindex="-1"
></button>
<div
  class="confirm-card search-card"
  data-testid="goto-dialog"
  role="dialog"
  aria-modal="true"
  aria-label="Search chats"
>
  <div class="search-bar">
    <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
    <!-- svelte-ignore a11y_autofocus -->
    <input
      class="search-input"
      data-testid="goto-input"
      placeholder="Search chats or paste a session link…"
      bind:value={gotoInput}
      autofocus
      onkeydown={onKeyDown}
    />
    {#if gotoInput}
      <button
        class="search-clear"
        data-testid="search-clear-btn"
        aria-label="Clear search"
        onclick={() => {
          gotoInput = "";
          document.querySelector<HTMLInputElement>("[data-testid=goto-input]")?.focus();
        }}
      >×</button>
    {/if}
  </div>
  <div class="search-results" data-testid="search-results" role="listbox" aria-label="Matching chats">
    {#each searchItems as it, idx (it.kind === "new" ? "new" : it.kind === "chat" ? it.chat.id : `${it.hit.serverId}:${it.hit.id}`)}
      {#if it.kind === "new"}
        <button
          class="search-row {idx === gotoSel ? 'sel' : ''}"
          role="option"
          aria-selected={idx === gotoSel}
          data-testid="search-new"
          onclick={() => onOpenItem(it)}
          onmousemove={() => (gotoSel = idx)}
        >
          <span class="search-ico"><svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg></span>
          <span class="search-text"><span class="search-title">New</span></span>
        </button>
      {:else}
        <button
          class="search-row {idx === gotoSel ? 'sel' : ''}"
          role="option"
          aria-selected={idx === gotoSel}
          data-testid="search-result"
          data-id={it.kind === "chat" ? it.chat.id : it.hit.id}
          data-host={it.kind === "remote" ? it.hit.serverName : srvLabel}
          data-remote={it.kind === "remote" ? "true" : undefined}
          onclick={() => onOpenItem(it)}
          onpointerdown={() => { if (it.kind === "chat" && it.chat.partial) onPrefetchChat(it.chat); }}
          onmousemove={() => (gotoSel = idx)}
        >
          <span class="search-ico">
            {#if it.kind === "remote"}
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/></svg>
            {:else}
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M7.9 20A9 9 0 1 0 4 16.1L2 22z"/></svg>
            {/if}
          </span>
          <span class="search-text">
            <span class="search-line">
              <span class="search-title">{@html highlightHtml(it.kind === "chat" ? it.chat.title : it.hit.title, searchQuery)}</span>
              {#if searchShowHosts}
                <span class="search-host" data-testid="search-host">{it.kind === "remote" ? it.hit.serverName : srvLabel}</span>
              {/if}
            </span>
            <span class="search-snippet">{@html highlightHtml(it.kind === "chat" ? (it.snippet ?? searchSnippet(it.chat, searchQuery)) : it.hit.snippet, searchQuery)}</span>
          </span>
        </button>
      {/if}
    {:else}
      <div class="no-match">No matching chats</div>
    {/each}
  </div>
  {#if remoteSearching}
    <p class="search-note" data-testid="search-remote-note">Searching connected servers…</p>
  {:else if localServerSearching}
    <p class="search-note" data-testid="search-local-note">Searching all messages…</p>
  {/if}
  {#each remoteFailures as f (f.serverName + f.message)}
    <p class="search-note search-fail" data-testid="search-remote-error">{f.serverName}: {f.message}</p>
  {/each}
  {#each remoteNotices as n (n)}
    <p class="search-note search-old" data-testid="search-remote-degraded">{n}: older Vicuna there — searched its session list directly (update the CLI for a faster search)</p>
  {/each}
  {#if gotoError}
    <p class="goto-error" data-testid="goto-error">{gotoError}</p>
  {/if}
</div>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .confirm-card.search-card { width: min(720px, calc(100vw - 48px)); top: 14%; transform: translate(-50%, 0); padding: 10px; }
  .search-bar { display: flex; align-items: center; gap: 10px; padding: 12px 14px; border-bottom: 1px solid var(--border); color: var(--muted); }
  .search-input { flex: 1; min-width: 0; border: none; outline: none; background: transparent; color: var(--text); font: inherit; font-size: 16px; }
  .search-input::placeholder { color: var(--faint); }
  .search-clear { border: none; background: transparent; color: var(--icon-dim); font-size: 18px; line-height: 1; cursor: pointer; border-radius: 6px; padding: 2px 7px; }
  .search-clear:hover { background: var(--hover); color: var(--text); }
  .search-results { max-height: 360px; overflow-y: auto; padding: 6px; display: flex; flex-direction: column; gap: 2px; }
  .search-row { display: flex; gap: 12px; align-items: flex-start; width: 100%; box-sizing: border-box; text-align: left; border: none; background: transparent; border-radius: 10px; padding: 10px 12px; cursor: pointer; color: var(--text); font: inherit; }
  .search-row:hover, .search-row.sel { background: var(--hover-2); }
  .search-ico { flex-shrink: 0; color: var(--muted-2); margin-top: 2px; display: flex; }
  .search-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; flex: 1; }
  .search-line { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .search-line .search-title { min-width: 0; }
  .search-title { font-size: 14px; font-weight: 650; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .search-host { flex-shrink: 0; margin-left: auto; max-width: 46%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 11px; font-weight: 600; color: var(--muted-2); border: 1px solid var(--border); border-radius: 999px; padding: 1px 7px; }
  .search-row.sel .search-host { color: var(--muted); border-color: var(--border-strong); }
  .search-note { margin: 0; padding: 4px 18px 8px; font-size: 12px; color: var(--muted-2); }
  .search-note.search-fail { color: var(--danger); }
  .search-note.search-old { color: var(--muted-2); font-style: italic; }
  .search-snippet { font-size: 13px; color: var(--muted-2); line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; word-break: break-word; }
  .search-snippet:empty { display: none; }
  .search-card :global(.search-mark) { background: #f0bd76; color: #1e1e1e; border-radius: 3px; padding: 0 1px; }
  .search-card .goto-error { margin: 4px 10px 6px 10px; }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  .no-match { padding: 12px 18px; font-size: 14px; color: var(--muted); }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
