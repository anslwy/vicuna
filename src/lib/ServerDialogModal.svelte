<script lang="ts">
  import type { ServerInfo } from "$lib/bridge";

  let {
    srvUrlDraft = $bindable(""),
    srvTokenDraft = $bindable(""),
    srvNameDraft = $bindable(""),
    srvNameTouched = $bindable(false),
    srvBusy,
    srvErr,
    srvList,
    srvEditingId,
    srvEditDraft = $bindable(""),
    srvRenaming,
    srvCopied,
    srvCmdCopied,
    setupCmd,
    defaultServerName,
    onClose,
    onSubmit,
    onFillRecent,
    onStartRename,
    onSaveRename,
    onCancelRename,
    onCopyUrl,
    onForget,
    onCopySetup
  }: {
    srvUrlDraft: string;
    srvTokenDraft: string;
    srvNameDraft: string;
    srvNameTouched: boolean;
    srvBusy: boolean;
    srvErr: string | null;
    srvList: ServerInfo[];
    srvEditingId: string | null;
    srvEditDraft: string;
    srvRenaming: boolean;
    srvCopied: string | null;
    srvCmdCopied: boolean;
    setupCmd: string;
    defaultServerName: (url: string) => string;
    onClose: () => void;
    onSubmit: () => void;
    onFillRecent: (sv: ServerInfo) => void;
    onStartRename: (sv: ServerInfo) => void;
    onSaveRename: () => void;
    onCancelRename: () => void;
    onCopyUrl: (url: string) => void;
    onForget: (id: string) => void;
    onCopySetup: () => void;
  } = $props();
</script>

