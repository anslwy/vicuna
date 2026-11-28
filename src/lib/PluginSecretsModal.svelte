<script lang="ts">
  // API keys for a plugin MCP server (declared `secrets`). Values are sent to
  // the server once and never come back: only `••••tail` is shown.
  import { invokeOrBridge } from "./bridge";
  import { openExternal } from "./chatDom";
  import type { InstalledPlugin } from "./pluginTheme";
  import { maskedSecret, safeHelpUrl, secretsPayload, type PluginMcpServer } from "./pluginMcp";

  let { pluginId, server, onClose, onSaved }: {
    pluginId: string;
    server: PluginMcpServer;
    onClose: () => void;
    onSaved?: (entry: InstalledPlugin) => void;
  } = $props();

  let updated = $state<PluginMcpServer | null>(null);
  const current = $derived(updated ?? server);
  let typed = $state<Record<string, string>>({});
  let busy = $state(false);
  let error = $state("");
  let notice = $state("");

  function portal(node: HTMLElement) {
    document.body.appendChild(node);
    return { destroy: () => node.remove() };
  }

  $effect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      e.stopImmediatePropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  async function run(action: string, args: Record<string, unknown>, done: string) {
    if (busy) return;
    busy = true;
    error = "";
    notice = "";
    try {
      const entry = await invokeOrBridge<InstalledPlugin>(action, args);
      const next = entry?.mcpServers?.find((m) => m.id === current.id);
      if (next) updated = next;
      typed = {};
      notice = done;
      onSaved?.(entry);
    } catch (e) {
      error = String((e as Error)?.message ?? e);
    } finally {
      busy = false;
    }
  }

  const hasTyped = $derived(Object.values(typed).some((v) => v.trim()));
  const save = () => run("plugin_mcp_set_secrets", secretsPayload(pluginId, current.id, typed), "Saved. The server restarts with the new keys on next use.");
  const clearOne = (key: string) => run("plugin_mcp_set_secrets", secretsPayload(pluginId, current.id, {}, [key]), `Cleared ${key}.`);
  const clearAll = () => run("plugin_mcp_clear_secrets", { id: pluginId, server: current.id }, "All keys cleared.");
</script>

<div use:portal class="ps-portal">
  <div class="ps-backdrop" role="presentation" onclick={onClose}></div>
  <div class="ps-card" role="dialog" aria-modal="true" aria-labelledby="ps-title" data-testid="plugin-secrets-modal">
    <h3 id="ps-title">Configure {current.label}</h3>
    <p class="ps-muted">Keys are stored on this Vicuna server in an owner-only file and passed to <code>{current.command}</code> as environment variables. They are never shown again.</p>
    <form onsubmit={(e) => { e.preventDefault(); void save(); }}>
      {#each current.secretStatus ?? [] as x (x.key)}
        {@const help = safeHelpUrl(x.helpUrl)}
        <div class="ps-field" data-testid="plugin-secret-{x.key}">
          <div class="ps-head">
            <label class="ps-label" for="ps-{x.key}">{x.label}{#if x.required}<span class="ps-req" title="Required">*</span>{/if}</label>
            <code class="ps-key">{x.key}</code>
            <span class="ps-spacer"></span>
            {#if x.configured}<span class="ps-badge ok" data-testid="plugin-secret-status-{x.key}">{maskedSecret(x)}</span>{:else}<span class="ps-badge {x.required ? 'warn' : ''}" data-testid="plugin-secret-status-{x.key}">Not set</span>{/if}
          </div>
          {#if x.description}<div class="ps-muted">{x.description}</div>{/if}
          <div class="ps-row">
            <input id="ps-{x.key}" type="password" autocomplete="off" spellcheck="false" placeholder={x.placeholder ?? (x.configured ? "Leave empty to keep" : "")} bind:value={typed[x.key]} data-testid="plugin-secret-input-{x.key}" />
            {#if help}<button type="button" class="ps-btn" data-testid="plugin-secret-help-{x.key}" title={help} onclick={() => void openExternal(help)}>Get key ↗</button>{/if}
            {#if x.configured}<button type="button" class="ps-btn" disabled={busy} data-testid="plugin-secret-clear-{x.key}" onclick={() => clearOne(x.key)}>Clear</button>{/if}
          </div>
        </div>
      {/each}
      {#if error}<div class="ps-error" role="alert">{error}</div>{/if}
      {#if notice}<div class="ps-notice">{notice}</div>{/if}
      <div class="ps-row ps-actions">
        {#if (current.secretStatus ?? []).some((x) => x.configured)}<button type="button" class="ps-btn" disabled={busy} data-testid="plugin-secrets-clear-all" onclick={clearAll}>Clear all</button>{/if}
        <span class="ps-spacer"></span>
        <button type="button" class="ps-btn" data-testid="plugin-secrets-close" onclick={onClose}>Close</button>
        <button type="submit" class="ps-btn primary" disabled={busy || !hasTyped} data-testid="plugin-secrets-save">{busy ? "Saving…" : "Save"}</button>
      </div>
    </form>
  </div>
</div>

<style>
  .ps-portal { position: fixed; inset: 0; z-index: 80; }
  .ps-backdrop { position: absolute; inset: 0; background: rgba(0,0,0,.35); }
  .ps-card { position: absolute; left: 50%; top: 50%; transform: translate(-50%, -50%); width: 480px; max-width: calc(100vw - 32px); max-height: calc(100vh - 48px); overflow: auto; box-sizing: border-box; background: var(--card); color: var(--text); border-radius: 18px; box-shadow: 0 24px 80px rgba(0,0,0,.25); padding: 20px 22px; user-select: text; }
  h3 { margin: 0 0 6px; font-size: 16px; }
  .ps-muted { font-size: 12.5px; color: var(--muted); margin: 2px 0; }
  .ps-field { margin-top: 14px; display: flex; flex-direction: column; gap: 4px; }
  .ps-head, .ps-row { display: flex; align-items: center; gap: 8px; }
  .ps-label { font-weight: 600; font-size: 13.5px; }
  .ps-req { color: var(--danger, #d33); margin-left: 2px; }
  .ps-key { font-size: 11px; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .ps-spacer { flex: 1; }
  .ps-badge { font-size: 11px; padding: 1px 7px; border-radius: 999px; border: 1px solid var(--border-2); color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .ps-badge.ok { color: var(--accent); border-color: var(--accent); }
  .ps-badge.warn { color: var(--danger, #d33); border-color: currentColor; }
  input { flex: 1; min-width: 0; font: inherit; font-size: 13px; padding: 7px 10px; border-radius: 9px; border: 1px solid var(--border-2); background: var(--input-bg, var(--card)); color: inherit; }
  .ps-btn { font: inherit; font-size: 12.5px; padding: 6px 11px; border-radius: 9px; border: 1px solid var(--border-2); background: transparent; color: inherit; cursor: pointer; white-space: nowrap; }
  .ps-btn.primary { background: var(--accent); color: var(--accent-fg, #fff); border-color: var(--accent); }
  .ps-btn:disabled { opacity: .5; cursor: default; }
  .ps-actions { margin-top: 18px; }
  .ps-error { margin-top: 10px; font-size: 12.5px; color: var(--danger, #d33); }
  .ps-notice { margin-top: 10px; font-size: 12.5px; color: var(--muted); }
</style>
