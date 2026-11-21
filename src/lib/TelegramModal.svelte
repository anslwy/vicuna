<script lang="ts">
  // Per-session Telegram bridge: save a bot token for a Work session, pair one
  // Telegram user with a code, toggle/unpair/remove. Each session's bridge runs
  // independently on the backend (src-tauri/src/server/telegram.rs); tokens are
  // never read back (masked tail only).
  import { onMount, onDestroy } from "svelte";
  import { invokeOrBridge } from "$lib/bridge";
  import { telegramConnState, type TelegramStatusSnapshot } from "$lib/telegram";

  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | undefined;
  async function copyCode(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = false), 1500);
    } catch {}
  }

  interface Status {
    sessionId?: string;
    configured: boolean;
    enabled: boolean;
    botUsername: string;
    tokenTail: string;
    paired: { id: number; name: string } | null;
    pairCode: string;
    polling: boolean;
    lastError: string;
  }

  interface WhisperModel { id: string; label: string; sizeMb: number; minRamMb: number; installed: boolean; fits: boolean }
  interface WhisperStatus {
    available: boolean;
    ramMb: number | null;
    models: WhisperModel[];
    installing: { model: string; done: number; total: number } | null;
    error: string;
  }

  interface SessionRef {
    id: string;
    title: string;
    archived?: boolean;
  }

  /** `lead` turns the dialog into the Make/Replace lead prompt: an explainer,
   *  this session's Telegram setup as an optional section, and Cancel + confirm. */
  let {
    sessionId,
    sessionTitle = "",
    lead = null,
    allStatuses = {},
    sessions = [],
    onSelectSession,
    onClose,
  }: {
    sessionId: string;
    sessionTitle?: string;
    lead?: { prevTitle: string | null; onConfirm: () => void } | null;
    allStatuses?: Record<string, TelegramStatusSnapshot>;
    sessions?: SessionRef[];
    onSelectSession?: (id: string) => void;
    onClose: () => void;
  } = $props();
  let showTelegram = $state(false);

  let st = $state<Status | null>(null);
  let token = $state("");
  let editing = $state(false);
  let busy = $state(false);
  let err = $state("");
  let ws = $state<WhisperStatus | null>(null);
  let wsBusy = $state(false);
  let timer: ReturnType<typeof setInterval> | null = null;

  const otherBots = $derived(
    Object.entries(allStatuses)
      .filter(([id, s]) => id !== sessionId && s?.configured)
      .map(([id, s]) => {
        const ref = sessions.find((x) => x.id === id);
        return {
          id,
          title: ref?.title || "Untitled session",
          archived: !!ref?.archived,
          botUsername: s.botUsername || "",
          state: telegramConnState(s),
        };
      })
  );

  async function refresh() {
    const sid = sessionId;
    try {
      const res = await invokeOrBridge<Status>("telegram_status", { sessionId: sid });
      if (sid === sessionId) st = res;
    } catch (e) {
      if (sid === sessionId) err = String(e);
    }
    try {
      ws = await invokeOrBridge<WhisperStatus>("whisper_status", {});
    } catch {
      ws = null; // older server without voice transcription
    }
  }

  async function run(cmd: string, args: Record<string, unknown> = {}) {
    busy = true;
    err = "";
    try {
      st = await invokeOrBridge<Status>(cmd, { sessionId, ...args });
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

  async function whisper(cmd: "whisper_install" | "whisper_uninstall", model: string) {
    wsBusy = true;
    err = "";
    try {
      ws = await invokeOrBridge<WhisperStatus>(cmd, { model });
    } catch (e) {
      err = String(e).replace(/^Error:\s*/, "");
    } finally {
      wsBusy = false;
    }
  }

  const gb = (mb: number) => `${(mb / 1024).toFixed(mb % 1024 ? 1 : 0)} GB`;

  $effect(() => {
    void sessionId;
    st = null;
    token = "";
    editing = false;
    err = "";
    void refresh();
  });

  onMount(() => {
    // pairing happens on the phone: poll so the status flips by itself
    timer = setInterval(refresh, 2000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
    clearTimeout(copiedTimer);
  });

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      onClose();
    }
  }
</script>

<svelte:window onkeydown={onKey} />

<button class="tg-backdrop" aria-label="Close" tabindex="-1" onclick={onClose}></button>
<div class="tg-card" role="dialog" aria-modal="true" aria-label={lead ? "Lead session" : "Telegram"} data-testid={lead ? "lead-dialog" : "telegram-dialog"}>
  {#if lead}
    <div class="tg-title">{lead.prevTitle ? "Replace lead" : "Make lead"}{#if sessionTitle}<span class="tg-muted tg-session-name">· “{sessionTitle}”</span>{/if}</div>
    <div class="tg-box" data-testid="lead-explainer">
      <div>The lead is the one session you talk to that manages the others. From its next turn it can create worker sessions (each on its own harness and model), message, stop and archive them, and it gets each worker's reply when that worker finishes.</div>
      <div class="tg-muted small">Use it as your home base: hand it a big task and let it split the work across sessions. It's pinned to the top of the sidebar. Deleting a session always asks you first.</div>
    </div>
    {#if lead.prevTitle}
      <div class="tg-warn" data-testid="lead-replace-note">Only one session can be the lead. “{lead.prevTitle}” stops being the lead (it stays pinned) and this session takes over. Existing Telegram bots stay attached to their own sessions.</div>
    {/if}
    <button class="tg-toggle" data-testid="lead-telegram-toggle" aria-expanded={showTelegram} onclick={() => (showTelegram = !showTelegram)}>
      <span class="tg-chev" class:open={showTelegram}>›</span>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="#2aabee" aria-hidden="true"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg>
      Talk to this session from Telegram <span class="tg-muted">(optional{st?.paired ? ` · paired with ${st.paired.name}` : st?.configured ? " · bot set up" : ""})</span>
    </button>
  {/if}
  {#if !lead || showTelegram}
    {#if !lead}
      <div class="tg-title">
        <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg>
        <span>Telegram</span>
        {#if sessionTitle}<span class="tg-muted tg-session-name">· “{sessionTitle}”</span>{/if}
      </div>
    {/if}

    {#if !st}
      <div class="tg-muted">Loading…</div>
    {:else if !st.configured || editing}
      <div class="tg-muted small">Each Work session can have its own Telegram bot. Messages go directly to this session.</div>
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
      <div class="tg-muted small">Stored on the server in <code>~/vicuna/config/telegram.json</code> (owner-only) and never shown again.</div>
    {:else}
      <div class="tg-status" data-testid="telegram-status">
        <span class="tg-dot" class:on={st.polling && st.enabled && !!st.paired} class:off={!st.enabled}></span>
        <a href={"https://t.me/" + st.botUsername} target="_blank" rel="noreferrer">@{st.botUsername}</a>
        <span class="tg-muted">{st.tokenTail}</span>
        <span class="tg-spacer"></span>
        <button class="tg-btn small" onclick={() => (editing = true)}>Change token</button>
      </div>

      {#if st.paired}
        <div class="tg-box" data-testid="telegram-paired">
          <div>Paired with <b>{st.paired.name}</b>. Messages they send to this bot go to this session; its replies and questions come back.</div>
        </div>
      {:else}
        <div class="tg-box" data-testid="telegram-pairing">
          <div>Send this to <a href={"https://t.me/" + st.botUsername + "?start=" + st.pairCode} target="_blank" rel="noreferrer">@{st.botUsername}</a> from your phone:</div>
          <div class="tg-code-row">
            <div class="tg-code" data-testid="telegram-code">/start {st.pairCode}</div>
            <button class="tg-btn small" data-testid="telegram-code-copy" onclick={() => copyCode("/start " + st!.pairCode)}>{copied ? "Copied" : "Copy"}</button>
          </div>
          <div class="tg-muted small">Only the account that sends the code can use this bot. Waiting…</div>
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

      {#if ws}
        <details class="tg-details" data-testid="whisper-section">
          <summary>
            <span>Voice memos</span>
            <span class="tg-muted small">shared across all bots{ws.models.some((m) => m.installed) ? " · active" : ""}</span>
          </summary>
          <div class="tg-details-body">
            {#if !ws.available}
              <div class="tg-muted small">This Vicuna build has no voice transcription. Voice memos still arrive as audio files.</div>
            {:else}
              {#each ws.models as m (m.id)}
                {@const dl = ws.installing?.model === m.id ? ws.installing : null}
                <div class="tg-row" data-testid={"whisper-" + m.id}>
                  <div class="tg-wm">
                    <div>{m.label} <span class="tg-muted small">· {m.sizeMb} MB</span></div>
                    {#if dl}
                      <div class="tg-bar"><div style:width={dl.total ? `${Math.min(100, (dl.done / dl.total) * 100)}%` : "4%"}></div></div>
                    {:else if !m.installed && !m.fits}
                      <div class="tg-muted small">Needs {gb(m.minRamMb)} RAM{ws.ramMb ? `; this server has ${gb(ws.ramMb)}` : ""}.</div>
                    {/if}
                  </div>
                  {#if dl}
                    <span class="tg-muted small">{dl.total ? Math.round((dl.done / dl.total) * 100) : 0}%</span>
                  {:else if m.installed}
                    <button class="tg-btn small" data-testid={"whisper-uninstall-" + m.id} disabled={wsBusy} onclick={() => whisper("whisper_uninstall", m.id)}>Uninstall</button>
                  {:else}
                    <button class="tg-btn small" data-testid={"whisper-install-" + m.id} disabled={wsBusy || !!ws.installing || !m.fits} onclick={() => whisper("whisper_install", m.id)}>Install</button>
                  {/if}
                </div>
              {/each}
              {#if !ws.models.some((m) => m.installed)}
                <div class="tg-muted small">Optional. Without a model, voice memos arrive as audio files.</div>
              {/if}
              {#if ws.error}<div class="tg-err">{ws.error}</div>{/if}
            {/if}
          </div>
        </details>
      {/if}

      <div class="tg-muted small">Bound to this session (changing Lead won't move it). Use one bot token per session.</div>
    {/if}

    {#if !lead && otherBots.length > 0}
      <details class="tg-details" data-testid="telegram-other-bots">
        <summary>
          <span>Other sessions with bots ({otherBots.length})</span>
        </summary>
        <div class="tg-details-body">
          {#each otherBots as b (b.id)}
            <button
              class="tg-other-row"
              disabled={!onSelectSession}
              onclick={() => onSelectSession?.(b.id)}
            >
              <span class="tg-dot" class:on={b.state === "connected"} class:off={b.state !== "connected"}></span>
              <span class="tg-other-title">{b.title}{b.archived ? " (archived)" : ""}</span>
              {#if b.botUsername}<span class="tg-muted small">@{b.botUsername}</span>{/if}
            </button>
          {/each}
        </div>
      </details>
    {/if}
  {/if}

  {#if (!lead || showTelegram) && (err || st?.lastError)}
    <div class="tg-err" data-testid="telegram-error">{err || st?.lastError}</div>
  {/if}

  <div class="tg-foot">
    {#if lead}
      <button class="tg-btn" data-testid="lead-cancel" onclick={onClose}>Cancel</button>
      <button class="tg-btn primary" data-testid="lead-confirm" onclick={lead.onConfirm}>{lead.prevTitle ? "Replace lead" : "Make lead"}</button>
    {:else}
      <button class="tg-btn" onclick={onClose}>Done</button>
    {/if}
  </div>
</div>

<style>
  .tg-backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, 0.18); border: none; cursor: default; }
  .tg-card { position: fixed; left: 50%; top: 50%; transform: translate(-50%, -50%); z-index: 61; width: min(500px, calc(100vw - 48px)); max-height: calc(100vh - 48px); overflow-y: auto; display: flex; flex-direction: column; gap: 12px; background: var(--card); border-radius: 20px; box-shadow: 0 24px 80px rgba(0, 0, 0, 0.18); padding: 18px; box-sizing: border-box; color: var(--text); font-size: 14px; }
  .tg-title { display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; min-width: 0; }
  .tg-title svg { flex: none; color: #2aabee; }
  .tg-session-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tg-muted { color: var(--muted); font-weight: 400; }
  .small { font-size: 12.5px; }
  .tg-steps { margin: 0; padding-left: 20px; line-height: 1.6; }
  .tg-row, .tg-status { display: flex; align-items: center; gap: 8px; }
  .tg-spacer { flex: 1; }
  .tg-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 10px; font: inherit; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; background: transparent; color: var(--text); }
  .tg-input:focus { outline: none; border-color: var(--accent); }
  .tg-box { border: 1px solid var(--border); border-radius: 12px; padding: 12px; display: flex; flex-direction: column; gap: 8px; line-height: 1.5; }
  .tg-details { border: 1px solid var(--border); border-radius: 12px; padding: 10px 12px; }
  .tg-details summary { display: flex; align-items: center; gap: 8px; cursor: pointer; font-weight: 500; user-select: none; }
  .tg-details-body { display: flex; flex-direction: column; gap: 8px; margin-top: 10px; line-height: 1.5; }
  .tg-other-row { display: flex; align-items: center; gap: 8px; width: 100%; border: none; background: transparent; padding: 6px 8px; border-radius: 8px; font: inherit; color: var(--text); cursor: pointer; text-align: left; }
  .tg-other-row:hover:not(:disabled) { background: var(--hover-2); }
  .tg-other-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tg-code-row { display: flex; align-items: center; gap: 10px; }
  .tg-code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 20px; letter-spacing: 1px; user-select: text; }
  .tg-dot { flex: none; width: 8px; height: 8px; border-radius: 50%; background: #ef4444; }
  .tg-dot.on { background: #3fb950; }
  .tg-dot.off { background: var(--border-2); }
  .tg-check { display: flex; align-items: center; gap: 6px; cursor: pointer; user-select: none; }
  .tg-err { color: var(--danger); font-size: 13px; overflow-wrap: anywhere; }
  .tg-wm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .tg-bar { height: 4px; border-radius: 2px; background: var(--border); overflow: hidden; }
  .tg-bar > div { height: 100%; background: var(--accent); transition: width 0.3s; }
  .tg-foot { display: flex; justify-content: flex-end; gap: 8px; }
  a { color: var(--accent); }
  code { font-size: 12.5px; }
  .tg-warn { font-size: 13px; padding: 8px 10px; border-radius: 8px; background: color-mix(in srgb, #e0a100 14%, transparent); }
  .tg-toggle { display: flex; align-items: center; gap: 8px; border: none; background: transparent; color: var(--text); font: inherit; font-size: 14px; padding: 4px 0; cursor: pointer; text-align: left; }
  .tg-chev { display: inline-block; width: 10px; transition: transform .15s; }
  .tg-chev.open { transform: rotate(90deg); }
  .tg-btn { border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 14px; font: inherit; font-size: 14px; background: transparent; color: var(--text); cursor: pointer; }
  .tg-btn.small { padding: 4px 10px; font-size: 12.5px; }
  .tg-btn:hover:not(:disabled) { background: var(--hover-2); }
  .tg-btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg, #fff); font-weight: 600; }
  .tg-btn.primary:hover:not(:disabled) { background: var(--accent-hover, var(--accent)); border-color: var(--accent-hover, var(--accent)); }
  .tg-btn.danger { color: var(--danger); }
  .tg-btn:disabled { opacity: 0.55; cursor: default; }
</style>
