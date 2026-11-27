// MCP servers for the Work composer: the file-backed server list, live
// connection state, the popup (Discover/Added, custom form, GitHub PAT prompt)
// and per-session / default enablement. Created once by ChatApp.
import { type McpServerConfig, type McpTool, type McpEnabledMap, toggleMcpEnabled, saveMcpDefaults, isMcpEnabled, saveMcpServer, isAuthError, listMcpServers, connectServer as connectMcpBackend, disconnectServer as disconnectMcpBackend, isFixedMcp, signInServer as signInMcpBackend, listServerTools as listMcpServerTools, loginPresent as loginPresentBackend, validateServer as validateMcpServer, parseStringMap as parseMcpStringMap, deleteMcpServer, signOutServer as signOutMcpBackend } from "./mcp";
import { BUILT_IN_MCPS, type BuiltinMcp, builtinToConfig } from "./mcp-builtins";
import { tick } from "svelte";
import { isBrowserMcpId } from "./browser";
import type { Chat } from "./store";
import type { BrowserStatus } from "./browser";

export interface McpStateDeps {
  readonly activeChat: Chat | null;
  readonly serverAction: (action: string, args: Record<string, unknown>) => Promise<Chat | null | false>;
  readonly chats: Chat[];
  readonly save: () => void;
  readonly activeId: string | null;
  readonly ensureBrowserDaemon: () => Promise<BrowserStatus>;
  readonly onServer: boolean;
  explorerOpen: boolean;
  readonly openSideBrowser: () => void;
  readonly flashExpNotice: (msg: string, ttl?: number) => void;
  error: string | null;
}

