<script lang="ts">
  import { isTauriRuntime } from "$lib/ollama";
  import { describeServer as describeMcpServer, type McpServerConfig, type McpTool } from "$lib/mcp";
  import type { BuiltinMcp } from "$lib/mcp-builtins";
  import { isBrowserMcpId } from "$lib/browser";
  import { findPluginServer, isPluginMcpServer, secretsState, type PluginMcpServer } from "$lib/pluginMcp";
  import { invokeOrBridge } from "$lib/bridge";
  import type { InstalledPlugin } from "$lib/pluginTheme";
  import PluginSecretsModal from "$lib/PluginSecretsModal.svelte";

  export type McpFormState = {
    name: string;
    transport: "stdio" | "http";
    command: string;
    args: string;
    envText: string;
    url: string;
    headersText: string;
    oauthId: string;
    oauthSecret: string;
    enabled: boolean;
  };

  export type McpLiveEntry = {
    connected: boolean;
    tools: McpTool[];
    error: string | null;
  };

  let {
    mcpOpen = $bindable(false),
    mcpView = $bindable<"builtin" | "custom">("builtin"),
    mcpQuery = $bindable(""),
    mcpSel = $bindable(0),
    mcpBuiltinSel = $bindable(0),
    mcpBuiltinError = $bindable<string | null>(null),
    mcpSearchEl = $bindable<HTMLInputElement | null>(null),
    mcpShowForm,
    mcpForm = $bindable<McpFormState>(),
    mcpFormError,
    mcpConfirmDelete,
    mcpHasLogin,
    mcpSavingDefaults,
    mcpDefaultFeedback,
    mcpBusy,
    mcpServers,
    mcpFiltered,
    mcpLive,
    builtinShown,
    mcpPatFor = $bindable<string | null>(null),
    mcpPatToken = $bindable(""),
    mcpPatError,
    findMcpServer,
    builtinInstalled,
    builtinStatus,
    builtinPrimary,
    mcpIsBusy,
    mcpMoveSel,
    mcpActivateSelected,
    mcpStatusOf,
    toggleMcpServer,
    signInMcpServer,
    openMcpForm,
    closeMcpForm,
    saveMcpForm,
    deleteMcpServerEntry,
    signOutMcp,
    setMcpDefaults,
    refreshMcpTools,
    openTokenPage,
    savePatAndConnect
  }: {
    mcpOpen: boolean;
    mcpView: "builtin" | "custom";
    mcpQuery: string;
    mcpSel: number;
    mcpBuiltinSel: number;
    mcpBuiltinError: string | null;
    mcpSearchEl: HTMLInputElement | null;
    mcpShowForm: boolean;
    mcpForm: McpFormState;
    mcpFormError: string | null;
    mcpConfirmDelete: string | null;
    mcpHasLogin: boolean;
    mcpSavingDefaults: boolean;
    mcpDefaultFeedback: string | null;
    mcpBusy: string[];
    mcpServers: McpServerConfig[];
    mcpFiltered: McpServerConfig[];
    mcpLive: Record<string, McpLiveEntry>;
    builtinShown: BuiltinMcp[];
    mcpPatFor: string | null;
    mcpPatToken: string;
    mcpPatError: string | null;
    findMcpServer: (id: string) => McpServerConfig | undefined;
    builtinInstalled: (b: BuiltinMcp) => McpServerConfig | undefined;
    builtinStatus: (b: BuiltinMcp) => { kind: "connected" | "disabled" | "error" | "idle" | "add"; label: string };
    builtinPrimary: (b: BuiltinMcp) => Promise<void>;
    mcpIsBusy: (id: string) => boolean;
    mcpMoveSel: (delta: 1 | -1) => void;
    mcpActivateSelected: () => void;
    mcpStatusOf: (s: McpServerConfig) => { kind: "connected" | "disabled" | "error" | "idle"; label: string };
    toggleMcpServer: (id: string) => Promise<void>;
    signInMcpServer: (id: string) => Promise<void>;
    openMcpForm: (existing?: McpServerConfig) => void;
    closeMcpForm: () => void;
    saveMcpForm: () => Promise<void>;
    deleteMcpServerEntry: (id: string) => Promise<void>;
    signOutMcp: () => Promise<void>;
    setMcpDefaults: () => Promise<void>;
    refreshMcpTools: (id: string) => Promise<void>;
    openTokenPage: (e: MouseEvent) => Promise<void>;
    savePatAndConnect: () => Promise<void>;
  } = $props();

  // Plugin servers with declared secrets get a Keys button; their masked
  // status comes from plugin_list (the MCP list itself never carries keys).
  let pluginList = $state<InstalledPlugin[]>([]);
  let secretsFor = $state<{ pluginId: string; server: PluginMcpServer } | null>(null);
  async function loadPlugins() {
    try {
      pluginList = await invokeOrBridge<InstalledPlugin[]>("plugin_list", {});
    } catch {
      pluginList = [];
    }
  }
  $effect(() => {
    if (mcpOpen && mcpServers.some((s) => isPluginMcpServer(s))) void loadPlugins();
  });
  const pluginServerOf = (s: McpServerConfig) => (isPluginMcpServer(s) ? findPluginServer(pluginList, s) : null);