<button class="confirm-backdrop" data-testid="server-dialog-backdrop" aria-label="Close" tabindex="-1" onclick={onClose}></button>
<div class="confirm-card srv-card" data-testid="server-dialog" role="dialog" aria-modal="true" aria-label="Connect to server">
  <label class="srv-field-label" for="srv-url">Server URL</label>
  <input id="srv-url" class="srv-input" data-testid="server-url-input" placeholder="https://my-vps.tailnet.ts.net:8443" autocomplete="off" spellcheck="false"
    bind:value={srvUrlDraft} disabled={srvBusy}
    onkeydown={(e) => { if (e.key === "Enter") onSubmit(); else if (e.key === "Escape") onClose(); }} />
  <label class="srv-field-label" for="srv-token">Token</label>
  <input id="srv-token" class="srv-input" data-testid="server-token-input" type="password" placeholder="printed by vicuna serve (kept for saved servers)" autocomplete="off"
    bind:value={srvTokenDraft} disabled={srvBusy}
    onkeydown={(e) => { if (e.key === "Enter") onSubmit(); else if (e.key === "Escape") onClose(); }} />
  <label class="srv-field-label" for="srv-name">Display name <span class="srv-optional">(optional)</span></label>
  <input id="srv-name" class="srv-input" data-testid="server-name-input" placeholder="defaults to host:port" autocomplete="off" spellcheck="false"
    bind:value={srvNameDraft} disabled={srvBusy}
    oninput={() => (srvNameTouched = true)}
    onkeydown={(e) => { if (e.key === "Enter") onSubmit(); else if (e.key === "Escape") onClose(); }} />
  {#if srvErr}<p class="goto-error" data-testid="server-error">{srvErr}</p>{/if}
  <button class="srv-connect-btn" data-testid="server-connect-btn" disabled={srvBusy || srvUrlDraft.trim() === ""} onclick={onSubmit}>{srvBusy ? "Connecting…" : "Connect"}</button>
  {#if srvList.length}
    <div class="srv-recent-label">Recent servers</div>
    {#each srvList as sv (sv.id)}
      <div class="srv-recent-row" data-testid="server-recent">
        {#if srvEditingId === sv.id}
          <input class="srv-recent-input" data-testid="server-rename-input" placeholder={defaultServerName(sv.url)} autocomplete="off" spellcheck="false"
            bind:value={srvEditDraft} disabled={srvRenaming}
            onkeydown={(e) => { if (e.key === "Enter") onSaveRename(); else if (e.key === "Escape") onCancelRename(); }} />
          <button class="srv-recent-icon" data-testid="server-rename-save" aria-label="Save name" title="Save" disabled={srvRenaming} onclick={onSaveRename}>✓</button>
          <button class="srv-recent-icon" data-testid="server-rename-cancel" aria-label="Cancel rename" title="Cancel" disabled={srvRenaming} onclick={onCancelRename}>×</button>
        {:else}
          <button class="srv-recent" title="Fill in {sv.url} (its saved token is reused)" onclick={() => onFillRecent(sv)}>{sv.name}</button>
          <button class="srv-recent-icon" data-testid="server-rename" aria-label="Rename {sv.name}" title="Rename" onclick={() => onStartRename(sv)}>
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5Z"/></svg>
          </button>
          <button class="srv-recent-icon" aria-label="Copy URL" title={srvCopied === sv.url ? "Copied" : "Copy URL"} onclick={() => onCopyUrl(sv.url)}>
            {#if srvCopied === sv.url}✓{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="8" y="3" width="8" height="4" rx="1"/><path d="M16 5h2a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V7a2 2 0 0 1 2-2h2"/></svg>{/if}
          </button>
          <button class="srv-recent-icon" data-testid="server-forget" aria-label="Remove {sv.name}" title="Remove" onclick={() => onForget(sv.id)}>×</button>
        {/if}
      </div>
    {/each}
  {/if}
  <p class="srv-help">Set up a server: run this on that machine. It prints the URL and token to paste above.</p>
  <div class="srv-cmd" data-testid="server-setup-cmd">
    <pre>{setupCmd}</pre>
    <button class="srv-cmd-copy" data-testid="server-setup-copy" aria-label="Copy commands" title={srvCmdCopied ? "Copied" : "Copy"} onclick={onCopySetup}>
      {#if srvCmdCopied}
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>
      {:else}
        <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="12" height="12" rx="2"/><path d="M5 15V5a2 2 0 0 1 2-2h10"/></svg>
      {/if}
    </button>
  </div>
  <p class="srv-help">Work sessions, files and agents then run there; Chat mode keeps using this computer's Ollama.</p>
</div>

<style>
  input { -webkit-user-select: text; user-select: text; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .confirm-card.srv-card { width: min(460px, calc(100vw - 48px)); display: flex; flex-direction: column; gap: 6px; }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  .srv-field-label { font-size: 13.5px; font-weight: 600; color: var(--text); margin-top: 4px; }
  .srv-optional { font-weight: 400; color: var(--muted); }
  .srv-input { border: 1px solid var(--border-2); border-radius: 12px; padding: 10px 12px; font: inherit; font-size: 14.5px; background: transparent; color: var(--text); }
  .srv-input:focus { outline: none; border-color: var(--accent); }
  .srv-connect-btn { margin-top: 8px; border: 1px solid var(--border-2); border-radius: 12px; padding: 10px; font: inherit; font-size: 15px; font-weight: 600; background: transparent; color: var(--text); cursor: pointer; }
  .srv-connect-btn:not(:disabled):hover { background: var(--hover-2); }
  .srv-connect-btn:disabled { opacity: .55; cursor: default; }
  .srv-recent-label { font-size: 13px; color: var(--muted); margin-top: 14px; }
  .srv-recent-row { display: flex; gap: 6px; }
  .srv-recent { flex: 1; min-width: 0; text-align: left; border: 1px solid var(--border-2); border-radius: 12px; padding: 8px 12px; font: inherit; font-size: 14px; background: transparent; color: var(--text); cursor: pointer; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .srv-recent-input { flex: 1; min-width: 0; border: 1px solid var(--accent); border-radius: 12px; padding: 8px 12px; font: inherit; font-size: 14px; background: transparent; color: var(--text); }
  .srv-recent-input:focus { outline: none; }
  .srv-recent:hover, .srv-recent-icon:hover { background: var(--hover-2); }
  .srv-recent-icon { flex: none; width: 40px; border: 1px solid var(--border-2); border-radius: 12px; background: transparent; color: var(--text-dim); cursor: pointer; font-size: 16px; display: flex; align-items: center; justify-content: center; }
  .srv-help { font-size: 12px; color: var(--muted); margin: 12px 0 0; line-height: 1.5; }
  .srv-cmd { position: relative; margin-top: 6px; border: 1px solid var(--border-2); border-radius: 12px; background: var(--hover-2, rgba(0,0,0,.03)); }
  .srv-cmd pre { margin: 0; padding: 10px 44px 10px 12px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.6; color: var(--text); white-space: pre-wrap; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
  .srv-cmd-copy { position: absolute; top: 6px; right: 6px; width: 30px; height: 30px; display: flex; align-items: center; justify-content: center; border: 1px solid var(--border-2); border-radius: 8px; background: var(--card); color: var(--text-dim); cursor: pointer; }
  .srv-cmd-copy:hover { color: var(--text); }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
