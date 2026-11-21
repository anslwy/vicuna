<script lang="ts">
  // Generic per-session bridge modal for bridge plugins (Discord, etc.).
  // Mirrors TelegramModal.svelte, driven by the plugin's `bridge` manifest.
  import { onMount, onDestroy } from "svelte";
  import { invokeOrBridge } from "$lib/bridge";
  import { safeColor, type InstalledPlugin } from "$lib/pluginTheme";
  import {
    bridgeConnState,
    bridgeDisplayName,
    safeIconPath,
    type BridgeStatusSnapshot,
  } from "$lib/bridgePlugin";

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

  let {
    plugin,
    sessionId,
    sessionTitle = "",
    allStatuses = {},
    sessions = [],
    onSelectSession,
    onClose,
  }: {
    plugin: InstalledPlugin;
    sessionId: string;
    sessionTitle?: string;
    allStatuses?: Record<string, BridgeStatusSnapshot>;
    sessions?: SessionRef[];
    onSelectSession?: (id: string) => void;
    onClose: () => void;
  } = $props();

  const bridge = $derived(plugin.bridge ?? {});
  const name = $derived(bridgeDisplayName(plugin));
  const iconPath = $derived(safeIconPath(bridge.iconPath));
  const brandColor = $derived(safeColor(bridge.color) ? bridge.color! : "var(--accent)");
  const pairPrefix = $derived(bridge.pairCommand?.trim() ? `${bridge.pairCommand.trim()} ` : "");

  let st = $state<BridgeStatusSnapshot | null>(null);
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
          state: bridgeConnState(s),
        };
      })
  );

  async function refresh() {
    const sid = sessionId;
    const pid = plugin.id;
    try {
      const res = await invokeOrBridge<BridgeStatusSnapshot>("bridge_status", { pluginId: pid, sessionId: sid });
      if (sid === sessionId && pid === plugin.id) st = res;
    } catch (e) {
      if (sid === sessionId && pid === plugin.id) err = String(e).replace(/^Error:\s*/, "");
    }
    if (bridge.supportsVoice) {
      try {
        ws = await invokeOrBridge<WhisperStatus>("whisper_status", {});
      } catch {
        ws = null;
      }
    } else {
      ws = null;
    }
  }

  async function run(cmd: "bridge_save" | "bridge_set" | "bridge_unpair" | "bridge_remove", args: Record<string, unknown> = {}) {
    busy = true;
    err = "";
    try {
      st = await invokeOrBridge<BridgeStatusSnapshot>(cmd, { pluginId: plugin.id, sessionId, ...args });
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
    if (await run("bridge_save", { token: token.trim() })) {
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
    void plugin.id;
    st = null;
    token = "";
    editing = false;
    err = "";
    void refresh();
  });

  onMount(() => {
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

<button class="br-backdrop" aria-label="Close" tabindex="-1" onclick={onClose}></button>
<div class="br-card" role="dialog" aria-modal="true" aria-label={name} data-testid="bridge-dialog-{plugin.id}">
  <div class="br-title">
    <svg width="18" height="18" viewBox="0 0 24 24" fill="currentColor" style:color={brandColor} aria-hidden="true"><path d={iconPath}/></svg>
    <span>{name}</span>
    {#if sessionTitle}<span class="br-muted br-session-name">· “{sessionTitle}”</span>{/if}
  </div>

  {#if !st}
    <div class="br-muted">Loading…</div>
  {:else if !st.configured || editing}
    <div class="br-muted small">Each Work session can have its own {name} bot. Messages go directly to this session.</div>
    {#if bridge.setupSteps?.length}
      <ol class="br-steps">
        {#each bridge.setupSteps as step, i (i)}
          <li>
            {step.text}
            {#if step.linkUrl && step.linkText}
              {" "}<a href={step.linkUrl} target="_blank" rel="noreferrer">{step.linkText}</a>
            {/if}
          </li>
        {/each}
      </ol>
    {/if}
    <div class="br-row">
      <input
        class="br-input"
        data-testid="bridge-token"
        type="password"
        autocomplete="off"
        placeholder={bridge.tokenPlaceholder || "Paste bot token…"}
        bind:value={token}
        onkeydown={(e) => e.key === "Enter" && void save()}
      />
      <button class="br-btn primary" data-testid="bridge-save" disabled={busy || !token.trim()} onclick={save}>{busy ? "Checking…" : "Save"}</button>
      {#if editing}<button class="br-btn" onclick={() => (editing = false)}>Cancel</button>{/if}
    </div>
    <div class="br-muted small">Stored on the server in <code>~/vicuna/config/plugins/{plugin.id}.json</code> (owner-only) and never shown again.</div>
  {:else}
    {@const online = Boolean(st.connected ?? st.polling)}
    <div class="br-status" data-testid="bridge-status">
      <span class="br-dot" class:on={online && !!st.enabled && !!st.paired} class:off={!st.enabled}></span>
      {#if st.botUrl}
        <a href={st.botUrl} target="_blank" rel="noreferrer">@{st.botUsername}</a>
      {:else if st.botUsername}
        <span>@{st.botUsername}</span>
      {/if}
      <span class="br-muted">{st.tokenTail}</span>
      <span class="br-spacer"></span>
      <button class="br-btn small" onclick={() => (editing = true)}>Change token</button>
    </div>

    {#if st.paired}
      <div class="br-box" data-testid="bridge-paired">
        <div>Paired with <b>{st.paired.name}</b>. Messages they send to this bot go to this session; its replies and questions come back.</div>
      </div>
    {:else}
      <div class="br-box" data-testid="bridge-pairing">
        {#if st.inviteUrl}
          <div>
            1. Add this bot to a server you're in so you can DM it:
            <a href={st.inviteUrl} target="_blank" rel="noreferrer" data-testid="bridge-invite-link">Invite @{st.botUsername || "bot"}</a>
          </div>
          <div>2. Send this in a DM to <b>@{st.botUsername || "the bot"}</b>:</div>
        {:else}
          <div>Send this to <b>@{st.botUsername || "the bot"}</b>:</div>
        {/if}
        <div class="br-code" data-testid="bridge-code">{pairPrefix}{st.pairCode}</div>
        <div class="br-muted small">Only the account that sends the code can use this bot. Waiting…</div>
      </div>
    {/if}

    <div class="br-row">
      <label class="br-check">
        <input type="checkbox" data-testid="bridge-enabled" checked={!!st.enabled} disabled={busy} onchange={(e) => run("bridge_set", { enabled: (e.target as HTMLInputElement).checked })} />
        Bridge on
      </label>
      <span class="br-spacer"></span>
      {#if st.paired}<button class="br-btn" data-testid="bridge-unpair" disabled={busy} onclick={() => run("bridge_unpair")}>Unpair</button>{/if}
      <button class="br-btn danger" data-testid="bridge-remove" disabled={busy} onclick={() => run("bridge_remove")}>Remove bot</button>
    </div>

    {#if ws}
      <details class="br-details" data-testid="whisper-section">
        <summary>
          <span>Voice memos</span>
          <span class="br-muted small">shared across all bots{ws.models.some((m) => m.installed) ? " · active" : ""}</span>
        </summary>
        <div class="br-details-body">
          {#if !ws.available}
            <div class="br-muted small">This Vicuna build has no voice transcription. Voice memos still arrive as audio files.</div>
          {:else}
            {#each ws.models as m (m.id)}
              {@const dl = ws.installing?.model === m.id ? ws.installing : null}
              <div class="br-row">
                <div class="br-wm">
                  <div>{m.label} <span class="br-muted small">· {m.sizeMb} MB</span></div>
                  {#if dl}
                    <div class="br-bar"><div style:width={dl.total ? `${Math.min(100, (dl.done / dl.total) * 100)}%` : "4%"}></div></div>
                  {:else if !m.installed && !m.fits}
                    <div class="br-muted small">Needs {gb(m.minRamMb)} RAM{ws.ramMb ? `; this server has ${gb(ws.ramMb)}` : ""}.</div>
                  {/if}
                </div>
                {#if dl}
                  <span class="br-muted small">{dl.total ? Math.round((dl.done / dl.total) * 100) : 0}%</span>
                {:else if m.installed}
                  <button class="br-btn small" disabled={wsBusy} onclick={() => whisper("whisper_uninstall", m.id)}>Uninstall</button>
                {:else}
                  <button class="br-btn small" disabled={wsBusy || !!ws.installing || !m.fits} onclick={() => whisper("whisper_install", m.id)}>Install</button>
                {/if}
              </div>
            {/each}
            {#if ws.error}<div class="br-err">{ws.error}</div>{/if}
          {/if}
        </div>
      </details>
    {/if}

    <div class="br-muted small">Bound to this session (changing Lead won't move it). Use one bot token per session.</div>
  {/if}

  {#if otherBots.length > 0}
    <details class="br-details" data-testid="bridge-other-bots">
      <summary>
        <span>Other sessions with {name} bots ({otherBots.length})</span>
      </summary>
      <div class="br-details-body">
        {#each otherBots as b (b.id)}
          <button
            class="br-other-row"
            disabled={!onSelectSession}
            onclick={() => onSelectSession?.(b.id)}
          >
            <span class="br-dot" class:on={b.state === "connected"} class:off={b.state !== "connected"}></span>
            <span class="br-other-title">{b.title}{b.archived ? " (archived)" : ""}</span>
            {#if b.botUsername}<span class="br-muted small">@{b.botUsername}</span>{/if}
          </button>
        {/each}
      </div>
    </details>
  {/if}

  {#if err || st?.lastError}
    <div class="br-err" data-testid="bridge-error">{err || st?.lastError}</div>
  {/if}

  <div class="br-foot"><button class="br-btn" onclick={onClose}>Done</button></div>
</div>

<style>
  .br-backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, 0.18); border: none; cursor: default; }
  .br-card { position: fixed; left: 50%; top: 50%; transform: translate(-50%, -50%); z-index: 61; width: min(500px, calc(100vw - 48px)); max-height: calc(100vh - 48px); overflow-y: auto; display: flex; flex-direction: column; gap: 12px; background: var(--card); border-radius: 20px; box-shadow: 0 24px 80px rgba(0, 0, 0, 0.18); padding: 18px; box-sizing: border-box; color: var(--text); font-size: 14px; }
  .br-title { display: flex; align-items: center; gap: 8px; font-size: 15px; font-weight: 600; min-width: 0; }
  .br-title svg { flex: none; }
  .br-session-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .br-muted { color: var(--muted); font-weight: 400; }
  .small { font-size: 12.5px; }
  .br-steps { margin: 0; padding-left: 20px; line-height: 1.6; }
  .br-row, .br-status { display: flex; align-items: center; gap: 8px; }
  .br-spacer { flex: 1; }
  .br-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 10px; font: inherit; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; background: transparent; color: var(--text); }
  .br-input:focus { outline: none; border-color: var(--accent); }
  .br-box { border: 1px solid var(--border); border-radius: 12px; padding: 12px; display: flex; flex-direction: column; gap: 8px; line-height: 1.5; }
  .br-details { border: 1px solid var(--border); border-radius: 12px; padding: 10px 12px; }
  .br-details summary { display: flex; align-items: center; gap: 8px; cursor: pointer; font-weight: 500; user-select: none; }
  .br-details-body { display: flex; flex-direction: column; gap: 8px; margin-top: 10px; line-height: 1.5; }
  .br-other-row { display: flex; align-items: center; gap: 8px; width: 100%; border: none; background: transparent; padding: 6px 8px; border-radius: 8px; font: inherit; color: var(--text); cursor: pointer; text-align: left; }
  .br-other-row:hover:not(:disabled) { background: var(--hover-2); }
  .br-other-title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .br-code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 20px; letter-spacing: 1px; user-select: text; }
  .br-dot { flex: none; width: 8px; height: 8px; border-radius: 50%; background: #ef4444; }
  .br-dot.on { background: #3fb950; }
  .br-dot.off { background: var(--border-2); }
  .br-check { display: flex; align-items: center; gap: 6px; cursor: pointer; user-select: none; }
  .br-err { color: var(--danger); font-size: 13px; overflow-wrap: anywhere; }
  .br-wm { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; }
  .br-bar { height: 4px; border-radius: 2px; background: var(--border); overflow: hidden; }
  .br-bar > div { height: 100%; background: var(--accent); transition: width 0.3s; }
  .br-foot { display: flex; justify-content: flex-end; }
  a { color: var(--accent); }
  code { font-size: 12.5px; }
  .br-btn { border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 14px; font: inherit; font-size: 14px; background: transparent; color: var(--text); cursor: pointer; }
  .br-btn.small { padding: 4px 10px; font-size: 12.5px; }
  .br-btn:hover:not(:disabled) { background: var(--hover-2); }
  .br-btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg, #fff); font-weight: 600; }
  .br-btn.danger { color: var(--danger); }
  .br-btn:disabled { opacity: 0.55; cursor: default; }
</style>