export function createMcpState(deps: McpStateDeps) {
  // ---- MCP servers (Work mode only) ----
  // Configs live in ~/vicuna/config/vicuna.json (opencode.json shape); the
  // file stores shared definitions and defaults. Work sessions store their
  // switches with chat metadata; connections are shared by the backend.
  let mcpServers = $state<McpServerConfig[]>([]);
  let mcpLive = $state<Record<string, { connected: boolean; tools: McpTool[]; error: string | null }>>({});
  let homeMcpEnabled = $state<McpEnabledMap>();
  let mcpDefaultFeedback = $state("");
  let mcpSavingDefaults = $state(false);
  let mcpDefaultTimer: ReturnType<typeof setTimeout> | undefined;
  function findMcpServer(id: string): McpServerConfig | undefined {
    return mcpServers.find((s) => s.id === id);
  }
  function currentMcpSelection() {
    return deps.activeChat ?? { mcpEnabled: homeMcpEnabled };
  }
  async function setMcpForSession(sid: string | null, id: string, enabled: boolean) {
    if (sid === null) {
      homeMcpEnabled = toggleMcpEnabled([], { mcpEnabled: homeMcpEnabled }, id, enabled);
      return true;
    }
    const updated = await deps.serverAction("session_update", { id: sid, mcpToggle: { id, enabled } });
    if (updated !== false) return updated !== null;
    const chat = deps.chats.find((c) => c.id === sid);
    if (!chat) return false;
    chat.mcpEnabled = toggleMcpEnabled(mcpServers, chat, id, enabled);
    deps.save();
    return true;
  }
  async function setMcpDefaults() {
    mcpSavingDefaults = true;
    mcpDefaultFeedback = "";
    clearTimeout(mcpDefaultTimer);
    try {
      mcpServers = await saveMcpDefaults(mcpServers, currentMcpSelection());
      mcpDefaultFeedback = "Saved as default";
      mcpDefaultTimer = setTimeout(() => { mcpDefaultFeedback = ""; }, 2500);
    } catch (e) {
      mcpDefaultFeedback = String(e);
    } finally {
      mcpSavingDefaults = false;
    }
  }
  /** ids with a connection op in flight (connect/disconnect/refresh/sign-in,
   *  or `builtin:<key>` while a Built-in Add saves). A single shared slot
   *  used to drop every launch reconnect after the first, so only one
   *  server ever came back after a restart — per-server tracking fixes it. */
  let mcpBusy = $state<string[]>([]);
  function mcpIsBusy(id: string): boolean {
    return mcpBusy.includes(id);
  }
  function mcpMarkBusy(id: string) {
    if (!mcpBusy.includes(id)) mcpBusy = [...mcpBusy, id];
  }
  function mcpUnmarkBusy(id: string) {
    mcpBusy = mcpBusy.filter((x) => x !== id);
  }
  /** which popup tab is showing: the Built-in gallery or Custom servers */
  let mcpView = $state<"builtin" | "custom">("builtin");
  /** keyboard selection within the Built-in gallery */
  let mcpBuiltinSel = $state(0);
  /** one-click Add failure (desktop-only writes in a plain browser) */
  let mcpBuiltinError = $state<string | null>(null);
  /** search input: focused on open so arrows work immediately (the
   *  `autofocus` attribute only applies on first page load, not for
   *  dynamically mounted popups) */
  let mcpSearchEl = $state<HTMLInputElement | null>(null);
  $effect(() => {
    if (mcpOpen) mcpSearchEl?.focus();
  });
  /** server id waiting on the browser login redirect (label override) */
  let mcpAuthWait = $state<string | null>(null);
  /** github-only token prompt: sign-in failed with no open registration, so
   *  ask for a PAT instead (with a link to generate one) */
  let mcpPatFor = $state<string | null>(null);
  let mcpPatToken = $state("");
  let mcpPatError = $state<string | null>(null);
  let mcpOpen = $state(false);
  let mcpQuery = $state("");
  let mcpSel = $state(0);
  let mcpShowForm = $state(false);
  let mcpEditingId = $state<string | null>(null);
  let mcpForm = $state({
    name: "",
    transport: "stdio" as "stdio" | "http",
    command: "",
    args: "",
    envText: "",
    url: "",
    headersText: "",
    oauthId: "",
    oauthSecret: "",
    enabled: true
  });
  let mcpFormError = $state<string | null>(null);
  let mcpConfirmDelete = $state<string | null>(null);
  /** whether the server being edited has a stored OAuth login */
  let mcpHasLogin = $state(false);
  const mcpFiltered = $derived(
    mcpServers.filter((s) => s.name.toLowerCase().includes(mcpQuery.trim().toLowerCase()))
  );
  const mcpEnabledCount = $derived(mcpServers.filter((s) => isMcpEnabled(s, currentMcpSelection())).length);
  const builtinShown = $derived(
    BUILT_IN_MCPS.filter(
      (b) =>
        b.name.toLowerCase().includes(mcpQuery.trim().toLowerCase()) ||
        b.blurb.toLowerCase().includes(mcpQuery.trim().toLowerCase())
    )
  );
  /** installed config matching a Built-in entry (by name), if the user added it */
  function builtinInstalled(b: BuiltinMcp): McpServerConfig | undefined {
    return mcpServers.find((s) => s.name.toLowerCase() === b.name.toLowerCase());
  }
  /** status line for a Built-in row: mirrors mcpStatusOf for installed ones */
  function builtinStatus(b: BuiltinMcp): { kind: "connected" | "disabled" | "error" | "idle" | "add"; label: string } {
    const s = builtinInstalled(b);
    if (!s) return { kind: "add", label: "+ Add" };
    return mcpStatusOf(s);
  }
  /** one-click Add: writes the entry to vicuna.json and connects (an auth
   *  failure flips the row to Sign in, same as Custom servers) */
  async function addBuiltin(b: BuiltinMcp) {
    if (builtinInstalled(b) || mcpIsBusy(`builtin:${b.key}`) || mcpIsBusy(b.name)) return;
    mcpBuiltinError = null;
    mcpMarkBusy(`builtin:${b.key}`);
    const sid = deps.activeId;
    try {
      mcpServers = await saveMcpServer(builtinToConfig(b));
      if (!await setMcpForSession(sid, b.name, true)) return;
    } catch (e: any) {
      mcpBuiltinError = e?.message ?? String(e);
      return;
    } finally {
      mcpUnmarkBusy(`builtin:${b.key}`);
    }
    if (sid === deps.activeId) await connectMcpServer(b.name);
  }
  /** Add or toggle a server from the Built-in gallery. */
  async function builtinPrimary(b: BuiltinMcp) {
    const s = builtinInstalled(b);
    if (!s) {
      await addBuiltin(b);
      return;
    }
    await toggleMcpServer(s.id);
  }
  /** arrow navigation shared by the popup container + search input */
  function mcpMoveSel(delta: 1 | -1) {
    if (mcpView === "builtin") {
      mcpBuiltinSel = builtinShown.length ? (mcpBuiltinSel + delta + builtinShown.length) % builtinShown.length : 0;
    } else {
      mcpSel = mcpFiltered.length ? (mcpSel + delta + mcpFiltered.length) % mcpFiltered.length : 0;
    }
    // keep the selected row in view: the gallery is taller than the popup,
    // so arrowing past the fold would otherwise move an invisible cursor
    void tick().then(() => {
      document.querySelector(".mcp-popup .mcp-row.sel")?.scrollIntoView({ block: "nearest" });
    });
  }
  /** Enter/Space on the selected row (both views; not while editing) */
  function mcpActivateSelected() {
    if (mcpShowForm) return;
    if (mcpView === "builtin") {
      const b = builtinShown[mcpBuiltinSel];
      if (b) void builtinPrimary(b);
    } else {
      const s = mcpFiltered[mcpSel];
      if (s) void toggleMcpServer(s.id);
    }
  }
  function mcpStatusOf(s: McpServerConfig): { kind: "connected" | "disabled" | "error" | "idle"; label: string } {
    const live = mcpLive[s.id];
    if (!isMcpEnabled(s, currentMcpSelection())) return { kind: "disabled", label: "Off" };
    if (live?.connected) return { kind: "connected", label: "On · Connected" };
    if (live?.error) {
      if (isAuthError(live.error)) return { kind: "error", label: "Sign in required →" };
      return { kind: "error", label: "Error ✕" };
    }
    if (mcpIsBusy(s.id)) return { kind: "idle", label: mcpAuthWait === s.id ? "Waiting for browser…" : "Connecting…" };
    return { kind: "idle", label: "On" };
  }
  async function loadMcpServers() {
    // listMcpServers resolves via Tauri, the loopback bridge, or [] when
    // neither is present (plain browser with no backend has no servers).
    try {
      mcpServers = await listMcpServers();
    } catch (e) {
      console.error("mcp load failed:", e);
      mcpServers = [];
      return;
    }
    for (const s of mcpServers) {
      if (!isBrowserMcpId(s.id) && isMcpEnabled(s, currentMcpSelection())) void connectMcpServer(s.id);
    }
  }
  async function connectMcpServer(id: string) {
    const cfg = findMcpServer(id);
    if (!cfg || mcpIsBusy(id)) return;
    mcpMarkBusy(id);
    mcpLive[id] = { connected: false, tools: [], error: null };
    try {
      // the Browser row drives the shared daemon: make sure it is up first
      // (adopt a running one or spawn ours) before `lb mcp` dials it
      let config = cfg;
      if (isBrowserMcpId(id)) {
        await deps.ensureBrowserDaemon();
        config = (await listMcpServers()).find((s) => s.id === id) ?? cfg;
      }
      const info = await connectMcpBackend(config);
      mcpLive[id] = { connected: true, tools: info.tools, error: null };
    } catch (e: any) {
      mcpLive[id] = { connected: false, tools: [], error: e?.message ?? String(e) };
    } finally {
      mcpUnmarkBusy(id);
    }
  }
  async function disconnectMcpServer(id: string, silent = false) {
    const cfg = findMcpServer(id);
    try {
      await disconnectMcpBackend(id, cfg?.transport ?? "stdio");
    } catch (e: any) {
      if (!silent) {
        mcpLive[id] = { connected: false, tools: [], error: e?.message ?? String(e) };
        return;
      }
    }
    delete mcpLive[id];
  }
  async function toggleMcpServer(id: string) {
    const cfg = findMcpServer(id);
    if (!cfg || isFixedMcp(id) || mcpIsBusy(id)) return;
    const sid = deps.activeId;
    const enable = !isMcpEnabled(cfg, currentMcpSelection());
    mcpMarkBusy(id);
    try {
      if (!await setMcpForSession(sid, id, enable)) return;
    } finally {
      mcpUnmarkBusy(id);
    }
    if (enable && sid === deps.activeId && isMcpEnabled(cfg, currentMcpSelection()) && !mcpLive[id]?.connected) {
      await connectMcpServer(id);
    }
  }
  /** OAuth sign-in uses the side-panel browser in a remote window.
   *  A server that turns out to need no auth falls back to plain connect. */
  async function signInMcpServer(id: string) {
    const cfg = findMcpServer(id);
    if (!cfg || mcpIsBusy(id)) return;
    mcpMarkBusy(id);
    mcpAuthWait = id;
    mcpLive[id] = { connected: false, tools: [], error: null };
    try {
      if (deps.onServer) {
        mcpOpen = false;
        deps.explorerOpen = true;
        deps.openSideBrowser();
        await deps.ensureBrowserDaemon();
      }
      const info = await signInMcpBackend(cfg, deps.onServer);
      mcpLive[id] = { connected: true, tools: info.tools, error: null };
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("does not require sign-in")) {
        mcpAuthWait = null;
        mcpUnmarkBusy(id);
        await connectMcpServer(id);
        return;
      }
      mcpLive[id] = { connected: false, tools: [], error: msg };
      if (deps.onServer) deps.flashExpNotice(msg);
      // github only: no open registration means the browser flow can never
      // start — prompt for a personal access token instead of dead-ending
      if (msg.includes("no open client registration") && /github/i.test(cfg?.name ?? "")) {
        mcpOpen = true;
        mcpPatFor = id;
        mcpPatToken = "";
        mcpPatError = null;
      }
    } finally {
      mcpAuthWait = null;
      mcpUnmarkBusy(id);
    }
  }
  async function refreshMcpTools(id: string) {
    const cfg = findMcpServer(id);
    if (!cfg || mcpIsBusy(id)) return;
    mcpMarkBusy(id);
    try {
      const tools = await listMcpServerTools(cfg);
      mcpLive[id] = { connected: true, tools, error: null };
    } catch (e: any) {
      mcpLive[id] = { connected: false, tools: [], error: e?.message ?? String(e) };
    } finally {
      mcpUnmarkBusy(id);
    }
  }
  /** Refresh shared definitions after agent changes and when the popup opens. */
  async function syncMcpServersAfterTurn() {
    let fresh: McpServerConfig[];
    try {
      fresh = await listMcpServers();
    } catch {
      return;
    }
    const prev = new Map(mcpServers.map((s) => [s.id, s]));
    const next = new Set(fresh.map((s) => s.id));
    mcpServers = fresh;
    for (const s of fresh) {
      const old = prev.get(s.id);
      if (old && JSON.stringify(old) === JSON.stringify(s)) continue;
      if (isMcpEnabled(s, currentMcpSelection()) && !mcpLive[s.id]?.connected) void connectMcpServer(s.id);
    }
    for (const id of prev.keys()) {
      if (!next.has(id)) delete mcpLive[id];
    }
  }
  function openMcpPopup() {
    mcpQuery = "";
    mcpSel = 0;
    mcpBuiltinSel = 0;
    // Open Added when there are servers to manage.
    mcpView = mcpServers.length ? "custom" : "builtin";
    mcpDefaultFeedback = "";
    mcpBuiltinError = null;
    mcpShowForm = false;
    mcpFormError = null;
    mcpConfirmDelete = null;
    mcpOpen = true;
    void syncMcpServersAfterTurn();
  }
  function openMcpForm(existing?: McpServerConfig) {
    mcpFormError = null;
    mcpConfirmDelete = null;
    mcpHasLogin = false;
    if (existing) {
      mcpEditingId = existing.id;
      mcpForm = {
        name: existing.name,
        transport: existing.transport,
        command: existing.command?.[0] ?? "",
        args: (existing.command ?? []).slice(1).join(" "),
        envText: existing.env ? JSON.stringify(existing.env, null, 2) : "",
        url: existing.url ?? "",
        headersText: existing.headers ? JSON.stringify(existing.headers, null, 2) : "",
        oauthId: existing.oauth?.client_id ?? "",
        oauthSecret: existing.oauth?.client_secret ?? "",
        enabled: isMcpEnabled(existing, currentMcpSelection())
      };
      // does this server hold a browser login? (drives the Sign out row)
      const checkId = existing.id;
      void loginPresentBackend(existing.id)
        .then((v) => {
          if (mcpEditingId === checkId) mcpHasLogin = v;
        })
        .catch(() => {});
    } else {
      mcpEditingId = null;
      mcpForm = { name: "", transport: "stdio", command: "", args: "", envText: "", url: "", headersText: "", oauthId: "", oauthSecret: "", enabled: true };
    }
    mcpShowForm = true;
  }
  function closeMcpForm() {
    mcpShowForm = false;
    mcpEditingId = null;
    mcpFormError = null;
  }
  async function saveMcpForm() {
    const sid = deps.activeId;
    const err = validateMcpServer(mcpForm);
    if (err) {
      mcpFormError = err;
      return;
    }
    const argParts = mcpForm.args.split(/\s+/).filter(Boolean);
    const oauthId = mcpForm.oauthId.trim();
    const cfg: McpServerConfig = {
      id: (mcpEditingId ?? mcpForm.name.trim()),
      name: mcpForm.name.trim(),
      transport: mcpForm.transport,
      command: mcpForm.transport === "stdio" ? [mcpForm.command.trim(), ...argParts] : undefined,
      env: mcpForm.transport === "stdio" ? parseMcpStringMap(mcpForm.envText) : undefined,
      url: mcpForm.transport === "http" ? mcpForm.url.trim() : undefined,
      headers: mcpForm.transport === "http" ? parseMcpStringMap(mcpForm.headersText) : undefined,
      oauth: mcpForm.transport === "http" && oauthId
        ? { client_id: oauthId, ...(mcpForm.oauthSecret.trim() ? { client_secret: mcpForm.oauthSecret.trim() } : {}) }
        : undefined,
      enabled: mcpForm.enabled
    };
    if (mcpServers.some((s) => s.id !== cfg.id && s.name.toLowerCase() === cfg.name.toLowerCase())) {
      mcpFormError = "A server with that name already exists.";
      return;
    }
    try {
      await disconnectMcpServer(cfg.id, true);
    } catch {
      /* replacing the config drops the old connection */
    }
    try {
      mcpServers = await saveMcpServer(cfg, mcpEditingId);
      if (!await setMcpForSession(sid, cfg.name, cfg.enabled)) {
        mcpEditingId = cfg.name;
        mcpFormError = "Server saved. The session switch could not be updated.";
        return;
      }
    } catch (e: any) {
      mcpFormError = e?.message ?? String(e);
      return;
    }
    closeMcpForm();
    if (cfg.enabled) void connectMcpServer(cfg.name);
  }
  async function deleteMcpServerEntry(id: string) {
    if (mcpConfirmDelete !== id) {
      mcpConfirmDelete = id;
      return;
    }
    mcpConfirmDelete = null;
    await disconnectMcpServer(id, true);
    try {
      mcpServers = await deleteMcpServer(id);
    } catch (e: any) {
      deps.error = e?.message ?? String(e);
    }
  }
  /** Forget the stored browser login for the server being edited. */
  async function signOutMcp() {
    const id = mcpEditingId;
    if (!id) return;
    try {
      await signOutMcpBackend(id);
    } catch (e: any) {
      mcpFormError = e?.message ?? String(e);
      return;
    }
    mcpHasLogin = false;
    delete mcpLive[id];
  }
  /** Save the pasted PAT into the server's Headers and connect. */
  async function savePatAndConnect() {
    const id = mcpPatFor;
    const token = mcpPatToken.trim();
    if (!id) return;
    if (!token) {
      mcpPatError = "Paste a token first.";
      return;
    }
    const cfg = findMcpServer(id);
    if (!cfg) {
      mcpPatFor = null;
      return;
    }
    try {
      mcpServers = await saveMcpServer({
        ...cfg,
        headers: { ...(cfg.headers ?? {}), Authorization: `Bearer ${token}` }
      });
    } catch (e: any) {
      mcpPatError = e?.message ?? String(e);
      return;
    }
    mcpPatFor = null;
    mcpPatToken = "";
    mcpPatError = null;
    await connectMcpServer(id);
  }

  return {
    get mcpServers() { return mcpServers; },
    set mcpServers(v) { mcpServers = v; },
    get mcpLive() { return mcpLive; },
    set mcpLive(v) { mcpLive = v; },
    get homeMcpEnabled() { return homeMcpEnabled; },
    set homeMcpEnabled(v) { homeMcpEnabled = v; },
    get mcpDefaultFeedback() { return mcpDefaultFeedback; },
    set mcpDefaultFeedback(v) { mcpDefaultFeedback = v; },
    get mcpSavingDefaults() { return mcpSavingDefaults; },
    set mcpSavingDefaults(v) { mcpSavingDefaults = v; },
    get mcpDefaultTimer() { return mcpDefaultTimer; },
    set mcpDefaultTimer(v) { mcpDefaultTimer = v; },
    findMcpServer,
    currentMcpSelection,
    setMcpForSession,
    setMcpDefaults,
    get mcpBusy() { return mcpBusy; },
    set mcpBusy(v) { mcpBusy = v; },
    mcpIsBusy,
    mcpMarkBusy,
    mcpUnmarkBusy,
    get mcpView() { return mcpView; },
    set mcpView(v) { mcpView = v; },
    get mcpBuiltinSel() { return mcpBuiltinSel; },
    set mcpBuiltinSel(v) { mcpBuiltinSel = v; },
    get mcpBuiltinError() { return mcpBuiltinError; },
    set mcpBuiltinError(v) { mcpBuiltinError = v; },
    get mcpSearchEl() { return mcpSearchEl; },
    set mcpSearchEl(v) { mcpSearchEl = v; },
    get mcpAuthWait() { return mcpAuthWait; },
    set mcpAuthWait(v) { mcpAuthWait = v; },
    get mcpPatFor() { return mcpPatFor; },
    set mcpPatFor(v) { mcpPatFor = v; },
    get mcpPatToken() { return mcpPatToken; },
    set mcpPatToken(v) { mcpPatToken = v; },
    get mcpPatError() { return mcpPatError; },
    set mcpPatError(v) { mcpPatError = v; },
    get mcpOpen() { return mcpOpen; },
    set mcpOpen(v) { mcpOpen = v; },
    get mcpQuery() { return mcpQuery; },
    set mcpQuery(v) { mcpQuery = v; },
    get mcpSel() { return mcpSel; },
    set mcpSel(v) { mcpSel = v; },
    get mcpShowForm() { return mcpShowForm; },
    set mcpShowForm(v) { mcpShowForm = v; },
    get mcpEditingId() { return mcpEditingId; },
    set mcpEditingId(v) { mcpEditingId = v; },
    get mcpForm() { return mcpForm; },
    set mcpForm(v) { mcpForm = v; },
    get mcpFormError() { return mcpFormError; },
    set mcpFormError(v) { mcpFormError = v; },
    get mcpConfirmDelete() { return mcpConfirmDelete; },
    set mcpConfirmDelete(v) { mcpConfirmDelete = v; },
    get mcpHasLogin() { return mcpHasLogin; },
    set mcpHasLogin(v) { mcpHasLogin = v; },
    get mcpFiltered() { return mcpFiltered; },
    get mcpEnabledCount() { return mcpEnabledCount; },
    get builtinShown() { return builtinShown; },
    builtinInstalled,
    builtinStatus,
    addBuiltin,
    builtinPrimary,
    mcpMoveSel,
    mcpActivateSelected,
    mcpStatusOf,
    loadMcpServers,
    connectMcpServer,
    disconnectMcpServer,
    toggleMcpServer,
    signInMcpServer,
    refreshMcpTools,
    syncMcpServersAfterTurn,
    openMcpPopup,
    openMcpForm,
    closeMcpForm,
    saveMcpForm,
    deleteMcpServerEntry,
    signOutMcp,
    savePatAndConnect
  };
}

export type McpState = ReturnType<typeof createMcpState>;