</script>

<button class="mcp-backdrop" data-testid="mcp-backdrop" aria-label="Close MCP servers" onclick={() => (mcpOpen = false)} tabindex="-1"></button>
<!-- opencode-style MCP popup: search + server list + custom management.
     Source of truth is ~/vicuna/config/vicuna.json (opencode.json shape). -->
<div
  class="mcp-popup"
  data-testid="mcp-popup"
  role="dialog"
  aria-label="MCP servers"
  onkeydown={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      mcpOpen = false;
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      mcpMoveSel(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      mcpMoveSel(-1);
    } else if ((e.key === "Enter" || e.key === " ") && e.target === e.currentTarget) {
      e.preventDefault();
      mcpActivateSelected();
    }
  }}
  tabindex="-1"
>
  <div class="mcp-head">
    <span class="mcp-title">MCP servers</span>
    <span class="mcp-esc">esc</span>
  </div>
  <div class="seg mcp-tabs" role="tablist" aria-label="MCP server lists">
    <button
      class="seg-btn {mcpView === 'builtin' ? 'sel' : ''}"
      data-testid="mcp-tab-builtin"
      role="tab"
      aria-selected={mcpView === "builtin"}
      onclick={() => {
        mcpView = "builtin";
        mcpBuiltinSel = 0;
        mcpBuiltinError = null;
      }}
    >Discover</button>
    <button
      class="seg-btn {mcpView === 'custom' ? 'sel' : ''}"
      data-testid="mcp-tab-custom"
      role="tab"
      aria-selected={mcpView === "custom"}
      onclick={() => {
        mcpView = "custom";
        mcpSel = 0;
      }}
    >Added</button>
  </div>
  <input
    class="mcp-search"
    data-testid="mcp-search"
    placeholder="Search"
    bind:this={mcpSearchEl}
    bind:value={mcpQuery}
    oninput={() => {
      mcpSel = 0;
      mcpBuiltinSel = 0;
    }}
    spellcheck={false}
    onkeydown={(e) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        mcpOpen = false;
      } else if (e.key === "ArrowDown") {
        e.preventDefault();
        e.stopPropagation();
        mcpMoveSel(1);
      } else if (e.key === "ArrowUp") {
        e.preventDefault();
        e.stopPropagation();
        mcpMoveSel(-1);
      } else if (e.key === "Enter") {
        e.preventDefault();
        e.stopPropagation();
        mcpActivateSelected();
      } else if (e.key === " " && mcpQuery.trim() === "") {
        e.preventDefault();
        e.stopPropagation();
        mcpActivateSelected();
      }
    }}
  />
  {#if mcpView === "builtin"}
  <div class="mcp-list builtin-list" data-testid="builtin-list" role="listbox" aria-label="Discover MCP servers">
    {#each builtinShown as b, i (b.key)}
      {@const st = builtinStatus(b)}
      {@const installed = builtinInstalled(b)}
      <div
        class="mcp-row builtin-row {i === mcpBuiltinSel ? 'sel' : ''}"
        data-testid="builtin-row"
        data-name={b.name}
        role="option"
        aria-selected={i === mcpBuiltinSel}
        tabindex="0"
        title={b.blurb}
        onclick={() => {
          mcpBuiltinSel = i;
          void builtinPrimary(b);
        }}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            e.stopPropagation();
            mcpBuiltinSel = i;
            void builtinPrimary(b);
          }
        }}
        onmousemove={() => {
          if (mcpBuiltinSel !== i) mcpBuiltinSel = i;
        }}
      >
        <span class="builtin-icon" aria-hidden="true"><svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{@html b.icon}</svg></span>
        <span class="builtin-info">
          <span class="mcp-name">{b.name}</span>
          <span class="builtin-blurb">{b.blurb}</span>
        </span>
        {#if st.kind === "add"}
          <button
            class="builtin-add"
            data-testid="builtin-add-btn"
            data-name={b.name}
            disabled={mcpIsBusy(`builtin:${b.key}`) || mcpIsBusy(b.name)}
            onclick={(e) => {
              e.stopPropagation();
              void builtinPrimary(b);
            }}
          >{mcpIsBusy(`builtin:${b.key}`) ? "Adding…" : "+ Add"}</button>
        {:else if st.kind === "error" && st.label === "Sign in required →"}
          <button
            class="builtin-add"
            data-testid="builtin-signin-btn"
            data-name={b.name}
            disabled={installed ? mcpIsBusy(installed.id) : false}
            onclick={(e) => {
              e.stopPropagation();
              if (installed) void signInMcpServer(installed.id);
            }}
          >Sign in →</button>
        {:else}
          <span class="mcp-status {st.kind}">{st.kind === "connected" && installed && mcpLive[installed.id]?.tools.length ? `${mcpLive[installed.id].tools.length} · ` : ""}{st.label}</span>
        {/if}
      </div>
    {:else}
      <div class="mcp-empty">No matches for “{mcpQuery.trim()}”.</div>
    {/each}
  </div>
  {#if mcpBuiltinError}
    <p class="goto-error" data-testid="builtin-error">{mcpBuiltinError}</p>
  {/if}
  {:else}
  <div class="mcp-list" data-testid="mcp-list" role="listbox" aria-label="MCP servers">
    {#each mcpFiltered as s, i (s.id)}
      {@const st = mcpStatusOf(s)}
      {@const live = mcpLive[s.id]}
      <div
        class="mcp-row {i === mcpSel ? 'sel' : ''}"
        data-testid="mcp-row"
        data-name={s.name}
        role="option"
        aria-selected={i === mcpSel}
        tabindex="0"
        title={isBrowserMcpId(s.id) ? "Built-in shared browser (lb mcp) — same tabs as the Browser tab" : isPluginMcpServer(s) ? `From the ${s.pluginName ?? s.pluginId} plugin — ${describeMcpServer(s)}` : describeMcpServer(s)}
        onclick={() => {
          mcpSel = i;
          void toggleMcpServer(s.id);
        }}
        onkeydown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            e.stopPropagation();
            mcpSel = i;
            void toggleMcpServer(s.id);
          }
        }}
        onmousemove={() => {
          if (mcpSel !== i) mcpSel = i;
        }}
      >
        <span class="mcp-name">{s.name}</span>
        {#if isPluginMcpServer(s)}<span class="mcp-plugin-badge" data-testid="mcp-plugin-badge" title="Provided by the {s.pluginName ?? s.pluginId} plugin; manage it in Settings → Plugins">{s.pluginName ?? s.pluginId}</span>{/if}
        {#if pluginServerOf(s) && secretsState(pluginServerOf(s)?.server) !== "none"}
          {@const ps = pluginServerOf(s)!}
          {@const ks = secretsState(ps.server)}
          <button class="mcp-keys-btn {ks}" data-testid="mcp-plugin-keys-{s.id}" title={ks === "missing" ? "A required API key is not set" : "Configure API keys"} onclick={(e) => {
            e.stopPropagation();
            secretsFor = ps;
          }}>{ks === "missing" ? "Keys needed" : "Keys"}</button>
        {/if}
        <span class="mcp-tools-n">{st.kind === "connected" && live ? `${live.tools.length} tool${live.tools.length === 1 ? "" : "s"}` : isBrowserMcpId(s.id) ? "built-in" : ""}</span>
        <span class="mcp-status {st.kind}">{st.label}</span>
        {#if st.label === "Sign in required →"}
          <button class="mcp-add" disabled={mcpIsBusy(s.id)} onclick={(e) => {
            e.stopPropagation();
            void signInMcpServer(s.id);
          }}>Sign in</button>
        {/if}
        {#if !isBrowserMcpId(s.id) && !isPluginMcpServer(s)}
        <span class="mcp-row-actions">
          <button
            class="mcp-mini"
            data-testid="mcp-edit-btn"
            data-name={s.name}
            title="Edit server"
            aria-label="Edit {s.name}"
            onclick={(e) => {
              e.stopPropagation();
              openMcpForm(s);
            }}
          >✎</button>
          {#if mcpConfirmDelete === s.id}
            <button
              class="mcp-mini danger"
              data-testid="mcp-confirm-delete-btn"
              data-name={s.name}
              onclick={(e) => {
                e.stopPropagation();
                void deleteMcpServerEntry(s.id);
              }}
            >Confirm?</button>
          {:else}
            <button
              class="mcp-mini"
              data-testid="mcp-delete-btn"
              data-name={s.name}
              title="Delete server"
              aria-label="Delete {s.name}"
              onclick={(e) => {
                e.stopPropagation();
                void deleteMcpServerEntry(s.id);
              }}
            >×</button>
          {/if}
        </span>
        {/if}
      </div>
      {#if st.kind === "error" && live?.error}
        <div class="mcp-row-error" title={live.error}>{live.error}</div>
      {/if}
    {:else}
      <div class="mcp-empty">
        {#if !isTauriRuntime()}
          MCP servers need the Vicuna desktop app — browsers can't spawn local commands.
        {:else}
          No MCP servers yet — add one below, or open the Browser tab for the built-in browser.
        {/if}
      </div>
    {/each}
  </div>
  {#if mcpShowForm}
    <div class="mcp-form" data-testid="mcp-form">
      <input
        class="pull-input"
        data-testid="mcp-name-input"
        placeholder="Name — e.g. live-browser"
        bind:value={mcpForm.name}
        spellcheck={false}
      />
      <div class="seg mcp-type" role="group" aria-label="Server type">
        <button
          class="seg-btn {mcpForm.transport === 'stdio' ? 'sel' : ''}"
          data-testid="mcp-type-stdio"
          onclick={() => (mcpForm.transport = "stdio")}
        >Local command</button>
        <button
          class="seg-btn {mcpForm.transport === 'http' ? 'sel' : ''}"
          data-testid="mcp-type-http"
          onclick={() => (mcpForm.transport = "http")}
        >Remote HTTP</button>
      </div>
      {#if mcpForm.transport === "stdio"}
        <input
          class="pull-input"
          data-testid="mcp-command-input"
          placeholder="Command — e.g. lb"
          bind:value={mcpForm.command}
          spellcheck={false}
        />
        <input
          class="pull-input"
          data-testid="mcp-args-input"
          placeholder="Args, space-separated — e.g. mcp"
          bind:value={mcpForm.args}
          spellcheck={false}
        />
        <textarea
          class="stopwords-input"
          data-testid="mcp-env-input"
          rows={2}
          spellcheck={false}
          placeholder={'Env JSON (optional) — e.g. {"KEY": "value"}'}
          bind:value={mcpForm.envText}
        ></textarea>
        <span class="setting-note">Runs on this machine — desktop app only. Plain browsers can't spawn processes.</span>
      {:else}
        <input
          class="pull-input"
          data-testid="mcp-url-input"
          placeholder="URL — e.g. https://example.com/mcp"
          bind:value={mcpForm.url}
          spellcheck={false}
        />
        <textarea
          class="stopwords-input"
          data-testid="mcp-headers-input"
          rows={2}
          spellcheck={false}
          placeholder={'Headers JSON (optional) — e.g. {"Authorization": "Bearer …"}'}
          bind:value={mcpForm.headersText}
        ></textarea>
        <input
          class="pull-input"
          data-testid="mcp-oauth-id-input"
          placeholder="OAuth client ID (optional — for servers without open registration)"
          bind:value={mcpForm.oauthId}
          spellcheck={false}
        />
        <input
          class="pull-input"
          data-testid="mcp-oauth-secret-input"
          type="password"
          placeholder="OAuth client secret (optional, rarely needed)"
          bind:value={mcpForm.oauthSecret}
          spellcheck={false}
          autocomplete="off"
        />
        <span class="setting-note">Servers like GitHub only talk to pre-registered apps: create an OAuth App in the provider's developer settings with callback URL http://127.0.0.1/callback (any port works), then paste its client ID here — or use a personal access token in Headers above instead.</span>
      {/if}
      <label class="mcp-check"><input type="checkbox" data-testid="mcp-enabled-check" bind:checked={mcpForm.enabled} /> Enable in this session</label>
      {#if mcpHasLogin}
        <div class="mcp-loginrow" data-testid="mcp-login-row">
          <span class="setting-note">Signed in via browser</span>
          <button class="mini-btn" data-testid="mcp-signout-btn" onclick={() => void signOutMcp()}>Sign out</button>
        </div>
      {/if}
      {#if mcpFormError}
        <p class="goto-error" data-testid="mcp-form-error">{mcpFormError}</p>
      {/if}
      <div class="confirm-actions">
        <button class="confirm-cancel" data-testid="mcp-cancel-btn" onclick={closeMcpForm}>Cancel</button>
        <button class="confirm-ok" data-testid="mcp-save-btn" onclick={() => void saveMcpForm()}>Save</button>
      </div>
    </div>
  {/if}
  {/if}
  <div class="mcp-foot">
    <span class="mcp-hint">toggle <span class="mcp-key">space</span></span>
    <span class="mcp-spacer"></span>
    {#if mcpShowForm}
      <span class="mcp-file">~/vicuna/config/vicuna.json</span>
    {:else if mcpView === "custom"}
      <button class="mcp-add" data-testid="mcp-add-btn" onclick={() => openMcpForm()}>+ Custom</button>
      <button class="mcp-add" data-testid="mcp-set-default-btn" disabled={mcpSavingDefaults || mcpBusy.length > 0} onclick={() => void setMcpDefaults()}>Set default</button>
      {#if mcpDefaultFeedback}<span role="status" class="mcp-file">{mcpDefaultFeedback}</span>{/if}
      <button
        class="mcp-add"
        data-testid="mcp-refresh-btn"
        title="Re-list tools on connected servers"
        disabled={mcpBusy.length > 0}
        onclick={() => {
          for (const s of mcpServers) if (mcpLive[s.id]?.connected) void refreshMcpTools(s.id);
        }}
      >refresh</button>
    {:else}
      <span class="mcp-file">~/vicuna/config/vicuna.json</span>
    {/if}
  </div>
  {#if mcpPatFor}
    {@const patCfg = findMcpServer(mcpPatFor ?? "")}
    <div
      class="mcp-pat-veil"
      data-testid="mcp-pat-modal"
      role="dialog"
      aria-label="GitHub personal access token"
      tabindex="-1"
      onkeydown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          mcpPatFor = null;
        }
      }}
    >
      <div class="mcp-pat-card">
        <div class="mcp-pat-title">Connect {patCfg?.name ?? "GitHub"}</div>
        <p class="mcp-pat-body">
          This server only talks to pre-registered apps, so the browser
          sign-in can't start. Paste a personal access token instead —
          it will be saved to this server's Headers and used from now on.
        </p>
        <a
          class="mcp-pat-link"
          href="https://github.com/settings/tokens/new"
          target="_blank"
          rel="noreferrer"
          onclick={(e) => void openTokenPage(e)}
        >Generate a token on github.com →</a>
        <p class="mcp-pat-body dim">Classic token with the <code>repo</code> scope (add <code>workflow</code> for Actions).</p>
        <input
          class="pull-input"
          data-testid="mcp-pat-input"
          type="password"
          placeholder="ghp_… or github_pat_…"
          bind:value={mcpPatToken}
          spellcheck={false}
          autocomplete="off"
          onkeydown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              e.stopPropagation();
              void savePatAndConnect();
            } else if (e.key === "ArrowUp" || e.key === "ArrowDown") {
              e.stopPropagation();
            }
          }}
        />
        {#if mcpPatError}
          <p class="goto-error" data-testid="mcp-pat-error">{mcpPatError}</p>
        {/if}
        <div class="confirm-actions">
          <button class="confirm-cancel" data-testid="mcp-pat-cancel-btn" onclick={() => (mcpPatFor = null)}>Cancel</button>
          <button class="confirm-ok" data-testid="mcp-pat-save-btn" onclick={() => void savePatAndConnect()}>Save &amp; Connect</button>
        </div>
      </div>
    </div>
  {/if}
</div>

{#if secretsFor}
  <PluginSecretsModal pluginId={secretsFor.pluginId} server={secretsFor.server} onClose={() => (secretsFor = null)} onSaved={() => void loadPlugins()} />
{/if}

<style>
  textarea, input { -webkit-user-select: text; user-select: text; }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .pull-input { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 13px; font-family: inherit; padding: 6px 14px; }
  .pull-input:focus { outline: none; border-color: var(--accent); }
  .pull-input:disabled { opacity: .5; }
  .stopwords-input { width: 100%; box-sizing: border-box; border: 1px solid var(--border-strong); border-radius: 12px; background: var(--card); color: var(--text); font: inherit; font-size: 13px; line-height: 1.6; padding: 10px 12px; resize: vertical; outline: none; }
  .stopwords-input:focus { border-color: var(--accent); }
  .setting-note { font-size: 12px; color: var(--muted); }
  .seg { display: flex; background: var(--hover); border-radius: 12px; padding: 3px; gap: 2px; }
  .seg-btn { border: none; background: transparent; cursor: pointer; font: inherit; font-size: 13.5px; color: var(--text-dim); padding: 6px 16px; border-radius: 9px; }
  .seg-btn.sel { background: var(--card); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
  .mcp-backdrop { position: fixed; inset: 0; z-index: 60; border: none; background: rgba(0,0,0,.45); cursor: default; padding: 0; }
  .mcp-popup { position: fixed; z-index: 61; left: 50%; top: 16%; transform: translateX(-50%); width: min(560px, calc(100vw - 48px)); max-height: 70vh; overflow-y: auto; background: var(--bg); color: var(--text); border: 1px solid var(--border-2); border-radius: 14px; box-shadow: 0 24px 64px rgba(0,0,0,.4); padding: 18px 18px 12px 18px; box-sizing: border-box; }
  .mcp-head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: 10px; }
  .mcp-title { font-size: 17px; font-weight: 800; }
  .mcp-esc { font-size: 13px; color: var(--muted); }
  .mcp-search { width: 100%; box-sizing: border-box; border: none; outline: none; background: transparent; color: var(--text); font: inherit; font-size: 17px; padding: 4px 2px 10px 2px; }
  .mcp-search::placeholder { color: var(--faint); }
  .mcp-tabs { align-self: stretch; margin-bottom: 6px; }
  .mcp-tabs .seg-btn { flex: 1; }
  .mcp-tabs .seg-btn.sel { background: var(--accent); color: var(--accent-fg); box-shadow: none; font-weight: 700; }
  .builtin-list { gap: 2px; }
  .builtin-row { align-items: center; }
  .builtin-icon { flex-shrink: 0; width: 32px; height: 32px; border-radius: 9px; display: flex; align-items: center; justify-content: center; background: color-mix(in srgb, var(--accent) 14%, transparent); color: var(--accent); }
  .mcp-row.sel .builtin-icon { background: rgba(255,255,255,.22); color: inherit; }
  .builtin-info { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 1px; }
  .builtin-info .mcp-name { font-size: 15px; }
  .builtin-blurb { font-size: 12.5px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-row.sel .builtin-blurb { color: inherit; opacity: .85; }
  .builtin-add { flex-shrink: 0; border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 12px; font: inherit; font-size: 12.5px; cursor: pointer; white-space: nowrap; }
  .builtin-add:hover:not(:disabled) { background: var(--hover); }
  .builtin-add:disabled { opacity: .5; cursor: default; }
  .mcp-row.sel .builtin-add { border-color: currentColor; color: inherit; }
  .mcp-row.sel .builtin-add:hover:not(:disabled) { background: rgba(255,255,255,.22); }
  .mcp-list { display: flex; flex-direction: column; }
  .mcp-row { display: flex; align-items: center; gap: 10px; width: 100%; min-width: 0; overflow: hidden; box-sizing: border-box; border: none; background: transparent; color: var(--text); font: inherit; font-size: 15.5px; padding: 5px 8px; border-radius: 8px; cursor: pointer; text-align: left; }
  .mcp-row.sel { background: var(--accent); color: var(--accent-fg); }
  .mcp-row.sel .mcp-status, .mcp-row.sel .mcp-tools-n, .mcp-row.sel .mcp-mini { color: inherit; opacity: .9; }
  .mcp-name { font-weight: 700; min-width: 0; flex-shrink: 1; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-tools-n { font-size: 12px; color: var(--muted); white-space: nowrap; flex-shrink: 0; }
  .mcp-status { margin-left: auto; min-width: 0; flex-shrink: 1; max-width: 45%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-size: 14.5px; }
  .mcp-status.connected { color: var(--accent); }
  .mcp-row.sel .mcp-status.connected { color: inherit; }
  .mcp-status.disabled { color: var(--muted); }
  .mcp-status.error { color: #d9732b; }
  .mcp-status.idle { color: var(--muted); }
  .mcp-row-actions { display: flex; align-items: center; gap: 2px; flex-shrink: 0; }
  .mcp-mini { border: none; background: transparent; color: var(--faint); font-size: 13px; line-height: 1; width: 22px; height: 22px; border-radius: 6px; cursor: pointer; padding: 0; }
  .mcp-mini:hover { background: rgba(127,127,127,.25); color: var(--text); }
  .mcp-row.sel .mcp-mini:hover { background: rgba(255,255,255,.25); color: inherit; }
  .mcp-mini.danger { width: auto; padding: 0 6px; font-size: 12px; }
  .mcp-row-error { font-size: 12px; color: #d9732b; padding: 0 8px 6px 8px; overflow-wrap: anywhere; white-space: pre-wrap; max-height: 4.5em; overflow-y: auto; }
  .mcp-empty { padding: 12px 8px; font-size: 13.5px; color: var(--muted); }
  .mcp-form { display: flex; flex-direction: column; gap: 8px; padding: 12px 2px 4px 2px; }
  .mcp-type { align-self: stretch; }
  .mcp-type .seg-btn { flex: 1; }
  .mcp-check { display: flex; align-items: center; gap: 8px; font-size: 13px; color: var(--text-dim); cursor: pointer; }
  .mcp-check input { accent-color: var(--accent); }
  .mcp-loginrow { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
  .mcp-foot { display: flex; align-items: center; gap: 12px; margin-top: 14px; padding-top: 10px; font-size: 14px; color: var(--text); }
  .mcp-hint { color: var(--text); }
  .mcp-key { color: var(--faint); }
  .mcp-spacer { flex: 1; }
  .mcp-add { border: none; background: transparent; color: var(--muted-2); font: inherit; font-size: 13px; cursor: pointer; padding: 4px 6px; border-radius: 6px; }
  .mcp-add:hover { background: var(--hover); color: var(--text); }
  .mcp-add:disabled { opacity: .5; cursor: default; }
  .mcp-file { font-size: 11.5px; color: var(--faint); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .mcp-pat-veil { position: absolute; inset: 0; z-index: 5; border-radius: 14px; background: rgba(0,0,0,.55); display: flex; align-items: center; justify-content: center; padding: 20px; box-sizing: border-box; }
  .mcp-pat-card { width: 100%; max-width: 380px; background: var(--bg); border: 1px solid var(--border-2); border-radius: 14px; box-shadow: 0 24px 64px rgba(0,0,0,.4); padding: 18px; box-sizing: border-box; display: flex; flex-direction: column; gap: 10px; }
  .mcp-pat-title { font-size: 16px; font-weight: 800; }
  .mcp-pat-body { margin: 0; font-size: 13.5px; line-height: 1.55; color: var(--text-dim); }
  .mcp-pat-body.dim { font-size: 12.5px; color: var(--muted); }
  .mcp-pat-body code { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; background: var(--code-inline); border-radius: 6px; padding: 1px 5px; }
  .mcp-pat-link { font-size: 13.5px; color: var(--accent); text-decoration: none; font-weight: 600; align-self: flex-start; border-radius: 6px; }
  .mcp-pat-link:hover { text-decoration: underline; }
  .mcp-plugin-badge { font-size: 10px; padding: 1px 6px; border-radius: 999px; border: 1px solid var(--border-2); color: var(--muted, inherit); white-space: nowrap; max-width: 120px; overflow: hidden; text-overflow: ellipsis; }
  .mcp-row.sel .mcp-plugin-badge { color: inherit; border-color: rgba(255,255,255,.55); opacity: .95; }
  .mcp-keys-btn { font: inherit; font-size: 10.5px; padding: 1px 8px; border-radius: 999px; border: 1px solid var(--border-2); background: transparent; color: inherit; cursor: pointer; white-space: nowrap; }
  .mcp-keys-btn.ok { color: var(--accent); border-color: var(--accent); }
  .mcp-keys-btn.missing { color: var(--danger, #d33); border-color: currentColor; }
  .mcp-row.sel .mcp-keys-btn { color: inherit; border-color: rgba(255,255,255,.65); }
</style>
