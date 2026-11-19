<script lang="ts">
  // Telegram bridge for the lead session: save a bot token, pair one
  // Telegram user with a code, toggle/unpair/remove. The bridge itself runs
  // on the backend (src-tauri/src/server/telegram.rs); this only calls its
  // telegram_* actions. The token is never read back (masked tail only).
  import { onMount, onDestroy } from "svelte";
  import { invokeOrBridge } from "$lib/bridge";

  interface Status {
    configured: boolean;
    enabled: boolean;
    botUsername: string;
    tokenTail: string;
    paired: { id: number; name: string } | null;
    pairCode: string;
    polling: boolean;
    lastError: string;
  }

  let { leadTitle = "", onClose }: { leadTitle?: string; onClose: () => void } = $props();

  let st = $state<Status | null>(null);
  let token = $state("");
  let editing = $state(false);
  let busy = $state(false);
  let err = $state("");
  let timer: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      st = await invokeOrBridge<Status>("telegram_status", {});
    } catch (e) {
      err = String(e);
    }
  }

  async function run(cmd: string, args: Record<string, unknown> = {}) {
    busy = true;
    err = "";
    try {
      st = await invokeOrBridge<Status>(cmd, args);
      return true;
    } catch (e) {
      err = String(e).replace(/^Error:\s*/, "");
      return false;
    } finally {
      busy = false;
    }
  }

  async function save() {
    if (!token.trim()) return;
    if (await run("telegram_save", { token: token.trim() })) {
      token = "";
      editing = false;
    }
  }

  onMount(() => {
    void refresh();
    // pairing happens on the phone: poll so the status flips by itself
    timer = setInterval(refresh, 2000);
  });
  onDestroy(() => timer && clearInterval(timer));

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<button class="tg-backdrop" aria-label="Close" tabindex="-1" onclick={onClose}></button>
<div class="tg-card" role="dialog" aria-modal="true" aria-label="Telegram" data-testid="telegram-dialog">
  <div class="tg-title">
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg>
    Telegram
    {#if leadTitle}<span class="tg-muted">· talk to “{leadTitle}” from your phone</span>{/if}
  </div>

  {#if !st}
    <div class="tg-muted">Loading…</div>
  {:else if !st.configured || editing}
    <ol class="tg-steps">
      <li>Open <a href="https://t.me/BotFather" target="_blank" rel="noreferrer">@BotFather</a> in Telegram and send <code>/newbot</code>.</li>
      <li>Paste the bot token it gives you here.</li>
    </ol>
    <div class="tg-row">
      <input
        class="tg-input"
        data-testid="telegram-token"
        type="password"
        autocomplete="off"
        placeholder="123456789:AA…"
        bind:value={token}
        onkeydown={(e) => e.key === "Enter" && void save()}
      />
      <button class="tg-btn primary" data-testid="telegram-save" disabled={busy || !token.trim()} onclick={save}>{busy ? "Checking…" : "Save"}</button>
      {#if editing}<button class="tg-btn" onclick={() => (editing = false)}>Cancel</button>{/if}
    </div>
    <div class="tg-muted small">The token is stored on the server in <code>~/vicuna/config/telegram.json</code> (owner-only) and is never shown again.</div>
  {:else}
    <div class="tg-status" data-testid="telegram-status">
      <span class="tg-dot" class:on={st.polling && st.enabled} class:off={!st.enabled}></span>
      <a href={"https://t.me/" + st.botUsername} target="_blank" rel="noreferrer">@{st.botUsername}</a>
      <span class="tg-muted">{st.tokenTail}</span>
      <span class="tg-spacer"></span>
      <button class="tg-btn small" onclick={() => (editing = true)}>Change token</button>
    </div>

    {#if st.paired}
      <div class="tg-box" data-testid="telegram-paired">
        Paired with <b>{st.paired.name}</b>. Messages they send to the bot go to your lead session; its replies, questions and worker reports come back.
      </div>
    {:else}
      <div class="tg-box" data-testid="telegram-pairing">
        <div>Send this to <a href={"https://t.me/" + st.botUsername + "?start=" + st.pairCode} target="_blank" rel="noreferrer">@{st.botUsername}</a> from your phone:</div>
        <div class="tg-code" data-testid="telegram-code">/start {st.pairCode}</div>
        <div class="tg-muted small">Only the account that sends the code can use the bot. Waiting…</div>
      </div>
    {/if}

    <div class="tg-row">
      <label class="tg-check">
        <input type="checkbox" data-testid="telegram-enabled" checked={st.enabled} disabled={busy} onchange={(e) => run("telegram_set", { enabled: (e.target as HTMLInputElement).checked })} />
        Bridge on
      </label>
      <span class="tg-spacer"></span>
      {#if st.paired}<button class="tg-btn" data-testid="telegram-unpair" disabled={busy} onclick={() => run("telegram_unpair")}>Unpair</button>{/if}
      <button class="tg-btn danger" data-testid="telegram-remove" disabled={busy} onclick={() => run("telegram_remove")}>Remove bot</button>
    </div>
    <div class="tg-muted small">Runs on this Vicuna server while it's up. Use one server per bot: Telegram lets only one server receive a bot's messages.</div>
  {/if}

  {#if err || st?.lastError}
    <div class="tg-err" data-testid="telegram-error">{err || st?.lastError}</div>
  {/if}

  <div class="tg-foot"><button class="tg-btn" onclick={onClose}>Done</button></div>
</div>

<style>
  .tg-backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, 0.18); border: none; cursor: default; }
  .tg-card { position: fixed; left: 50%; top: 50%; transform: translate(-50%, -50%); z-index: 61; width: min(500px, calc(100vw - 48px)); display: flex; flex-direction: column; gap: 12px; background: var(--card); border-radius: 20px; box-shadow: 0 24px 80px rgba(0, 0, 0, 0.18); padding: 18px; box-sizing: border-box; color: var(--text); font-size: 14px; }
  .tg-title { display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; }
  .tg-title svg { color: #2aabee; }
  .tg-muted { color: var(--muted); font-weight: 400; }
  .small { font-size: 12.5px; }
  .tg-steps { margin: 0; padding-left: 20px; line-height: 1.6; }
  .tg-row, .tg-status { display: flex; align-items: center; gap: 8px; }
  .tg-spacer { flex: 1; }
  .tg-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 10px; font: inherit; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; background: transparent; color: var(--text); }
  .tg-input:focus { outline: none; border-color: var(--accent); }
  .tg-box { border: 1px solid var(--border); border-radius: 12px; padding: 12px; display: flex; flex-direction: column; gap: 8px; line-height: 1.5; }
  .tg-code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 20px; letter-spacing: 1px; user-select: text; }
  .tg-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--muted); }
  .tg-dot.on { background: #3fb950; }
  .tg-dot.off { background: var(--border-2); }
  .tg-check { display: flex; align-items: center; gap: 6px; cursor: pointer; user-select: none; }
  .tg-err { color: var(--danger); font-size: 13px; overflow-wrap: anywhere; }
  .tg-foot { display: flex; justify-content: flex-end; }
  a { color: var(--accent); }
  code { font-size: 12.5px; }
  .tg-btn { border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 14px; font: inherit; font-size: 14px; background: transparent; color: var(--text); cursor: pointer; }
  .tg-btn.small { padding: 4px 10px; font-size: 12.5px; }
  .tg-btn:hover:not(:disabled) { background: var(--hover-2); }
  .tg-btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg, #fff); font-weight: 600; }
  .tg-btn.danger { color: var(--danger); }
  .tg-btn:disabled { opacity: 0.55; cursor: default; }
</style>
