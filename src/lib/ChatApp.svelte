<script lang="ts">
  import { onMount, onDestroy, tick } from "svelte";
  import { goto } from "$app/navigation";
  import { listModels, listRunning, loadModelIntoMemory, unloadModelFromMemory, pullModel, deleteModel, chatStream, generateStream, setOllamaBase, ollamaBase as activeOllamaBase, isCloudModel, serverVersion, startServer as startOllamaServer, stopServer as stopOllamaServer, quitApp, setTrayStatus, isTauriRuntime, modelContextLength, modelThinking, type ChatMsg, type RunningModel } from "$lib/ollama";
  import { renderMarkdown } from "$lib/markdown";
  import { MODEL_CATALOG, CATALOG_UPDATED, type CatalogCat, type CatalogEntry } from "$lib/catalog";
  import { refreshCatalog, type CatalogOverride } from "$lib/library";
  import {
    initStorage,
    loadChats,
    saveChats,
    loadModel,
    saveModel,
    loadCatalog,
    saveCatalog,
    loadPrefs,
    savePrefs,
    readLocalPrefs,
    writePendingStream,
    readPendingStream,
    clearPendingStream,
    readDraft,
    writeDraft,
    clearDraft,
    loadActiveChat,
    saveActiveChat,
    DEFAULT_STOP_WORDS,
    type Chat
  } from "$lib/store";
  import { buildWorkSystemPrompt, formatToolCallHead, formatToolCallTail, formatToolResult, formatAskRecord, WORK_TOOLS, GOAL_DONE_TOOL, listWorkTree, collectDropEntries, fileToBase64, stageUploadFile, renameWorkFile, deleteWorkFile, spawnTerm, writeTerm, resizeTerm, killTerm, completeMentionText, estimateWorkTokens, startWorkAgentTurn, stopWorkAgentTurn, queueWorkAgentMsg, removeWorkAgentMsg, answerWorkAgentAsk, snapshotChats, type WorkTreeEntry, type WorkAgentEvent, type McpRoute } from "$lib/work";
  import Tooltip from "$lib/Tooltip.svelte";
  import { invokeOrBridge } from "$lib/bridge";
  import { listMcpServers, saveMcpServer, deleteMcpServer, connectServer as connectMcpBackend, disconnectServer as disconnectMcpBackend, listServerTools as listMcpServerTools, describeServer as describeMcpServer, validateServer as validateMcpServer, parseStringMap as parseMcpStringMap, sanitizeToolName, signInServer as signInMcpBackend, signOutServer as signOutMcpBackend, loginPresent as loginPresentBackend, isAuthError, type McpServerConfig, type McpTool } from "$lib/mcp";
  import { BUILT_IN_MCPS, builtinToConfig, type BuiltinMcp } from "$lib/mcp-builtins";
  import { cliStatus, cliInstall, cliUninstall, focusAppWindow, extractOpenPath, type CliStatus } from "$lib/cli";
  import type { Terminal as XTerm } from "xterm";
  import type { FitAddon as XFitAddon } from "@xterm/addon-fit";

  let { sessionId = null }: { sessionId: string | null } = $props();

  let chats = $state<Chat[]>([]);
  let activeId = $state<string | null>(null);
  let ready = $state(false);
  const initialPrefs = typeof window !== "undefined" ? readLocalPrefs() : { theme: "light" as const, accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "", zoom: 1, defaultWorkspace: "", thinkLevel: "auto" };
  let sidebarOpen = $state(initialPrefs.sidebarOpen);
  let defaultWorkspace = $state(initialPrefs.defaultWorkspace ?? "");
  /** mode + workspace picker for the session being started (home or empty draft) */
  let pendingMode = $state<"chat" | "work">("chat");
  let pendingWorkspace = $state(initialPrefs.defaultWorkspace ?? "");
  let workspaceError = $state<string | null>(null);
  /** true while the in-thread workspace input is focused: keeps the picker
   *  mounted while typing (it hides once a workspace is set) */
  let workspaceEditing = $state(false);
  /** visible "command running" indicator while a work tool executes */
  let toolStatus = $state<{ chatId: string; label: string } | null>(null);
  /** live multiple-choice question from the work agent (null when none is
   *  pending). The turn blocks in dispatchWorkTool until the user answers. */
  let pendingAsk = $state<{
    chatId: string;
    index: number;
    question: string;
    options: string[];
    other: string;
    otherOpen: boolean;
    resolve: (answer: string, outcome: "answer" | "dismiss" | "cancel") => void;
  } | null>(null);
  /** file explorer side panel (work mode): lists the workspace tree */
  let explorerOpen = $state(false);
  let explorerEntries = $state<WorkTreeEntry[]>([]);
  let explorerLoading = $state(false);
  let explorerError = $state<string | null>(null);
  let explorerQuery = $state("");
  let collapsedDirs = $state<string[]>([]);
  /** composer drop highlight + drag depth counter (flicker-free) */
  let composerDropActive = $state(false);
  let compDragDepth = 0;
  /** right-click menu on an explorer row: {x, y, path, isDir} */
  let expCtx = $state<{ x: number; y: number; path: string; isDir: boolean } | null>(null);
  /** inline rename state: row path + draft (null when not renaming) */
  let expRenaming = $state<string | null>(null);
  let expRenameDraft = $state("");
  /** two-step delete confirm: armed row path (null = unarmed) */
  let expConfirmDelete = $state<string | null>(null);
  /** transient action notice (open/reveal/rename/delete/import failures):
   *  unlike explorerError it never replaces the file list */
  let expNotice = $state<string | null>(null);
  let expNoticeTimer: ReturnType<typeof setTimeout> | null = null;

  function flashExpNotice(msg: string) {
    expNotice = msg;
    if (expNoticeTimer) clearTimeout(expNoticeTimer);
    expNoticeTimer = setTimeout(() => {
      expNotice = null;
      expNoticeTimer = null;
    }, 4500);
  }
  /** explorer panel width (drag the left edge; in-memory only) */
  let explorerWidth = $state(300);
  let expResizing = $state(false);
  /** side-panel tabs: one Files view max, any number of terminals */
  interface SideTab {
    key: string;
    kind: "files" | "terminal";
    title: string;
    termId?: string;
    dead?: boolean;
    error?: string | null;
  }
  let sideTabs = $state<SideTab[]>([{ key: "files", kind: "files", title: "Files" }]);
  let activeSideTab = $state("files");
  let sideTabSeq = 1;
  let newTabMenu = $state(false);
  /** live xterm instances by tab key (non-reactive on purpose) */
  const termViews = new Map<string, { term: XTerm; fit: XFitAddon; ro: ResizeObserver; resizeTimer: ReturnType<typeof setTimeout> | null }>();
  let lastExplorerRoot = "";

  const EXPLORER_MIN_W = 220;
  const EXPLORER_DEFAULT_W = 300;

  function clampExplorerWidth(w: number): number {
    const maxW = Math.max(240, window.innerWidth - 360);
    return Math.min(Math.max(EXPLORER_MIN_W, Math.round(w)), maxW);
  }

  /** drag the panel's left edge to resize (dragging left widens) */
  function onExpResizeStart(e: PointerEvent) {
    const handle = e.currentTarget as HTMLElement;
    const startX = e.clientX;
    const startW = explorerWidth;
    expResizing = true;
    document.body.style.cursor = "ew-resize";
    document.body.style.userSelect = "none";
    try {
      handle.setPointerCapture(e.pointerId);
    } catch {
      /* mouse path still works via window listeners below */
    }
    const move = (ev: PointerEvent) => {
      explorerWidth = clampExplorerWidth(startW + (startX - ev.clientX));
    };
    const up = () => {
      expResizing = false;
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      window.removeEventListener("pointercancel", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
    window.addEventListener("pointercancel", up);
  }

  function onExpResizeKey(e: KeyboardEvent) {
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      explorerWidth = clampExplorerWidth(explorerWidth + 10);
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      explorerWidth = clampExplorerWidth(explorerWidth - 10);
    } else if (e.key === "Home") {
      e.preventDefault();
      explorerWidth = EXPLORER_DEFAULT_W;
    }
  }
  let explorerTimer: ReturnType<typeof setTimeout> | null = null;
  let input = $state("");
  let models = $state<string[]>(["gemma4:31b-cloud"]);
  let selectedModel = $state("gemma4:31b-cloud");
  let modelOpen = $state(false);
  let modelQuery = $state("");
  let streaming = $state(false);
  /** session id of the live chat-completion stream (null when idle). Together
   *  with workRunning/workChatId this drives the sidebar shimmer so a
   *  streaming session stays visible even when it isn't the open one. */
  let streamingChatId = $state<string | null>(null);
  /** wall-clock start of the live chat stream (null when idle): sidebar hover
   *  shows elapsed time since the query started */
  let streamingStartAt = $state<number | null>(null);
  /** true for the whole Work turn, including tool dispatch + the wait for the
   *  next round's first token — `streaming` only covers a single request */
  let workRunning = $state(false);
  /** manual compaction in flight (/compact): blocks sends + edits like a turn */
  let compacting = $state(false);
  let workChatId = $state<string | null>(null);
  /** wall-clock start of the live Work turn (null when idle): sidebar hover
   *  shows elapsed time since the task started */
  let workStartAt = $state<number | null>(null);
  /** ticking clock for live elapsed labels: advances once a second while a
   *  turn is live so sidebar hover tooltips stay current */
  let now = $state(Date.now());
  let workStop = false;
  let workTurnSeq = 0;
  /** backend turn id of the live Rust agent loop (null when idle) — needed
   *  for stop/steer/ask-answer routing while a turn runs */
  let workTurnId: string | null = null;
  // ---- MCP servers (Work mode only) ----
  // Configs live in ~/vicuna/config/vicuna.json (opencode.json shape); the
  // file is the source of truth and the UI edits it via the backend.
  // Connections are runtime-only in the Rust McpManager. Tools merge into
  // the Work agent's native tool loop; chat mode never sees them.
  let mcpServers = $state<McpServerConfig[]>([]);
  let mcpLive = $state<Record<string, { connected: boolean; tools: McpTool[]; error: string | null }>>({});
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
  const mcpConnectedCount = $derived(Object.values(mcpLive).filter((l) => l.connected).length);
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
    try {
      mcpServers = await saveMcpServer(builtinToConfig(b));
    } catch (e: any) {
      mcpUnmarkBusy(`builtin:${b.key}`);
      mcpBuiltinError = e?.message ?? String(e);
      return;
    }
    mcpUnmarkBusy(`builtin:${b.key}`);
    await connectMcpServer(b.name);
  }

  /** primary row action for a Built-in: Add, Connect, or Sign in */
  async function builtinPrimary(b: BuiltinMcp) {
    const s = builtinInstalled(b);
    if (!s) {
      await addBuiltin(b);
      return;
    }
    if (mcpLive[s.id]?.connected || !s.enabled) {
      await toggleMcpServer(s.id);
      return;
    }
    if (isAuthError(mcpLive[s.id]?.error)) {
      await signInMcpServer(s.id);
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
    if (!s.enabled) return { kind: "disabled", label: "Disabled ○" };
    if (live?.connected) return { kind: "connected", label: `Connected ✓` };
    if (live?.error) {
      // an auth failure is an invitation, not a dead end: selecting the
      // row starts the browser sign-in flow (see toggleMcpServer)
      if (isAuthError(live.error)) return { kind: "error", label: "Sign in required →" };
      return { kind: "error", label: live.error.slice(0, 80) };
    }
    if (mcpIsBusy(s.id)) return { kind: "idle", label: mcpAuthWait === s.id ? "Waiting for browser…" : "Connecting…" };
    return { kind: "idle", label: "Disconnected ○" };
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
    // reconnect launch-enabled servers (best-effort: failures surface as
    // row errors in the popup instead of blocking startup)
    for (const s of mcpServers) {
      if (s.enabled) void connectMcpServer(s.id);
    }
  }

  async function connectMcpServer(id: string) {
    const cfg = mcpServers.find((s) => s.id === id);
    if (!cfg || mcpIsBusy(id)) return;
    mcpMarkBusy(id);
    mcpLive[id] = { connected: false, tools: [], error: null };
    try {
      const info = await connectMcpBackend(cfg);
      mcpLive[id] = { connected: true, tools: info.tools, error: null };
    } catch (e: any) {
      mcpLive[id] = { connected: false, tools: [], error: e?.message ?? String(e) };
    } finally {
      mcpUnmarkBusy(id);
    }
  }

  async function disconnectMcpServer(id: string, silent = false) {
    const cfg = mcpServers.find((s) => s.id === id);
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
    const cfg = mcpServers.find((s) => s.id === id);
    if (!cfg || mcpIsBusy(id)) return;
    if (mcpLive[id]?.connected) {
      await disconnectMcpServer(id);
      // disconnecting is the disable preference: persist it so a restart
      // won't reconnect everything the user just turned off
      mcpMarkBusy(id);
      try {
        mcpServers = await saveMcpServer({ ...cfg, enabled: false });
      } catch (e: any) {
        error = e?.message ?? String(e);
      } finally {
        mcpUnmarkBusy(id);
      }
      return;
    }
    // a server that failed with 401/403 isn't retried blindly — selecting
    // it starts the browser sign-in flow instead (opencode parity)
    if (isAuthError(mcpLive[id]?.error)) {
      await signInMcpServer(id);
      return;
    }
    if (!cfg.enabled) {
      // enabling a disabled server persists the flip, then connects
      mcpMarkBusy(id);
      try {
        const next = await saveMcpServer({ ...cfg, enabled: true });
        mcpServers = next;
      } catch (e: any) {
        mcpUnmarkBusy(id);
        mcpLive[id] = { connected: false, tools: [], error: e?.message ?? String(e) };
        return;
      }
      mcpUnmarkBusy(id);
      await connectMcpServer(id);
      return;
    }
    await connectMcpServer(id);
  }

  /** Browser sign-in for an OAuth-protected server (e.g. higgsfield).
   *  Opens the system browser; the backend resolves once login completes.
   *  A server that turns out to need no auth falls back to plain connect. */
  async function signInMcpServer(id: string) {
    const cfg = mcpServers.find((s) => s.id === id);
    if (!cfg || mcpIsBusy(id)) return;
    mcpMarkBusy(id);
    mcpAuthWait = id;
    mcpLive[id] = { connected: false, tools: [], error: null };
    try {
      const info = await signInMcpBackend(cfg);
      mcpLive[id] = { connected: true, tools: info.tools, error: null };
      // an explicit sign-in implies the user wants the server: enable it
      if (!mcpServers.find((s) => s.id === id)?.enabled) {
        try {
          mcpServers = await saveMcpServer({ ...cfg, enabled: true });
        } catch (e: any) {
          error = e?.message ?? String(e);
        }
      }
    } catch (e: any) {
      const msg = e?.message ?? String(e);
      if (msg.includes("does not require sign-in")) {
        mcpAuthWait = null;
        mcpUnmarkBusy(id);
        await connectMcpServer(id);
        return;
      }
      mcpLive[id] = { connected: false, tools: [], error: msg };
      // github only: no open registration means the browser flow can never
      // start — prompt for a personal access token instead of dead-ending
      if (msg.includes("no open client registration") && /github/i.test(cfg?.name ?? "")) {
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
    const cfg = mcpServers.find((s) => s.id === id);
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

  function openMcpPopup() {
    mcpQuery = "";
    mcpSel = 0;
    mcpBuiltinSel = 0;
    // default to the Added list when the user has servers, Discover only
    // when there is nothing to show there yet
    mcpView = mcpServers.length ? "custom" : "builtin";
    mcpBuiltinError = null;
    mcpShowForm = false;
    mcpFormError = null;
    mcpConfirmDelete = null;
    mcpOpen = true;
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
        enabled: existing.enabled
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
      mcpServers = await saveMcpServer(cfg);
    } catch (e: any) {
      mcpFormError = e?.message ?? String(e);
      return;
    }
    closeMcpForm();
    if (cfg.enabled) void connectMcpServer(cfg.id);
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
      error = e?.message ?? String(e);
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

  /** Open the token generation page (github-only PAT prompt). */
  async function openTokenPage(e: MouseEvent) {
    e.preventDefault();
    const url = "https://github.com/settings/tokens/new";
    if (isTauriRuntime()) {
      try {
        const { openUrl } = await import("@tauri-apps/plugin-opener");
        await openUrl(url);
        return;
      } catch {
        /* fall through to window.open */
      }
    }
    window.open(url, "_blank", "noopener");
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
    const cfg = mcpServers.find((s) => s.id === id);
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

  // Tools offered to the Work model this turn: every connected + enabled
  // server's catalog, readable names, prefixed only when two servers collide.
  // The route map translates the model's call back to (server, real name).
  function assembleMcpTools(): { tools: import("$lib/ollama").OllamaToolDef[]; route: Map<string, { server: McpServerConfig; name: string }> } {
    const tools: import("$lib/ollama").OllamaToolDef[] = [];
    const route = new Map<string, { server: McpServerConfig; name: string }>();
    const builtin = new Set(WORK_TOOLS.map((t) => t.function.name));
    const seen = new Set<string>(builtin);
    for (const s of mcpServers) {
      const live = mcpLive[s.id];
      if (!s.enabled || !live?.connected) continue;
      for (const t of live.tools) {
        let name = sanitizeToolName(t.name);
        if (seen.has(name)) name = `${sanitizeToolName(s.name)}__${name}`;
        if (seen.has(name)) continue;
        seen.add(name);
        route.set(name, { server: s, name: t.name });
        tools.push({
          type: "function",
          function: {
            name,
            description: t.description ? `${t.description} (via ${s.name})` : `${t.name} (via ${s.name})`,
            parameters: (t.inputSchema as Record<string, any> | undefined) ?? { type: "object" }
          }
        });
      }
    }
    return { tools, route };
  }
  /** prompts typed while a work turn is live: they join the context at the
   *  next round (after the tool results), never as a parallel turn */
  let queuedMsgs = $state<{ chatId: string; id: string; msg: ChatMsg }[]>([]);
  /** first Esc arms interrupt, a second Esc within 2.5s stops the agent */
  let escArmed = $state(false);
  let escTimer: ReturnType<typeof setTimeout> | null = null;
  let continuing = $state(false);
  /** picked or pasted image attachments as data URLs (shown as thumbnails, sent as base64) */
  let attachImages = $state<string[]>([]);
  /** composer file attachments (any type): staged to ~/vicuna/uploads/<session>/ on send */
  let attachFiles = $state<{ id: number; name: string; size: number; file: File }[]>([]);
  let attachFileSeq = 0;
  let error = $state<string | null>(null);
  /** auto-clear delay for the error box (ms): command notices flash by
   *  fast, real failures linger long enough to read — nothing squats until
   *  restart anymore. Plain holder (not $state) so reading it here doesn't
   *  re-trigger this effect; flashNotice() lowers it for its own message. */
  const errorCfg = { ttl: 12000 };
  let errorTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const msg = error;
    if (errorTimer) {
      clearTimeout(errorTimer);
      errorTimer = null;
    }
    if (!msg) return;
    const ttl = errorCfg.ttl;
    errorCfg.ttl = 12000;
    errorTimer = setTimeout(() => {
      errorTimer = null;
      if (error === msg) error = null;
    }, ttl);
    return () => {
      if (errorTimer) {
        clearTimeout(errorTimer);
        errorTimer = null;
      }
    };
  });
  let aborter = $state<AbortController | null>(null);
  let messagesEl = $state<HTMLDivElement | null>(null);
  let composerEl = $state<HTMLTextAreaElement | null>(null);
  let composerBackdrop = $state<HTMLDivElement | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);
  let editingIndex = $state<number | null>(null);
  let editDraft = $state("");
  let editImages = $state<string[]>([]);
  let copiedKey = $state<string | null>(null);
  let isPinned = $state(true);
  // scrollTop value of our last programmatic jump; a scroll event landing
  // exactly here is our own doing and must not unpin
  let lastJumpTop = Number.MIN_SAFE_INTEGER;
  // set when swapping chats: content replacement clamps scrollTop and fires
  // one reset event that belongs to us, not the user
  let suppressReset = false;

  let lastSeenTop = 0;
  function onMessagesScroll() {
    if (!messagesEl) return;
    if (suppressReset) {
      suppressReset = false;
      lastSeenTop = messagesEl.scrollTop;
      return;
    }
    const { scrollTop, scrollHeight, clientHeight } = messagesEl;
    // our own jump lands exactly on lastJumpTop (1px tolerance for rounding)
    if (Math.abs(scrollTop - lastJumpTop) < 2) {
      lastSeenTop = scrollTop;
      return;
    }
    const dist = scrollHeight - scrollTop - clientHeight;
    if (scrollTop < lastSeenTop - 1 && dist >= 1) {
      // user moved up, even by 1px: pause auto-follow immediately
      isPinned = false;
    } else {
      // at the very bottom the arrow hides and follow resumes;
      // anywhere else it stays paused
      isPinned = dist < 80;
    }
    lastSeenTop = scrollTop;
  }

  function jumpToBottom() {
    isPinned = true;
    scrollBottom();
  }

  // opencode-style run stats: "27m 48s", "8.1s" under a minute
  function formatDuration(ms: number): string {
    const s = ms / 1000;
    if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)}s`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ${Math.round(s % 60)}s`;
    return `${Math.floor(m / 60)}h ${m % 60}m`;
  }

  function formatPullRate(bps: number): string {
    if (!isFinite(bps) || bps <= 0) return "";
    if (bps >= 1e9) return `${(bps / 1e9).toFixed(1)} GB/s`;
    if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} MB/s`;
    if (bps >= 1e3) return `${(bps / 1e3).toFixed(0)} KB/s`;
    return `${Math.round(bps)} B/s`;
  }

  const activeChat = $derived(chats.find((c) => c.id === activeId) ?? null);
  // composer reflects the starter toggle while starting (home or empty draft);
  // a thread with messages keeps its own mode
  const composerMode = $derived(
    !activeChat || activeChat.messages.length === 0 ? pendingMode : (activeChat.mode ?? "chat")
  );
  const queuedForActive = $derived(queuedMsgs.filter((q) => q.chatId === activeId));
  /** a turn is live: either a single request or a whole Work agent turn */
  const busy = $derived(streaming || workRunning || compacting);
  /** sidebar shimmer: a session with a live chat stream or work turn */
  function isSessionStreaming(id: string): boolean {
    return (streaming && streamingChatId === id) || (workRunning && workChatId === id);
  }
  // live elapsed ("32m 25s", "1h 32m", "3d 2h") since the task/query started
  function formatLiveElapsed(ms: number): string {
    const totalS = Math.max(0, Math.floor(ms / 1000));
    const s = totalS % 60;
    const totalM = Math.floor(totalS / 60);
    const m = totalM % 60;
    const totalH = Math.floor(totalM / 60);
    const h = totalH % 24;
    const d = Math.floor(totalH / 24);
    if (d > 0) return `${d}d ${h}h`;
    if (totalH > 0) return `${totalH}h ${m}m`;
    if (totalM > 0) return `${totalM}m ${s}s`;
    return `${s}s`;
  }
  /** elapsed ms for a live session (work turn start wins between rounds);
   *  null when the session isn't live. Reads `now` so hover tooltips tick. */
  function liveElapsedMs(id: string): number | null {
    if (workRunning && workChatId === id && workStartAt != null) return now - workStartAt;
    if (streaming && streamingChatId === id && streamingStartAt != null) return now - streamingStartAt;
    return null;
  }
  /** sidebar hover tooltip for a live session ("Working · 32m 25s") */
  function liveTitle(id: string): string | undefined {
    const ms = liveElapsedMs(id);
    if (ms == null) return undefined;
    const kind = workRunning && workChatId === id ? "Working" : "Streaming";
    return `${kind} · ${formatLiveElapsed(ms)}`;
  }
  // advance the live clock while a turn runs so elapsed tooltips stay current
  $effect(() => {
    if (!streaming && !workRunning) return;
    const t = setInterval(() => {
      now = Date.now();
    }, 1000);
    return () => clearInterval(t);
  });
  // keep the tray menu in sync with live sessions (desktop only; best-effort
  // — a tray failure must never break a turn). Status line up top, elapsed
  // line under Quit. Reports are throttled: idle/starting transitions send
  // immediately, but running→running updates go out at most once per 60s —
  // rebuilding the menu more often flashes an open menu (round boundaries
  // alone can fire every few seconds on fast turns).
  let lastTrayKey: string | null = null;
  let lastTrayRunning = false;
  let lastTraySentAt = 0;
  $effect(() => {
    if (!isTauriRuntime()) return;
    // minute-bucket tick so long static turns still refresh, without per-second churn
    const minuteBucket = Math.floor(now / 60000);
    void minuteBucket;
    const starts: number[] = [];
    const ids = new Set<string>();
    if (streaming && streamingChatId) {
      ids.add(streamingChatId);
      if (streamingStartAt != null) starts.push(streamingStartAt);
    }
    if (workRunning && workChatId) {
      ids.add(workChatId);
      if (workStartAt != null) starts.push(workStartAt);
    }
    const n = ids.size;
    const at = Date.now();
    const oldest = starts.length ? Math.min(...starts) : null;
    const running = n > 0;
    const status = !running
      ? "No running sessions"
      : `${n} running work session${n === 1 ? "" : "s"}`;
    const elapsed = !running ? null : formatLiveElapsed(at - (oldest ?? at));
    const key = `${status}\n${elapsed ?? ""}`;
    if (key === lastTrayKey) return;
    if (running && lastTrayRunning && at - lastTraySentAt < 60000) return;
    lastTrayKey = key;
    lastTrayRunning = running;
    lastTraySentAt = at;
    void setTrayStatus(status, elapsed);
  });
  async function quitVicuna() {
    try {
      await quitApp();
    } catch (e: any) {
      error = e?.message ?? String(e);
    }
  }
  /** workspace the explorer panel shows: the thread's, else the pending one */
  const explorerRoot = $derived(
    activeChat && activeChat.messages.length > 0 && (activeChat.mode ?? "chat") === "work"
      ? (activeChat.workspace ?? pendingWorkspace)
      : pendingWorkspace
  );
  const explorerRootBase = $derived(workspaceBase(explorerRoot.trim()) || explorerRoot.trim());
  /** tree rows honoring collapse + search (search flattens the hierarchy) */
  const explorerVisible = $derived((() => {
    const q = explorerQuery.trim().toLowerCase();
    if (q) return explorerEntries.filter((e) => e.path.toLowerCase().includes(q));
    const collapsed = new Set(collapsedDirs);
    const under = (path: string, dir: string) =>
      path !== dir && (path.startsWith(dir + "/") || path.startsWith(dir + "\\"));
    return explorerEntries.filter((e) => {
      if (e.depth === 0) return true;
      for (const d of collapsed) {
        if (under(e.path, d)) return false;
      }
      return true;
    });
  })());

  // the composer box is global and append-only across navigation: switching
  // sessions or starting a new one never touches it. The text persists
  // across restarts via one shared draft key; only sending (or the user
  // clearing it by hand) empties it.
  $effect(() => {
    if (!ready) return;
    const text = input;
    if (text) writeDraft(null, text);
    else clearDraft(null);
  });

  // remember where the user was so a restart reopens the session (and its
  // unsent draft) instead of always landing on a blank home
  $effect(() => {
    if (!ready) return;
    saveActiveChat(activeId);
  });

  function save() {
    void saveChats(chats);
    void saveModel(selectedModel);
  }

  let saveTimer: ReturnType<typeof setTimeout> | null = null;
  let lastSaveAt = 0;
  function saveSoon() {
    // leading + trailing throttle: persist at most every 2s mid-stream,
    // plus a trailing write after tokens stop, so a refresh never
    // loses more than ~2s of streamed content
    const now = Date.now();
    if (now - lastSaveAt > 2000) {
      lastSaveAt = now;
      save();
      return;
    }
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => {
      saveTimer = null;
      lastSaveAt = Date.now();
      save();
    }, 600);
  }

  function flushSave() {
    if (saveTimer) {
      clearTimeout(saveTimer);
      saveTimer = null;
    }
    save();
  }

  // set during page unload: an abort then means "refresh/close", not
  // "user pressed Stop" — the pending-stream marker must survive so the
  // next load resumes instead of dropping the turn
  let unloading = false;
  function handleUnload() {
    unloading = true;
    if (input) writeDraft(null, input);
    else clearDraft(null);
    flushSave();
    // best-effort: don't orphan shells when the window closes
    for (const t of sideTabs) {
      if (t.termId) void killTerm(t.termId);
    }
  }

  function newChat() {
    aborter?.abort();
    aborter = null;
    streaming = false;
    streamingChatId = null;
    streamingStartAt = null;
    // the composer box is global: New never touches the textbox.
    // No placeholder session is created: the sidebar stays unchanged and
    // the session only appears once the first message is sent (send()
    // builds it inline with the first message as its title).
    if (chats.some((c) => c.messages.length === 0)) {
      chats = chats.filter((c) => c.messages.length > 0);
      save();
    }
    activeId = null;
    isPinned = true;
    editingIndex = null;
    editDraft = "";
    error = null;
    void goto("/");
  }

  function selectChat(id: string) {
    if (id === activeId) return;
    aborter?.abort();
    aborter = null;
    streaming = false;
    streamingChatId = null;
    streamingStartAt = null;
    editingIndex = null;
    editDraft = "";
    copiedKey = null;
    pendingMsgDelete = null;
    dropActiveDraft(id);
    activeId = id;
    isPinned = true;
    suppressReset = true;
    void goto(`/s/${id}`);
  }

  // adopt URL changes that didn't come from a local action (back/forward).
  // starter toggle follows the empty draft being viewed: opening a work
  // draft shows Work + its workspace, home keeps the last toggle
  $effect(() => {
    if (!ready) return;
    const c = activeChat;
    workspaceEditing = false;
    if (c && c.messages.length === 0) {
      pendingMode = c.mode ?? "chat";
      pendingWorkspace = c.workspace ?? defaultWorkspace ?? "";
      workspaceError = null;
    }
  });
  // keyed on the URL itself, NOT on activeId: our own actions set activeId
  // first and navigate after, which must never be mistaken for external nav
  // (that misread aborted fresh streams and wiped the session).
  let appliedSession: string | null | undefined = undefined;
  $effect(() => {
    if (!ready) return;
    const sid = sessionId;
    if (sid === appliedSession) return;
    appliedSession = sid;
    if (sid === activeId) return;
    aborter?.abort();
    aborter = null;
    streaming = false;
    streamingChatId = null;
    streamingStartAt = null;
    editingIndex = null;
    editDraft = "";
    copiedKey = null;
    pendingMsgDelete = null;
    dropActiveDraft(sid);
    if (sid && chats.some((c) => c.id === sid)) {
      activeId = sid;
      isPinned = true;
      suppressReset = true;
    } else if (!sid) {
      activeId = null;
    } else {
      void goto("/");
    }
  });

  function deleteChat(id: string) {
    if (pendingAsk?.chatId === id) cancelPendingAsk("(the chat was deleted before answering)");
    if (workRunning && workChatId === id && workTurnId) void stopWorkAgentTurn(workTurnId).catch(() => {});
    aborter?.abort();
    aborter = null;
    streaming = false;
    if (streamingChatId === id) {
      streamingChatId = null;
      streamingStartAt = null;
    }
    if (workChatId === id) {
      workChatId = null;
      workRunning = false;
      workStartAt = null;
      workTurnId = null;
    }
    ctxMenu = null;
    pendingDeleteId = null;
    const remaining = chats.filter((c) => c.id !== id);
    chats = remaining;
    if (id === activeId) {
      activeId = null;
      save();
      void goto("/");
    } else {
      save();
    }
  }

  let pendingDeleteId = $state<string | null>(null);
  let pendingMsgDelete = $state<number | null>(null);
  let gotoOpen = $state(false);
  let gotoInput = $state("");
  let gotoError = $state<string | null>(null);
  let gotoSel = $state(0);
  let settingsOpen = $state(false);
  let settingsTab = $state<"general" | "models">("general");
  /** Vicuna CLI launcher status (desktop only, loaded when Settings opens) */
  let cliState = $state<CliStatus | null>(null);
  let cliOp = $state<null | "load" | "install" | "uninstall">(null);
  let cliError = $state<string | null>(null);
  /** last install/uninstall outcome line (e.g. where it landed, PATH note) */
  let cliNote = $state<string | null>(null);
  let runners = $state<RunningModel[]>([]);
  let runnersError = $state<string | null>(null);
  let modelOp = $state<string | null>(null);
  let pullName = $state("");
  let pulling = $state<string | null>(null);
  let pullStatus = $state("");
  let pullDone = $state(0);
  let pullTotal = $state(0);
  let pullRate = $state(0); // bytes/sec, EMA of completed deltas
  let pullLastAt = 0;
  let pullLastDone = 0;
  let pullLastDigest: string | null = null;
  let pullAborter: AbortController | null = null;
  let confirmDeleteModel = $state<string | null>(null);
  let browseView = $state<"installed" | "discover">("installed");
  let browseQuery = $state("");
  let browseCat = $state<"all" | CatalogCat | "small" | "mlx">("all");
  let variantPick = $state<Record<string, string>>({});
  // discover catalog: bundled list, replaced by a fetched override when the
  // user hits Fetch (persisted via store so it survives restarts)
  let catalog = $state<CatalogEntry[]>([...MODEL_CATALOG]);
  let catalogUpdated = $state(CATALOG_UPDATED);
  let fetching = $state<{ done: number; total: number } | null>(null);
  let catalogError = $state<string | null>(null);

  async function fetchCatalog() {
    if (fetching) return;
    catalogError = null;
    fetching = { done: 0, total: catalog.length };
    try {
      const over: CatalogOverride = await refreshCatalog(catalog, (done, total) => {
        fetching = { done, total };
      });
      catalog = over.entries;
      catalogUpdated = over.updated;
      saveCatalog(over);
    } catch (e: any) {
      catalogError = e?.message ?? String(e);
    } finally {
      fetching = null;
    }
  }

  async function openSettings() {
    settingsTab = "general";
    settingsOpen = true;
    void refreshCliStatus();
  }

  /** (Re)read the `vicuna` launcher status; silent unless it fails. */
  async function refreshCliStatus() {
    if (!isTauriRuntime() || cliOp) return;
    cliOp = "load";
    cliError = null;
    try {
      cliState = await cliStatus();
    } catch (e: any) {
      cliError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      cliOp = null;
    }
  }

  async function installCliFlow() {
    if (!isTauriRuntime() || cliOp) return;
    cliOp = "install";
    cliError = null;
    try {
      const res = await cliInstall();
      cliState = await cliStatus();
      cliNote = res.note || `Installed to ${res.location}.`;
    } catch (e: any) {
      cliError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      cliOp = null;
    }
  }

  async function uninstallCliFlow() {
    if (!isTauriRuntime() || cliOp) return;
    cliOp = "uninstall";
    cliError = null;
    try {
      const removed = await cliUninstall();
      cliState = await cliStatus();
      cliNote = `Removed ${removed}.`;
    } catch (e: any) {
      cliError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      cliOp = null;
    }
  }

  async function openModelsTab() {
    settingsTab = "models";
    confirmDeleteModel = null;
    await refreshModels();
    await refreshRunners();
    await refreshServerStatus();
  }

  let ollamaVersion: string | null = $state(null);
  let serverChecking = $state(false);
  let serverOp: null | "start" | "stop" = $state(null);
  let serverError: string | null = $state(null);

  async function refreshServerStatus() {
    if (serverChecking) return;
    serverChecking = true;
    serverError = null;
    try {
      ollamaVersion = await serverVersion();
    } catch (e: any) {
      ollamaVersion = null;
      serverError = e?.message ?? String(e);
    } finally {
      serverChecking = false;
    }
  }

  async function startServerFlow() {
    if (serverOp) return;
    serverOp = "start";
    serverError = null;
    try {
      ollamaVersion = await startOllamaServer();
      await refreshModels();
      await refreshRunners();
    } catch (e: any) {
      ollamaVersion = null;
      serverError = e?.message ?? String(e);
    } finally {
      serverOp = null;
    }
  }

  async function stopServerFlow() {
    if (serverOp) return;
    serverOp = "stop";
    serverError = null;
    try {
      await stopOllamaServer();
      ollamaVersion = null;
      await refreshModels();
      await refreshRunners();
    } catch (e: any) {
      serverError = e?.message ?? String(e);
    } finally {
      serverOp = null;
    }
  }

  async function refreshRunners() {
    runnersError = null;
    try {
      runners = await listRunning();
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
      runners = [];
    }
  }

  async function toggleModelLoaded(name: string, loaded: boolean) {
    if (modelOp || pulling) return;
    modelOp = name;
    runnersError = null;
    try {
      if (loaded) await unloadModelFromMemory(name);
      else await loadModelIntoMemory(name);
      await refreshRunners();
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
    } finally {
      modelOp = null;
    }
  }

  async function submitPull() {
    const name = pullName.trim();
    if (!name || pulling || modelOp) return;
    await submitPullName(name);
    pullName = "";
  }

  async function submitPullName(name: string) {
    if (!name || pulling || modelOp) return;
    pulling = name;
    pullStatus = "starting…";
    pullDone = 0;
    pullTotal = 0;
    pullRate = 0;
    pullLastAt = 0;
    pullLastDone = 0;
    pullLastDigest = null;
    runnersError = null;
    pullAborter = new AbortController();
    try {
      await pullModel(
        name,
        (p) => {
          pullStatus = p.status;
          if (typeof p.completed === "number") pullDone = p.completed;
          if (typeof p.total === "number") pullTotal = p.total;
          // Ollama reports per-layer completed/total with no rate, so derive
          // download speed from completed deltas (EMA over ~second windows).
          if (typeof p.completed === "number" && p.status.startsWith("pulling")) {
            const digest = p.digest ?? p.status;
            const now = performance.now();
            if (pullLastAt === 0 || digest !== pullLastDigest) {
              pullLastAt = now;
              pullLastDone = p.completed;
              pullLastDigest = digest;
            } else {
              const dt = (now - pullLastAt) / 1000;
              if (dt >= 0.2) {
                const delta = p.completed - pullLastDone;
                if (delta < 0) {
                  // new layer / restart: re-baseline without spiking
                  pullLastAt = now;
                  pullLastDone = p.completed;
                } else {
                  const inst = delta / dt;
                  pullRate = pullRate > 0 ? pullRate * 0.7 + inst * 0.3 : inst;
                  pullLastAt = now;
                  pullLastDone = p.completed;
                }
                pullLastDigest = digest;
              }
            }
          } else if (!p.status.startsWith("pulling")) {
            pullRate = 0;
          }
        },
        pullAborter.signal
      );
      pullName = "";
      await refreshModels();
      if (models.includes(name)) {
        selectedModel = name;
        save();
      } else if (models.length > 0 && !models.includes(selectedModel)) {
        selectedModel = models[0];
        save();
      }
      await refreshRunners();
    } catch (e: any) {
      if (e?.name !== "AbortError") runnersError = e?.message ?? String(e);
    } finally {
      pulling = null;
      pullRate = 0;
      pullAborter = null;
    }
  }

  function cancelPull() {
    pullAborter?.abort();
  }

  async function requestDeleteModel(name: string) {
    if (modelOp || pulling) return;
    if (confirmDeleteModel !== name) {
      confirmDeleteModel = name;
      return;
    }
    confirmDeleteModel = null;
    modelOp = name;
    runnersError = null;
    try {
      // a loaded model may refuse deletion: unload first, then delete
      if (runners.some((r) => r.name === name)) {
        try {
          await unloadModelFromMemory(name);
        } catch {
          // fall through: the delete below surfaces the real error
        }
      }
      await deleteModel(name);
      await refreshModels();
      await refreshRunners();
      if (!models.includes(selectedModel) && models.length > 0) {
        selectedModel = models[0];
        save();
      }
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
    } finally {
      modelOp = null;
    }
  }

  let theme = $state<"light" | "dark">(initialPrefs.theme);
  let accent = $state(initialPrefs.accent);
  let fontSize = $state(initialPrefs.fontSize);
  let stopWords = $state<string[]>([...initialPrefs.stopWords]);
  let stopWordsText = $state(stopWordsToText(initialPrefs.stopWords));
  let ollamaBase = $state(initialPrefs.ollamaBase);

  function stopWordsToText(list: string[]): string {
    return list.map((s) => s.replace(/\n/g, "\\n")).join("\n");
  }

  function textToStopWords(text: string): string[] {
    return text
      .split("\n")
      .map((line) => line.trim().replace(/\\n/g, "\n"))
      .filter((s) => s.length > 0);
  }

  const ACCENTS: Record<string, { label: string; light: { bg: string; hover: string; fg: string }; dark: { bg: string; hover: string; fg: string } }> = {
    orange: { label: "Orange", light: { bg: "#ef6c00", hover: "#d96000", fg: "#ffffff" }, dark: { bg: "#ef6c00", hover: "#d96000", fg: "#ffffff" } },
    black: { label: "Black", light: { bg: "#111111", hover: "#000000", fg: "#ffffff" }, dark: { bg: "#e8e8e8", hover: "#ffffff", fg: "#111111" } },
    blue: { label: "Blue", light: { bg: "#2563eb", hover: "#1d4ed8", fg: "#ffffff" }, dark: { bg: "#3b82f6", hover: "#2563eb", fg: "#ffffff" } },
    green: { label: "Green", light: { bg: "#16a34a", hover: "#15803d", fg: "#ffffff" }, dark: { bg: "#22c55e", hover: "#16a34a", fg: "#111111" } }
  };

  function applyAppearance() {
    const root = document.documentElement;
    root.dataset.theme = theme;
    const a = ACCENTS[accent] ?? ACCENTS.orange;
    const v = theme === "dark" ? a.dark : a.light;
    root.style.setProperty("--accent", v.bg);
    root.style.setProperty("--accent-hover", v.hover);
    root.style.setProperty("--accent-fg", v.fg);
    root.style.setProperty("--msg-font-size", `${fontSize}px`);
  }

  $effect(() => {
    theme;
    accent;
    fontSize;
    applyAppearance();
  });

  function setTheme(t: "light" | "dark") {
    theme = t;
    persistPrefs();
  }

  function setAccent(a: string) {
    if (!ACCENTS[a]) return;
    accent = a;
    persistPrefs();
  }

  function setFontSize(n: number) {
    fontSize = Math.min(24, Math.max(12, Math.round(n)));
    persistPrefs();
  }

  function setStopWords(text: string) {
    stopWordsText = text;
    stopWords = textToStopWords(text);
    persistPrefs();
  }

  function persistPrefs() {
    savePrefs({ theme, accent, fontSize, sidebarOpen, stopWords, ollamaBase, zoom: zoomLevel, defaultWorkspace, thinkLevel });
  }

  function setOllamaHost(text: string) {
    ollamaBase = text.trim();
    setOllamaBase(ollamaBase);
    persistPrefs();
    refreshModels();
  }

  function openGoto() {
    gotoInput = "";
    gotoError = null;
    gotoSel = 0;
    gotoOpen = true;
  }

  function closeGoto() {
    gotoOpen = false;
    gotoInput = "";
    gotoError = null;
    gotoSel = 0;
  }

  // search modal: filter sessions by title, message text, or id substring.
  // A pasted session id/link still resolves directly via gotoIdHit.
  const searchQuery = $derived(gotoInput.trim());
  const searchQueryLow = $derived(searchQuery.toLowerCase());
  const gotoIdHit = $derived.by((): { id: string; chat: Chat | null } | null => {
    const m = gotoInput.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
    if (!m) return null;
    const id = m[0].toLowerCase();
    return { id, chat: chats.find((c) => c.id.toLowerCase() === id) ?? null };
  });
  const searchResults = $derived.by((): Chat[] => {
    const q = searchQueryLow;
    if (!q) return chats.filter((c) => c.messages.length > 0).slice(0, 8);
    return chats.filter((c) => {
      if (c.messages.length === 0) return false;
      if (c.title.toLowerCase().includes(q)) return true;
      if (c.id.toLowerCase().includes(q)) return true;
      return c.messages.some((m) => m.content.toLowerCase().includes(q));
    }).slice(0, 20);
  });
  type SearchItem = { kind: "new" } | { kind: "chat"; chat: Chat };
  const searchItems = $derived.by((): SearchItem[] => {
    const items: SearchItem[] = [{ kind: "new" }];
    const seen = new Set<string>();
    // a pasted session link surfaces its session first, even though the raw
    // link text matches nothing
    if (gotoIdHit?.chat) {
      items.push({ kind: "chat", chat: gotoIdHit.chat });
      seen.add(gotoIdHit.chat.id);
    }
    for (const c of searchResults) {
      if (seen.has(c.id)) continue;
      items.push({ kind: "chat", chat: c });
    }
    return items;
  });

  // excerpt centered on the first content match (or the opening message)
  function searchSnippet(c: Chat, q: string): string {
    const firstUser = c.messages.find((m) => m.role === "user")?.content ?? c.messages[0]?.content ?? "";
    if (!q) return firstUser.slice(0, 140);
    const low = q.toLowerCase();
    const hit = c.messages.find((m) => m.content.toLowerCase().includes(low));
    const src = hit?.content ?? firstUser;
    const idx = src.toLowerCase().indexOf(low);
    if (idx < 0) return src.slice(0, 140);
    const start = Math.max(0, idx - 48);
    const end = Math.min(src.length, idx + q.length + 96);
    return (start > 0 ? "…" : "") + src.slice(start, end) + (end < src.length ? "…" : "");
  }

  function highlightHtml(text: string, q: string): string {
    const esc = (s: string) =>
      s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
    const base = esc(text);
    const needle = q.trim();
    if (!needle) return base;
    const pattern = esc(needle).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
    try {
      return base.replace(new RegExp(`(${pattern})`, "gi"), '<mark class="search-mark">$1</mark>');
    } catch {
      return base;
    }
  }

  function openSearchItem(it: SearchItem) {
    closeGoto();
    if (it.kind === "new") newChat();
    else selectChat(it.chat.id);
  }

  function submitSearch() {
    if (gotoIdHit && !gotoIdHit.chat) {
      gotoError = "No chat found for that session.";
      return;
    }
    const it = searchItems[Math.min(gotoSel, searchItems.length - 1)];
    if (!it) {
      gotoError = "No matching chats.";
      return;
    }
    openSearchItem(it);
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") {
      e.preventDefault();
      gotoSel = (gotoSel + 1) % searchItems.length;
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      gotoSel = (gotoSel - 1 + searchItems.length) % searchItems.length;
    } else if (e.key === "Enter") {
      e.preventDefault();
      submitSearch();
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeGoto();
    }
  }

  // typing restarts keyboard selection at the top; the selection-follow
  // effect below keeps the highlighted row visible while arrowing
  $effect(() => {
    if (!gotoOpen) return;
    gotoInput;
    gotoSel = 0;
  });
  $effect(() => {
    if (!gotoOpen) return;
    gotoSel;
    requestAnimationFrame(() => {
      document.querySelector("[data-testid=goto-dialog] .search-row.sel")?.scrollIntoView({ block: "nearest" });
    });
  });

  function requestDelete(id: string) {
    ctxMenu = null;
    pendingDeleteId = id;
  }

  function confirmDelete() {
    const id = pendingDeleteId;
    pendingDeleteId = null;
    if (id) deleteChat(id);
  }

  let ctxMenu = $state<{ chatId: string; x: number; y: number } | null>(null);
  let renamingId = $state<string | null>(null);
  let renameDraft = $state("");

  function openMenu(e: MouseEvent, id: string) {
    e.preventDefault();
    e.stopPropagation();
    renamingId = null;
    ctxMenu = {
      chatId: id,
      x: Math.min(e.clientX, window.innerWidth - 170),
      y: Math.min(e.clientY, window.innerHeight - 120)
    };
  }

  function copySessionLink(id: string) {
    ctxMenu = null;
    // desktop links use the app's URL scheme so they reopen Vicuna;
    // `tauri://…` URLs are webview-internal and dead outside the app
    void copyText(isTauriRuntime() ? `vicuna://s/${id}` : `${window.location.origin}/s/${id}`);
  }

  // deep links (`vicuna://s/<id>`, e.g. pasted into a browser, and
  // `vicuna://open?path=…` from the `vicuna` CLI launcher) land here via
  // the deep-link plugin; navigate once chats are loaded, stash until then
  let pendingDeepLinkId: string | null = null;
  let pendingDeepLinkPath: string | null = null;
  function extractSessionId(urls: string[]): string | null {
    for (const u of urls) {
      const path = u.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
      const m = path.match(/^\/?s\/([A-Za-z0-9-]+)\/?$/);
      if (m) return m[1];
    }
    return null;
  }
  function handleDeepLinkUrls(urls: string[] | null) {
    if (!urls?.length) return;
    const id = extractSessionId(urls);
    if (id) {
      if (ready && chats.some((c) => c.id === id)) {
        activeId = id;
        isPinned = true;
        void goto(`/s/${id}`);
      } else {
        // initApp flushes this after load; unknown ids are dropped there
        pendingDeepLinkId = id;
      }
      return;
    }
    const path = extractOpenPath(urls);
    if (!path) return;
    if (ready) void openWorkspacePath(path);
    else pendingDeepLinkPath = path;
  }

  /** Open a folder from the `vicuna` CLI (`vicuna://open?path=…`): validate
   *  it, make it the workspace, and raise a work session rooted there
   *  (reusing an empty one when there is one). Never disturbs other
   *  sessions' streams. */
  async function openWorkspacePath(raw: string) {
    const p = raw.trim();
    if (!p || !isTauriRuntime()) return;
    // links can arrive while hidden in the tray — come forward
    void focusAppWindow();
    let canon: string;
    try {
      canon = await canonicalWorkspace(p);
    } catch (e: any) {
      error = `Couldn't open "${p}": ${typeof e === "string" ? e : (e?.message ?? String(e))}`;
      return;
    }
    defaultWorkspace = canon;
    pendingWorkspace = canon;
    pendingMode = "work";
    persistPrefs();
    const existing = chats.find(
      (c) => (c.mode ?? "chat") === "work" && c.workspace === canon && c.messages.length === 0
    );
    if (existing) {
      activeId = existing.id;
      isPinned = true;
      error = null;
      workspaceError = null;
      void goto(`/s/${existing.id}`);
      return;
    }
    // like New: don't pile up empty drafts
    if (chats.some((c) => c.messages.length === 0)) {
      chats = chats.filter((c) => c.messages.length > 0);
    }
    const chat: Chat = {
      id: crypto.randomUUID(),
      title: workspaceBase(canon).slice(0, 40) || "New session",
      createdAt: Date.now(),
      messages: [],
      mode: "work",
      workspace: canon
    };
    chats = [chat, ...chats];
    activeId = chat.id;
    isPinned = true;
    error = null;
    workspaceError = null;
    save();
    void goto(`/s/${chat.id}`);
  }

  function startRename(id: string) {
    const c = chats.find((x) => x.id === id);
    if (!c) return;
    ctxMenu = null;
    renamingId = id;
    renameDraft = c.title;
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLInputElement>("[data-testid=rename-input]");
      if (el) {
        el.focus();
        el.select();
      }
    });
  }

  function commitRename() {
    if (renamingId == null) return;
    const text = renameDraft.trim();
    if (text) {
      const c = chats.find((x) => x.id === renamingId);
      if (c) {
        c.title = text;
        chats = [...chats];
        save();
      }
    }
    renamingId = null;
  }

  function cancelRename() {
    renamingId = null;
  }

  // an empty session you navigate away from without sending anything is
  // discarded instead of lingering in the list doing nothing. The composer
  // box is global and is never touched here.
  function dropActiveDraft(exceptId: string | null) {
    if (!activeId || activeId === exceptId) return;
    const c = chats.find((x) => x.id === activeId);
    if (c && c.messages.length === 0) {
      chats = chats.filter((x) => x.id !== activeId);
      save();
    }
  }

  function requestMsgDelete(i: number) {
    if (busy) return;
    ctxMenu = null;
    pendingMsgDelete = i;
  }

  function confirmMsgDelete() {
    const chat = chats.find((c) => c.id === activeId);
    const i = pendingMsgDelete;
    pendingMsgDelete = null;
    if (!chat || i == null || busy) return;
    const target = chat.messages[i];
    if (!target) return;
    // deleting a user message also removes the reply that follows it;
    // deleting a bot message also removes the user message above it
    let from = i;
    let count = 1;
    if (target.role === "user" && chat.messages[i + 1]?.role === "assistant") {
      count = 2;
    } else if (target.role === "assistant" && i > 0 && chat.messages[i - 1]?.role === "user") {
      from = i - 1;
      count = 2;
    }
    if (editingIndex != null && editingIndex >= from) cancelEdit();
    copiedKey = null;
    chat.messages = [...chat.messages.slice(0, from), ...chat.messages.slice(from + count)];
    chats = [...chats];
    save();
  }

  function toggleModel() {
    modelOpen = !modelOpen;
    if (modelOpen) modelQuery = "";
  }

  // reasoning effort, next to the model picker: string think levels are
  // per-model (gpt-oss low/med/high, qwen3.8 xhigh, deepseek max…), read
  // from /api/show. The pill only appears for models reporting levels;
  // boolean-only and non-thinking models keep today's think:true default.
  let thinkLevel = $state(initialPrefs.thinkLevel ?? "auto");
  let thinkLevels = $state<string[]>([]);
  let thinkDefault = $state<string | null>(null);
  let thinkCanDisable = $state(false);
  let effortOpen = $state(false);

  function capLevel(l: string): string {
    return l.length > 0 ? l[0].toUpperCase() + l.slice(1) : l;
  }

  async function refreshThinking() {
    const m = selectedModel;
    if (!m) {
      thinkLevels = [];
      thinkDefault = null;
      thinkCanDisable = false;
      return;
    }
    const info = await modelThinking(m).catch(() => null);
    if (selectedModel !== m) return; // switched mid-fetch: drop the stale result
    thinkLevels = info?.levels ?? [];
    thinkDefault = info?.default ?? null;
    thinkCanDisable = info?.canDisable ?? false;
  }

  // keep capabilities in step with the selected model
  $effect(() => {
    selectedModel;
    thinkLevels = [];
    thinkDefault = null;
    thinkCanDisable = false;
    effortOpen = false;
    void refreshThinking();
  });

  /** think value for a fresh send: explicit level/off when the current model
   *  supports it, otherwise today's default (thinking on). Continue and Work
   *  passes think:false explicitly and never reach this. */
  function effectiveThink(): boolean | string {
    if (thinkLevel !== "auto" && thinkLevel !== "off" && thinkLevels.includes(thinkLevel)) return thinkLevel;
    if (thinkLevel === "off" && thinkCanDisable) return false;
    return true;
  }

  /** what's actually in effect for the current model: the stored level only
   *  when supported, else Auto (the pill must never claim an unsupported
   *  level — e.g. High stored from gpt-oss while on qwen3.8, which has xhigh) */
  const shownThink = $derived(
    thinkLevel === "off"
      ? (thinkCanDisable ? "off" : "auto")
      : thinkLevel === "auto" || thinkLevels.includes(thinkLevel)
        ? thinkLevel
        : "auto"
  );

  function setThinkLevel(v: string) {
    thinkLevel = v;
    effortOpen = false;
    persistPrefs();
  }

  // ⌘N on macOS, Ctrl+N elsewhere (shown as the New Chat hover hint)
  const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.userAgent ?? "");

  const filteredModels = $derived(
    models.filter((m) => m.toLowerCase().includes(modelQuery.trim().toLowerCase()))
  );

  // manage tab: current chat's model first, then loaded models, then the rest
  const sortedManageModels = $derived.by(() => {
    const running = new Set(runners.map((r) => r.name));
    const rank = (m: string) => (m === selectedModel ? 0 : running.has(m) ? 1 : 2);
    return models.map((m, i) => ({ m, i })).sort((a, b) => rank(a.m) - rank(b.m) || a.i - b.i).map((x) => x.m);
  });

  // discover tab: curated registry catalog with client-side search + filters
  const BROWSE_CATS = ["all", "chat", "code", "reasoning", "vision", "embedding", "small", "mlx"] as const;
  /** variant tags ending in -mlx are Apple-silicon builds (see refresh-catalog.py) */
  function isMlxVariant(tag: string): boolean {
    return /-mlx$/i.test(tag);
  }
  const filteredCatalog = $derived.by(() => {
    const q = browseQuery.trim().toLowerCase();
    return catalog.filter((e) => {
      if (browseCat === "small") {
        if (Math.max(...e.variants.map((v) => v.sizeMB)) > 2560) return false;
      } else if (browseCat === "mlx") {
        if (!e.variants.some((v) => isMlxVariant(v.tag))) return false;
      } else if (browseCat !== "all" && !e.cats.includes(browseCat)) return false;
      if (!q) return true;
      return (
        e.name.toLowerCase().includes(q) ||
        e.blurb.toLowerCase().includes(q) ||
        e.cats.some((c) => c.includes(q)) ||
        e.variants.some((v) => v.tag.toLowerCase().includes(q))
      );
    });
  });

  function pickedVariant(entryName: string, fallback: string): string {
    return variantPick[entryName] ?? fallback;
  }

  // generic starter prompts (no tools required on the model's side)
  const SUGGESTIONS = [
    { title: "Explain a concept", prompt: "Explain black holes in simple terms", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></svg>' },
    { title: "Write code", prompt: "Write a Python function to sort a list", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>' },
    { title: "Draft a message", prompt: "Write a friendly follow-up email", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>' },
    { title: "Brainstorm ideas", prompt: "Give me ideas for a weekend project", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18h6"/><path d="M10 22h4"/><path d="M12 2a7 7 0 0 0-4 12.7c.6.5 1 1.4 1 2.3h6c0-.9.4-1.8 1-2.3A7 7 0 0 0 12 2z"/></svg>' },
    { title: "Summarize text", prompt: "Summarize this paragraph in two sentences:", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>' },
    { title: "What can you do?", prompt: "What can you do?", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7L4.4 10.6l5.7-1.9z"/></svg>' }
  ];

  // work starters: each one maps to a real Work-mode capability (list/read,
  // grep, exec, write, sessions) so the cards demo what the agent can do
  const WORK_SUGGESTIONS = [
    { title: "Explore this folder", prompt: "List the files here and explain what this project is", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>' },
    { title: "Find in files", prompt: "Search the workspace for TODO comments and group them by file", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>' },
    { title: "Run a command", prompt: "Run `git status` and summarize what changed", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>' },
    { title: "Write a file", prompt: "Create a README.md that explains this project", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="12" y1="18" x2="12" y2="12"/><line x1="9" y1="15" x2="15" y2="15"/></svg>' },
    { title: "Search past chats", prompt: "Search my past chats for this project and summarize what we did", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><polyline points="12 7 12 12 15.5 14"/></svg>' },
    { title: "What can you do?", prompt: "What can you do in Work mode?", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7L4.4 10.6l5.7-1.9z"/></svg>' }
  ];

  function useSuggestion(prompt: string) {
    input = prompt;
    requestAnimationFrame(() => {
      document.querySelector<HTMLTextAreaElement>("[data-testid=message-input]")?.focus();
    });
  }

  const forkInfo = $derived(activeChat?.forkedFrom ?? null);
  const forkNoteIndex = $derived(activeChat?.forkFromIndex ?? null);

  const grouped = $derived.by(() => {
    const now = new Date();
    const startOfToday = new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
    const startOfYesterday = startOfToday - 24 * 3600 * 1000;
    const weekAgo = Date.now() - 7 * 24 * 3600 * 1000;
    const today: Chat[] = [];
    const yesterday: Chat[] = [];
    const week: Chat[] = [];
    const older: Chat[] = [];
    for (const c of chats) {
      // empty drafts are never listed: a session appears only after the
      // first message is sent
      if (c.messages.length === 0) continue;
      if (c.createdAt >= startOfToday) today.push(c);
      else if (c.createdAt >= startOfYesterday) yesterday.push(c);
      else if (c.createdAt >= weekAgo) week.push(c);
      else older.push(c);
    }
    return { today, yesterday, week, older };
  });

  async function refreshModels() {
    try {
      const tags = await listModels();
      models = tags.map((t) => t.name);
      if (models.length > 0 && !models.includes(selectedModel)) selectedModel = models[0];
      error = null;
    } catch (err: any) {
      error = `Cannot reach Ollama at ${activeOllamaBase()}. Is \`ollama serve\` running?`;
    }
  }

  function scrollBottom() {
    // never yank the viewport while the user is selecting text: every
    // streamed token calls this, which would otherwise drag the selection
    // (and the view) out from under them mid-drag
    const sel = window.getSelection();
    if (sel && sel.rangeCount > 0 && !sel.isCollapsed && messagesEl?.contains(sel.anchorNode)) return;
    requestAnimationFrame(() => {
      if (!messagesEl || !isPinned) return;
      messagesEl.scrollTop = messagesEl.scrollHeight;
      // record the clamped value actually applied: a scroll event reporting
      // exactly this position is our own doing and must not unpin
      lastJumpTop = messagesEl.scrollTop;
    });
  }

  $effect(() => {
    activeChat?.messages.length;
    scrollBottom();
  });

  // The streaming reply re-renders `{@html renderMarkdown(...)}` on every
  // token, which replaces the answer DOM and collapses any selection inside
  // it. Capture the selection as character offsets before the update and
  // restore it after Svelte applies the new HTML, so selecting mid-stream
  // no longer "goes off on its own".
  type StreamSel = { cls: string; a: number; f: number };
  function textOffsetIn(root: Element, node: Node | null, nodeOffset: number): number | null {
    if (!node) return null;
    try {
      // length of the scope's text from its start up to the caret: correct
      // for text anchors, element anchors, and markup-nested carets alike
      const range = document.createRange();
      range.selectNodeContents(root);
      range.setEnd(node, nodeOffset);
      return range.toString().length;
    } catch {
      return null;
    }
  }
  function nodeForOffset(root: Element, offset: number): { node: Node; off: number } {
    const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    let cur: Node | null = walker.nextNode();
    let acc = 0;
    let last: Node = root;
    while (cur) {
      const len = cur.textContent?.length ?? 0;
      if (acc + len >= offset) return { node: cur, off: offset - acc };
      acc += len;
      last = cur;
      cur = walker.nextNode();
    }
    return { node: last, off: last.textContent?.length ?? 0 };
  }
  function captureStreamSel(index: number): StreamSel | null {
    const sel = window.getSelection();
    if (!sel || sel.rangeCount === 0 || sel.isCollapsed || !messagesEl) return null;
    const row = messagesEl.querySelector(`[data-msg-index="${index}"]`);
    if (!row || !row.contains(sel.anchorNode) || !row.contains(sel.focusNode)) return null;
    const ans = row.querySelector(".answer");
    const th = row.querySelector(".thinking-body");
    let scope: Element | null = null;
    if (ans && ans.contains(sel.anchorNode) && ans.contains(sel.focusNode)) scope = ans;
    else if (th && th.contains(sel.anchorNode) && th.contains(sel.focusNode)) scope = th;
    else return null;
    const a = textOffsetIn(scope, sel.anchorNode, sel.anchorOffset);
    const f = textOffsetIn(scope, sel.focusNode, sel.focusOffset);
    if (a == null || f == null) return null;
    return { cls: scope.className, a, f };
  }
  function restoreStreamSel(index: number, s: StreamSel) {
    if (!messagesEl) return;
    const row = messagesEl.querySelector(`[data-msg-index="${index}"]`);
    const scope = row?.querySelector(`.${CSS.escape(s.cls.split(" ")[0])}`);
    if (!scope) return;
    const total = scope.textContent?.length ?? 0;
    const a = Math.min(s.a, total);
    const f = Math.min(s.f, total);
    const pa = nodeForOffset(scope, a);
    const pf = nodeForOffset(scope, f);
    try {
      const sel = window.getSelection();
      if (!sel) return;
      sel.setBaseAndExtent(pa.node, pa.off, pf.node, pf.off);
    } catch {
      // selection restore is best-effort; a failed restore just leaves the
      // caret where the re-render put it
    }
  }

  // the composer stays one row tall by default and grows with its content
  // (rows={1} alone never expands, and resize:none blocks manual Drag to
  // grow — without this, multiline drafts scroll inside a 1-line box)
  function autosizeComposer() {
    const el = composerEl;
    if (!el) return;
    el.style.height = "auto";
    const lh = parseFloat(getComputedStyle(el).lineHeight) || 22;
    const max = Math.round(lh * 8);
    el.style.height = Math.min(el.scrollHeight, max) + "px";
    el.style.overflowY = el.scrollHeight > max ? "auto" : "hidden";
  }

  $effect(() => {
    input;
    autosizeComposer();
  });

  $effect(() => {
    if (!pendingDeleteId && pendingMsgDelete == null && !gotoOpen && !settingsOpen && !lightboxSrc && !pendingSaveConfirm) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        pendingDeleteId = null;
        pendingMsgDelete = null;
        closeGoto();
        settingsOpen = false;
        closeLightbox();
        pendingSaveConfirm = false;
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // press `/` anywhere (outside text fields and modals) to jump to the composer
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key !== "/" || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.isContentEditable)) return;
      if (gotoOpen || settingsOpen || pendingDeleteId || pendingMsgDelete != null || modelOpen || lightboxSrc || pendingSaveConfirm) return;
      e.preventDefault();
      composerEl?.focus();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // Cmd/Ctrl+N anywhere creates a new chat (preventDefault blocks the
  // browser's new-window default)
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "n") {
        e.preventDefault();
        newChat();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // Cmd/Ctrl+P anywhere opens the search popup (preventDefault blocks the
  // browser's print default). Skipped inside other modals so dialogs don't
  // stack on top of each other.
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "p") {
        if (settingsOpen || pendingDeleteId || pendingMsgDelete != null || pendingSaveConfirm || lightboxSrc || modelOpen) return;
        e.preventDefault();
        openGoto();
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // Cmd/Ctrl+B anywhere toggles the sidebar (preventDefault blocks the
  // browser's bookmarks-bar default)
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && !e.altKey && e.key.toLowerCase() === "b") {
        e.preventDefault();
        setSidebar(!sidebarOpen);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // delegated handler for code-block copy buttons rendered via {@html}
  const CODE_CHECK_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>';
  $effect(() => {
    const el = messagesEl;
    if (!el) return;
    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      const btn = t?.closest?.("[data-copy-code]") as HTMLElement | null;
      if (btn) {
        const code = btn.closest(".codeblock")?.querySelector("code");
        const txt = code?.textContent ?? "";
        if (!txt) return;
        void copyText(txt).then((ok) => {
          if (!ok) return;
          const orig = btn.innerHTML;
          btn.innerHTML = CODE_CHECK_SVG;
          btn.classList.add("ok");
          setTimeout(() => {
            btn.innerHTML = orig;
            btn.classList.remove("ok");
          }, 1200);
        });
        return;
      }
      // inline code (`...`): click copies, with a transient "Copied" tip
      const inl = t?.closest?.("code.inl") as HTMLElement | null;
      if (!inl || inl.closest("pre")) return;
      e.preventDefault();
      const txt = inl.textContent ?? "";
      if (!txt) return;
      void copyText(txt).then((ok) => {
        if (!ok) return;
        inl.classList.add("copied");
        setTimeout(() => inl.classList.remove("copied"), 1200);
      });
    };
    el.addEventListener("click", onClick);
    return () => el.removeEventListener("click", onClick);
  });

  async function copyText(t: string): Promise<boolean> {
    try {
      await navigator.clipboard.writeText(t);
      return true;
    } catch {
      try {
        const ta = document.createElement("textarea");
        ta.value = t;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        const ok = document.execCommand("copy");
        ta.remove();
        return ok;
      } catch {
        return false;
      }
    }
  }

  function copyMessage(i: number) {
    const m = activeChat?.messages[i];
    if (!m) return;
    const key = `${activeId}:${i}`;
    void copyText(m.content).then((ok) => {
      if (ok) {
        copiedKey = key;
        setTimeout(() => {
          if (copiedKey === key) copiedKey = null;
        }, 1200);
      } else {
        error = "Copy failed (clipboard unavailable)";
      }
    });
  }

  function startEdit(i: number) {
    const m = activeChat?.messages[i];
    if (!m || busy) return;
    editingIndex = i;
    editDraft = m.content;
    editImages = [...(m.images ?? [])];
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLTextAreaElement>("[data-testid=edit-input]");
      if (el) {
        el.focus();
        el.setSelectionRange(el.value.length, el.value.length);
      }
    });
  }

  function cancelEdit() {
    editingIndex = null;
    editDraft = "";
    editImages = [];
    pendingSaveConfirm = false;
  }

  // saving an edit drops everything after the message. Saving the last
  // message is lossless; saving anywhere earlier asks first, and offers a
  // fork so the original thread (and its progress) survives the edit.
  let pendingSaveConfirm = $state(false);
  function saveOrConfirm() {
    const c = activeChat;
    if (!c || editingIndex == null || streaming) return;
    if (editingIndex >= c.messages.length - 1) {
      void saveEdit();
    } else {
      pendingSaveConfirm = true;
    }
  }
  function cancelSaveConfirm() {
    pendingSaveConfirm = false;
  }
  function confirmSaveHere() {
    pendingSaveConfirm = false;
    void saveEdit();
  }
  async function forkAndSave() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || editingIndex == null || streaming) return;
    const i = editingIndex;
    const draft = editDraft;
    const imgs = [...editImages];
    forkAt(i);
    // forkAt clears the edit state; restore the pending edits onto the fork
    editingIndex = i;
    editDraft = draft;
    editImages = imgs;
    await saveEdit();
  }

  // fullscreen image viewer (composer thumbnails + in-thread pictures)
  let lightboxSrc = $state<string | null>(null);
  function openLightbox(src: string) {
    lightboxSrc = src;
  }
  function closeLightbox() {
    lightboxSrc = null;
  }

  async function saveEdit() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || editingIndex == null || streaming || workRunning) return;
    const text = editDraft.trim();
    const images = editImages.length ? [...editImages] : undefined;
    if (!text && !images) return;
    const target = chat.messages[editingIndex];
    if (!target) {
      cancelEdit();
      return;
    }
    if (target.role === "assistant") {
      // editing a reply: replace it in place and drop everything after,
      // no regeneration (use Continue to extend it)
      chat.messages = [
        ...chat.messages.slice(0, editingIndex),
        { ...target, content: text, images }
      ];
      chats = [...chats];
      cancelEdit();
      isPinned = true;
      save();
      scrollBottom();
      return;
    }
    // editing own message: replace it, drop everything after, regenerate
    chat.messages = [...chat.messages.slice(0, editingIndex), { role: "user", content: text, ...(images ? { images } : {}) }];
    if (text && (chat.title === "New session" || chat.title === "New Session")) {
      chat.title = text.slice(0, 32) + (text.length > 32 ? ".." : "");
    }
    cancelEdit();
    if ((chat.mode ?? "chat") === "work") {
      // work threads re-run the agent, not a plain reply (mirrors regenerate)
      if (!chat.workspace?.trim()) {
        error = "Work mode needs a workspace folder — pick one with the folder pill, then regenerate.";
        return;
      }
      chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
      chats = [...chats];
      await runWorkTurn(chat, chat.messages.length - 1, chat.workspace);
      return;
    }
    const history: ChatMsg[] = [
      ...compactedHistory(chat.messages)
    ];
    chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    await streamInto(chat, chat.messages.length - 1, history, true);
  }

  function onEditKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      cancelEdit();
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      saveOrConfirm();
    }
  }

  function forkAt(i: number) {
    const c = chats.find((x) => x.id === activeId);
    if (!c || busy) return;
    aborter?.abort();
    aborter = null;
    const fork: Chat = {
      id: crypto.randomUUID(),
      title: `${c.title} (fork)`,
      createdAt: Date.now(),
      messages: JSON.parse(JSON.stringify(c.messages.slice(0, i + 1))),
      forkedFrom: { id: c.id, title: c.title },
      forkFromIndex: i,
      mode: c.mode ?? "chat",
      workspace: c.workspace ?? null
    };
    chats = [fork, ...chats];
    activeId = fork.id;
    // keep the starter toggle consistent with the fork (matters for empty
    // forks; non-empty threads render from their own mode anyway)
    pendingMode = fork.mode ?? "chat";
    if (fork.workspace) pendingWorkspace = fork.workspace;
    isPinned = true;
    editingIndex = null;
    editDraft = "";
    editImages = [];
    pendingSaveConfirm = false;
    save();
    void goto(`/s/${fork.id}`);
  }

  async function streamInto(chat: Chat, index: number, history: ChatMsg[], removeIfEmpty: boolean, opts: { think?: boolean | string; temperature?: number; generatePrompt?: string; stop?: string[]; mode?: "fresh" | "continue"; tools?: import("$lib/ollama").OllamaToolDef[]; onImagesStripped?: () => void; pinFollow?: boolean } = {}): Promise<{ text: string; toolCalls: import("$lib/ollama").OllamaToolCall[] }> {
    streaming = true;
    streamingChatId = chat.id;
    streamingStartAt = Date.now();
    const ctrl = new AbortController();
    aborter = ctrl;
    let produced = false;
    let generatedText = "";
    // follow the new output — unless this is a mid-turn work round or the
    // wrap-up generation: re-pinning there would yank a user who scrolled
    // up to read earlier tool output, on every single round
    if (opts.pinFollow ?? true) {
      isPinned = true;
    }
    // fresh sends with no explicit think use the effort picker (model default
    // when "auto"); continue/work pass think:false and bypass it
    const think = opts.think ?? effectiveThink();
    // mark the in-flight stream synchronously: a refresh before settle
    // resumes from the persisted partial instead of losing the turn
    writePendingStream({
      chatId: chat.id,
      index,
      mode: opts.mode ?? "fresh",
      model: selectedModel,
      think,
      temperature: opts.temperature,
      stop: opts.stop
    });
    save();
    scrollBottom();
    const wallStart = Date.now();
    let runEvalCount = 0;
    let runEvalDurationNs = 0;
    let runToolCalls: import("$lib/ollama").OllamaToolCall[] = [];
    // token batching: cloud models burst dozens of tokens per frame, and each
    // state write re-renders + re-highlights the whole message — with multi-KB
    // tool outputs in the transcript that cost grows per token until the UI
    // stalls. Buffer ~120ms so renders stay flat regardless of burst rate.
    let pendingTok = "";
    let pendingTh = "";
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    function flushTokens() {
      flushTimer = null;
      if (!pendingTok && !pendingTh) return;
      const tok = pendingTok;
      const th = pendingTh;
      pendingTok = "";
      pendingTh = "";
      const keep = captureStreamSel(index);
      const c = chats.find((x) => x.id === chat.id);
      if (c && c.messages[index]) {
        const m = c.messages[index];
        c.messages[index] = { role: "assistant", content: m.content + tok, thinking: (m.thinking ?? "") + th, thinkingOpen: m.thinkingOpen, model: m.model ?? selectedModel, genMs: m.genMs, genTps: m.genTps };
        chats = [...chats];
      }
      if (keep) void tick().then(() => restoreStreamSel(index, keep));
      scrollBottom();
      saveSoon();
    }
    function scheduleFlush() {
      if (flushTimer) return;
      flushTimer = setTimeout(flushTokens, 120);
    }
    // Ollama only ships tool-call args once complete, so a work round goes
    // silent while the model generates them. Surface that instead of a
    // dead-looking pause. Armed after the first token: before that, a slow
    // first token is just latency, not a call.
    let stallTimer: ReturnType<typeof setTimeout> | null = null;
    function clearStall() {
      if (stallTimer) {
        clearTimeout(stallTimer);
        stallTimer = null;
      }
    }
    function dropStallHint() {
      if (toolStatus?.chatId === chat.id && toolStatus.label === "Preparing tool call…") toolStatus = null;
    }
    function armStall() {
      clearStall();
      if (!opts.tools) return;
      stallTimer = setTimeout(() => {
        stallTimer = null;
        if (!pendingTok && !pendingTh && !toolStatus) {
          toolStatus = { chatId: chat.id, label: "Preparing tool call…" };
        }
      }, 1000);
    }
    try {
      const onTok = (tok: string) => {
        produced = true;
        generatedText += tok;
        pendingTok += tok;
        scheduleFlush();
        dropStallHint();
        armStall();
      };
      const onTh = (tok: string) => {
        produced = true;
        pendingTh += tok;
        scheduleFlush();
        dropStallHint();
        armStall();
      };
      if (opts.generatePrompt !== undefined) {
        await generateStream(selectedModel, opts.generatePrompt, onTok, ctrl.signal, undefined, 0, { temperature: opts.temperature, stop: opts.stop });
      } else {
        const res = await chatStream(selectedModel, history, onTok, ctrl.signal, onTh, 0, { think, temperature: opts.temperature, stop: opts.stop, tools: opts.tools, onImagesStripped: opts.onImagesStripped ?? noteImagesStripped });
        runEvalCount = res.evalCount;
        runEvalDurationNs = res.evalDurationNs;
        runToolCalls = res.toolCalls ?? [];
      }
    } catch (err: any) {
      if (err?.name === "AbortError") {
        // keep partial
      } else {
        error = err?.message ?? String(err);
        const c = chats.find((x) => x.id === chat.id);
        if (c && removeIfEmpty && !produced) {
          c.messages = c.messages.filter((_, j) => j !== index);
          chats = [...chats];
        }
      }
    } finally {
      streaming = false;
      if (streamingChatId === chat.id) {
        streamingChatId = null;
        streamingStartAt = null;
      }
      aborter = null;
      clearStall();
      dropStallHint();
      // drop any scheduled flush and push the tail synchronously: the final
      // content must be in state before stats/resume logic runs below
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      flushTokens();
      // record generation stats: wall time always, tok/s from the server's
      // eval counters when present (cloud APIs omit eval_duration — then
      // fall back to eval tokens over wall time so they still get a rate)
      const wallMs = Date.now() - wallStart;
      const c = chats.find((x) => x.id === chat.id);
      if (c && c.messages[index] && c.messages[index].role === "assistant") {
        const m = c.messages[index];
        const tps =
          runEvalDurationNs > 0 && runEvalCount > 0
            ? runEvalCount / (runEvalDurationNs / 1e9)
            : runEvalCount > 0 && wallMs > 0
              ? runEvalCount / (wallMs / 1000)
              : m.genTps;
        c.messages[index] = {
          ...m,
          genMs: (m.genMs ?? 0) + wallMs,
          genTps: tps
        };
        chats = [...chats];
      }
      // user stop / natural settle clears the marker; a refresh/close
      // abort keeps it so the next load resumes the turn
      if (!unloading) clearPendingStream();
      save();
      scrollBottom();
      return { text: generatedText, toolCalls: runToolCalls };
    }
  }

  async function continueFrom(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || busy) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    error = null;
    // Continue via /api/chat: resend the history ending with the partial
    // reply as-is so the model carries on exactly where it stopped instead
    // of starting a fresh turn. No re-thinking and low temperature: no
    // fresh plans, no wandering. Custom stop words end the generation if
    // it tries to start a new turn ("User:" etc.).
    const history: ChatMsg[] = compactedHistory(chat.messages.slice(0, i + 1));
    continuing = true;
    try {
      await streamInto(chat, i, history, false, { think: false, temperature: 0.2, stop: stopWords, mode: "continue" });
    } finally {
      continuing = false;
    }
  }

  async function regenerate(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || busy) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    error = null;
    cancelEdit();
    // drop the old reply and generate fresh with the currently selected model
    chat.messages = [...chat.messages.slice(0, i)];
    const freshHistory: ChatMsg[] = compactedHistory(chat.messages);
    chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    chats = [...chats];
    if ((chat.mode ?? "chat") === "work") {
      // work threads re-run the agent, not a plain reply — and stopping the
      // agent mid-run still leaves the (empty) placeholder resumable
      if (!chat.workspace?.trim()) {
        error = "Work mode needs a workspace folder — pick one with the folder pill, then regenerate.";
        return;
      }
      await runWorkTurn(chat, chat.messages.length - 1, chat.workspace);
      return;
    }
    await streamInto(chat, chat.messages.length - 1, freshHistory, true);
  }

  /** /compact: append a flagged summary of the model-visible transcript.
   *  The UI keeps every message; only future model context is trimmed. */
  async function compactSession() {
    const chat = chats.find((c) => c.id === activeId);
    // command feedback (not a failure): the global error auto-clear picks it
    // up with a short fuse — real inference errors below get the longer one
    const flashNotice = (text: string) => {
      errorCfg.ttl = 6000;
      error = text;
    };
    if (!chat) {
      flashNotice("/compact needs a Work session — open one first.");
      return;
    }
    if ((chat.mode ?? "chat") !== "work") {
      flashNotice("/compact is only available in Work sessions.");
      return;
    }
    if (busy) return;
    if (!chat.messages.length) {
      flashNotice("Nothing to compact — the session has no messages yet.");
      return;
    }
    // model-visible transcript (drops anything already compacted): the UI
    // keeps every message, only the model's context is trimmed
    const visible = compactedHistory(chat.messages);
    if (visible.length <= 1 && chat.messages.some((m) => m.summary)) {
      flashNotice("Already compacted — send more messages first.");
      return;
    }
    const startLen = chat.messages.length;
    const transcript = visible.map((h) => {
      const who = h.role === "user" ? "User" : h.role === "assistant" ? "Assistant" : h.role === "tool" ? "Tool" : "System";
      const imgs = h.images?.length ? ` [${h.images.length} image(s) attached]` : "";
      return `${who}: ${h.content}${imgs}`;
    }).join("\n\n");
    compacting = true;
    error = null;
    toolStatus = { chatId: chat.id, label: "Compacting context…" };
    let summary = "";
    try {
      const res = await chatStream(
        selectedModel,
        [
          { role: "system", content: "Summarize this conversation transcript concisely: files listed/read/written, commands run and their key output, decisions made, and what remains to do. Keep file paths and facts; drop chatter." },
          { role: "user", content: transcript }
        ],
        () => {},
        undefined,
        undefined,
        0,
        { think: false, temperature: 0.2 }
      );
      summary = res.text;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      compacting = false;
      if (toolStatus?.chatId === chat.id) toolStatus = null;
    }
    if (!summary.trim()) return;
    const c = chats.find((x) => x.id === chat.id);
    // messages moved under us (edit/delete raced the summary): bail instead
    // of flagging a shifted transcript
    if (!c || c.messages.length !== startLen) {
      flashNotice("Session changed during compaction — try again.");
      return;
    }
    // visible divider + summary for the user; the flag is what trims the
    // model's context (compactedHistory drops everything up to it). Any
    // older summary loses its flag so only the latest boundary counts.
    for (const m of c.messages) {
      if (m.summary) m.summary = undefined;
    }
    c.messages = [
      ...c.messages,
      { role: "assistant", content: summary.trim(), model: selectedModel, summary: true }
    ];
    chats = [...chats];
    save();
    scrollBottom();
  }

  async function send() {
    const text = input.trim();
    if (!text && attachImages.length === 0 && attachFiles.length === 0) return;
    if (compacting) return;
    if (streaming || workRunning) {
      // work turns stay interactive: a prompt typed mid-run queues for the
      // next round instead of being dropped or starting a parallel turn
      const running = chats.find((c) => c.id === workChatId);
      if (workRunning && workChatId === activeId && (running?.mode ?? "chat") === "work") {
        if (attachFiles.length) {
          error = "Wait for the turn to finish before sending files.";
          return;
        }
        queuePrompt(text, attachImages.length ? [...attachImages] : undefined);
      }
      return;
    }
    const sentFromId = activeId;
    let chat = chats.find((c) => c.id === activeId);
    let freshChat = false;
    const images = attachImages.length ? [...attachImages] : undefined;
    // dropped files are staged per session *before* the user message is
    // built, so the message can cite where they landed
    const stagedId = chat?.id ?? crypto.randomUUID();
    let uploadNote = "";
    let firstUploadName: string | null = null;
    if (attachFiles.length > 0) {
      if (attachFiles.length > 20) {
        error = `Too many attached files (max 20, got ${attachFiles.length}) — remove some and resend.`;
        return;
      }
      try {
        const staged: string[] = [];
        for (const a of attachFiles) {
          if (a.file.size > 16 * 1024 * 1024) throw new Error(`"${a.name}" is over 16MB.`);
          staged.push(await stageUploadFile(stagedId, a.name, await fileToBase64(a.file)));
        }
        const dir = staged[0].replace(/[/\\][^/\\]*$/, "");
        const names = staged.map((s) => ` - ${s.slice(dir.length).replace(/^[/\\]/, "")}`);
        firstUploadName = attachFiles[0].name;
        uploadNote = `\n\n[Attached files saved to ${dir} (use the read_upload tool with the file NAME to read them):\n${names.join("\n")}]`;
      } catch (e: any) {
        error = typeof e === "string" ? e : (e?.message ?? String(e));
        return;
      }
    }
    const fullText = text + uploadNote;
    // effective mode: a pre-existing thread keeps its mode (including an
    // untouched empty draft created with an explicit mode, e.g. via the
    // `vicuna` CLI); only a home send falls back to the starter toggle
    const effectiveMode: "chat" | "work" = chat
      ? (chat.mode ?? (chat.messages.length === 0 ? pendingMode : "chat"))
      : pendingMode;
    let workRoot: string | null = null;
    if (effectiveMode === "work") {
      // workspace required before the session begins. Always canonicalized
      // through the backend (Tauri or bridge) so a stale/missing folder
      // fails fast here instead of per-tool mid-turn.
      const rawWs = chat && chat.messages.length > 0 ? (chat.workspace ?? pendingWorkspace) : pendingWorkspace;
      if (!rawWs?.trim()) {
        workspaceError = "Pick a workspace folder first.";
        error = "Work mode needs a workspace folder — set one above, then send.";
        return;
      }
      try {
        // desktop canonicalizes (must exist); plain browsers validate through
        // the bridge (same check, server-side). Without either backend the
        // call below throws and the turn never starts.
        workRoot = await canonicalWorkspace(rawWs);
      } catch (e: any) {
        const msg = typeof e === "string" ? e : (e?.message ?? String(e));
        workspaceError = msg;
        error = `Workspace invalid: ${msg}`;
        return;
      }
      workspaceError = null;
      defaultWorkspace = workRoot;
      pendingWorkspace = workRoot;
      persistPrefs();
    }
    if (!chat) {
      // sending from home (or any stateless view): create the session inline.
      // navigation is deferred until the stream finishes, otherwise the
      // page unmount would abort the just-started request.
      // NOTE: the session must enter reactive state COMPLETE (both messages).
      // Svelte 5 caches proxied reads, so mutating the raw object after
      // prepending stays invisible until the first streamed token writes
      // through the proxy — the composer would look stuck on empty.
      const titleSrc = text || (images ? "Image" : (firstUploadName ?? "New session"));
      chat = {
        id: stagedId,
        title: titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : ""),
        createdAt: Date.now(),
        messages: [
          { role: "user", content: fullText, ...(images ? { images } : {}) },
          { role: "assistant", content: "", thinking: "", model: selectedModel }
        ],
        mode: effectiveMode,
        workspace: effectiveMode === "work" ? workRoot : null
      };
      chats = [chat, ...chats];
      activeId = chat.id;
      isPinned = true;
      freshChat = true;
      save();
    } else {
      if (chat.messages.length === 0) {
        chat.mode = effectiveMode;
        if (effectiveMode === "work") chat.workspace = workRoot;
      }
      chat.messages = [...chat.messages, { role: "user", content: fullText, ...(images ? { images } : {}) }];
      if (chat.title === "New session" || chat.title === "New Session") {
        const titleSrc = text || (images ? "Image" : (firstUploadName ?? "New session"));
        chat.title = titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : "");
      }
      chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    }
    input = "";
    mention = null;
    clearDraft(null);
    attachImages = [];
    attachFiles = [];
    error = null;

    if ((chat.mode ?? "chat") === "work" && chat.workspace) {
      await runWorkTurn(chat, chat.messages.length - 1, chat.workspace);
    } else {
      // history excludes the trailing placeholder being generated
      const history: ChatMsg[] = compactedHistory(chat.messages.slice(0, -1));
      await streamInto(chat, chat.messages.length - 1, history, true);
    }
    if (freshChat && activeId === chat.id) {
      // user stayed put: now reflect the session in the URL
      void goto(`/s/${chat.id}`);
    }
  }

  function queuePrompt(text: string, images?: string[]) {
    const chatId = activeId;
    if (!chatId || (!text && !images?.length)) return;
    const id = crypto.randomUUID();
    const msg: ChatMsg = { role: "user", content: text, ...(images ? { images } : {}) };
    queuedMsgs = [...queuedMsgs, { chatId, id, msg }];
    if (workTurnId) void queueWorkAgentMsg(workTurnId, id, msg).catch(() => {});
    input = "";
    clearDraft(null);
    attachImages = [];
    error = null;
  }

  /** drain the queued prompts for a chat (they are consumed at once) */
  function takeQueued(chatId: string): ChatMsg[] {
    const taken = queuedMsgs.filter((q) => q.chatId === chatId);
    if (!taken.length) return [];
    queuedMsgs = queuedMsgs.filter((q) => q.chatId !== chatId);
    return taken.map((q) => q.msg);
  }

  /** drop backend-confirmed steering messages (QueueDrained): they have
   *  bubbles now, so only the unconfirmed tail stays queued */
  function confirmQueuedDrained(ids: string[]) {
    if (!ids.length) return;
    const gone = new Set(ids);
    queuedMsgs = queuedMsgs.filter((q) => !gone.has(q.id));
  }

  function unqueue(index: number) {
    const removed = queuedMsgs.filter((_, i) => i === index);
    queuedMsgs = queuedMsgs.filter((_, i) => i !== index);
    for (const q of removed) {
      if (workTurnId) void removeWorkAgentMsg(workTurnId, q.id).catch(() => {});
    }
  }

  function formatSize(n: number): string {
    if (n < 1024) return `${n}B`;
    if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)}KB`;
    return `${(n / (1024 * 1024)).toFixed(1)}MB`;
  }

  /** (re)load the explorer tree for the current work root */
  async function loadExplorer() {
    explorerEntries = [];
    explorerError = null;
    const root = explorerRoot.trim();
    // "/" is never a valid workspace (backend refuses the filesystem root:
    // crawling it hits macOS TCC-protected spots on every refresh).
    if (!root || root === "/") {
      explorerError = "Pick a workspace folder first.";
      return;
    }
    explorerLoading = true;
    try {
      explorerEntries = await listWorkTree(root);
    } catch (e: any) {
      explorerError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      explorerLoading = false;
    }
  }

  function toggleExplorer() {
    explorerOpen = !explorerOpen;
    if (explorerOpen) {
      lastExplorerRoot = explorerRoot.trim();
      void loadExplorer();
      // the aside was display:none: xterm needs a re-fit now it has size
      void tick().then(() => fitSideTerm(activeSideTab));
    }
  }

  function toggleExplorerDir(path: string) {
    collapsedDirs = collapsedDirs.includes(path)
      ? collapsedDirs.filter((d) => d !== path)
      : [...collapsedDirs, path];
  }

  /** clicking a file drops an @path reference into the composer */
  function insertExplorerPath(path: string) {
    input = input ? `${input.trimEnd()} @${path} ` : `@${path} `;
    requestAnimationFrame(() => composerEl?.focus());
  }

  function escapeHtmlText(s: string): string {
    return s
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;");
  }

  function escapeHtmlAttr(s: string): string {
    return escapeHtmlText(s).replace(/"/g, "&quot;").replace(/'/g, "&#39;");
  }

  /** join an upload dir and a relative file name with the dir's separator */
  function joinUploadPath(dir: string, name: string): string {
    const sep = dir.includes("\\") && !dir.includes("/") ? "\\" : "/";
    return dir.replace(/[\\/]+$/, "") + sep + name;
  }

  interface MentionToken {
    start: number;
    end: number;
    core: string;
  }

  /** @path tokens in text: @ + non-space run at start/after whitespace (so
   *  a@b.com stays plain), trailing [.,;:!?)] excluded. Shared by rendering,
   *  caret snapping, and unit deletion so all three always agree. */
  function findMentions(text: string): MentionToken[] {
    const out: MentionToken[] = [];
    const re = /(^|[\s(])(@[^\s]+)/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(text)) !== null) {
      const tok: string = m[2];
      const tail = tok.match(/[.,;:!?)]+$/);
      const core = tail ? tok.slice(0, -tail[0].length) : tok;
      if (core.length <= 1) continue;
      const start = m.index + m[1].length;
      out.push({ start, end: start + core.length, core });
    }
    return out;
  }

  const mentionFolderIcon = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>';
  const mentionFileIcon = '<svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M13 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9z"/><polyline points="13 2 13 9 20 9"/></svg>';

  function mentionIconFor(core: string): string {
    const base = core.slice(1).split(/[/\\]/).pop() ?? "";
    return base.includes(".") ? mentionFileIcon : mentionFolderIcon;
  }

  /** render @path mentions as green pills (ws-badge style) with a folder/file
   *  icon. Scans the escaped text (entities contain no @/space/paren, so
   *  boundaries match the raw text). Padding/margin cancel so the composer
   *  backdrop aligns exactly with the transparent textarea text. */
  function renderMentions(text: string): string {
    const esc = escapeHtmlText(text);
    const toks = findMentions(esc);
    if (!toks.length) return esc;
    let out = "";
    let last = 0;
    for (const t of toks) {
      out += esc.slice(last, t.start);
      out += `<span class="mention-pill">${mentionIconFor(t.core)}<span>${esc.slice(t.start, t.end)}</span></span>`;
      last = t.end;
    }
    return out + esc.slice(last);
  }

  /** user bubbles: @-mentions become pills, and attached-files notes collapse
   *  to a clickable file list at the top of the bubble (raw text keeps the
   *  full note so the agent still sees paths + tool instructions in history) */
  function renderUserMessage(text: string): string {
    const re = /\[Attached files saved to ([^\]]*?) \(use the read_upload tool[^)]*\):\r?\n((?:[ \t]*-[ \t]+[^\r\n]*\r?\n?)+)\]/g;
    const files: { name: string; path: string }[] = [];
    const body = text.replace(re, (_full, dir: string, list: string) => {
      for (const x of list.matchAll(/^[ \t]*-[ \t]*(.+?)[ \t]*$/gm)) {
        const name = x[1];
        if (name) files.push({ name, path: joinUploadPath(dir, name) });
      }
      return "";
    });
    if (!files.length) return renderMentions(text);
    const pills = files
      .map(
        (f) =>
          `<button type="button" class="mention-pill file-pill" data-reveal-path="${escapeHtmlAttr(f.path)}" title="Reveal in Finder">${mentionFileIcon}<span>${escapeHtmlText(f.name)}</span></button>`
      )
      .join("");
    const rest = renderMentions(body.replace(/^\s+/, "").replace(/\s+$/, ""));
    return `<div class="bubble-files">${pills}</div>${rest}`;
  }

  /** click a file pill in a user bubble → reveal it in Finder/Explorer */
  async function onMessageClick(e: MouseEvent) {
    const el = (e.target as HTMLElement | null)?.closest?.("[data-reveal-path]") as HTMLElement | null;
    if (!el || !isTauriRuntime()) return;
    const path = el.getAttribute("data-reveal-path");
    if (!path) return;
    try {
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(path);
    } catch (err: any) {
      error = typeof err === "string" ? err : (err?.message ?? String(err));
    }
  }

  // delegated (the pills are injected via {@html}); attached natively so the
  // log container stays a plain non-interactive element
  $effect(() => {
    const el = messagesEl;
    if (!el) return;
    const handler = (e: MouseEvent) => void onMessageClick(e);
    el.addEventListener("click", handler);
    return () => el.removeEventListener("click", handler);
  });
  /** snap a collapsed caret out of a mention interior (arrows/clicks treat
   *  pills atomically: the caret rests before or after, never inside) */
  function snapCaretOut(caret: number, dir: -1 | 0 | 1): number {
    for (const t of findMentions(input)) {
      if (caret > t.start && caret < t.end) {
        if (dir !== 0) return dir < 0 ? t.start : t.end;
        return caret - t.start <= t.end - caret ? t.start : t.end;
      }
    }
    return caret;
  }

  function snapComposerCaret() {
    const el = composerEl;
    if (!el || el.selectionStart !== el.selectionEnd) return;
    const caret = el.selectionStart ?? input.length;
    const snapped = snapCaretOut(caret, 0);
    if (snapped !== caret) {
      requestAnimationFrame(() => {
        composerEl?.setSelectionRange(snapped, snapped);
        syncBackdropScroll();
        refreshMentionMenu();
      });
    }
  }

  /** active @-mention query under the caret (work mode only) */
  function activeMentionQuery(): { start: number; query: string } | null {
    if (composerMode !== "work" || !explorerRoot.trim()) return null;
    const el = composerEl;
    const caret = el ? (el.selectionStart ?? input.length) : input.length;
    if (el && el.selectionStart !== el.selectionEnd) return null;
    // a caret strictly inside a completed mention is snapped out elsewhere,
    // so anything matching here is a live query being typed
    const m = input.slice(0, caret).match(/(^|[\s(])@([^\s]*)$/);
    if (!m) return null;
    return { start: caret - m[2].length - 1, query: m[2] };
  }

  /** @-mention popup state: anchor, query, ranked files, keyboard index */
  let mention = $state<{ start: number; query: string; items: WorkTreeEntry[]; index: number } | null>(null);
  /** caret arrived via an atomic arrow hop (not typing): unit-delete applies
   *  even with the popup open. Cleared by any other key or text input. */
  let caretHopped = false;
  /** keydown consumed the popup (select/close/nav): skip the follow-up keyup
   *  refresh, which would otherwise run on a stale caret and reopen it */
  let popupKeyHandled = false;

  /** recompute the mention popup from input + caret (input/click/keyup) */
  function refreshMentionMenu() {
    const q = activeMentionQuery();
    if (!q) {
      mention = null;
      return;
    }
    const needle = q.query.toLowerCase();
    const scored: { score: number; entry: WorkTreeEntry }[] = [];
    for (const e of explorerEntries) {
      const p = e.path.toLowerCase();
      if (needle && !p.includes(needle)) continue;
      const base = e.name.toLowerCase();
      const score = !needle
        ? (e.is_dir ? 1 : 0)
        : base.startsWith(needle)
          ? 0
          : base.includes(needle)
            ? 1
            : p.startsWith(needle)
              ? 2
              : 3;
      scored.push({ score, entry: e });
    }
    scored.sort((a, b) => a.score - b.score || a.entry.path.localeCompare(b.entry.path));
    const items = scored.slice(0, 8).map((s) => s.entry);
    const keepIndex = mention && mention.query === q.query ? mention.index : 0;
    mention = { start: q.start, query: q.query, items, index: items.length ? Math.min(keepIndex, items.length - 1) : 0 };
  }

  /** complete the active mention with a picked file */
  function pickMention(item: WorkTreeEntry) {
    const m = mention;
    mention = null;
    caretHopped = false;
    if (!m) return;
    const { text, caret } = completeMentionText(input, m.start, m.query, item.path);
    input = text;
    requestAnimationFrame(() => {
      composerEl?.setSelectionRange(caret, caret);
      composerEl?.focus();
      syncBackdropScroll();
    });
  }

  // files for the mention popup come from the explorer tree; fetch on demand
  // (the panel may never have been opened in this session)
  $effect(() => {
    if (mention && !explorerEntries.length && !explorerLoading && explorerRoot.trim() && explorerRoot.trim() !== "/" && isTauriRuntime()) {
      void loadExplorer();
    }
  });

  /** slash commands: like @-mentions but triggered by a leading "/" in an
   *  otherwise empty composer (typing "/usr/…" filters, anything else hides).
   *  Args ride along after the command word ("/goal ship it"). */
  const SLASH_COMMANDS = [
    { name: "compact", desc: "Summarize older messages into one note to free context" },
    { name: "goal", desc: "Set, show, pause, resume or clear the session objective", takesArgs: true }
  ];
  let slash = $state<{ query: string; items: { name: string; desc: string; takesArgs?: boolean }[]; index: number } | null>(null);

  /** recompute the slash menu from the input (input/click/keyup/blur) */
  function refreshSlashMenu() {
    // work-only: slash commands steer the agent, meaningless in chat
    if (composerMode !== "work" || !input.startsWith("/") || input.includes("\n") || streaming || workRunning || compacting) {
      slash = null;
      return;
    }
    // match on the head word so "/goal ship it" still offers /goal
    const q = input.slice(1).trim().toLowerCase();
    const head = q.split(/\s+/)[0] ?? "";
    const items = SLASH_COMMANDS.filter((c) => !head || c.name.startsWith(head));
    slash = items.length ? { query: q, items, index: 0 } : null;
  }

  /** /goal forms: show, set/replace, pause, resume, clear (ZCode-style).
   *  Setting mid-turn is allowed — the loop reads the live goal. */
  function goalCommand(args: string) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat) {
      // empty starter: no session yet. With an objective, create the Work
      // session and kick it off (the goal is the first task). Bare `/goal`
      // raises an empty work session so the header (with Configure) has
      // somewhere to live.
      if (composerMode !== "work") {
        error = "/goal is only available in Work sessions.";
        return;
      }
      if (!args) {
        const ws0 = (pendingWorkspace || defaultWorkspace || "").trim();
        const blank: Chat = {
          id: crypto.randomUUID(),
          title: "New session",
          createdAt: Date.now(),
          messages: [],
          mode: "work",
          workspace: ws0 || null
        };
        chats = [blank, ...chats];
        activeId = blank.id;
        isPinned = true;
        error = null;
        save();
        goalPromptSession = blank.id;
        goalDraft = "";
        void goto(`/s/${blank.id}`);
        return;
      }
      void startGoalOnHome(args);
      return;
    }
    if ((chat.mode ?? "chat") !== "work") {
      error = "/goal is only available in Work sessions.";
      return;
    }
    const cur = chatGoal(chat);
    const set = (text: string) => {
      chat.goal = JSON.stringify({ text, paused: false, startedAt: Date.now(), pausedAt: null });
      chats = [...chats];
      save();
    };
    if (!args) {
      // bare `/goal`: summon the header strip with its empty state —
      // Configure opens the popup from there
      goalPromptSession = chat.id;
      return;
    }
    if (args === "pause") {
      if (!cur) {
        error = "No goal to pause — /goal <objective> to set one.";
        return;
      }
      if (cur.paused) {
        error = "Goal is already paused.";
        return;
      }
      chat.goal = JSON.stringify({ ...cur, paused: true, pausedAt: Date.now() });
      chats = [...chats];
      save();
      return;
    }
    if (args === "resume") {
      if (!cur) {
        error = "No goal to resume — /goal <objective> to set one.";
        return;
      }
      if (!cur.paused) {
        error = "Goal isn't paused.";
        return;
      }
      const shift = cur.pausedAt != null ? Date.now() - cur.pausedAt : 0;
      chat.goal = JSON.stringify({ ...cur, paused: false, startedAt: cur.startedAt + shift, pausedAt: null });
      chats = [...chats];
      save();
      if (!busy) void startGoalTurn();
      return;
    }
    if (args === "clear") {
      if (!cur) {
        error = "No goal set.";
        return;
      }
      chat.goal = null;
      chats = [...chats];
      save();
      goalPromptSession = chat.id;
      return;
    }
    const m = args.match(/^replace\s+(.+)$/);
    const text = (m ? m[1] : args).trim();
    if (!text) {
      error = "Goal text is empty — /goal <objective> to set one.";
      return;
    }
    set(text);
    if (!busy) void startGoalTurn();
  }

  /** `/goal <text>` on the empty starter: raise the Work session now (the
   *  goal is its first task) and kick the agent — no extra prompt needed. */
  async function startGoalOnHome(text: string) {
    if (busy) return;
    const ws = (pendingWorkspace || defaultWorkspace || "").trim();
    const c: Chat = {
      id: crypto.randomUUID(),
      title: text.slice(0, 32) + (text.length > 32 ? ".." : ""),
      createdAt: Date.now(),
      messages: [],
      mode: "work",
      workspace: ws || null,
      goal: JSON.stringify({ text, paused: false, startedAt: Date.now(), pausedAt: null })
    };
    chats = [c, ...chats];
    activeId = c.id;
    isPinned = true;
    error = null;
    save();
    await startGoalTurn();
    // deferred navigation (like send): the route only changes once the
    // turn is underway, and never if the user moved on meanwhile
    if (activeId === c.id) void goto(`/s/${c.id}`);
  }

  /** setting or resuming a goal kicks the agent immediately (when idle)
   *  instead of waiting for another prompt — the goal itself is the task */
  async function startGoalTurn() {
    const c = chats.find((x) => x.id === activeId);
    if (!c || (c.mode ?? "chat") !== "work" || busy) return;
    const g = chatGoal(c);
    if (!g || g.paused) return;
    if (!c.workspace?.trim()) {
      error = "Work mode needs a workspace folder — pick one with the folder pill, then send a message.";
      return;
    }
    if (c.title === "New session" || c.title === "New Session") {
      c.title = g.text.slice(0, 40);
    }
    c.messages = [...c.messages, { role: "user", content: g.text }, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    chats = [...chats];
    save();
    scrollBottom();
    await runWorkTurn(c, c.messages.length - 1, c.workspace);
  }

  /** run the highlighted slash command, then clear the composer */
  function runSlash() {
    const cmd = slash?.items[slash.index]?.name;
    slash = null;
    if (!cmd) return;
    // args ride along after the command word ("/goal ship it") — grab
    // them before the composer is cleared
    const sp = cmd === "goal" ? input.indexOf(" ") : -1;
    const args = sp < 0 ? "" : input.slice(sp + 1).trim();
    input = "";
    clearDraft(null);
    autosizeComposer();
    composerEl?.focus();
    if (cmd === "goal") {
      goalCommand(args);
      return;
    }
    if (cmd === "compact") void compactSession();
  }

  /** Tab on a bare args-taking command ("/go", "/goal") completes it with a
   *  trailing space so args can follow, instead of running it with no args.
   *  Tab with args present (or on arg-less commands) runs as usual. */
  function completeSlash() {
    const item = slash?.items[slash.index];
    if (!item) return;
    input = `/${item.name} `;
    clearDraft(null);
    refreshSlashMenu();
    requestAnimationFrame(() => {
      composerEl?.setSelectionRange(input.length, input.length);
      composerEl?.focus();
      syncBackdropScroll();
    });
  }

  /** context window of the selected model for the usage ring (cached per
   *  model via modelContextLength; 32768 fallback until it resolves) */
  let ctxWindow = $state(32768);
  async function refreshCtxWindow() {
    const m = selectedModel;
    const len = await modelContextLength(m).catch(() => null);
    if (selectedModel !== m) return;
    if (len) ctxWindow = len;
  }
  $effect(() => {
    selectedModel;
    void refreshCtxWindow();
  });

  /** usage for the Work header ring: system prompt + send-shaped history
   *  over the model's window — the same fraction the auto-compact watches,
   *  so the ring hitting ~70% explains a compaction */
  const workCtx = $derived.by(() => {
    const c = activeChat;
    if (!c || (c.mode ?? "chat") !== "work") return null;
    const used = estimateWorkTokens([{ content: buildWorkSystemPrompt(c.workspace ?? "") }, ...compactedHistory(c.messages)]);
    const win = ctxWindow > 0 ? ctxWindow : 32768;
    return { used, win, frac: Math.min(1, used / win) };
  });

  /** keep the composer backdrop scrolled with the textarea */
  function syncBackdropScroll() {
    const ta = composerEl;
    const bd = composerBackdrop;
    if (ta && bd) {
      bd.scrollTop = ta.scrollTop;
      bd.scrollLeft = ta.scrollLeft;
    }
  }

  /** absolute FS path for opener-plugin calls (root is canonical absolute) */
  function absExpPath(rel: string): string {
    const root = displayWorkspace(explorerRoot.trim()).replace(/\/+$/, "");
    return `${root}/${rel}`;
  }

  /** right-click a row: open the file context menu at the cursor (clamped) */
  function openExpCtx(e: MouseEvent, path: string, isDir: boolean) {
    e.preventDefault();
    e.stopPropagation();
    expConfirmDelete = null;
    expCtx = {
      x: Math.max(8, Math.min(e.clientX, window.innerWidth - 240)),
      y: Math.max(8, Math.min(e.clientY, window.innerHeight - 260)),
      path,
      isDir
    };
  }

  function closeExpCtx() {
    expCtx = null;
  }

  async function expOpenFile() {
    const p = expCtx;
    closeExpCtx();
    if (!p || !isTauriRuntime()) return;
    try {
      const { openPath } = await import("@tauri-apps/plugin-opener");
      await openPath(absExpPath(p.path));
    } catch (e: any) {
      flashExpNotice(`Couldn't open: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  async function expReveal() {
    const p = expCtx;
    closeExpCtx();
    if (!p || !isTauriRuntime()) return;
    try {
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(absExpPath(p.path));
    } catch (e: any) {
      flashExpNotice(`Couldn't reveal: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  function expCopyMention() {
    const p = expCtx;
    closeExpCtx();
    if (!p) return;
    try {
      void navigator.clipboard?.writeText(`@${p.path}`);
    } catch {
      flashExpNotice("Couldn't copy to clipboard.");
    }
  }

  function expAddToChat() {
    const p = expCtx;
    closeExpCtx();
    if (!p || p.isDir) return;
    insertExplorerPath(p.path);
  }

  /** side-panel tabs: focus the Files view, creating it if closed */
  function openSideExplorer() {
    newTabMenu = false;
    const existing = sideTabs.find((t) => t.kind === "files");
    if (existing) {
      activeSideTab = existing.key;
    } else {
      sideTabs = [...sideTabs, { key: "files", kind: "files", title: "Files" }];
      activeSideTab = "files";
    }
    if (!explorerEntries.length && !explorerLoading) void loadExplorer();
  }

  function termTheme(): Record<string, string> {
    // Full 16-color ANSI palettes: PowerShell / PSReadLine assume a dark
    // console (yellow prompt segments, white text), so the defaults that
    // ship with xterm go unreadable on a light background. Both palettes
    // keep the app's chrome bg/fg but remap every ANSI slot to stay legible.
    if (theme === "dark") {
      return {
        background: "#161616",
        foreground: "#e8e8e8",
        cursor: "#e8e8e8",
        selection: "rgba(255,255,255,.25)",
        black: "#000000",
        red: "#cd3131",
        green: "#0dbc79",
        yellow: "#e5e510",
        blue: "#2472c8",
        magenta: "#bc3fbc",
        cyan: "#11a8cd",
        white: "#e5e5e5",
        brightBlack: "#666666",
        brightRed: "#f14c4c",
        brightGreen: "#23d18b",
        brightYellow: "#f5f543",
        brightBlue: "#3b8eea",
        brightMagenta: "#d670d6",
        brightCyan: "#29b8db",
        brightWhite: "#ffffff"
      };
    }
    return {
      background: "#f4f4f4",
      foreground: "#1a1a1a",
      cursor: "#1a1a1a",
      selection: "rgba(0,0,0,.15)",
      black: "#000000",
      red: "#b3261e",
      green: "#0a7a42",
      yellow: "#795e00",
      blue: "#0a4fb3",
      magenta: "#9a3a9a",
      cyan: "#0a7a8a",
      white: "#767676",
      brightBlack: "#5a5a5a",
      brightRed: "#d32f2f",
      brightGreen: "#1a7f37",
      brightYellow: "#8a6d00",
      brightBlue: "#2563eb",
      brightMagenta: "#d63384",
      brightCyan: "#0a8a9a",
      brightWhite: "#333333"
    };
  }
  // re-theme live terminals when the app theme flips (terminals are created
  // once with the theme at open time, so existing tabs would go stale)
  $effect(() => {
    const t = termTheme();
    for (const [, v] of termViews) {
      try {
        v.term.options.theme = t as never;
      } catch {
        /* terminal mid-dispose */
      }
    }
  });

  /** fit the active terminal to its container and tell the pty the size */
  function fitSideTerm(key: string) {
    const el = document.querySelector(`[data-term="${key}"]`);
    if (!(el instanceof HTMLElement) || el.clientWidth === 0 || el.clientHeight === 0) return;
    const v = termViews.get(key);
    const t = sideTabs.find((x) => x.key === key);
    if (!v || !t?.termId || t.dead) return;
    try {
      v.fit.fit();
      const id = t.termId;
      if (v.resizeTimer) clearTimeout(v.resizeTimer);
      v.resizeTimer = setTimeout(() => {
        void resizeTerm(id, v.term.cols, v.term.rows).catch(() => {});
      }, 150);
    } catch {
      /* container hidden mid-switch */
    }
  }

  /** open a Terminal tab rooted at the workspace (desktop only) */
  async function openSideTerminal() {
    newTabMenu = false;
    const key = `term-${sideTabSeq++}`;
    sideTabs = [...sideTabs, { key, kind: "terminal", title: explorerRootBase || "Terminal" }];
    activeSideTab = key;
    await tick();
    if (!isTauriRuntime()) {
      sideTabs = sideTabs.map((t) =>
        t.key === key ? { ...t, error: "Terminal needs the Vicuna desktop app." } : t
      );
      return;
    }
    const root = explorerRoot.trim();
    if (!root) {
      sideTabs = sideTabs.map((t) =>
        t.key === key ? { ...t, error: "Pick a workspace folder first." } : t
      );
      return;
    }
    try {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("xterm"),
        import("@xterm/addon-fit")
      ]);
      await import("xterm/css/xterm.css");
      const el = document.querySelector(`[data-term="${key}"]`);
      if (!(el instanceof HTMLElement)) throw new Error("terminal view missing");
      const term = new Terminal({
        theme: termTheme(),
        fontSize: 13,
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        cursorBlink: true,
        scrollback: 5000
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      fit.fit();
      const setTab = (patch: Partial<SideTab>) => {
        sideTabs = sideTabs.map((t) => (t.key === key ? { ...t, ...patch } : t));
      };
      const termId = await spawnTerm(
        root,
        term.cols,
        term.rows,
        (text) => term.write(text),
        (code) => {
          setTab({ dead: true });
          term.write(`\r\n[process exited${code == null ? "" : ` with code ${code}`} — close the tab or open a new one]`);
        }
      );
      term.onData((d) => {
        void writeTerm(termId, d).catch(() => {});
      });
      const ro = new ResizeObserver(() => fitSideTerm(key));
      ro.observe(el);
      termViews.set(key, { term, fit, ro, resizeTimer: null });
      setTab({ termId });
      term.focus();
    } catch (e: any) {
      sideTabs = sideTabs.map((t) =>
        t.key === key ? { ...t, error: typeof e === "string" ? e : (e?.message ?? String(e)) } : t
      );
    }
  }

  /** close a side tab, killing its pty; never strand the active key */
  function closeSideTab(key: string) {
    const t = sideTabs.find((x) => x.key === key);
    if (t?.termId) void killTerm(t.termId);
    const v = termViews.get(key);
    if (v) {
      v.ro.disconnect();
      if (v.resizeTimer) clearTimeout(v.resizeTimer);
      try {
        v.term.dispose();
      } catch {
        /* already gone */
      }
      termViews.delete(key);
    }
    sideTabs = sideTabs.filter((x) => x.key !== key);
    if (activeSideTab === key) {
      activeSideTab = sideTabs.length ? sideTabs[sideTabs.length - 1].key : "";
      if (activeSideTab) void tick().then(() => fitSideTerm(activeSideTab));
    }
  }

  function switchSideTab(key: string) {
    activeSideTab = key;
    const t = sideTabs.find((x) => x.key === key);
    if (t?.kind === "terminal") {
      void tick().then(() => {
        fitSideTerm(key);
        termViews.get(key)?.term.focus();
      });
    }
  }

  /** whether a drag carries OS files (vs. text selection etc.) */
  function hasDroppedFiles(e: DragEvent): boolean {
    const types = e.dataTransfer?.types;
    return !!types && Array.from(types).includes("Files");
  }

  /** composer drop target highlight */
  function onComposerDragEnter(e: DragEvent) {
    if (!hasDroppedFiles(e)) return;
    compDragDepth += 1;
    composerDropActive = true;
  }

  function onComposerDragOver(e: DragEvent) {
    e.preventDefault();
  }

  function onComposerDragLeave() {
    compDragDepth = Math.max(0, compDragDepth - 1);
    if (compDragDepth === 0) composerDropActive = false;
  }

  /** dropped files land as pending composer attachments: images join the
   *  image pipeline, everything else becomes a per-session upload staged
   *  under ~/vicuna/uploads/<id>/ when the message is sent */
  async function onComposerDrop(e: DragEvent) {
    e.preventDefault();
    compDragDepth = 0;
    composerDropActive = false;
    const dt = e.dataTransfer;
    if (!dt) return;
    try {
      const entries = await collectDropEntries(dt);
      if (!entries.length) return;
      await ingestDroppedFiles(entries.map((en) => en.file));
    } catch (err: any) {
      error = typeof err === "string" ? err : (err?.message ?? String(err));
    }
  }

  /** start inline rename of a row (menu closes first) */
  function startExpRename(path: string, name: string) {
    closeExpCtx();
    expConfirmDelete = null;
    expRenaming = path;
    expRenameDraft = name;
    requestAnimationFrame(() => {
      const el = document.querySelector<HTMLInputElement>('[data-testid="explorer-rename"]');
      if (el) {
        el.focus();
        el.select();
      }
    });
  }

  function cancelExpRename() {
    expRenaming = null;
  }

  /** menu wrappers: read expCtx now (template narrowing doesn't survive
   *  the click closure) */
  function renameFromMenu() {
    const p = expCtx;
    if (!p) return;
    startExpRename(p.path, p.path.split(/[/\\]/).pop() ?? p.path);
  }

  function deleteFromMenu() {
    const p = expCtx;
    if (!p) return;
    void requestExpDelete(p.path);
  }

  /** commit the inline rename (Enter/blur); errors keep the box open */
  async function commitExpRename() {
    const path = expRenaming;
    if (path == null) return;
    const name = expRenameDraft.trim();
    expRenaming = null;
    if (!name || name === path.split(/[/\\]/).pop()) return;
    try {
      await renameWorkFile(explorerRoot.trim(), path, name);
      await loadExplorer();
    } catch (e: any) {
      flashExpNotice(`Couldn't rename: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
      expRenaming = path;
      expRenameDraft = name;
      requestAnimationFrame(() => {
        document.querySelector<HTMLInputElement>('[data-testid="explorer-rename"]')?.focus();
      });
    }
  }

  /** two-step delete: first click arms, second click runs (menu closes) */
  async function requestExpDelete(path: string) {
    if (expConfirmDelete !== path) {
      expConfirmDelete = path;
      return;
    }
    expConfirmDelete = null;
    closeExpCtx();
    try {
      await deleteWorkFile(explorerRoot.trim(), path);
      // drop stale collapse state under the deleted dir
      const sep = (d: string) => d !== path && !d.startsWith(path + "/") && !d.startsWith(path + "\\");
      collapsedDirs = collapsedDirs.filter(sep);
      await loadExplorer();
    } catch (e: any) {
      flashExpNotice(`Couldn't delete: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  // refresh the tree when the work root changes under an open panel
  // (debounced: the workspace input updates per keystroke; "/" is never
  // valid — older stored roots — so don't even debounce it)
  $effect(() => {
    const root = explorerRoot.trim();
    if (!explorerOpen || root === lastExplorerRoot || root === "/") return;
    lastExplorerRoot = root;
    if (explorerTimer) clearTimeout(explorerTimer);
    explorerTimer = setTimeout(() => void loadExplorer(), 400);
  });

  // refresh the tree when an agent turn settles (files may have changed)
  let wasWorkRunning = false;
  $effect(() => {
    if (wasWorkRunning && !workRunning && explorerOpen && !explorerLoading) {
      void loadExplorer();
    }
    wasWorkRunning = workRunning;
  });

  /** work mode: Esc once arms the interrupt, Esc again within 2.5s stops it */
  function onEscape() {
    if (!workRunning && !streaming) return false;
    if (!escArmed) {
      escArmed = true;
      if (escTimer) clearTimeout(escTimer);
      escTimer = setTimeout(() => {
        escArmed = false;
        escTimer = null;
      }, 2500);
      return true;
    }
    if (escTimer) clearTimeout(escTimer);
    escTimer = null;
    escArmed = false;
    stop();
    return true;
  }

  /** user answered a live ask card: resolve the blocked tool call (the work
   *  loop appends the visible record) */
  function answerAsk(value: string) {
    const a = pendingAsk;
    if (!a) return;
    pendingAsk = null;
    a.resolve(value, "answer");
    scrollBottom();
  }

  /** user dismissed the question without answering: tell the model so it
   *  stops waiting and doesn't re-ask */
  function dismissAsk() {
    const a = pendingAsk;
    if (!a) return;
    pendingAsk = null;
    a.resolve(
      "User dismissed the question without answering. Do not ask it again — continue with your best judgment or explain what you need.",
      "dismiss"
    );
    scrollBottom();
  }

  /** dismiss a pending ask without an answer (stop / chat deleted) so the
   *  awaiting work loop can unwind instead of hanging forever */
  function cancelPendingAsk(reason: string) {
    const a = pendingAsk;
    if (!a) return;
    pendingAsk = null;
    a.resolve(reason, "cancel");
  }

  function stop() {
    if (workRunning) workStop = true;
    if (workTurnId) void stopWorkAgentTurn(workTurnId).catch(() => {});
    cancelPendingAsk("(the user interrupted before answering)");
    aborter?.abort();
  }

  function onKey(e: KeyboardEvent) {
    const noMod = !e.metaKey && !e.ctrlKey && !e.altKey;
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") caretHopped = false;
    // slash-command menu (leading "/" in an empty composer): arrows move,
    // Enter/Tab runs, Esc closes. Checked before @-mentions — input starting
    // with "/" belongs to this menu.
    if (slash) {
      if ((e.key === "ArrowDown" || e.key === "ArrowUp") && noMod) {
        e.preventDefault();
        popupKeyHandled = true;
        slash.index = (slash.index + (e.key === "ArrowDown" ? 1 : slash.items.length - 1)) % slash.items.length;
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        popupKeyHandled = true;
        const item = slash.items[slash.index];
        // bare args-taking command ("/go", "/goal"): complete it with a
        // trailing space so args can follow, instead of running with none.
        // Tab with args present, or on arg-less commands, runs as usual.
        if (item?.takesArgs && !/\s\S/.test(input)) {
          completeSlash();
        } else {
          runSlash();
        }
        return;
      }
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        popupKeyHandled = true;
        runSlash();
        return;
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        popupKeyHandled = true;
        slash = null;
        return;
      }
    }
    // @-mention popup navigation (work mode): arrows move, Enter/Tab select,
    // Esc closes. Plain arrows also hop over completed pills atomically.
    if (mention) {
      if (e.key === "ArrowDown" && noMod) {
        e.preventDefault();
        popupKeyHandled = true;
        if (mention.items.length) mention.index = (mention.index + 1) % mention.items.length;
        return;
      }
      if (e.key === "ArrowUp" && noMod) {
        e.preventDefault();
        popupKeyHandled = true;
        if (mention.items.length) mention.index = (mention.index - 1 + mention.items.length) % mention.items.length;
        return;
      }
      if ((e.key === "Enter" && !e.shiftKey) || e.key === "Tab") {
        const item = mention.items[mention.index];
        popupKeyHandled = true;
        if (item) {
          e.preventDefault();
          pickMention(item);
          return;
        }
        // no matches: dismiss and fall through to normal handling
        mention = null;
      } else if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        popupKeyHandled = true;
        mention = null;
        return;
      }
    }
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    } else if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && noMod && !e.shiftKey) {
      // pills are atomic: arrows hop over a whole mention instead of
      // landing inside it (the caret rests before or after, never within)
      const el = composerEl;
      if (el && el.selectionStart === el.selectionEnd) {
        const caret = el.selectionStart ?? input.length;
        const dir = e.key === "ArrowLeft" ? -1 : 1;
        let target: number | null = null;
        for (const t of findMentions(input)) {
          if (dir < 0 && caret === t.end) {
            target = t.start;
            break;
          }
          if (dir > 0 && caret === t.start) {
            target = t.end;
            break;
          }
          if (caret > t.start && caret < t.end) {
            target = dir < 0 ? t.start : t.end;
            break;
          }
        }
        if (target != null) {
          e.preventDefault();
          caretHopped = true;
          requestAnimationFrame(() => {
            composerEl?.setSelectionRange(target, target);
            syncBackdropScroll();
            refreshMentionMenu();
          });
        }
      }
    } else if ((e.key === "Backspace" || e.key === "Delete") && noMod) {
      // mentions delete as a unit: backspace at the end (or delete at the
      // start) of an @path token removes the whole token at once. Skipped
      // while the popup is open — that's a live query being typed, where
      // backspace must eat characters one by one — unless the caret just
      // arrow-hopped here, which is explicit navigation, not typing.
      if (mention && !caretHopped) return;
      const el = composerEl;
      if (el && el.selectionStart === el.selectionEnd) {
        const caret = el.selectionStart ?? input.length;
        const tok = findMentions(input).find((t) =>
          e.key === "Backspace" ? caret === t.end : caret === t.start
        );
        if (tok) {
          e.preventDefault();
          input = input.slice(0, tok.start) + input.slice(tok.end);
          requestAnimationFrame(() => {
            composerEl?.setSelectionRange(tok.start, tok.start);
            composerEl?.focus();
            syncBackdropScroll();
          });
        }
      }
    }
  }

  function pickFile() {
    fileInput?.click();
  }

  function decodeEntities(s: string): string {
    return s
      .replace(/&lt;/g, "<")
      .replace(/&gt;/g, ">")
      .replace(/&quot;/g, '"')
      .replace(/&#39;/g, "'")
      .replace(/&amp;/g, "&");
  }

  /** Harness markup (collapsed tool blocks, ask records) is display-only.
   *  Models that see it in their own history imitate the HTML instead of
   *  calling tools, so history sends a compact plain-text form instead:
   *  tool blocks drop out, ask records become prose. */
  function stripHarnessMarkup(content: string): string {
    return content
      .replace(/<details class="tool-result">[\s\S]*?<\/details>/g, "")
      .replace(
        /<div class="ask-log">\s*<div class="ask-log-q">([\s\S]*?)<\/div>\s*<div class="ask-log-a">([\s\S]*?)<\/div>\s*<\/div>/g,
        (_m, q: string, a: string) => {
          const answer = /ask-log-dismissed/.test(a)
            ? "dismissed the question"
            : `answered: ${decodeEntities(a.replace(/<[^>]+>/g, "").replace(/^Answer:\s*/, ""))}`;
          return `Asked the user: ${decodeEntities(q)} — they ${answer}.`;
        }
      )
      .replace(/<details class="tool-result">[\s\S]*$/g, "")
      .replace(/<div class="ask-log">[\s\S]*$/g, "")
      .replace(/\n{3,}/g, "\n\n")
      .trim();
  }

  // history entries keep attached images so regenerate/continue/resume
  // resend the same picture instead of silently dropping it; tool calls and
  // tool names pass through for native agent histories
  function toHistory(m: ChatMsg): ChatMsg {
    const content = m.role === "assistant" ? stripHarnessMarkup(m.content) : m.content;
    const base: ChatMsg = m.images?.length
      ? { role: m.role, content, images: [...m.images] }
      : { role: m.role, content };
    if (m.tool_calls?.length) base.tool_calls = m.tool_calls;
    if (m.tool_name) base.tool_name = m.tool_name;
    return base;
  }

  /** Model-visible transcript: everything from the latest compaction
   *  summary onward. Older messages stay on screen for the user (under a
   *  "Session compacted" divider) but never reach the model again — the
   *  summary carries their context instead. No-op for threads never
   *  compacted, so every history-building site uses this. */
  function compactedHistory(messages: ChatMsg[]): ChatMsg[] {
    let from = 0;
    for (let i = 0; i < messages.length; i++) {
      if (messages[i].summary) from = i;
    }
    return messages.slice(from).map(toHistory);
  }

  /** Work-only session objective (/goal), persisted as a JSON blob on the
   *  chat. The turn loop keeps running until an unpaused goal reads as met. */
  interface SessionGoal {
    text: string;
    paused: boolean;
    startedAt: number;
    pausedAt: number | null;
  }

  /** parse the stored goal; null when unset or corrupt */
  function chatGoal(c: Chat | undefined | null): SessionGoal | null {
    if (!c?.goal) return null;
    try {
      const g = JSON.parse(c.goal) as Partial<SessionGoal>;
      if (typeof g.text !== "string" || !g.text) return null;
      return {
        text: g.text,
        paused: g.paused === true,
        startedAt: typeof g.startedAt === "number" ? g.startedAt : Date.now(),
        pausedAt: typeof g.pausedAt === "number" ? g.pausedAt : null
      };
    } catch {
      return null;
    }
  }

  /** the open thread's goal, for the header strip */
  const goalNow = $derived(activeChat ? chatGoal(activeChat) : null);
  /** goal settings popup (Configure button) + editable objective draft */
  let goalOpen = $state(false);
  let goalDraft = $state("");
  /** show the goal strip's "<no goal yet>" empty state for one session:
   *  keyed by chat id (not a boolean) so creating/navigating to the session
   *  doesn't race the activeId-change effect that closes the popup */
  let goalPromptSession: string | null = $state(null);
  // settings belong to one session: close them when navigating away
  $effect(() => {
    activeId;
    goalOpen = false;
  });

  function noteImagesStripped() {
    error = `Note: ${selectedModel} doesn't support images, so the attached picture was not sent.`;
  }

  const isWindows = typeof navigator !== "undefined" && /win/i.test(navigator.userAgent ?? "");
  /** Strip Rust's Windows verbatim prefix (`\\?\C:\...`, `\\?\UNC\...`) so
   *  older stored sessions display cleanly. New values are already clean
   *  (the backend strips before returning), this covers stored ones. */
  function displayWorkspace(p: string): string {
    if (p.startsWith("\\\\?\\UNC\\")) return "\\" + p.slice("\\\\?\\UNC\\".length);
    if (p.startsWith("\\\\?\\")) return p.slice("\\\\?\\".length);
    return p;
  }
  /** Final path segment on any platform (`/` and `\` separators). */
  function workspaceBase(p: string): string {
    return displayWorkspace(p).trim().split(/[/\\]/).filter(Boolean).pop() ?? "";
  }

  async function validateWorkspace(path: string): Promise<string | null> {
    const p = path.trim();
    if (!p) return null;
    try {
      await invokeOrBridge<string>("work_validate_root", { path: p });
      return null;
    } catch (e: any) {
      return typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }

  async function canonicalWorkspace(path: string): Promise<string> {
    return await invokeOrBridge<string>("work_validate_root", { path: path.trim() });
  }

  /** Workspace pill (composer bar, work contexts): native folder picker on
   *  desktop; manual prompt in plain browsers (no FS access for a dialog). */
  async function pickWorkspace() {
    if (!isTauriRuntime()) {
      const el = document.querySelector<HTMLInputElement>("[data-testid=workspace-input-active]");
      if (el) {
        el.focus();
        el.scrollIntoView({ block: "nearest" });
        return;
      }
      const current = (composerMode === "work" ? (activeChat?.workspace ?? pendingWorkspace) : pendingWorkspace).trim();
      const answer = window.prompt("Workspace absolute path:", current || (isWindows ? "" : "/"));
      if (answer == null) return; // cancelled
      pendingWorkspace = answer.trim();
      if (composerMode === "work" && activeChat) activeChat.workspace = pendingWorkspace || null;
      workspaceEditing = false;
      return;
    }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      // "/" is the Unix fresh-install default, not a valid Windows path —
      // never hand it to the native dialog as a default folder.
      const hint = pendingWorkspace.trim() || defaultWorkspace.trim();
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: hint && !(isWindows && hint === "/") ? hint : undefined
      });
      if (typeof picked !== "string" || !picked) return; // cancelled
      pendingWorkspace = picked;
      if (composerMode === "work" && activeChat) activeChat.workspace = picked;
      workspaceError = await validateWorkspace(picked);
      if (!workspaceError) {
        defaultWorkspace = picked;
        persistPrefs();
      }
    } catch (e: any) {
      workspaceError = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }

  /** Work-mode agent loop, owned by the Rust backend (work_agent.rs).
   *  Each round streams narration text into the live assistant message and
   *  returns `tool_calls`; the backend executes them, and the frontend
   *  mirrors the semantic event stream below into the bubble (collapsed
   *  markdown results for persistence, ask cards, steering bubbles) exactly
   *  like the old in-thread loop did. Settle paths (leftovers, follow-up
   *  turns, goal retirement, stats) are unchanged. */
  async function runWorkTurn(chat: Chat, index: number, workspace: string, seedContext: ChatMsg[] = []) {
    const turnId = ++workTurnSeq;
    // No Tauri gate here: plain browsers drive the same flow through the
    // loopback bridge (startWorkAgentTurn throws the desktop/bridge error
    // when neither is present, and the catch below surfaces it).
    workRunning = true;
    workChatId = chat.id;
    workStartAt = Date.now();
    workStop = false;
    let curIndex = index;
    // in-flight tool head (for say/ask/done display) + the ask answer the
    // frontend itself sent (the record mirrors it once the result lands)
    let pendingHead: { name: string; args: Record<string, any> } | null = null;
    let askOutcome: "answer" | "dismiss" | null = null;
    let askText = "";
    // token batching: same ~120ms behavior the old loop had — every state
    // write re-renders the whole message, so per-token writes go O(n²)
    let pendingTok = "";
    let pendingTh = "";
    let flushTimer: ReturnType<typeof setTimeout> | null = null;
    function flushTokens() {
      flushTimer = null;
      if (!pendingTok && !pendingTh) return;
      const tok = pendingTok;
      const th = pendingTh;
      pendingTok = "";
      pendingTh = "";
      const keep = captureStreamSel(curIndex);
      const c = chats.find((x) => x.id === chat.id);
      if (c && c.messages[curIndex]) {
        const m = c.messages[curIndex];
        c.messages[curIndex] = { role: "assistant", content: m.content + tok, thinking: (m.thinking ?? "") + th, thinkingOpen: m.thinkingOpen, model: m.model ?? selectedModel, genMs: m.genMs, genTps: m.genTps };
        chats = [...chats];
      }
      if (keep) void tick().then(() => restoreStreamSel(curIndex, keep));
      scrollBottom();
      saveSoon();
    }
    function scheduleFlush() {
      if (flushTimer) return;
      flushTimer = setTimeout(flushTokens, 120);
    }
    // slow first-token pause while the model generates tool args (same
    // "Preparing tool call…" stall hint the old rounds showed)
    let stallTimer: ReturnType<typeof setTimeout> | null = null;
    function clearStall() {
      if (stallTimer) {
        clearTimeout(stallTimer);
        stallTimer = null;
      }
    }
    function dropStallHint() {
      if (toolStatus?.chatId === chat.id && toolStatus.label === "Preparing tool call…") toolStatus = null;
    }
    function armStall() {
      clearStall();
      stallTimer = setTimeout(() => {
        stallTimer = null;
        if (!pendingTok && !pendingTh && !toolStatus) {
          toolStatus = { chatId: chat.id, label: "Preparing tool call…" };
        }
      }, 1000);
    }
    const live = () => chats.find((x) => x.id === chat.id);
    function appendContent(s: string) {
      const c = live();
      if (!c?.messages[curIndex]) return;
      const m = c.messages[curIndex];
      c.messages[curIndex] = { ...m, content: m.content + s };
      chats = [...chats];
    }
    // native history: proper tool roles for the model; the display message
    // holds narration + markdown results for persistence. seedContext carries
    // hidden context (e.g. a continued chat transcript) never shown here.
    const baseHistory = compactedHistory(live()?.messages.slice(0, index) ?? []);
    let seed = seedContext;
    if (!seed.length && chat.forkedFrom && chat.forkFromIndex == null) {
      // continued thread regenerated (or re-run) without its original seed:
      // rebuild it from the linked source so the agent keeps its context
      const src = chats.find((x) => x.id === (chat.forkedFrom as { id: string }).id);
      if (src) {
        seed = [
          ...compactedHistory(src.messages.slice(-20)),
          { role: "user", content: workspace ? `Continue the above in the workspace (${workspace}).` : "Continue the above in a workspace." }
        ];
      }
    }
    const startGoal = chatGoal(live());
    const native: ChatMsg[] = [
      { role: "system", content: buildWorkSystemPrompt(workspace, startGoal?.text ?? null) },
      ...seed,
      ...baseHistory
    ];
    // MCP tools (Work mode only): same assembly as before — connected
    // servers lend extra tools alongside the built-in workspace ones.
    const mcp = assembleMcpTools();
    const allTools = [...WORK_TOOLS, ...(startGoal && !startGoal.paused ? [GOAL_DONE_TOOL] : []), ...mcp.tools] as any;
    const routes: Record<string, McpRoute> = {};
    for (const [k, v] of mcp.route) routes[k] = { server_id: v.server.id, tool_name: v.name };
    const think = effectiveThink();

    async function settle(stopped: boolean) {
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      flushTokens();
      clearStall();
      dropStallHint();
      if (!unloading) clearPendingStream();
      save();
      if (escTimer) {
        clearTimeout(escTimer);
        escTimer = null;
      }
      escArmed = false;
      // prompts that arrived after the last round: continue the agent with them
      // (an aborted turn returns them to the composer instead)
      const aborted = workStop || stopped;
      const leftovers = turnId === workTurnSeq ? takeQueued(chat.id) : [];
      if (leftovers.length) {
        if (aborted) {
          if (activeId === chat.id) {
            input = [input.trim(), ...leftovers.map((q) => q.content.trim())].filter(Boolean).join("\n\n");
            attachImages = [...attachImages, ...leftovers.flatMap((q) => q.images ?? [])];
          }
        } else {
          const c = live();
          if (c) {
            for (const q of leftovers) {
              c.messages = [...c.messages, q, { role: "assistant", content: "", thinking: "", model: selectedModel }];
            }
            chats = [...chats];
            save();
            // hand off to a fresh turn; the inner call clears workRunning
            await runWorkTurn(chat, c.messages.length - 1, workspace);
            return;
          }
        }
      }
      // natural end or abort: no step cap — the backend loop already exits
      // when the model makes no calls, repeats finished work, or stops
      if (turnId === workTurnSeq) {
        workRunning = false;
        workChatId = null;
        workStartAt = null;
        workTurnId = null;
      }
    }

    function handleEvent(ev: WorkAgentEvent) {
      if (turnId !== workTurnSeq) return;
      switch (ev.type) {
        case "token":
          pendingTok += ev.text;
          scheduleFlush();
          dropStallHint();
          armStall();
          break;
        case "think":
          pendingTh += ev.text;
          scheduleFlush();
          dropStallHint();
          armStall();
          break;
        case "status":
          if (ev.label) toolStatus = { chatId: chat.id, label: ev.label };
          else if (toolStatus?.chatId === chat.id) toolStatus = null;
          break;
        case "roundstart":
          // a generation round is in flight: persist the resume marker (same
          // shape the old per-round streamInto wrote) so a refresh resumes it
          writePendingStream({ chatId: chat.id, index: curIndex, mode: ev.mode as "fresh" | "continue", model: selectedModel, think });
          save();
          armStall();
          break;
        case "roundend": {
          clearStall();
          dropStallHint();
          if (!unloading) clearPendingStream();
          // per-round stats accumulate onto the live bubble (same math the
          // old streamInto settle used: wall time always, server eval rate
          // when present, eval tokens over wall time for cloud APIs)
          const t = live();
          const m = t?.messages[curIndex];
          if (t && m && m.role === "assistant") {
            t.messages[curIndex] = { ...m, genMs: (m.genMs ?? 0) + ev.gen_ms, genTps: ev.gen_tps ?? m.genTps };
            chats = [...chats];
          }
          save();
          break;
        }
        case "displaynote":
          appendContent(ev.text);
          scrollBottom();
          saveSoon();
          break;
        case "queuedrained": {
          // steering prompts the backend just consumed: render each as a
          // user bubble followed by a fresh assistant bubble, exactly like
          // the old loop did at the top of the next round
          confirmQueuedDrained(ev.ids);
          const t = live();
          if (t) {
            for (const q of ev.messages) {
              t.messages = [
                ...t.messages,
                { role: "user", content: q.content, ...(q.images ? { images: q.images } : {}) },
                { role: "assistant", content: "", thinking: "", model: selectedModel }
              ];
            }
            chats = [...chats];
            curIndex = t.messages.length - 1;
            scrollBottom();
            saveSoon();
          }
          break;
        }
        case "toolstart":
          clearStall();
          dropStallHint();
          pendingHead = { name: ev.name, args: ev.args };
          // `ask` (live card) and `say` (normal prose) own their display
          if (ev.name === "ask" || ev.name === "say") break;
          appendContent(formatToolCallHead(ev.name, ev.args));
          scrollBottom();
          break;
        case "toolend": {
          const head = pendingHead;
          pendingHead = null;
          const t = live();
          if (ev.name === "say") {
            // display channel for models that emit tool calls only: the text
            // becomes normal assistant prose, not a collapsed tool block
            const text = String(head?.args.text ?? "").trim();
            if (text && !ev.output.startsWith("Error:") && t?.messages[curIndex]) {
              const m = t.messages[curIndex];
              const sep = m.content.trim() ? "\n\n" : "";
              t.messages[curIndex] = { ...m, content: `${m.content}${sep}${text}\n\n` };
              chats = [...chats];
            } else if (t?.messages[curIndex]) {
              const m = t.messages[curIndex];
              t.messages[curIndex] = { ...m, content: m.content + formatToolCallHead(ev.name, head?.args ?? {}) + formatToolCallTail(ev.output) };
              chats = [...chats];
            }
          } else if (ev.name === "ask") {
            // visible harness record of the Q/A (or dismissal); a cancelled
            // ask (Stop / chat deleted) emits no ToolEnd, so nothing lands
            if ((askOutcome === "answer" || askOutcome === "dismiss") && t?.messages[curIndex]) {
              const m = t.messages[curIndex];
              t.messages[curIndex] = {
                ...m,
                content: m.content + formatAskRecord(String(head?.args.question ?? ""), askText, askOutcome === "dismiss")
              };
              chats = [...chats];
            }
            askOutcome = null;
            askText = "";
          } else {
            if (t?.messages[curIndex]) {
              const m = t.messages[curIndex];
              t.messages[curIndex] = { ...m, content: m.content + formatToolCallTail(ev.output) };
              chats = [...chats];
            }
            if (ev.name === "done" && t && t.goal) {
              // self-stop (goal mode): the model declared the objective met —
              // retire the goal and settle the turn with no wrap-up
              t.goal = null;
              chats = [...chats];
            }
          }
          scrollBottom();
          saveSoon();
          break;
        }
        case "bgcompleted": {
          const t = live();
          if (t?.messages[curIndex]) {
            const m = t.messages[curIndex];
            t.messages[curIndex] = {
              ...m,
              content: m.content + formatToolResult("exec", { command: `${ev.command} (${ev.job_id})` }, ev.output)
            };
            chats = [...chats];
          }
          scrollBottom();
          saveSoon();
          break;
        }
        case "contextcompacted":
          appendContent(`\n${ev.note}\n`);
          scrollBottom();
          saveSoon();
          break;
        case "askrequest":
          askOutcome = null;
          askText = "";
          pendingAsk = {
            chatId: chat.id,
            index: curIndex,
            question: ev.question,
            options: ev.options,
            other: "",
            otherOpen: false,
            resolve: (answer, outcome) => {
              // cancel (Stop / chat deleted) needs no backend call: the stop
              // path already unblocked the waiting tool with no record
              if (outcome === "cancel") return;
              askOutcome = outcome;
              askText = answer;
              const tid = workTurnId;
              if (tid) void answerWorkAgentAsk(tid, ev.ask_id, answer).catch(() => {});
              scrollBottom();
            }
          };
          requestAnimationFrame(() => scrollBottom());
          break;
        case "done":
          toolStatus = null;
          void settle(ev.stopped);
          break;
        case "error":
          error = ev.message;
          toolStatus = null;
          void settle(false);
          break;
      }
    }

    let agentTurn: string;
    try {
      agentTurn = await startWorkAgentTurn({
        model: selectedModel,
        workspace,
        messages: native,
        think,
        tools: allTools,
        sessionId: chat.id,
        goalText: startGoal?.text ?? null,
        goalPaused: startGoal?.paused ?? true,
        chats: snapshotChats(chats),
        mcpRoutes: routes,
        onEvent: (ev) => handleEvent(ev)
      });
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
      await settle(true);
      return;
    }
    workTurnId = agentTurn;
  }

  /** "Continue in Work": fork a chat transcript into a new work session rooted
   *  at the default (last-used) workspace. The thread shows only
   *  "Continue from chat [link]" followed by the assistant reply — the source
   *  transcript travels as hidden seed context, never as a visible bubble. */
  async function continueInWork(id: string) {
    const src = chats.find((c) => c.id === id);
    if (!src || streaming) return;
    ctxMenu = null;
    aborter?.abort();
    aborter = null;
    streaming = false;
    streamingChatId = null;
    streamingStartAt = null;
    const ws = defaultWorkspace || pendingWorkspace || "";
    const seedContext: ChatMsg[] = [
      ...compactedHistory(src.messages.slice(-20)),
      { role: "user", content: ws ? `Continue the above in the workspace (${ws}).` : "Continue the above in a workspace." }
    ];
    const fork: Chat = {
      id: crypto.randomUUID(),
      title: `${src.title} (work)`.slice(0, 40),
      createdAt: Date.now(),
      messages: [
        { role: "assistant", content: "", thinking: "", model: selectedModel }
      ],
      mode: "work",
      workspace: ws || null,
      forkedFrom: { id: src.id, title: src.title },
      forkFromIndex: null
    };
    chats = [fork, ...chats];
    activeId = fork.id;
    isPinned = true;
    pendingMode = "work";
    pendingWorkspace = ws;
    error = null;
    save();
    void goto(`/s/${fork.id}`);
    if (!ws) {
      error = "Pick a workspace folder above the composer, then send a message to start.";
      return;
    }
    await runWorkTurn(fork, fork.messages.length - 1, ws, seedContext);
  }

  // downscale pasted images: full screenshots blow up localStorage quotas
  // and model context windows, and vision models don't need more than this
  async function imageFileToDataUrl(f: File | Blob): Promise<string> {
    const bitmap = await createImageBitmap(f);
    const MAX = 1568;
    const scale = Math.min(1, MAX / Math.max(bitmap.width, bitmap.height));
    const w = Math.max(1, Math.round(bitmap.width * scale));
    const h = Math.max(1, Math.round(bitmap.height * scale));
    const canvas = document.createElement("canvas");
    canvas.width = w;
    canvas.height = h;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      bitmap.close();
      throw new Error("canvas unavailable");
    }
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close();
    return canvas.toDataURL("image/jpeg", 0.85);
  }

  async function onPaste(e: ClipboardEvent) {
    const files = pastedImageFiles(e);
    if (!files.length) return;
    // image paste: attach thumbnails instead of dumping anything as text
    e.preventDefault();
    for (const f of files) {
      try {
        attachImages = [...attachImages, await imageFileToDataUrl(f)];
      } catch {
        error = `Couldn't read pasted image (${f.name || f.type || "unknown type"})`;
      }
    }
    autosizeComposer();
    requestAnimationFrame(() => composerEl?.focus());
  }

  function pastedImageFiles(e: ClipboardEvent): File[] {
    const items = e.clipboardData?.items;
    if (!items) return [];
    const files: File[] = [];
    for (const it of items) {
      if (it.type.startsWith("image/")) {
        const f = it.getAsFile();
        if (f) files.push(f);
      }
    }
    return files;
  }

  async function onEditPaste(e: ClipboardEvent) {
    const files = pastedImageFiles(e);
    if (!files.length) return;
    e.preventDefault();
    for (const f of files) {
      try {
        editImages = [...editImages, await imageFileToDataUrl(f)];
      } catch {
        error = `Couldn't read pasted image (${f.name || f.type || "unknown type"})`;
      }
    }
  }

  /** shared ingest for picked/dropped files: images join the vision
   *  attachments, everything else becomes a per-session staged upload */
  async function ingestDroppedFiles(files: File[]) {
    for (const f of files) {
      if (f.type.startsWith("image/")) {
        try {
          attachImages = [...attachImages, await imageFileToDataUrl(f)];
        } catch {
          error = `Couldn't read image (${f.name || "unknown type"})`;
        }
        continue;
      }
      if (f.size > 16 * 1024 * 1024) {
        error = `"${f.name}" is over 16MB — skipped.`;
        continue;
      }
      if (attachFiles.length >= 20) {
        error = "Too many attached files (max 20) — remove some first.";
        break;
      }
      const base = f.name.split("/");
      const name = base[base.length - 1] || f.name;
      if (attachFiles.some((a) => a.name === name)) {
        attachFiles = attachFiles.filter((a) => a.name !== name);
      }
      attachFileSeq += 1;
      attachFiles = [...attachFiles, { id: attachFileSeq, name, size: f.size, file: f }];
    }
    autosizeComposer();
    requestAnimationFrame(() => composerEl?.focus());
  }

  async function onFile(e: Event) {
    const el = e.target as HTMLInputElement;
    const files = el.files ? [...el.files] : [];
    el.value = "";
    if (!files.length) return;
    await ingestDroppedFiles(files);
  }

  // Desktop webview zoom (Cmd/Ctrl +/−/0). Plain browsers zoom natively and
  // never reach the backend: the chord handler below returns early there so
  // the browser default is untouched.
  let zoomLevel = $state(initialPrefs.zoom);
  async function applyZoom(factor: number) {
    zoomLevel = Math.min(3, Math.max(0.5, factor));
    persistPrefs();
    if (!isTauriRuntime()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      zoomLevel = await invoke<number>("zoom_apply", { factor: zoomLevel });
    } catch {
      // backend zoom unavailable: the persisted level still applies next launch
    }
  }
  async function stepZoom(direction: "in" | "out" | "reset") {
    if (!isTauriRuntime()) return;
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      zoomLevel =
        direction === "reset"
          ? await invoke<number>("zoom_apply", { factor: 1 })
          : await invoke<number>("zoom_step", { direction });
      persistPrefs();
    } catch {
      // ignore: zoom stays where it was
    }
  }

  // Cmd/Ctrl +/−/0 adjusts the desktop webview zoom (browsers do this natively)
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if (!isTauriRuntime()) return;
      if (!(e.metaKey || e.ctrlKey) || e.altKey) return;
      const k = e.key.toLowerCase();
      if (k === "+" || k === "=") {
        e.preventDefault();
        void stepZoom("in");
      } else if (k === "-" || k === "_") {
        e.preventDefault();
        void stepZoom("out");
      } else if (k === "0") {
        e.preventDefault();
        void stepZoom("reset");
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  // Esc twice stops a running Work turn; the first press only arms it. Open
  // overlays own their Esc, so bail out when any of them is up.
  $effect(() => {
    const h = (e: KeyboardEvent) => {
      if (e.key !== "Escape" || e.defaultPrevented) return;
      if (expCtx) {
        closeExpCtx();
        e.preventDefault();
        return;
      }
      if (newTabMenu) {
        newTabMenu = false;
        e.preventDefault();
        return;
      }
      if (mcpOpen) {
        mcpOpen = false;
        e.preventDefault();
        return;
      }
      if (settingsOpen || gotoOpen || lightboxSrc || editingIndex != null || renamingId || pendingDeleteId || modelOpen) return;
      if (!workRunning) return;
      if (onEscape()) e.preventDefault();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  function setSidebar(open: boolean) {
    sidebarOpen = open;
    persistPrefs();
  }

  onMount(() => {
    window.addEventListener("beforeunload", handleUnload);
    void initApp();
    // deep links (Copy Link in desktop mode) arrive here, both at launch
    // (getCurrent) and while running (onOpenUrl, e.g. link opened in Safari)
    let unlistenDeepLink: (() => void) | null = null;
    if (isTauriRuntime()) {
      void import("@tauri-apps/plugin-deep-link")
        .then(({ onOpenUrl, getCurrent }) => {
          getCurrent()
            .then((urls) => handleDeepLinkUrls(urls))
            .catch(() => {});
          onOpenUrl((urls: string[]) => handleDeepLinkUrls(urls))
            .then((u) => {
              unlistenDeepLink = u;
            })
            .catch(() => {});
        })
        .catch(() => {});
    }
    return () => {
      window.removeEventListener("beforeunload", handleUnload);
      unlistenDeepLink?.();
    };
  });

  onDestroy(() => {
    aborter?.abort();
  });

  async function initApp() {
    try {
      await initStorage();
      const prefs = await loadPrefs();
      theme = prefs.theme;
      accent = ACCENTS[prefs.accent] ? prefs.accent : "orange";
      fontSize = prefs.fontSize;
      stopWords = [...prefs.stopWords];
      stopWordsText = stopWordsToText(stopWords);
      ollamaBase = prefs.ollamaBase;
      setOllamaBase(ollamaBase);
      defaultWorkspace = prefs.defaultWorkspace ?? "";
      pendingWorkspace = prefs.defaultWorkspace ?? "";
      // "/" was the old fresh-install default and is no longer a valid
      // workspace (backend refuses the filesystem root), so treat a
      // stored "/" as unset (pick a folder).
      if (defaultWorkspace === "/" || pendingWorkspace === "/") {
        defaultWorkspace = "";
        pendingWorkspace = "";
      }
      zoomLevel = prefs.zoom;
      if (isTauriRuntime() && zoomLevel !== 1) void applyZoom(zoomLevel);
      thinkLevel = prefs.thinkLevel ?? "auto";
      // narrow screens start collapsed regardless of the saved preference
      sidebarOpen = window.innerWidth < 700 ? false : prefs.sidebarOpen;
      chats = await loadChats();
      // reopen where the user left off (saved on every navigation); the
      // draft below then restores their unsent text into the composer
      const savedActive = await loadActiveChat();
      // empty drafts are never listed: drop any persisted from older
      // builds so a stale "New session" row can't linger in the sidebar
      if (chats.some((c) => c.messages.length === 0)) {
        chats = chats.filter((c) => c.messages.length > 0);
        save();
      }
      if (sessionId) {
        // deep link: only open the chat named in the URL
        if (chats.some((c) => c.id === sessionId)) {
          activeId = sessionId;
        } else {
          void goto("/");
        }
      } else if (savedActive && chats.some((c) => c.id === savedActive)) {
        // relaunch where you left off: navigate to the last session and let
        // the route effect below activate it (setting activeId here directly
        // would race it — the route still shows home at this point, so it
        // would read that as an external nav home and wipe the restore)
        void goto(`/s/${savedActive}`);
      } else {
        // home: no active session
        activeId = null;
      }
      // app launched via a vicuna:// link: the listener stashed it above
      if (pendingDeepLinkId) {
        const id = pendingDeepLinkId;
        pendingDeepLinkId = null;
        if (chats.some((c) => c.id === id)) {
          activeId = id;
          isPinned = true;
          void goto(`/s/${id}`);
        }
      }
      // app launched via `vicuna [path]`: raise a work session there
      if (pendingDeepLinkPath) {
        const p = pendingDeepLinkPath;
        pendingDeepLinkPath = null;
        await openWorkspacePath(p);
      }
      // restore the unsent composer text (one global box, not per session)
      input = readDraft(null);
      const savedCatalog = await loadCatalog();
      if (savedCatalog) {
        catalog = savedCatalog.entries;
        catalogUpdated = savedCatalog.updated;
      }
      const m = await loadModel();
      if (m) selectedModel = m;
      await loadMcpServers();
    } catch (e) {
      console.error("storage load failed:", e);
    } finally {
      ready = true;
    }
    refreshModels();
    // resume a stream interrupted by a refresh: the partial tokens persisted
    // via the regular save, the pending marker tells us how to continue it
    // (user-stopped streams clear the marker, so they never resume here)
    const pending = readPendingStream();
    if (pending && (!sessionId || sessionId === pending.chatId)) {
      const pc = chats.find((c) => c.id === pending.chatId);
      const pm = pc?.messages[pending.index];
      if (pc && pm && pm.role === "assistant") {
        activeId = pc.id;
        if (pending.model) {
          selectedModel = pending.model;
          void saveModel(pending.model);
        }
        error = null;
        if (pending.mode === "fresh") {
          // fresh generations restart from scratch (history excludes the
          // placeholder): drop the stale partial first, or the resumed run
          // regenerates those tokens and duplicates them on append
          pm.content = "";
          pm.thinking = "";
          chats = [...chats];
        } else if (!pm.content) {
          // nothing answered yet: drop the stale partial thinking so the
          // resumed generation re-thinks cleanly instead of doubling it
          pm.thinking = "";
          chats = [...chats];
        }
        const resumeHistory: ChatMsg[] = compactedHistory(
          pending.mode === "continue" ? pc.messages.slice(0, pending.index + 1) : pc.messages.slice(0, pending.index)
        );
        if (pending.mode === "continue") continuing = true;
        try {
          await streamInto(pc, pending.index, resumeHistory, false, {
            think: pending.think,
            temperature: pending.temperature,
            stop: pending.stop,
            mode: pending.mode
          });
        } finally {
          continuing = false;
        }
      } else {
        clearPendingStream();
      }
    }
  }
</script>

{#if ready}
<div class="app">
  {#if sidebarOpen}
    <aside
      class="sidebar"
      data-testid="sidebar"
      oncontextmenu={(e) => {
        // right-click on empty sidebar space just closes the menu
        if (e.defaultPrevented) return;
        e.preventDefault();
        ctxMenu = null;
      }}
    >
      <div class="topbar">
        <button class="icon-btn" data-testid="sidebar-toggle" aria-label="Toggle sidebar" title="Toggle sidebar ({isMac ? '⌘B' : 'Ctrl+B'})" onclick={() => setSidebar(false)}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="2" y="3" width="16" height="14" rx="2"/><line x1="8" y1="3" x2="8" y2="17"/></svg>
        </button>
      </div>

      <div class="side-actions">
        <button class="newchat" data-testid="new-chat-btn" title="New ({isMac ? '⌘N' : 'Ctrl+N'})" onclick={newChat}>
          <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg>
          <span>New</span>
          <span class="kbd-hint">{isMac ? "⌘N" : "Ctrl+N"}</span>
        </button>
        <button class="goto" data-testid="goto-btn" title="Search chats ({isMac ? '⌘P' : 'Ctrl+P'})" onclick={openGoto}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
          <span>Search</span>
          <span class="kbd-hint">{isMac ? "⌘P" : "Ctrl+P"}</span>
        </button>
      </div>

      <div class="history">
        {#snippet chatList(items: Chat[])}
          {#each items as c (c.id)}
            {@const live = isSessionStreaming(c.id)}
            <button
              class="chat-item {c.id === activeId ? 'active' : ''} {live ? 'streaming' : ''}"
              data-testid="chat-item"
              data-streaming={live ? "true" : null}
              title={live ? liveTitle(c.id) : undefined}
              onclick={() => selectChat(c.id)}
              oncontextmenu={(e) => openMenu(e, c.id)}
            >
              {#if renamingId === c.id}
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
                <span class="title {live ? 'shimmer' : ''}">{c.title}</span>
                {@const liveMs = live ? liveElapsedMs(c.id) : null}
                {#if liveMs != null}<span class="live-elapsed" data-testid="live-elapsed">{formatLiveElapsed(liveMs)}</span>{/if}
                {#if c.mode === "work"}
                  {@const wsBase = workspaceBase(c.workspace ?? "")}
                  {#if wsBase}
                    <span class="work-badge ws-badge" title={displayWorkspace(c.workspace ?? "")}>
                      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                      <span>{wsBase}</span>
                    </span>
                  {:else}
                    <span class="work-badge" title="Work session">Work</span>
                  {/if}
                {/if}
              {/if}
            </button>
          {/each}
        {/snippet}
        {#if grouped.today.length > 0}
          <div class="group-label">Today</div>
          {@render chatList(grouped.today)}
        {/if}
        {#if grouped.yesterday.length > 0}
          <div class="group-label">Yesterday</div>
          {@render chatList(grouped.yesterday)}
        {/if}
        {#if grouped.week.length > 0}
          <div class="group-label">This week</div>
          {@render chatList(grouped.week)}
        {/if}
        {#if grouped.older.length > 0}
          <div class="group-label">Older</div>
          {@render chatList(grouped.older)}
        {/if}
        {#if grouped.today.length + grouped.yesterday.length + grouped.week.length + grouped.older.length === 0}
          <div class="group-label">Today</div>
          <div class="empty-hint">No chats yet</div>
        {/if}
      </div>
      <div class="side-footer">
        <button class="goto" data-testid="settings-btn" aria-label="Settings" title="Settings" onclick={openSettings}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path fill="currentColor" stroke="none" d="M12 21a8.985 8.985 0 0 1-1.755-.173 1 1 0 0 1-.791-.813l-.273-1.606a6.933 6.933 0 0 1-1.32-.762l-1.527.566a1 1 0 0 1-1.1-.278 8.977 8.977 0 0 1-1.756-3.041 1 1 0 0 1 .31-1.092l1.254-1.04a6.979 6.979 0 0 1 0-1.524L3.787 10.2a1 1 0 0 1-.31-1.092 8.977 8.977 0 0 1 1.756-3.042 1 1 0 0 1 1.1-.278l1.527.566a6.933 6.933 0 0 1 1.32-.762l.274-1.606a1 1 0 0 1 .791-.813 8.957 8.957 0 0 1 3.51 0 1 1 0 0 1 .791.813l.273 1.606a6.933 6.933 0 0 1 1.32.762l1.527-.566a1 1 0 0 1 1.1.278 8.977 8.977 0 0 1 1.756 3.041 1 1 0 0 1-.31 1.092l-1.254 1.04a6.979 6.979 0 0 1 0 1.524l1.254 1.04a1 1 0 0 1 .31 1.092 8.977 8.977 0 0 1-1.756 3.041 1 1 0 0 1-1.1.278l-1.527-.566a6.933 6.933 0 0 1-1.32.762l-.273 1.606a1 1 0 0 1-.791.813A8.985 8.985 0 0 1 12 21zm-.7-2.035a6.913 6.913 0 0 0 1.393 0l.247-1.451a1 1 0 0 1 .664-.779 4.974 4.974 0 0 0 1.696-.975 1 1 0 0 1 1.008-.186l1.381.512a7.012 7.012 0 0 0 .7-1.206l-1.133-.939a1 1 0 0 1-.343-.964 5.018 5.018 0 0 0 0-1.953 1 1 0 0 1 .343-.964l1.124-.94a7.012 7.012 0 0 0-.7-1.206l-1.38.512a1 1 0 0 1-1-.186 4.974 4.974 0 0 0-1.688-.976 1 1 0 0 1-.664-.779l-.248-1.45a6.913 6.913 0 0 0-1.393 0l-.25 1.45a1 1 0 0 1-.664.779A4.974 4.974 0 0 0 8.7 8.24a1 1 0 0 1-1 .186l-1.385-.512a7.012 7.012 0 0 0-.7 1.206l1.133.939a1 1 0 0 1 .343.964 5.018 5.018 0 0 0 0 1.953 1 1 0 0 1-.343.964l-1.128.94a7.012 7.012 0 0 0 .7 1.206l1.38-.512a1 1 0 0 1 1 .186 4.974 4.974 0 0 0 1.688.976 1 1 0 0 1 .664.779zm.7-3.725a3.24 3.24 0 0 1 0-6.48 3.24 3.24 0 0 1 0 6.48zm0-4.48A1.24 1.24 0 1 0 13.24 12 1.244 1.244 0 0 0 12 10.76z"/></svg>
          <span>Settings</span>
        </button>
      </div>
      {#if ctxMenu}
        <button
          class="ctx-backdrop"
          data-testid="session-menu-backdrop"
          aria-label="Close menu"
          onclick={() => (ctxMenu = null)}
          oncontextmenu={(e) => {
            e.preventDefault();
            ctxMenu = null;
          }}
          tabindex="-1"
        ></button>
        <div
          class="ctx-menu"
          data-testid="session-menu"
          style="left: {ctxMenu.x}px; top: {ctxMenu.y}px;"
        >
          <button class="ctx-item" data-testid="rename-item" onclick={() => startRename(ctxMenu!.chatId)}>Rename</button>
          <button class="ctx-item" data-testid="copy-link-item" onclick={() => copySessionLink(ctxMenu!.chatId)}>Copy Link</button>
          <button class="ctx-item" data-testid="continue-work-item" onclick={() => continueInWork(ctxMenu!.chatId)}>Continue in Work</button>
          <button class="ctx-item danger" data-testid="delete-item" onclick={() => requestDelete(ctxMenu!.chatId)}>Delete</button>
        </div>
      {/if}
    </aside>
  {/if}

  <main class="main">
    {#if composerMode === "work"}
      <button
        class="icon-btn explorer-toggle-btn"
        data-testid="explorer-toggle"
        aria-label="Toggle file explorer"
        title="File explorer"
        aria-pressed={explorerOpen}
        onclick={toggleExplorer}
      >
        <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="2" y="3" width="16" height="14" rx="2"/><line x1="12" y1="3" x2="12" y2="17"/></svg>
      </button>
    {/if}
    {#snippet forkNote(title: string, sourceId: string, prefix = "Forked from")}
      {@const source = chats.find((c) => c.id === sourceId)}
      <div class="fork-note" data-testid="fork-note">
        <span>{prefix} </span>
        {#if source}
          <button class="fork-link" data-testid="fork-source-link" onclick={() => selectChat(source.id)}>{title}</button>
        {:else}
          <span>{title}</span>
        {/if}
      </div>
    {/snippet}
    {#if !sidebarOpen}
      <div class="collapsed-rail">
        <button class="icon-btn" data-testid="sidebar-toggle" aria-label="Open sidebar" title="Toggle sidebar ({isMac ? '⌘B' : 'Ctrl+B'})" onclick={() => setSidebar(true)}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="2" y="3" width="16" height="14" rx="2"/><line x1="8" y1="3" x2="8" y2="17"/></svg>
        </button>
        <button class="icon-btn" data-testid="settings-btn-collapsed" aria-label="Open settings" title="Settings" onclick={openSettings}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path fill="currentColor" stroke="none" d="M12 21a8.985 8.985 0 0 1-1.755-.173 1 1 0 0 1-.791-.813l-.273-1.606a6.933 6.933 0 0 1-1.32-.762l-1.527.566a1 1 0 0 1-1.1-.278 8.977 8.977 0 0 1-1.756-3.041 1 1 0 0 1 .31-1.092l1.254-1.04a6.979 6.979 0 0 1 0-1.524L3.787 10.2a1 1 0 0 1-.31-1.092 8.977 8.977 0 0 1 1.756-3.042 1 1 0 0 1 1.1-.278l1.527.566a6.933 6.933 0 0 1 1.32-.762l.274-1.606a1 1 0 0 1 .791-.813 8.957 8.957 0 0 1 3.51 0 1 1 0 0 1 .791.813l.273 1.606a6.933 6.933 0 0 1 1.32.762l1.527-.566a1 1 0 0 1 1.1.278 8.977 8.977 0 0 1 1.756 3.041 1 1 0 0 1-.31 1.092l-1.254 1.04a6.979 6.979 0 0 1 0 1.524l1.254 1.04a1 1 0 0 1 .31 1.092 8.977 8.977 0 0 1-1.756 3.041 1 1 0 0 1-1.1.278l-1.527-.566a6.933 6.933 0 0 1-1.32.762l-.273 1.606a1 1 0 0 1-.791.813A8.985 8.985 0 0 1 12 21zm-.7-2.035a6.913 6.913 0 0 0 1.393 0l.247-1.451a1 1 0 0 1 .664-.779 4.974 4.974 0 0 0 1.696-.975 1 1 0 0 1 1.008-.186l1.381.512a7.012 7.012 0 0 0 .7-1.206l-1.133-.939a1 1 0 0 1-.343-.964 5.018 5.018 0 0 0 0-1.953 1 1 0 0 1 .343-.964l1.124-.94a7.012 7.012 0 0 0-.7-1.206l-1.38.512a1 1 0 0 1-1-.186 4.974 4.974 0 0 0-1.688-.976 1 1 0 0 1-.664-.779l-.248-1.45a6.913 6.913 0 0 0-1.393 0l-.25 1.45a1 1 0 0 1-.664.779A4.974 4.974 0 0 0 8.7 8.24a1 1 0 0 1-1 .186l-1.385-.512a7.012 7.012 0 0 0-.7 1.206l1.133.939a1 1 0 0 1 .343.964 5.018 5.018 0 0 0 0 1.953 1 1 0 0 1-.343.964l-1.128.94a7.012 7.012 0 0 0 .7 1.206l1.38-.512a1 1 0 0 1 1 .186 4.974 4.974 0 0 0 1.688.976 1 1 0 0 1 .664.779zm.7-3.725a3.24 3.24 0 0 1 0-6.48 3.24 3.24 0 0 1 0 6.48zm0-4.48A1.24 1.24 0 1 0 13.24 12 1.244 1.244 0 0 0 12 10.76z"/></svg>
        </button>
      </div>
    {/if}

    {#if !activeChat || activeChat.messages.length === 0}
      <div class="empty">
        <div class="seg mode-toggle" role="tablist" aria-label="Session mode" data-testid="mode-toggle">
          <button class="seg-btn {pendingMode === 'chat' ? 'sel' : ''}" data-testid="mode-chat-btn" role="tab" onclick={() => { pendingMode = "chat"; workspaceError = null; if (activeChat && activeChat.messages.length === 0) { activeChat.mode = "chat"; activeChat.workspace = null; } }}>Chat</button>
          <button class="seg-btn {pendingMode === 'work' ? 'sel' : ''}" data-testid="mode-work-btn" role="tab" onclick={() => { pendingMode = "work"; if (activeChat && activeChat.messages.length === 0) { activeChat.mode = "work"; activeChat.workspace = pendingWorkspace || null; } }}>Work</button>
        </div>
        <h1 class="empty-title">{pendingMode === "work" ? "What should I work on?" : "How can I help you today?"}</h1>
        <div class="suggest-grid" data-testid="empty-suggestions">
          {#each composerMode === "work" ? WORK_SUGGESTIONS : SUGGESTIONS as s, idx}
            <button class="suggest-card" data-testid="suggest-card" onclick={() => useSuggestion(s.prompt)}>
              <span class="suggest-text">
                <span class="suggest-title">{s.title}</span>
                <span class="suggest-sub">{s.prompt}</span>
              </span>
              <span class="suggest-icon">{@html s.icon}</span>
            </button>
          {/each}
        </div>
      </div>
    {:else}
      {#if activeChat.mode === "work"}
        <div class="work-banner" data-testid="work-banner">
          <span class="work-badge">Work</span>
          <span class="work-path" title={activeChat.workspace ? displayWorkspace(activeChat.workspace) : ""}>{activeChat.workspace ? displayWorkspace(activeChat.workspace) : "no workspace"}</span>
          {#if workCtx}
            {@const pct = Math.round(workCtx.frac * 100)}
            <span class="ctx-wrap" data-testid="ctx-ring" role="img" aria-label={`Context usage ${pct}%, ${workCtx.used.toLocaleString()} tokens`}>
              <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
                <circle cx="9" cy="9" r="7" fill="none" stroke="var(--border-2)" stroke-width="2.5" />
                <circle cx="9" cy="9" r="7" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray={`${(workCtx.frac * 43.98).toFixed(1)} 44`} transform="rotate(-90 9 9)" />
              </svg>
              <span class="ctx-popup" data-testid="ctx-popup">
                <span class="ctx-row"><span>Usage</span><span>{pct}%</span></span>
                <span class="ctx-row"><span>Tokens</span><span>{workCtx.used.toLocaleString()}</span></span>
              </span>
            </span>
          {/if}
        </div>
        {#if goalNow || goalPromptSession === activeId}
          {@const goalLive = goalNow && workRunning && workChatId === activeId}
          <div class="goal-wrap">
            <div class="goal-strip" data-testid="goal-strip">
              <span class="goal-badge">Goal</span>
              <span class="goal-text" title={goalNow?.text ?? ""}>{goalNow ? goalNow.text : "<no goal yet>"}</span>
              {#if goalNow}
                <span class="goal-state {goalLive && !goalNow.paused ? 'shimmer' : ''}" data-testid="goal-status">{goalNow.paused ? "paused" : goalLive ? "running" : "stopped"}</span>
              {:else}
                <button class="goal-badge goal-btn goal-dim" data-testid="goal-dismiss-btn" title="Dismiss goal header" onclick={() => (goalPromptSession = null)}>Dismiss</button>
              {/if}
              <button class="goal-badge goal-btn" data-testid="goal-configure-btn" title="Manage goal" onclick={() => { goalDraft = goalNow?.text ?? ""; goalOpen = true; }}>Configure</button>
            </div>
            {#if goalOpen}
              <button class="model-backdrop" data-testid="goal-backdrop" aria-label="Close goal settings" onclick={() => (goalOpen = false)} tabindex="-1"></button>
              <div class="goal-popup" data-testid="goal-popup" role="dialog" aria-label="Goal settings" tabindex="-1" onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); goalOpen = false; } }}>
                <div class="goal-edit-row">
                  <input
                    class="pull-input"
                    data-testid="goal-input"
                    placeholder="Session objective…"
                    bind:value={goalDraft}
                    onkeydown={(e) => { if (e.key === "Enter") { e.preventDefault(); goalCommand("replace " + goalDraft.trim()); goalOpen = false; } }}
                  />
                  <button class="mini-btn primary" data-testid="goal-save-btn" disabled={goalDraft.trim() === ""} onclick={() => { goalCommand("replace " + goalDraft.trim()); goalOpen = false; }}>Save</button>
                </div>
                {#if goalNow}
                  <div class="goal-actions-row">
                    {#if goalNow.paused}
                      <button class="mini-btn" data-testid="goal-resume-btn" onclick={() => goalCommand("resume")}>Resume</button>
                    {:else}
                      <button class="mini-btn" data-testid="goal-pause-btn" onclick={() => goalCommand("pause")}>Pause</button>
                    {/if}
                    <button class="mini-btn" data-testid="goal-clear-btn" onclick={() => goalCommand("clear")}>Clear</button>
                  </div>
                {/if}
              </div>
            {/if}
          </div>
        {/if}
      {/if}
      <div
        class="messages"
        bind:this={messagesEl}
        data-testid="messages"
        role="log"
        aria-label="Chat messages"
        onscroll={onMessagesScroll}
      >
        {#if forkInfo && forkNoteIndex == null}
          {@render forkNote(forkInfo.title, forkInfo.id, "Continue from chat")}
        {/if}
        {#each activeChat.messages as m, i}
          {@const copyKey = `${activeId}:${i}`}
          {#if m.role === "user"}
            <div class="row user-row {editingIndex === i ? 'is-editing' : ''}" data-msg-index={i}><div class="col">
              {#if editingIndex === i}
                <div class="editbox" data-testid="edit-box">
                  {#if editImages.length}
                    <div class="image-strip" data-testid="edit-image-strip">
                      {#each editImages as img, k}
                        <div class="image-chip">
                          <button class="image-open" aria-label="View attached image {k + 1} fullscreen" onclick={() => openLightbox(img)}>
                            <img src={img} alt="" />
                          </button>
                          <button class="image-remove" aria-label="remove image {k + 1}" onclick={() => { editImages = editImages.filter((_, j) => j !== k); }}>×</button>
                        </div>
                      {/each}
                    </div>
                  {/if}
                  <textarea data-testid="edit-input" bind:value={editDraft} rows={Math.min(12, Math.max(3, editDraft.split("\n").length + 1))} onkeydown={onEditKey} onpaste={onEditPaste}></textarea>
                  <div class="edit-actions">
                    <span class="edit-hint">Esc to cancel · ⌘↵ to save · paste images to attach</span>
                    <button class="edit-cancel" data-testid="cancel-edit-btn" onclick={cancelEdit}>Cancel</button>
                    <button class="edit-fork" data-testid="fork-save-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => void forkAndSave()}>Fork &amp; Save</button>
                    <button class="edit-save" data-testid="save-edit-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => saveOrConfirm()}>Save</button>
                  </div>
                </div>
              {:else}
              {#if m.images?.length}
                <div class="msg-images">
                  {#each m.images as img, k}
                    <button class="msg-image-open" aria-label="View attached picture {k + 1} fullscreen" onclick={() => openLightbox(img)}>
                      <img src={img} alt="" />
                    </button>
                  {/each}
                </div>
              {/if}
              <div class="bubble user">{@html renderUserMessage(m.content)}</div>
              <div class="actions">
                <button class="act" data-testid="copy-btn" aria-label="Copy message" title="Copy" onclick={() => copyMessage(i)}>
                  {#if copiedKey === copyKey}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>{/if}
                </button>
                <button class="act" data-testid="edit-btn" aria-label="Edit message" title="Edit" disabled={busy} onclick={() => startEdit(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
                </button>
                <button class="act" data-testid="msg-delete-btn" aria-label="Delete message" title="Delete" disabled={busy} onclick={() => requestMsgDelete(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
                </button>
              </div>
              {/if}
            </div></div>
          {:else}
            {@const isLast = i === activeChat.messages.length - 1}
            {@const thinkingActive = streaming && isLast && !m.content && !continuing}
            {@const isWorkMsg = (activeChat?.mode ?? "chat") === "work"}
            <!-- work turns keep thinking collapsed between rounds, so the label
                 stays shimmering for the whole turn, not just the first stream -->
            {@const thinkingLive = (streaming || workRunning) && isLast && !continuing && (thinkingActive || isWorkMsg)}
            <div class="row asst-row {editingIndex === i ? 'is-editing' : ''}" data-msg-index={i}><div class="col">
              {#if m.summary}
                <div class="compact-div" data-testid="compact-divider">
                  <Tooltip text="Messages above this line are no longer in context — the model only sees the summary below."><span>Session compacted</span></Tooltip>
                </div>
              {/if}
              {#if editingIndex === i}
                <div class="editbox" data-testid="edit-box">
                  {#if editImages.length}
                    <div class="image-strip" data-testid="edit-image-strip">
                      {#each editImages as img, k}
                        <div class="image-chip">
                          <button class="image-open" aria-label="View attached image {k + 1} fullscreen" onclick={() => openLightbox(img)}>
                            <img src={img} alt="" />
                          </button>
                          <button class="image-remove" aria-label="remove image {k + 1}" onclick={() => { editImages = editImages.filter((_, j) => j !== k); }}>×</button>
                        </div>
                      {/each}
                    </div>
                  {/if}
                  <textarea data-testid="edit-input" bind:value={editDraft} rows={Math.min(12, Math.max(3, editDraft.split("\n").length + 1))} onkeydown={onEditKey} onpaste={onEditPaste}></textarea>
                  <div class="edit-actions">
                    <span class="edit-hint">Esc to cancel · ⌘↵ to save · paste images to attach</span>
                    <button class="edit-cancel" data-testid="cancel-edit-btn" onclick={cancelEdit}>Cancel</button>
                    <button class="edit-fork" data-testid="fork-save-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => void forkAndSave()}>Fork &amp; Save</button>
                    <button class="edit-save" data-testid="save-edit-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => saveOrConfirm()}>Save</button>
                  </div>
                </div>
              {:else}
              <div class="bubble asst">
              {#if m.thinking}
                {@const expanded = (thinkingActive && !isWorkMsg) || (m.thinkingOpen ?? false)}
                <div class="thinking" data-testid="thinking-block">
                  <button
                    class="thinking-head"
                    data-testid="thinking-toggle"
                    aria-expanded={expanded}
                    onclick={() => { m.thinkingOpen = !expanded; chats = [...chats]; }}
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18h6"/><path d="M10 21h4"/><path d="M12 3a6 6 0 0 0-4 10.5c.8.7 1 1.5 1 2.5h6c0-1 .2-1.8 1-2.5A6 6 0 0 0 12 3z"/></svg>
                    <span class={thinkingLive ? "thinking-label shimmer" : "thinking-label"}>{thinkingActive ? "Thinking..." : "Thinking"}</span>
                    <span class="chev">{expanded ? "▾" : "▸"}</span>
                  </button>
                  {#if expanded}
                    <div class="thinking-body">{m.thinking}</div>
                  {/if}
                </div>
              {/if}
              <div class="answer">{#if m.content}{@html renderMarkdown(m.content)}{:else if streaming && isLast && !m.thinking}<span class="typing" aria-label="Generating"><span></span><span></span><span></span></span>{/if}</div>
              {#if pendingAsk && pendingAsk.chatId === activeChat?.id && pendingAsk.index === i}
                <div class="ask-card" data-testid="ask-card">
                  <div class="ask-question">{pendingAsk.question}</div>
                  <div class="ask-options">
                    {#each pendingAsk.options as opt, oi}
                      <button class="ask-option" data-testid="ask-option-{oi}" onclick={() => answerAsk(opt)}>{opt}</button>
                    {/each}
                    {#if pendingAsk.otherOpen}
                      <form
                        class="ask-other"
                        onsubmit={(e) => {
                          e.preventDefault();
                          const v = pendingAsk?.other.trim() ?? "";
                          if (v) answerAsk(v);
                        }}
                      >
                        <input
                          class="ask-input"
                          data-testid="ask-input"
                          bind:value={pendingAsk.other}
                          placeholder="Type your answer…"
                          aria-label="Custom answer"
                          onkeydown={(e) => {
                            if (e.key === "Escape") {
                              e.preventDefault();
                              e.stopPropagation();
                              if (pendingAsk) pendingAsk.otherOpen = false;
                            }
                          }}
                        />
                        <button class="ask-send" type="submit" data-testid="ask-send" disabled={!pendingAsk.other.trim()}>Send</button>
                        <button class="ask-dismiss" type="button" data-testid="ask-dismiss" onclick={dismissAsk}>Dismiss</button>
                      </form>
                    {:else}
                      <div class="ask-other ask-other-closed">
                        <button
                          class="ask-option ask-other-toggle"
                          data-testid="ask-other"
                          onclick={() => {
                            if (!pendingAsk) return;
                            pendingAsk.otherOpen = true;
                            requestAnimationFrame(() => document.querySelector<HTMLInputElement>('[data-testid="ask-input"]')?.focus());
                          }}
                        >Other…</button>
                        <button class="ask-dismiss" type="button" data-testid="ask-dismiss" onclick={dismissAsk}>Dismiss</button>
                      </div>
                    {/if}
                  </div>
                </div>
              {/if}
              <div class="actions">
                <button class="act" data-testid="copy-btn" aria-label="Copy message" title="Copy" onclick={() => copyMessage(i)}>
                  {#if copiedKey === copyKey}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>{/if}
                </button>
                <button class="act" data-testid="edit-btn" aria-label="Edit message" title="Edit" disabled={busy} onclick={() => startEdit(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
                </button>
                {#if isLast}
                  {@const noContinue = isCloudModel(selectedModel)}
                  {@const isWork = (activeChat?.mode ?? "chat") === "work"}
                  {#if !isWork}
                    <button class="act" data-testid="continue-btn" aria-label="Continue generating" title={noContinue ? "Continue isn't supported for cloud models" : "Continue"} disabled={streaming || noContinue} onclick={() => continueFrom(i)}>
                      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>
                    </button>
                  {/if}
                  <button class="act" data-testid="regen-btn" aria-label="Regenerate response" title={isWork ? "Re-run agent" : "Regenerate"} disabled={busy} onclick={() => void regenerate(i)}>
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>
                  </button>
                {/if}
                <button class="act" data-testid="fork-btn" aria-label="Fork chat from here" title="Fork" disabled={busy} onclick={() => forkAt(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.2"/><circle cx="6" cy="18" r="2.2"/><circle cx="18" cy="8" r="2.2"/><path d="M6 8.2v7.6"/><path d="M18 10.2c0 3.5-5 2.8-8.5 5.3"/></svg>
                </button>
                <button class="act" data-testid="msg-delete-btn" aria-label="Delete message" title="Delete" disabled={busy} onclick={() => requestMsgDelete(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
                </button>
                {#if m.model}
                  {@const liveTurn = workRunning && workChatId === activeChat?.id && isLast}
                  <span class="model-tag" data-testid="msg-model" title="Generated by {m.model}">{m.model}{#if !liveTurn && m.genMs != null && m.genMs > 0}{" "}{formatDuration(m.genMs)}{#if m.genTps != null}{" "}{m.genTps.toFixed(1)} tok/s{/if}{/if}</span>
                {/if}
              </div>
            </div>
            {/if}
          </div></div>
          {/if}
          {#if forkInfo && forkNoteIndex === i}
            {@render forkNote(forkInfo.title, forkInfo.id)}
          {/if}
        {/each}
      </div>
    {/if}

    {#if error}
      <div class="err" data-testid="error-box">{error}</div>
    {/if}
    {#if toolStatus && toolStatus.chatId === activeId}
      <div class="tool-status" data-testid="tool-status">
        <span class="tool-status-label shimmer">{toolStatus.label}</span>
      </div>
    {/if}

    <div class="composer-wrap">
      {#if queuedForActive.length}
        <div class="queue-strip" data-testid="queue-strip">
          {#each queuedMsgs as q, qi}
            {#if q.chatId === activeId}
              <div class="queue-chip">
                <span class="queue-text">{q.msg.content || "Image"}</span>
                <button class="queue-x" aria-label="Remove queued message" title="Remove from queue" onclick={() => unqueue(qi)}>×</button>
              </div>
            {/if}
          {/each}
          <span class="queue-hint">queued · joins at the next tool call</span>
        </div>
      {/if}
      {#if streaming || workRunning || compacting}
        <div class="stream-bar" data-testid="stream-bar" role="progressbar" aria-label="Working"><span></span></div>
      {/if}
      {#if escArmed}
        <div class="esc-hint" data-testid="esc-hint">Esc again to interrupt</div>
      {/if}
      {#if !isPinned}
        <button
          class="scroll-jump"
          data-testid="jump-bottom"
          aria-label="Scroll to bottom and resume auto-scroll"
          onclick={jumpToBottom}
        >↓</button>
      {/if}
      {#if activeChat?.mode === "work" && activeChat.messages.length > 0 && (!activeChat.workspace || workspaceEditing)}
        <div class="work-composer-bar" data-testid="work-composer-bar">
          <span class="work-badge">Work</span>
          <input
            class="pull-input workspace-input"
            data-testid="workspace-input-active"
            placeholder={isWindows ? "C:\\path\\to\\workspace" : "/absolute/path/to/workspace"}
            value={pendingWorkspace}
            spellcheck={false}
            onfocus={() => (workspaceEditing = true)}
            oninput={(e) => {
              const v = (e.target as HTMLInputElement).value;
              pendingWorkspace = v;
              if (activeChat) activeChat.workspace = v || null;
            }}
            onblur={() => { workspaceEditing = false; void validateWorkspace(pendingWorkspace).then((e) => (workspaceError = e)); }}
          />
        </div>
      {/if}
      <div
        class="composer {composerDropActive ? 'drop-active' : ''}"
        role="group"
        aria-label="Message composer — drop files here to attach them"
        data-testid="composer"
        ondragenter={(e) => {
          if (e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files")) {
            compDragDepth += 1;
            composerDropActive = true;
          }
        }}
        ondragover={(e) => e.preventDefault()}
        ondragleave={() => {
          compDragDepth = Math.max(0, compDragDepth - 1);
          if (compDragDepth === 0) composerDropActive = false;
        }}
        ondrop={(e) => void onComposerDrop(e)}
      >
        {#if attachImages.length}
          <div class="image-strip" data-testid="image-strip">
            {#each attachImages as img, k}
              <div class="image-chip" data-testid="image-chip">
                <button class="image-open" aria-label="View attached image {k + 1} fullscreen" onclick={() => openLightbox(img)}>
                  <img src={img} alt="" />
                </button>
                <button class="image-remove" aria-label="remove image {k + 1}" onclick={(e) => { e.stopPropagation(); attachImages = attachImages.filter((_, j) => j !== k); }}>×</button>
              </div>
            {/each}
          </div>
        {/if}
        {#if attachFiles.length}
          <div class="file-strip" data-testid="file-strip">
            {#each attachFiles as f, k}
              <div class="file-chip" data-testid="file-chip" title={f.name}>
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                <span class="file-name">{f.name}</span>
                <span class="exp-size">{formatSize(f.size)}</span>
                <button class="file-remove" aria-label="Remove attached file {f.name}" onclick={() => { attachFiles = attachFiles.filter((_, j) => j !== k); autosizeComposer(); }}>×</button>
              </div>
            {/each}
          </div>
        {/if}
        <div class="composer-field">
          <div class="composer-backdrop" data-testid="composer-backdrop" aria-hidden="true" bind:this={composerBackdrop}>{@html renderMentions(input)}</div>
          <textarea
            data-testid="message-input"
            placeholder={composerMode === "work" ? (workRunning ? "Queue a follow-up — it joins at the next tool call…" : "Ask to explore, run, or change workspace files… ( / to focus)") : "Send a message ( / to focus)"}
            bind:this={composerEl}
            bind:value={input}
            onkeydown={onKey}
            onpaste={onPaste}
            onscroll={syncBackdropScroll}
            oninput={() => {
              caretHopped = false;
              syncBackdropScroll();
              refreshMentionMenu();
              refreshSlashMenu();
            }}
            onclick={() => {
              caretHopped = false;
              snapComposerCaret();
              refreshMentionMenu();
              refreshSlashMenu();
            }}
            onkeyup={(e) => {
              // keydown may have consumed the popup (select/close/nav) while
              // the caret is still pre-action: skip that stale refresh
              if (popupKeyHandled) {
                popupKeyHandled = false;
                return;
              }
              refreshMentionMenu();
              refreshSlashMenu();
            }}
            onblur={() => {
              // picking a suggestion uses mousedown (no blur); any real
              // blur means the user clicked away — close the popup
              mention = null;
              slash = null;
            }}
            rows={1}
          ></textarea>
        </div>
        {#if slash}
          <div class="mention-menu" data-testid="slash-menu" role="listbox" aria-label="Commands">
            {#each slash.items as item, i (item.name)}
              <button
                class="mention-item {i === slash.index ? 'sel' : ''}"
                data-testid="slash-item"
                role="option"
                aria-selected={i === slash.index}
                title={item.desc}
                onmousedown={(e) => {
                  // before blur: keep focus in the composer
                  e.preventDefault();
                  if (slash) slash.index = i;
                  const item = slash?.items[i];
                  // same as Tab: bare args-taking commands complete
                  // ("/goal" → "/goal ") instead of running with no args
                  if (item?.takesArgs && !/\s\S/.test(input)) {
                    completeSlash();
                  } else {
                    runSlash();
                  }
                }}
                onmousemove={() => {
                  if (slash && slash.index !== i) slash.index = i;
                }}
              >
                <span class="mention-path">/{item.name}</span>
                <span class="slash-desc">{item.desc}</span>
              </button>
            {/each}
          </div>
        {:else if mention && mention.items.length}
          <div class="mention-menu" data-testid="mention-menu" role="listbox" aria-label="Mention a file">
            {#each mention.items as item, i (item.path)}
              <button
                class="mention-item {i === mention.index ? 'sel' : ''}"
                data-testid="mention-item"
                role="option"
                aria-selected={i === mention.index}
                title={item.path}
                onmousedown={(e) => {
                  // before blur: keep focus in the composer
                  e.preventDefault();
                  pickMention(item);
                }}
                onmousemove={() => {
                  if (mention && mention.index !== i) mention.index = i;
                }}
              >
                {#if item.is_dir}
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                {:else}
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                {/if}
                <span class="mention-path">{item.path}</span>
                {#if item.size != null}
                  <span class="exp-size">{formatSize(item.size)}</span>
                {/if}
              </button>
            {/each}
          </div>
        {:else if mention}
          <div class="mention-menu" data-testid="mention-menu">
            <div class="mention-empty">No matching files</div>
          </div>
        {/if}
        <div class="composer-bar">
          <div class="spacer"></div>
          {#if composerMode === "work"}
            <button class="icon-btn" data-testid="attach-btn" aria-label="Attach files" title="Attach files (images, documents, anything)" onclick={pickFile}>
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>
            </button>
            <button
              class="icon-btn mcp-btn"
              data-testid="mcp-btn"
              aria-label="MCP servers"
              title={mcpConnectedCount ? `MCP servers (${mcpConnectedCount} connected)` : "MCP servers"}
              aria-pressed={mcpOpen}
              onclick={openMcpPopup}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14.7 6.3a4.5 4.5 0 0 0-6 6L3 18l3 3 5.7-5.7a4.5 4.5 0 0 0 6-6L14 13l-3-3 3.7-3.7z"/></svg>
              {#if mcpConnectedCount}
                <span class="mcp-dot" data-testid="mcp-dot">{mcpConnectedCount}</span>
              {/if}
            </button>
            {@const wsPath = displayWorkspace(activeChat && activeChat.messages.length > 0 && activeChat.mode === "work" ? (activeChat.workspace ?? pendingWorkspace) : pendingWorkspace)}
            {@const wsName = wsPath.trim() ? (workspaceBase(wsPath) || wsPath.trim()) : ""}
            <button
              class="model-pill ws-pill"
              data-testid="workspace-pill"
              title={wsPath.trim() ? `Workspace: ${wsPath.trim()} — click to change` : "Pick a workspace folder"}
              aria-label="Pick workspace folder"
              onclick={() => void pickWorkspace()}
            >
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
              <span class="model-name">{wsName || "Workspace"}</span>
            </button>
          {/if}
          <div class="model-pill-wrap">
            <button class="model-pill" data-testid="model-selector" onclick={toggleModel}>
              <span class="model-name">{selectedModel}</span>
              <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 6l4 4 4-4"/></svg>
            </button>
            {#if modelOpen}
              <button class="model-backdrop" data-testid="model-backdrop" aria-label="Close model menu" onclick={() => (modelOpen = false)} tabindex="-1"></button>
              <div class="model-drop" data-testid="model-dropdown">
                <input
                  class="model-search"
                  data-testid="model-search"
                  placeholder="Find model..."
                  bind:value={modelQuery}
                  autofocus
                />
                <div class="model-list-drop">
                  {#each filteredModels as m}
                    <button
                      class="model-opt {m === selectedModel ? 'sel' : ''}"
                      onclick={() => { selectedModel = m; modelOpen = false; save(); }}
                    >{m}</button>
                  {:else}
                    <div class="no-match">No matches</div>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
          {#if thinkLevels.length > 0}
            <div class="model-pill-wrap">
              <button class="model-pill" data-testid="effort-selector" title="Reasoning effort for {selectedModel}" onclick={() => (effortOpen = !effortOpen)}>
                <span class="model-name">{shownThink === "auto" ? `Auto${thinkDefault ? ` · ${capLevel(thinkDefault)}` : ""}` : shownThink === "off" ? "Off" : capLevel(shownThink)}</span>
                <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 6l4 4 4-4"/></svg>
              </button>
              {#if effortOpen}
                <button class="model-backdrop" data-testid="effort-backdrop" aria-label="Close effort menu" onclick={() => (effortOpen = false)} tabindex="-1"></button>
                <div class="model-drop" data-testid="effort-dropdown">
                  <div class="model-list-drop">
                    <button
                      class="model-opt {shownThink === 'auto' ? 'sel' : ''}"
                      onclick={() => setThinkLevel("auto")}
                    >Auto{thinkDefault ? ` (${capLevel(thinkDefault)})` : ""}</button>
                    {#if thinkCanDisable}
                      <button
                        class="model-opt {shownThink === 'off' ? 'sel' : ''}"
                        onclick={() => setThinkLevel("off")}
                      >Off</button>
                    {/if}
                    {#each thinkLevels as l}
                      <button
                        class="model-opt {shownThink === l ? 'sel' : ''}"
                        onclick={() => setThinkLevel(l)}
                      >{capLevel(l)}</button>
                    {/each}
                  </div>
                </div>
              {/if}
            </div>
          {/if}
          {#if workRunning}
            <button class="send" data-testid="queue-btn" aria-label="Queue message" title="Queue for the next tool call" disabled={input.trim() === "" && attachImages.length === 0} onclick={send}>↑</button>
            <button class="send streaming" data-testid="stop-btn" aria-label="Stop" title="Stop (Esc twice)" onclick={stop}>■</button>
          {:else if streaming}
            <button class="send streaming" data-testid="stop-btn" aria-label="Stop" onclick={stop}>■</button>
          {:else}
            <button class="send" data-testid="send-btn" aria-label="Send" disabled={input.trim() === "" && attachImages.length === 0} onclick={send}>↑</button>
          {/if}
        </div>
      </div>
    </div>
    {#if mcpOpen}
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
              // Enter always activates the selected row
              e.preventDefault();
              e.stopPropagation();
              mcpActivateSelected();
            } else if (e.key === " " && mcpQuery.trim() === "") {
              // Space activates too when there is nothing to search —
              // the filter trims, so a leading space could never match
              // anything (and the footer advertises "connect space").
              // With actual query text Space keeps typing normally.
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
              title={describeMcpServer(s)}
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
              <span class="mcp-tools-n">{live?.connected ? `${live.tools.length} tool${live.tools.length === 1 ? "" : "s"}` : ""}</span>
              <span class="mcp-status {st.kind}">{st.label}</span>
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
            </div>
            {#if live?.error}
              <div class="mcp-row-error" title={live.error}>{live.error}</div>
            {/if}
          {:else}
            <div class="mcp-empty">
              {#if !isTauriRuntime()}
                MCP servers need the Vicuna desktop app — browsers can't spawn local commands.
              {:else}
                No MCP servers yet — add one below, e.g. a local `lb mcp` for the live browser.
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
            <label class="mcp-check"><input type="checkbox" data-testid="mcp-enabled-check" bind:checked={mcpForm.enabled} /> Connect automatically on launch</label>
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
          <span class="mcp-hint">connect <span class="mcp-key">space</span></span>
          <span class="mcp-spacer"></span>
          {#if mcpShowForm}
            <span class="mcp-file">~/vicuna/config/vicuna.json</span>
          {:else if mcpView === "custom"}
            <button class="mcp-add" data-testid="mcp-add-btn" onclick={() => openMcpForm()}>+ custom</button>
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
          {@const patCfg = mcpServers.find((s) => s.id === mcpPatFor)}
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
                    // keep caret/selection motion local to the field
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
    {/if}
  </main>
  {#if composerMode === "work"}
    <aside
      class="explorer {expResizing ? 'resizing' : ''}"
      class:hidden={!explorerOpen}
      data-testid="explorer"
      style="width: {explorerWidth}px"
      ondragover={(e) => e.preventDefault()}
      ondrop={(e) => e.preventDefault()}
    >
      <div
        class="exp-resize"
        data-testid="explorer-resize"
        role="slider"
        aria-orientation="vertical"
        aria-label="Resize side panel"
        aria-valuenow={explorerWidth}
        aria-valuemin={EXPLORER_MIN_W}
        aria-valuemax={560}
        tabindex="0"
        title="Drag to resize · double-click to reset"
        onpointerdown={onExpResizeStart}
        ondblclick={() => (explorerWidth = EXPLORER_DEFAULT_W)}
        onkeydown={onExpResizeKey}
      ></div>
      <div class="side-tabs" data-testid="side-tabs" role="tablist" aria-label="Side panel tabs">
        {#each sideTabs as t (t.key)}
          <div
            class="side-tab {t.key === activeSideTab ? 'sel' : ''}"
            data-testid="side-tab"
            role="tab"
            aria-selected={t.key === activeSideTab}
            tabindex="0"
            title={displayWorkspace(explorerRoot.trim()) || t.title}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") {
                e.preventDefault();
                switchSideTab(t.key);
              } else if (e.key === "Delete" || e.key === "Backspace") {
                e.preventDefault();
                closeSideTab(t.key);
              }
            }}
            onmousedown={(e) => {
              // middle-click must not autoscroll; auxclick below closes
              if (e.button === 1) e.preventDefault();
            }}
            onauxclick={(e) => {
              if (e.button === 1) {
                e.preventDefault();
                closeSideTab(t.key);
              }
            }}
          >
            <button
              class="side-tab-label"
              aria-label={t.kind === "files" ? "Files tab" : `${t.title} tab`}
              onclick={() => switchSideTab(t.key)}
            >
              {#if t.kind === "files"}
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
              {:else}
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>
              {/if}
              <span class="side-tab-title">{t.title}</span>
            </button>
            <button
              class="side-tab-x"
              data-testid="side-tab-close"
              aria-label="Close {t.title} tab"
              title="Close tab"
              onclick={(e) => {
                e.stopPropagation();
                closeSideTab(t.key);
              }}
            >×</button>
          </div>
        {/each}
        <button
          class="icon-btn side-add"
          data-testid="side-add"
          aria-label="New panel tab"
          title="New tab: Explorer or Terminal"
          onclick={() => (newTabMenu = !newTabMenu)}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M12 5v14M5 12h14"/></svg>
        </button>
        <div class="spacer"></div>
        <button
          class="icon-btn exp-btn"
          data-testid="explorer-close"
          aria-label="Close side panel"
          title="Close panel"
          onclick={() => (explorerOpen = false)}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>
        </button>
        {#if newTabMenu}
          <button
            class="ctx-backdrop"
            data-testid="new-tab-backdrop"
            aria-label="Close new tab menu"
            onclick={() => (newTabMenu = false)}
            tabindex="-1"
          ></button>
          <div class="ctx-menu new-tab-menu" data-testid="new-tab-menu">
            <button class="ctx-item" data-testid="new-tab-explorer" onclick={openSideExplorer}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
              <span>Explorer</span>
            </button>
            <button class="ctx-item" data-testid="new-tab-terminal" onclick={() => void openSideTerminal()}>
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>
              <span>Terminal</span>
            </button>
          </div>
        {/if}
      </div>
      {#if sideTabs.length === 0}
        <div class="exp-empty">No tabs open — press + to add an Explorer or Terminal.</div>
      {/if}
      {#each sideTabs as t (t.key)}
        <div class="side-view" class:hidden={t.key !== activeSideTab}>
          {#if t.kind === "files"}
            <div class="explorer-search">
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="8"/><path d="m21 21-4.3-4.3"/></svg>
              <input
                class="explorer-search-input"
                data-testid="explorer-search"
                placeholder="Search files"
                bind:value={explorerQuery}
                spellcheck={false}
              />
              <button
                class="icon-btn exp-btn"
                data-testid="explorer-refresh"
                aria-label="Refresh files"
                title="Refresh"
                disabled={explorerLoading}
                onclick={() => void loadExplorer()}
              >
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.64-6.36"/><polyline points="21 3 21 9 15 9"/></svg>
              </button>
            </div>
      <div class="explorer-list" data-testid="explorer-list">
        {#if explorerLoading}
          <div class="exp-empty">Loading…</div>
        {:else if explorerError}
          <div class="exp-empty">{explorerError}</div>
        {:else if explorerVisible.length === 0}
          <div class="exp-empty">{explorerQuery.trim() ? "No matching files." : "Empty folder."}</div>
        {:else}
          {#each explorerVisible as e (e.path)}
            {#if expRenaming === e.path}
              <div class="exp-row exp-renaming" style="padding-left: {10 + e.depth * 14}px">
                {#if e.is_dir}
                  <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1-2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                {:else}
                  <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                {/if}
                <input
                  class="exp-rename"
                  data-testid="explorer-rename"
                  bind:value={expRenameDraft}
                  aria-label="New name for {e.name}"
                  onclick={(ev) => ev.stopPropagation()}
                  onkeydown={(ev) => {
                    if (ev.key === "Enter") {
                      ev.preventDefault();
                      void commitExpRename();
                    } else if (ev.key === "Escape") {
                      ev.preventDefault();
                      ev.stopPropagation();
                      cancelExpRename();
                    }
                  }}
                  onblur={() => void commitExpRename()}
                />
              </div>
            {:else if e.is_dir}
              {@const collapsed = collapsedDirs.includes(e.path)}
              <button
                class="exp-row exp-dir"
                style="padding-left: {10 + e.depth * 14}px"
                data-testid="explorer-dir"
                title={e.path}
                onclick={() => toggleExplorerDir(e.path)}
                oncontextmenu={(ev) => openExpCtx(ev, e.path, true)}
              >
                <span class="exp-chev">{collapsed ? "▸" : "▾"}</span>
                <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                <span class="exp-name">{e.name}</span>
              </button>
            {:else}
              <button
                class="exp-row exp-file"
                style="padding-left: {10 + e.depth * 14}px"
                data-testid="explorer-file"
                title={e.path}
                onclick={() => insertExplorerPath(e.path)}
                oncontextmenu={(ev) => openExpCtx(ev, e.path, false)}
              >
                <svg class="exp-icon" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/></svg>
                <span class="exp-name">{e.name}</span>
                {#if e.size != null}
                  <span class="exp-size">{formatSize(e.size)}</span>
                {/if}
              </button>
            {/if}
          {/each}
        {/if}
      </div>
      {#if expNotice}
        <div class="explorer-toast" data-testid="explorer-notice" role="status">{expNotice}</div>
      {/if}
          {:else}
            {#if t.error}
              <div class="exp-empty">{t.error}</div>
            {:else}
              <div class="term-wrap" data-term={t.key} data-testid="terminal-view"></div>
            {/if}
          {/if}
        </div>
      {/each}
    </aside>
  {/if}
</div>
{#if expCtx}
  <button
    class="ctx-backdrop"
    data-testid="explorer-menu-backdrop"
    aria-label="Close file menu"
    onclick={closeExpCtx}
    oncontextmenu={(e) => {
      e.preventDefault();
      closeExpCtx();
    }}
    tabindex="-1"
  ></button>
  <div
    class="ctx-menu"
    data-testid="explorer-menu"
    style="left: {expCtx.x}px; top: {expCtx.y}px;"
  >
    <button class="ctx-item" data-testid="explorer-open" onclick={() => void expOpenFile()}>Open</button>
    <button class="ctx-item" data-testid="explorer-reveal" onclick={() => void expReveal()}>Reveal in Finder</button>
    <button class="ctx-item" data-testid="explorer-rename" onclick={renameFromMenu}>Rename</button>
    <button
      class="ctx-item {expCtx && expConfirmDelete === expCtx.path ? 'danger' : ''}"
      data-testid="explorer-delete"
      title={expCtx?.path ?? "Delete"}
      onclick={deleteFromMenu}
    >{expCtx && expConfirmDelete === expCtx.path ? "Confirm delete" : "Delete"}</button>
    {#if !expCtx.isDir}
      <button class="ctx-item" data-testid="explorer-copy-mention" onclick={expCopyMention}>Copy mention</button>
      <button class="ctx-item" data-testid="explorer-add-chat" onclick={expAddToChat}>Add to chat</button>
    {/if}
  </div>
{/if}
{#if lightboxSrc}
  <button
    class="lightbox-backdrop"
    data-testid="image-lightbox"
    aria-label="Close fullscreen image"
    onclick={closeLightbox}
  >
    <img src={lightboxSrc} alt="" />
  </button>
{/if}
{#if pendingDeleteId}
  <button
    class="confirm-backdrop"
    data-testid="delete-confirm-backdrop"
    aria-label="Cancel"
    onclick={() => (pendingDeleteId = null)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="delete-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm delete chat"
  >
    <p class="confirm-text">Are you sure you want to remove this chat?</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-delete-btn" onclick={() => (pendingDeleteId = null)}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-delete-btn" onclick={confirmDelete}>Confirm</button>
    </div>
  </div>
{/if}
{#if pendingMsgDelete != null && activeChat && activeChat.messages[pendingMsgDelete]}
  {@const delTarget = activeChat.messages[pendingMsgDelete]}
  {@const delCascadeNext = delTarget.role === "user" && activeChat.messages[pendingMsgDelete + 1]?.role === "assistant"}
  {@const delCascadePrev = delTarget.role === "assistant" && pendingMsgDelete > 0 && activeChat.messages[pendingMsgDelete - 1]?.role === "user"}
  <button
    class="confirm-backdrop"
    data-testid="delete-msg-backdrop"
    aria-label="Cancel"
    onclick={() => (pendingMsgDelete = null)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="delete-msg-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm delete message"
  >
    <p class="confirm-text">{delCascadeNext ? "Remove this message and the following reply?" : delCascadePrev ? "Remove this message and the question above it?" : "Are you sure you want to remove this message?"}</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-msg-delete-btn" onclick={() => (pendingMsgDelete = null)}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-msg-delete-btn" onclick={confirmMsgDelete}>Confirm</button>
    </div>
  </div>
{/if}
{#if pendingSaveConfirm && activeChat && editingIndex != null && editingIndex < activeChat.messages.length - 1}
  {@const saveAfterCount = activeChat.messages.length - 1 - editingIndex}
  <button
    class="confirm-backdrop"
    data-testid="edit-confirm-backdrop"
    aria-label="Cancel"
    onclick={cancelSaveConfirm}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card"
    data-testid="edit-confirm"
    role="alertdialog"
    aria-modal="true"
    aria-label="Confirm save"
  >
    <p class="confirm-text">Saving here will permanently remove the {saveAfterCount} message{saveAfterCount === 1 ? "" : "s"} after it.</p>
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-edit-choice-btn" onclick={cancelSaveConfirm}>Cancel</button>
      <button class="confirm-ok" data-testid="confirm-save-btn" onclick={confirmSaveHere}>Save here</button>
    </div>
  </div>
{/if}
{#if gotoOpen}
  <button
    class="confirm-backdrop"
    data-testid="goto-backdrop"
    aria-label="Cancel"
    onclick={closeGoto}
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
      <input
        class="search-input"
        data-testid="goto-input"
        placeholder="Search chats or paste a session link…"
        bind:value={gotoInput}
        autofocus
        onkeydown={onSearchKey}
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
      {#each searchItems as it, idx (it.kind === "new" ? "new" : it.chat.id)}
        {#if it.kind === "new"}
          <button
            class="search-row {idx === gotoSel ? 'sel' : ''}"
            role="option"
            aria-selected={idx === gotoSel}
            data-testid="search-new"
            onclick={() => openSearchItem(it)}
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
            data-id={it.chat.id}
            onclick={() => openSearchItem(it)}
            onmousemove={() => (gotoSel = idx)}
          >
            <span class="search-ico"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M7.9 20A9 9 0 1 0 4 16.1L2 22z"/></svg></span>
            <span class="search-text">
              <span class="search-title">{@html highlightHtml(it.chat.title, searchQuery)}</span>
              <span class="search-snippet">{@html highlightHtml(searchSnippet(it.chat, searchQuery), searchQuery)}</span>
            </span>
          </button>
        {/if}
      {:else}
        <div class="no-match">No matching chats</div>
      {/each}
    </div>
    {#if gotoError}
      <p class="goto-error" data-testid="goto-error">{gotoError}</p>
    {/if}
  </div>
{/if}
{#if settingsOpen}
  <button
    class="confirm-backdrop"
    data-testid="settings-backdrop"
    aria-label="Close settings"
    onclick={() => (settingsOpen = false)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card settings-card"
    data-testid="settings-modal"
    role="dialog"
    aria-modal="true"
    aria-label="Settings"
  >
    <button class="modal-x" data-testid="close-settings-btn" aria-label="Close settings" onclick={() => (settingsOpen = false)}>×</button>
    <p class="confirm-text">Settings</p>
    <div class="seg settings-tabs" role="tablist" aria-label="Settings sections">
      <button class="seg-btn {settingsTab === 'general' ? 'sel' : ''}" data-testid="settings-tab-general" role="tab" onclick={() => (settingsTab = "general")}>General</button>
      <button class="seg-btn {settingsTab === 'models' ? 'sel' : ''}" data-testid="settings-tab-models" role="tab" onclick={openModelsTab}>Models</button>
    </div>
    {#if settingsTab === "general"}
    <div class="setting-row">
      <span class="setting-label">Theme</span>
      <div class="seg">
        <button class="seg-btn {theme === 'light' ? 'sel' : ''}" data-testid="theme-light-btn" onclick={() => setTheme("light")}>Light</button>
        <button class="seg-btn {theme === 'dark' ? 'sel' : ''}" data-testid="theme-dark-btn" onclick={() => setTheme("dark")}>Dark</button>
      </div>
    </div>
    <div class="setting-row">
      <span class="setting-label">Accent color</span>
      <div class="swatches">
        {#each Object.entries(ACCENTS) as [key, a]}
          <button
            class="swatch {accent === key ? 'sel' : ''}"
            data-testid="accent-{key}"
            aria-label={a.label}
            title={a.label}
            style="background: {theme === 'dark' ? a.dark.bg : a.light.bg}"
            onclick={() => setAccent(key)}
          >{#if accent === key}<span class="swatch-check">✓</span>{/if}</button>
        {/each}
      </div>
    </div>
    <div class="setting-row">
      <span class="setting-label">Font size</span>
      <div class="seg">
        <button class="seg-btn" data-testid="font-decrease" aria-label="Decrease font size" onclick={() => setFontSize(fontSize - 1)}>−</button>
        <span class="font-val" data-testid="font-size-value">{fontSize}px</span>
        <button class="seg-btn" data-testid="font-increase" aria-label="Increase font size" onclick={() => setFontSize(fontSize + 1)}>+</button>
      </div>
    </div>
    <div class="setting-stacked">
      <span class="setting-label">Ollama host <span class="setting-hint">· browser mode only</span></span>
      <input
        class="pull-input"
        data-testid="ollama-host-input"
        placeholder="http://localhost:11434"
        bind:value={ollamaBase}
        onblur={() => setOllamaHost(ollamaBase)}
        onkeydown={(e) => { if (e.key === "Enter") (e.target as HTMLInputElement).blur(); }}
        spellcheck={false}
      />
      <span class="setting-note">Where this browser reaches Ollama. Empty = this machine. For remote access use the https tailscale URL. The Tauri shell always uses its local Ollama.</span>
    </div>
    <div class="setting-stacked">
      <span class="setting-label">Stop words <span class="setting-hint">· applied when continuing a reply</span></span>
      <textarea
        class="stopwords-input"
        data-testid="stopwords-input"
        rows="4"
        spellcheck={false}
        placeholder={"\\nUser:\\nAssistant:"}
        bind:value={stopWordsText}
        onblur={() => setStopWords(stopWordsText)}
      ></textarea>
      <span class="setting-note">One per line. Use \n for newlines. Generation stops when the model outputs one of these.</span>
    </div>
    {#if isTauriRuntime()}
      <div class="setting-stacked">
        <span class="setting-label">Vicuna CLI</span>
        <div class="browse-actions">
          {#if cliState?.found && cliState.managed}
            <button class="mini-btn" data-testid="cli-reinstall-btn" disabled={cliOp !== null} onclick={installCliFlow}>{cliOp === "install" ? "…" : "Reinstall"}</button>
            <button class="mini-btn" data-testid="cli-uninstall-btn" disabled={cliOp !== null} onclick={uninstallCliFlow}>{cliOp === "uninstall" ? "…" : "Uninstall"}</button>
          {:else}
            <button class="mini-btn primary" data-testid="cli-install-btn" disabled={cliOp !== null} onclick={installCliFlow}>{cliOp === "install" ? "…" : "Install"}</button>
          {/if}
          {#if cliOp === "load"}<span class="setting-note">Checking…</span>{/if}
        </div>
        {#if cliState}
          <span class="setting-note" data-testid="cli-status">
            {#if cliState.found && cliState.managed}Installed: {cliState.found}{cliState.on_path ? "" : " (not on PATH yet — open a new terminal)"}{:else if cliState.found}Another `{cliState.exe_name}` already exists at {cliState.found} — Install writes to {cliState.install_dir}.{:else}Not installed — Install writes to {cliState.install_dir}.{/if}
          </span>
        {/if}
        {#if cliNote}<span class="setting-note" data-testid="cli-note">{cliNote}</span>{/if}
        {#if cliError}<p class="goto-error" data-testid="cli-error">{cliError}</p>{/if}
        <span class="setting-note">Run `{cliState?.exe_name ?? "vicuna"} [path]` in a terminal to open that folder in Vicuna (default: current directory).</span>
      </div>
      <div class="setting-row quit-row">
        <span class="setting-label">Quit Vicuna</span>
        <button class="mini-btn danger" data-testid="quit-app-btn" onclick={quitVicuna}>Quit Vicuna</button>
      </div>
      <span class="setting-note">Closing the window keeps Vicuna in the tray so streams continue. Quit stops everything.</span>
    {/if}
    {:else}
    <div class="model-manage-list" data-testid="model-manage-list">
      <div class="server-row" data-testid="ollama-server-row">
        <div class="model-manage-info">
          <span class="model-manage-name">Ollama server</span>
          <span class="model-manage-sub">
            {#if ollamaVersion}
              <span class="dot-live"></span> Running · v{ollamaVersion}
            {:else if serverChecking || serverOp}
              <span class="dot-idle"></span> {serverOp === "start" ? "Starting…" : serverOp === "stop" ? "Stopping…" : "Checking…"}
            {:else}
              <span class="dot-idle"></span> Not reachable{serverError ? ` — ${serverError}` : ""}
            {/if}
          </span>
        </div>
        <div class="model-manage-actions">
          {#if isTauriRuntime()}
            {#if ollamaVersion}
              <button class="mini-btn" data-testid="stop-server-btn" disabled={serverOp !== null} onclick={stopServerFlow}>{serverOp === "stop" ? "…" : "Stop"}</button>
            {:else}
              <button class="mini-btn primary" data-testid="start-server-btn" disabled={serverOp !== null} onclick={startServerFlow}>{serverOp === "start" ? "…" : "Start"}</button>
            {/if}
          {/if}
        </div>
      </div>
      {#if serverError}
        <p class="goto-error" data-testid="server-error">{serverError}</p>
      {/if}
      <div class="seg browse-tabs" role="tablist" aria-label="Model library sections">
        <button class="seg-btn {browseView === 'installed' ? 'sel' : ''}" data-testid="browse-installed-btn" role="tab" onclick={() => (browseView = "installed")}>Installed</button>
        <button class="seg-btn {browseView === 'discover' ? 'sel' : ''}" data-testid="browse-discover-btn" role="tab" onclick={() => (browseView = "discover")}>Discover</button>
      </div>
      {#if browseView === "installed"}
      {#if runnersError}
        <p class="goto-error" data-testid="models-error">{runnersError}</p>
      {/if}
      {#each sortedManageModels as name}
        {@const running = runners.find((r) => r.name === name)}
        <div class="model-manage-row" data-testid="model-row" data-name={name}>
          <div class="model-manage-info">
            <span class="model-manage-name">{name}{#if name === selectedModel} <span class="model-inuse" title="Used by current chat">• in use</span>{/if}</span>
            <span class="model-manage-sub">
              {#if running}
                <span class="dot-live"></span> Loaded · {(running.size_vram / 1e9).toFixed(1)} GB VRAM
              {:else if isCloudModel(name)}
                <span class="dot-live"></span> Cloud · ready, no load needed
              {:else}
                <span class="dot-idle"></span> Unloaded
              {/if}
            </span>
          </div>
          <div class="model-manage-actions">
            {#if !isCloudModel(name)}
              {#if running}
                <button class="mini-btn" data-testid="unload-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => toggleModelLoaded(name, true)}>{modelOp === name ? "…" : "Unload"}</button>
              {:else}
                <button class="mini-btn primary" data-testid="load-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => toggleModelLoaded(name, false)}>{modelOp === name ? "…" : "Load"}</button>
              {/if}
            {/if}
            {#if confirmDeleteModel === name}
              <button class="mini-btn danger" data-testid="confirm-delete-model-btn" data-name={name} disabled={modelOp !== null} onclick={() => requestDeleteModel(name)}>{modelOp === name ? "…" : "Confirm?"}</button>
              <button class="mini-btn" data-testid="keep-model-btn" data-name={name} disabled={modelOp !== null} onclick={() => (confirmDeleteModel = null)}>Keep</button>
            {:else}
              <button class="mini-btn" data-testid="delete-model-btn" data-name={name} disabled={modelOp !== null || pulling !== null} onclick={() => requestDeleteModel(name)}>Delete</button>
            {/if}
          </div>
        </div>
      {:else}
        <p class="no-match">No models installed.</p>
      {/each}
      {:else}
      <div class="pull-row" data-testid="model-pull-row">
        <input
          class="pull-input"
          data-testid="model-pull-input"
          placeholder="Pull by exact name, e.g. gemma4:e4b"
          bind:value={pullName}
          disabled={pulling !== null}
          onkeydown={(e) => { if (e.key === "Enter") void submitPull(); }}
        />
        {#if pulling !== null}
          <button class="mini-btn" data-testid="cancel-pull-btn" onclick={cancelPull}>Cancel</button>
        {:else}
          <button class="mini-btn primary" data-testid="pull-model-btn" disabled={pullName.trim() === "" || modelOp !== null} onclick={() => void submitPull()}>Pull</button>
        {/if}
      </div>
      {#if pulling !== null}
        <div class="pull-progress" data-testid="model-pull-progress">
          <span class="pull-status">{pulling} · {pullStatus}</span>
          {#if pullTotal > 0}
            <div class="pull-bar"><div class="pull-fill" style="width: {Math.min(100, (pullDone / pullTotal) * 100).toFixed(1)}%"></div></div>
            <span class="pull-pct">{((pullDone / pullTotal) * 100).toFixed(0)}% · {(pullDone / 1e9).toFixed(1)} / {(pullTotal / 1e9).toFixed(1)} GB{#if pullRate > 0} · {formatPullRate(pullRate)}{/if}</span>
          {/if}
        </div>
      {/if}
      <div class="pull-row" data-testid="model-search-row">
        <input
          class="pull-input"
          data-testid="discover-search-input"
          placeholder="Search the library — try llama, code, vision…"
          bind:value={browseQuery}
        />
      </div>
      <div class="chip-row" data-testid="discover-chips" role="group" aria-label="Filter by capability">
        {#each BROWSE_CATS as c}
          <button class="chip {browseCat === c ? 'sel' : ''}" data-testid="discover-chip" data-cat={c} onclick={() => (browseCat = c)}>{c === "all" ? "All" : c === "mlx" ? "MLX" : c[0].toUpperCase() + c.slice(1)}</button>
        {/each}
      </div>
      <p class="browse-count" data-testid="discover-count">{filteredCatalog.length} of {catalog.length} models · catalog {catalogUpdated}
        {#if fetching}
          <span class="fetch-progress" data-testid="catalog-fetch-progress">Fetching {fetching.done}/{fetching.total}…</span>
        {:else}
          <button class="mini-btn" data-testid="catalog-fetch-btn" onclick={fetchCatalog}>Fetch</button>
        {/if}
      </p>
      {#if catalogError}
        <p class="goto-error" data-testid="catalog-fetch-error">{catalogError}</p>
      {/if}
      {#each filteredCatalog.filter((e) => e.variants.length > 0) as entry}
        {@const mlxFirst = entry.variants.find((v) => isMlxVariant(v.tag))?.tag}
        {@const picked = pickedVariant(entry.name, browseCat === "mlx" && mlxFirst ? mlxFirst : entry.variants[0].tag)}
        {@const fullName = `${entry.name}:${picked}`}
        {@const installed = entry.variants.some((v) => models.includes(`${entry.name}:${v.tag}`))}
        <div class="browse-card" data-testid="browse-card" data-name={entry.name}>
          <div class="browse-head">
            <span class="browse-name">{entry.name}</span>
            {#if installed}<span class="browse-installed" title="A variant of this model is in your library">In library</span>{/if}
          </div>
          <p class="browse-blurb">{entry.blurb}</p>
          <div class="browse-meta">
            {#each entry.cats as c}<span class="browse-cat">{c}</span>{/each}
            <span class="browse-params">{entry.params}</span>
          </div>
          <div class="browse-actions">
            <select
              class="browse-variant"
              data-testid="variant-select"
              aria-label="Variant of {entry.name}"
              value={picked}
              onchange={(e) => (variantPick = { ...variantPick, [entry.name]: e.currentTarget.value })}
            >
              {#each entry.variants as v}
                <option value={v.tag}>{v.tag} · {v.size}</option>
              {/each}
            </select>
            <button class="mini-btn primary" data-testid="browse-pull-btn" data-name={fullName} disabled={pulling !== null || modelOp !== null} onclick={() => void submitPullName(fullName)}>{pulling === fullName ? "…" : "Pull"}</button>
          </div>
        </div>
      {:else}
        <p class="no-match">No matches — try another search, or pull by exact name above.</p>
      {/each}
      {/if}
    </div>
    {/if}
  </div>
{/if}
{/if}

<input type="file" bind:this={fileInput} hidden multiple onchange={onFile} data-testid="file-input" />

<style>
  :global(html) { color-scheme: light; }
  :global(html[data-theme="dark"]) { color-scheme: dark; }
  :global(:root) {
    --bg: #ffffff; --text: #111111; --text-dim: #333333;
    --muted: #9a9a9a; --muted-2: #8a8a8a; --faint: #aaaaaa;
    --hover: #f0f0f0; --hover-2: #ececec; --hover-3: #f2f2f3;
    --active-item: #efefef; --item-bg: #f4f4f5; --composer: #f4f4f5;
    --card: #ffffff; --border: #ececec; --border-2: #e2e2e2; --border-3: #e5e5e5;
    --border-strong: #c9c9c9; --input-border: #dddddd;
    --bubble-user: #efefef; --chip: #eeeeee;
    --icon: #555555; --icon-dim: #909090;
    --thinking: #7a7a7a; --link: #0b57d0; --link-hover: #084298;
    --danger: #b3261e; --err-text: #b3261e; --err-bg: #fdecea;
    --code-bg: #f4f4f5; --code-inline: #f0f0f0;
    --code-keyword: #d73a49; --code-string: #032f62; --code-comment: #6a737d;
    --code-number: #005cc5; --code-title: #6f42c1; --code-builtin: #005cc5;
    --code-literal: #005cc5; --code-attr: #6f42c1;
    --send-disabled: #d9d9de;
    --cancel-bg: #ececf1; --cancel-hover: #e0e0e6;
    --ok: #1a7f37;
    --pink: #d63384;
    --accent: #ef6c00; --accent-hover: #d96000; --accent-fg: #ffffff;
  }
  :global(html[data-theme="dark"]) {
    --bg: #1e1e1e; --text: #ececec; --text-dim: #bbbbbb;
    --muted: #9a9a9a; --muted-2: #8a8a8a; --faint: #777777;
    --hover: #2c2c2c; --hover-2: #333333; --hover-3: #2a2a2a;
    --active-item: #2e2e2e; --item-bg: #2b2b2b; --composer: #2b2b2b;
    --card: #2b2b2b; --border: #3a3a3a; --border-2: #444444; --border-3: #444444;
    --border-strong: #555555; --input-border: #444444;
    --bubble-user: #2f2f2f; --chip: #383838;
    --icon: #bbbbbb; --icon-dim: #999999;
    --thinking: #a3a3a3; --link: #8ab4f8; --link-hover: #aecbfa;
    --danger: #ff8a80; --err-text: #ffb4a9; --err-bg: #4a1f1a;
    --code-bg: #282828; --code-inline: #383838;
    --code-keyword: #ff7b72; --code-string: #a5d6ff; --code-comment: #8b949e;
    --code-number: #79c0ff; --code-title: #d2a8ff; --code-builtin: #79c0ff;
    --code-literal: #79c0ff; --code-attr: #7ee787;
    --send-disabled: #3a3a3a;
    --cancel-bg: #3a3a3a; --cancel-hover: #444444;
    --ok: #7ee787;
    --pink: #f472b6;
  }
  :global(html, body) { margin: 0; height: 100%; background: var(--bg); color: var(--text); font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", Inter, Helvetica, Arial, sans-serif; }
  .app { display: flex; height: 100vh; background: var(--bg); -webkit-user-select: none; user-select: none; }
  /* native-app feel: chrome text isn't selectable; message content, code,
     thinking, and every editable field opt back in below */
  .answer, .bubble.user, .thinking-body { -webkit-user-select: text; user-select: text; }
  textarea, input { -webkit-user-select: text; user-select: text; }
  .sidebar { width: 264px; min-width: 264px; padding: 10px 10px 20px 10px; display: flex; flex-direction: column; }
  .topbar { display: flex; align-items: center; justify-content: flex-end; padding: 2px 4px 12px 4px; }
  .icon-btn { background: transparent; border: none; cursor: pointer; color: var(--icon); padding: 4px; border-radius: 6px; }
  .icon-btn:hover { background: var(--hover); }
  .side-actions { display: flex; flex-direction: column; gap: 2px; margin-bottom: 18px; }
  .newchat { display: flex; gap: 8px; align-items: center; background: transparent; border: none; border-radius: 8px; padding: 8px 10px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text-dim); }
  .newchat:hover { background: var(--hover-2); }
  .newchat .kbd-hint, .goto .kbd-hint { margin-left: auto; font-size: 12px; font-weight: 500; color: var(--muted-2); opacity: 0; transition: opacity .15s; white-space: nowrap; }
  .newchat:hover .kbd-hint, .newchat:focus-visible .kbd-hint, .goto:hover .kbd-hint, .goto:focus-visible .kbd-hint { opacity: 1; }
  .goto { display: flex; gap: 8px; align-items: center; background: transparent; border: none; border-radius: 8px; padding: 8px 10px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text-dim); }
  .goto:hover { background: var(--hover-2); }
  .side-footer { margin-top: auto; padding-top: 10px; display: flex; flex-direction: column; align-items: stretch; }
  .side-footer .goto { width: 100%; box-sizing: border-box; }
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
  .search-title { font-size: 14px; font-weight: 650; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .search-snippet { font-size: 13px; color: var(--muted-2); line-height: 1.4; display: -webkit-box; -webkit-line-clamp: 2; line-clamp: 2; -webkit-box-orient: vertical; overflow: hidden; word-break: break-word; }
  .search-snippet:empty { display: none; }
  .search-card :global(.search-mark) { background: #f0bd76; color: #1e1e1e; border-radius: 3px; padding: 0 1px; }
  .search-card .goto-error { margin: 4px 10px 6px 10px; }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  
  .history { overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
  .group-label { font-size: 12px; color: var(--muted); margin: 12px 4px 6px 4px; }
  .chat-item { display: flex; justify-content: space-between; align-items: center; border: none; background: transparent; padding: 7px 8px; border-radius: 8px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text); user-select: none; -webkit-user-select: none; }
  .chat-item:hover { background: var(--hover-3); }
  .chat-item.active { background: var(--active-item); }
  .chat-item .title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; }
  .chat-item.streaming { background: var(--hover-3); }
  .chat-item.streaming.active { background: var(--active-item); }
  .stream-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex-shrink: 0; margin-right: 7px; animation: stream-pulse 1.2s ease-in-out infinite; }
  @keyframes stream-pulse { 0%, 100% { opacity: .35; transform: scale(.85); } 50% { opacity: 1; transform: scale(1.15); } }
  .live-elapsed { flex-shrink: 0; margin-left: 8px; font-size: 11px; font-variant-numeric: tabular-nums; white-space: nowrap; color: var(--muted-2); }
  .rename-input { flex: 1; min-width: 0; border: 1px solid var(--border-strong); border-radius: 6px; outline: none; font: inherit; font-size: 14px; padding: 2px 6px; background: var(--card); color: var(--text); }
  .ctx-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 40; }
  .ctx-menu { position: fixed; z-index: 41; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; min-width: 150px; display: flex; flex-direction: column; }
  .ctx-item { border: none; background: transparent; text-align: left; font: inherit; font-size: 14.5px; padding: 8px 14px; border-radius: 10px; cursor: pointer; color: var(--text); }
  .ctx-item:hover { background: var(--hover-3); }
  .ctx-item.danger { color: var(--danger); }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  .empty-hint { color: var(--faint); font-size: 13px; padding: 0 8px; }
  .main { flex: 1; display: flex; flex-direction: column; position: relative; min-width: 0; }
  .stream-bar { position: relative; height: 3px; border-radius: 999px; overflow: hidden; margin: 0 4px 8px 4px; background: var(--border); pointer-events: none; }
  .stream-bar span { position: absolute; top: 0; bottom: 0; width: 40%; background: linear-gradient(90deg, transparent, var(--accent), transparent); animation: stream-slide 1.2s ease-in-out infinite; }
  @keyframes stream-slide { from { left: -40%; } to { left: 100%; } }
  .queue-strip { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 0 4px 8px 4px; }
  .queue-chip { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; background: var(--hover-3); border: 1px solid var(--border-2); border-radius: 999px; padding: 3px 6px 3px 10px; font-size: 12px; color: var(--muted); }
  .queue-text { max-width: 320px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .queue-x { border: none; background: transparent; color: var(--muted); cursor: pointer; font-size: 14px; line-height: 1; padding: 0 2px; }
  .queue-x:hover { color: var(--text); }
  .queue-hint { font-size: 11.5px; color: var(--faint); }
  .esc-hint { position: absolute; right: 24px; bottom: 100%; margin-bottom: 6px; background: var(--card); border: 1px solid var(--border-3); border-radius: 10px; padding: 5px 10px; font-size: 12px; color: var(--muted); box-shadow: 0 4px 16px rgba(0,0,0,.12); z-index: 6; }
  .collapsed-rail { position: absolute; left: 0; top: 0; bottom: 0; display: flex; flex-direction: column; justify-content: space-between; align-items: stretch; width: 60px; padding: 10px 8px; box-sizing: border-box; z-index: 5; }
  .collapsed-rail .icon-btn { width: 100%; padding: 8px 0; border-radius: 8px; }
  .empty { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; padding: 24px; overflow-y: auto; }
  .empty-title { font-size: 24px; font-weight: 500; color: var(--text-dim); margin: 0 0 28px 0; text-align: center; }
  .suggest-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; width: 100%; max-width: 860px; }
  .suggest-card { display: flex; flex-direction: row; align-items: center; justify-content: space-between; gap: 12px; text-align: left; background: var(--card); border: 1px solid var(--border); border-radius: 16px; padding: 14px 14px 14px 18px; cursor: pointer; min-width: 0; }
  .suggest-card:hover { box-shadow: 0 4px 16px rgba(0,0,0,.08); border-color: var(--accent); }
  .suggest-text { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .suggest-title { font-size: 15px; font-weight: 650; color: var(--text); }
  .suggest-sub { font-size: 13.5px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .suggest-icon { flex-shrink: 0; width: 36px; height: 36px; border-radius: 10px; background: var(--accent); color: var(--accent-fg); display: flex; align-items: center; justify-content: center; }
  .messages { flex: 1; overflow-y: auto; overflow-x: hidden; min-width: 0; padding: 40px 24px 12px 24px; display: flex; flex-direction: column; gap: 14px; max-width: 860px; width: 100%; margin: 0 auto; box-sizing: border-box; }
  .row { display: flex; }
  .user-row { justify-content: flex-end; }
  .asst-row { justify-content: flex-start; }
  .bubble { max-width: 100%; padding: 10px 14px; border-radius: 16px; font-size: var(--msg-font-size, 14.5px); line-height: 1.5; white-space: pre-wrap; word-break: break-word; }
  .bubble.user { background: var(--accent); color: var(--accent-fg); }
  .bubble.asst { background: transparent; padding-left: 0; padding-right: 0; white-space: normal; user-select: text; }
  .answer { user-select: text; white-space: normal; }
  .ask-card { margin: 12px 0 2px; max-width: 560px; border: 1px solid var(--border-2); border-radius: 14px; background: var(--card); padding: 12px; }
  .ask-question { font-size: 14px; font-weight: 600; color: var(--text); margin-bottom: 10px; line-height: 1.45; }
  .ask-options { display: flex; flex-direction: column; gap: 6px; }
  .ask-option { width: 100%; text-align: left; border: 1px solid var(--border-2); background: var(--bg); color: var(--text); border-radius: 10px; padding: 8px 11px; font: inherit; font-size: 13.5px; cursor: pointer; }
  .ask-option:hover { border-color: var(--accent); background: var(--hover-3); }
  .ask-other-toggle { color: var(--muted); border-style: dashed; }
  .ask-other { display: flex; gap: 6px; }
  .ask-other-closed { align-items: stretch; }
  .ask-other-toggle { flex: 1; width: auto; }
  .ask-dismiss { border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 10px; padding: 8px 12px; font: inherit; font-size: 13px; cursor: pointer; white-space: nowrap; }
  .ask-dismiss:hover { color: var(--text); border-color: var(--border-3); background: var(--hover-3); }
  .ask-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; background: var(--bg); color: var(--text); padding: 7px 10px; font: inherit; font-size: 13.5px; }
  .ask-input:focus { outline: none; border-color: var(--accent); }
  .ask-send { border: none; border-radius: 10px; background: var(--accent); color: var(--accent-fg); padding: 0 13px; font: inherit; font-size: 13px; font-weight: 500; cursor: pointer; }
  .ask-send:disabled { opacity: .45; cursor: default; }
  .col { display: flex; flex-direction: column; }
  .user-row .col { align-items: flex-end; }
  .asst-row .col { align-items: stretch; flex: 1; min-width: 0; }
  .bubble.asst { width: 100%; box-sizing: border-box; min-width: 0; }
  .actions { display: flex; gap: 2px; margin-top: 6px; align-items: center; -webkit-user-select: none; user-select: none; }
  .model-tag { font-size: 12px; color: var(--muted-2); margin-left: 6px; white-space: nowrap; max-width: 320px; overflow: hidden; text-overflow: ellipsis; }
  .fork-note { display: flex; justify-content: center; align-items: baseline; gap: 4px; font-size: 13px; color: var(--muted-2); margin: 4px 0 8px 0; }
  .fork-link { background: transparent; border: none; cursor: pointer; font: inherit; font-size: 13px; color: var(--link); padding: 0; text-decoration: underline; text-underline-offset: 2px; }
  .fork-link:hover { color: var(--link-hover); }
  .act { background: transparent; border: none; cursor: pointer; color: var(--icon-dim); width: 25px; height: 25px; padding: 0; border-radius: 7px; display: inline-flex; align-items: center; justify-content: center; }
  .act:hover:not(:disabled) { background: var(--hover-3); color: var(--text); }
  .act:disabled { opacity: .35; cursor: default; }
  .answer :global(.codeblock) { position: relative; }
  .answer :global(.codecopy) { position: absolute; top: 8px; right: 8px; background: var(--card); border: 1px solid var(--border-2); border-radius: 8px; cursor: pointer; color: var(--icon-dim); width: 24px; height: 24px; padding: 0; display: flex; align-items: center; justify-content: center; }
  .answer :global(.codecopy:hover) { background: var(--hover); color: var(--text); }
  .answer :global(.codecopy.ok) { color: var(--ok); }
  .thinking { margin-bottom: 10px; }
  .thinking-head { display: flex; align-items: center; gap: 8px; font-size: 17px; font-weight: 500; color: var(--text); margin-bottom: 14px; background: transparent; border: none; padding: 0; cursor: pointer; font-family: inherit; text-align: left; }
  .thinking-head .chev { color: var(--muted); font-size: 13px; }
  .shimmer { background: linear-gradient(90deg, var(--muted-2) 25%, var(--text) 50%, var(--muted-2) 75%); background-size: 200% 100%; -webkit-background-clip: text; background-clip: text; color: transparent; animation: shimmer-slide 1.4s linear infinite; }
  @keyframes shimmer-slide { to { background-position: -200% 0; } }
  .thinking-body { color: var(--thinking); font-size: 15px; line-height: 1.65; white-space: pre-wrap; word-break: break-word; }
  .answer:empty { display: none; }
  .typing { display: inline-flex; gap: 5px; align-items: center; padding: 8px 2px; }
  .typing span { width: 7px; height: 7px; border-radius: 50%; background: var(--muted-2); animation: typing-bounce 1.2s infinite ease-in-out; }
  .typing span:nth-child(2) { animation-delay: .15s; }
  .typing span:nth-child(3) { animation-delay: .3s; }
  @keyframes typing-bounce { 0%, 60%, 100% { transform: translateY(0); opacity: .4; } 30% { transform: translateY(-5px); opacity: 1; } }
  @media (prefers-reduced-motion: reduce) { .typing span { animation: none; opacity: .7; } .shimmer { animation: none; } .stream-bar span { animation: none; width: 100%; } .stream-dot { animation: none; opacity: .8; } }
  .answer :global(p) { margin: 0 0 12px 0; }
  .answer :global(p:last-child) { margin-bottom: 0; }
  .answer :global(strong) { font-weight: 700; }
  .answer :global(em) { font-style: italic; }
  .answer :global(code) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; background: var(--code-inline); border-radius: 6px; padding: 1px 5px; }
  .answer :global(code.inl) { color: var(--pink); cursor: pointer; position: relative; }
  .answer :global(code.inl:hover)::before { content: "Copy"; position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translateX(-50%); background: var(--text); color: var(--bg); font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", Inter, Helvetica, Arial, sans-serif; font-size: 11.5px; font-weight: 500; padding: 3px 8px; border-radius: 6px; white-space: nowrap; pointer-events: none; }
  .answer :global(code.inl.copied:hover)::before { display: none; }
  .answer :global(code.inl.copied)::after { content: "Copied"; position: absolute; bottom: calc(100% + 6px); left: 50%; transform: translateX(-50%); background: var(--text); color: var(--bg); font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", Inter, Helvetica, Arial, sans-serif; font-size: 11.5px; font-weight: 500; padding: 3px 8px; border-radius: 6px; white-space: nowrap; pointer-events: none; }
  .answer :global(pre) { background: var(--code-bg); border-radius: 12px; padding: 12px 14px; overflow-x: auto; font-size: 13.5px; line-height: 1.6; }
  .answer :global(pre code) { background: transparent; padding: 0; }
  .answer :global(details.tool-result) { background: var(--code-bg); border-radius: 12px; margin: 8px 0; overflow: hidden; }
  .answer :global(.ask-log) { margin: 10px 0; padding: 9px 12px 10px; max-width: 560px; border-left: 3px solid var(--accent); border-radius: 0 10px 10px 0; background: var(--hover-3); }
  .answer :global(.ask-log-q) { font-size: 13.5px; font-weight: 600; color: var(--text); line-height: 1.45; }
  .answer :global(.ask-log-a) { margin-top: 3px; font-size: 13px; color: var(--muted); }
  .answer :global(.ask-log-a strong) { color: var(--muted); font-weight: 600; }
  .answer :global(.ask-log-dismissed) { font-style: italic; }
  .answer :global(details.tool-result summary) { cursor: pointer; padding: 9px 12px; font-size: 12.5px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; color: var(--muted-2); list-style: none; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .answer :global(details.tool-result summary::-webkit-details-marker) { display: none; }
  .answer :global(details.tool-result summary::before) { content: "▸ "; color: var(--muted); }
  .answer :global(details.tool-result[open] summary::before) { content: "▾ "; }
  .answer :global(details.tool-result summary:hover) { color: var(--text); }
  .answer :global(details.tool-result pre) { margin: 0; border-radius: 0; border-top: 1px solid var(--border-2); background: transparent; }
  .answer :global(.hljs-keyword), .answer :global(.hljs-selector-tag) { color: var(--code-keyword); }
  .answer :global(.hljs-string), .answer :global(.hljs-regexp) { color: var(--code-string); }
  .answer :global(.hljs-comment), .answer :global(.hljs-quote) { color: var(--code-comment); font-style: italic; }
  .answer :global(.hljs-number), .answer :global(.hljs-literal) { color: var(--code-number); }
  .answer :global(.hljs-title), .answer :global(.hljs-title.function_) { color: var(--code-title); }
  .answer :global(.hljs-built_in) { color: var(--code-builtin); }
  .answer :global(.hljs-attr), .answer :global(.hljs-attribute), .answer :global(.hljs-variable), .answer :global(.hljs-template-variable) { color: var(--code-attr); }
  .answer :global(.hljs-name), .answer :global(.hljs-selector-id), .answer :global(.hljs-selector-class) { color: var(--code-builtin); }
  .answer :global(.hljs-type), .answer :global(.hljs-params) { color: var(--text); }
  .answer :global(.hljs-addition) { color: var(--ok); }
  .answer :global(.hljs-deletion) { color: var(--danger); }
  .answer :global(.hljs-emphasis) { font-style: italic; }
  .answer :global(.hljs-strong) { font-weight: 700; }
  .answer :global(ul), .answer :global(ol) { margin: 0 0 12px 0; padding-left: 22px; }
  .answer :global(li) { margin: 4px 0; }
  .answer :global(h1), .answer :global(h2), .answer :global(h3), .answer :global(h4) { margin: 14px 0 8px 0; font-weight: 650; line-height: 1.3; }
  .answer :global(h1) { font-size: 20px; } .answer :global(h2) { font-size: 18px; } .answer :global(h3) { font-size: 16px; } .answer :global(h4) { font-size: 15px; }
  .answer :global(a) { color: var(--link); }
  .answer :global(blockquote) { border-left: 3px solid var(--border-2); margin: 0 0 12px 0; padding-left: 12px; color: var(--text-dim); }
  .answer :global(table) { border-collapse: collapse; margin: 0; }
  .answer :global(.table-wrap) { overflow-x: auto; max-width: 100%; margin: 0 0 12px 0; }
  .answer :global(th), .answer :global(td) { border: 1px solid var(--border-2); padding: 6px 10px; font-size: 13.5px; }
  .err { max-width: 720px; margin: 0 auto; color: var(--err-text); background: var(--err-bg); border-radius: 10px; padding: 8px 12px; font-size: 13px; }
  .tool-status { max-width: 860px; width: 100%; margin: 0 auto 8px auto; box-sizing: border-box; display: flex; align-items: center; gap: 8px; color: var(--muted-2); font-size: 12.5px; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; padding: 0 24px; }
  .tool-status-label { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .composer-wrap { position: relative; padding: 12px 24px 22px 24px; max-width: 860px; width: 100%; margin: 0 auto; box-sizing: border-box; }
  .scroll-jump { position: absolute; top: -44px; left: 50%; transform: translateX(-50%); width: 44px; height: 36px; background: var(--card); border: 1px solid var(--border-3); border-radius: 12px; box-shadow: 0 4px 16px rgba(0,0,0,.10); cursor: pointer; font-size: 17px; line-height: 1; color: var(--text); z-index: 5; }
  .scroll-jump:hover { background: var(--composer); }
  .composer { background: var(--composer); border-radius: 22px; padding: 12px 12px 10px 16px; }
  .composer.drop-active { box-shadow: inset 0 0 0 1.5px var(--accent); }
  .file-strip { display: flex; gap: 8px; flex-wrap: wrap; padding: 2px 2px 10px 2px; }
  .file-chip { display: inline-flex; align-items: center; gap: 7px; max-width: 100%; background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; padding: 6px 6px 6px 10px; font-size: 12.5px; color: var(--text); }
  .file-chip svg { flex-shrink: 0; color: var(--muted-2); }
  .file-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; }
  .file-chip .exp-size { flex-shrink: 0; }
  .file-remove { flex-shrink: 0; border: none; background: var(--hover-3); color: var(--muted); cursor: pointer; font-size: 13px; line-height: 1; width: 20px; height: 20px; border-radius: 50%; padding: 0; }
  .file-remove:hover { color: var(--text); }
  .composer textarea { position: relative; z-index: 1; width: 100%; box-sizing: border-box; border: none; background: transparent; resize: none; outline: none; font-size: 15px; font-family: inherit; color: transparent; caret-color: var(--text); overflow-x: hidden; overflow-y: auto; overflow-wrap: break-word; max-height: 200px; padding: 0; margin: 0; display: block; }
  .composer-field { position: relative; }
  .composer-backdrop { position: absolute; inset: 0; overflow: hidden; pointer-events: none; white-space: pre-wrap; overflow-wrap: break-word; word-break: normal; tab-size: 8; font-size: 15px; font-family: inherit; color: var(--text); padding: 0; }
  /* global: injected by renderMentions() into {@html} regions, so runtime
     HTML carries no svelte scoping class. Two variants: the composer backdrop
     pill must add ZERO width (no icon, no padding delta, same font size) or
     the drawn caret would drift from the real one; sent bubbles get the
     full icon badge. */
  .composer-backdrop :global(.mention-pill) { background: var(--accent); color: var(--accent-fg); border-radius: 6px; padding: 0 3px; margin: 0 -3px; }
  .composer-backdrop :global(.mention-pill svg) { display: none; }
  :global(.bubble.user .mention-pill) { background: var(--accent); color: var(--accent-fg); border-radius: 999px; padding: 1px 9px 2px 7px; font-size: .92em; overflow-wrap: anywhere; }
  :global(.bubble.user .mention-pill svg) { vertical-align: -1.5px; margin-right: 4px; }
  :global(.bubble.user .bubble-files) { display: flex; flex-direction: row; flex-wrap: wrap; align-items: center; gap: 6px; margin-bottom: 8px; }
  :global(.bubble.user .bubble-files:last-child) { margin-bottom: 0; }
  :global(.bubble.user .file-pill) { border: 1px solid color-mix(in srgb, var(--accent-fg) 30%, transparent); background: color-mix(in srgb, var(--accent-fg) 9%, var(--accent)); font-family: inherit; font-weight: inherit; line-height: inherit; text-align: left; cursor: pointer; }
  :global(.bubble.user .file-pill:hover) { background: color-mix(in srgb, var(--accent-fg) 17%, var(--accent)); }
  :global(.bubble.user .file-pill:focus-visible) { outline: 2px solid var(--accent-fg); outline-offset: 1px; }
  .image-strip { display: flex; gap: 8px; flex-wrap: wrap; padding: 2px 2px 10px 2px; }
  .image-chip { position: relative; width: 64px; height: 64px; border-radius: 14px; overflow: hidden; background: var(--card); flex: none; }
  .image-chip img { width: 100%; height: 100%; object-fit: cover; display: block; }
  .image-open { display: block; width: 100%; height: 100%; border: none; background: none; padding: 0; cursor: zoom-in; }
  .msg-image-open { border: none; background: none; padding: 0; cursor: zoom-in; display: block; }
  .image-chip .image-remove { position: absolute; top: 2px; right: 2px; width: 20px; height: 20px; border-radius: 50%; border: none; background: rgba(0,0,0,.55); color: #fff; font-size: 12px; line-height: 1; cursor: pointer; padding: 0; }
  .msg-images { display: flex; gap: 8px; flex-wrap: wrap; margin-bottom: 8px; }
  .msg-images img { display: block; max-width: min(100%, 320px); max-height: 220px; border-radius: 12px; }
  .lightbox-backdrop { position: fixed; inset: 0; z-index: 60; border: none; padding: 24px; box-sizing: border-box; background: rgba(0, 0, 0, 0.85); cursor: zoom-out; display: flex; align-items: center; justify-content: center; }
  .lightbox-backdrop img { max-width: 100%; max-height: 100%; object-fit: contain; border-radius: 12px; }
  .composer-bar { display: flex; align-items: center; gap: 8px; margin-top: 10px; justify-content: flex-end; }
  .mention-menu { position: absolute; bottom: calc(100% + 6px); left: 24px; width: min(420px, calc(100% - 48px)); z-index: 30; background: var(--card); border: 1px solid var(--border-2); border-radius: 14px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; max-height: 264px; overflow-y: auto; }
  .mention-item { display: flex; align-items: center; gap: 8px; width: 100%; box-sizing: border-box; border: none; background: transparent; font: inherit; font-size: 13px; color: var(--text); padding: 7px 10px; border-radius: 10px; cursor: pointer; text-align: left; min-width: 0; }
  .mention-item:hover { background: var(--hover-3); }
  .mention-item.sel { background: var(--hover-3); background: color-mix(in srgb, var(--accent) 18%, transparent); }
  .mention-item svg { flex-shrink: 0; color: var(--muted-2); }
  .mention-path { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .mention-empty { padding: 8px 12px; font-size: 13px; color: var(--muted); }
  .slash-desc { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 12px; color: var(--muted); text-align: right; }
  .spacer { flex: 1; }
  .model-pill { display: flex; align-items: center; gap: 6px; background: var(--card); color: var(--text); border: none; border-radius: 16px; padding: 7px 12px; font-size: 13.5px; cursor: pointer; max-width: 260px; }
  .model-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: inherit; }
  .model-pill-wrap { position: relative; }
  .model-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 9; }
  .model-drop { position: absolute; bottom: 40px; right: 0; background: var(--card); border: 1px solid var(--border); border-radius: 20px; box-shadow: 0 12px 40px rgba(0,0,0,.14); width: 300px; overflow: hidden; z-index: 10; }
  .model-search { width: 100%; box-sizing: border-box; border: none; border-bottom: 1px solid var(--border); outline: none; padding: 13px 18px; font-size: 17px; font-family: inherit; background: var(--card); color: var(--text); }
  .model-search::placeholder { color: var(--faint); }
  .model-list-drop { max-height: 252px; overflow-y: auto; padding: 4px 0 8px 0; }
  .model-opt { display: block; width: 100%; text-align: left; border: none; background: transparent; padding: 9px 18px; font-size: 17px; cursor: pointer; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .model-opt:hover { background: var(--hover-3); }
  .model-opt.sel { background: var(--hover-3); }
  .no-match { padding: 12px 18px; font-size: 14px; color: var(--muted); }
  .send { width: 32px; height: 32px; border-radius: 50%; border: none; background: var(--send-disabled); color: #fff; font-size: 18px; cursor: pointer; }
  .send:disabled { opacity: .8; cursor: default; }
  .send:not(:disabled) { background: var(--accent); color: var(--accent-fg); }
  .editbox { width: 100%; border: 1.5px solid var(--border-strong); border-radius: 16px; padding: 10px 12px 8px 12px; background: var(--card); }
  .editbox textarea { width: 100%; box-sizing: border-box; border: none; outline: none; resize: none; font: inherit; font-size: 14.5px; line-height: 1.5; background: transparent; color: var(--text); }
  .edit-actions { display: flex; align-items: center; gap: 8px; margin-top: 8px; }
  .edit-hint { font-size: 11.5px; color: var(--muted-2); margin-right: auto; }
  .edit-cancel { background: transparent; border: none; cursor: pointer; font-size: 13.5px; font-family: inherit; color: var(--text-dim); padding: 6px 12px; border-radius: 8px; }
  .edit-cancel:hover { background: var(--hover); }
  .edit-fork { background: transparent; border: none; cursor: pointer; font-size: 13.5px; font-family: inherit; color: var(--text-dim); padding: 6px 12px; border-radius: 8px; white-space: nowrap; }
  .edit-fork:hover:not(:disabled) { background: var(--hover); }
  .edit-fork:disabled { opacity: .5; cursor: default; }
  .edit-save { background: var(--accent); color: var(--accent-fg); border: none; cursor: pointer; font-size: 13.5px; font-family: inherit; padding: 6px 18px; border-radius: 16px; }
  .edit-save:disabled { opacity: .5; cursor: default; }
  .side-footer { margin-top: auto; padding: 10px 4px 2px 4px; display: flex; flex-direction: column; align-items: flex-start; gap: 2px; }
  .settings-card { width: 80vw; max-width: 900px; height: 80vh; max-height: 820px; top: 50%; display: flex; flex-direction: column; overflow-y: auto; }
  .settings-tabs { margin-bottom: 16px; align-self: stretch; }
  .settings-tabs .seg-btn { flex: 1; }
  .font-val { font-size: 14px; min-width: 48px; text-align: center; color: var(--text); font-variant-numeric: tabular-nums; margin-top: 5px; }
  .model-manage-list { display: flex; flex-direction: column; gap: 4px; flex: 1; min-height: 0; overflow-y: auto; margin-bottom: 4px; }
  .server-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 10px 12px; margin-bottom: 8px; background: var(--composer); border-radius: 16px; }
  .model-manage-row { display: flex; align-items: center; justify-content: space-between; gap: 10px; padding: 8px 4px; border-bottom: 1px solid var(--border); }
  .model-manage-row:last-child { border-bottom: none; }
  .model-manage-info { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .model-manage-name { font-size: 13.5px; font-weight: 600; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .model-inuse { color: var(--accent); font-weight: 500; }
  .model-manage-sub { font-size: 12px; color: var(--muted); display: flex; align-items: center; gap: 6px; }
  .dot-live { width: 7px; height: 7px; border-radius: 50%; background: #22c55e; display: inline-block; }
  .dot-idle { width: 7px; height: 7px; border-radius: 50%; background: var(--muted); display: inline-block; }
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .mini-btn.primary { background: var(--accent); border-color: transparent; color: var(--accent-fg); }
  .mini-btn.primary:hover:not(:disabled) { background: var(--accent-hover); }
  .mini-btn.danger { border-color: var(--danger); color: var(--danger); }
  .mini-btn.danger:hover:not(:disabled) { background: var(--err-bg); }
  .model-manage-actions { display: flex; gap: 6px; flex-shrink: 0; }
  .pull-row { display: flex; gap: 8px; padding: 4px 4px 10px 4px; }
  .pull-input { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 13px; font-family: inherit; padding: 6px 14px; }
  .pull-input:focus { outline: none; border-color: var(--accent); }
  .pull-input:disabled { opacity: .5; }
  .pull-progress { display: flex; flex-direction: column; gap: 6px; padding: 0 4px 10px 4px; font-size: 12px; color: var(--muted); }
  .pull-status { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pull-bar { height: 6px; border-radius: 999px; background: var(--hover); overflow: hidden; }
  .pull-fill { height: 100%; background: var(--accent); border-radius: 999px; transition: width .2s; }
  .pull-pct { font-variant-numeric: tabular-nums; }
  .browse-tabs { margin-bottom: 8px; }
  .chip-row { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 4px 8px 4px; }
  .chip { border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 999px; padding: 3px 12px; font-size: 12px; font-family: inherit; cursor: pointer; }
  .chip:hover { background: var(--hover); color: var(--text); }
  .chip.sel { background: var(--text); border-color: transparent; color: var(--bg); }
  .browse-count { font-size: 12px; color: var(--muted); margin: 0; padding: 0 4px 8px 4px; display: flex; align-items: center; gap: 8px; }
  .fetch-progress { font-variant-numeric: tabular-nums; }
  .browse-card { border: 1px solid var(--border); border-radius: 14px; padding: 10px 12px; margin: 0 0 8px 0; display: flex; flex-direction: column; gap: 6px; }
  .browse-head { display: flex; align-items: center; gap: 8px; }
  .browse-name { font-size: 13.5px; font-weight: 700; color: var(--text); }
  .browse-installed { font-size: 11px; font-weight: 600; color: var(--ok); border: 1px solid var(--ok); border-radius: 999px; padding: 1px 8px; }
  .browse-blurb { font-size: 12.5px; color: var(--text-dim); margin: 0; line-height: 1.4; }
  .browse-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; }
  .browse-cat { font-size: 11px; color: var(--muted); background: var(--hover); border-radius: 999px; padding: 1px 8px; }
  .browse-params { font-size: 11.5px; color: var(--muted); }
  .browse-actions { display: flex; gap: 8px; align-items: center; }
  .browse-variant { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 12.5px; font-family: inherit; padding: 5px 10px; }
  .browse-variant:focus { outline: none; border-color: var(--accent); }
  .setting-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin: 0 0 16px 0; }
  .quit-row { margin-top: 20px; margin-bottom: 6px; padding-top: 16px; border-top: 1px solid var(--border); }
  .setting-label { font-size: 14px; color: var(--text); }
  .seg { display: flex; background: var(--hover); border-radius: 12px; padding: 3px; gap: 2px; }
  .seg-btn { border: none; background: transparent; cursor: pointer; font: inherit; font-size: 13.5px; color: var(--text-dim); padding: 6px 16px; border-radius: 9px; }
  .seg-btn.sel { background: var(--card); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
  .swatches { display: flex; gap: 10px; }
  .swatch { width: 32px; height: 32px; border-radius: 50%; border: 2px solid transparent; cursor: pointer; display: flex; align-items: center; justify-content: center; font-size: 14px; color: #fff; padding: 0; }
  .swatch.sel { border-color: var(--text); }
  .swatch-check { line-height: 1; text-shadow: 0 1px 2px rgba(0,0,0,.4); }
  .setting-stacked { display: flex; flex-direction: column; gap: 8px; margin: 0 0 4px 0; }
  .setting-hint { color: var(--muted); font-weight: 400; }
  .stopwords-input { width: 100%; box-sizing: border-box; border: 1px solid var(--border-strong); border-radius: 12px; background: var(--card); color: var(--text); font: inherit; font-size: 13px; line-height: 1.6; padding: 10px 12px; resize: vertical; outline: none; }
  .stopwords-input:focus { border-color: var(--accent); }
  .setting-note { font-size: 12px; color: var(--muted); }
  .mode-toggle { margin-bottom: 18px; }
  .mode-toggle .seg-btn { min-width: 90px; }
  .mode-toggle .seg-btn.sel { background: var(--accent); color: var(--accent-fg); box-shadow: none; font-weight: 700; }
  .workspace-input { width: 100%; box-sizing: border-box; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .work-badge { flex-shrink: 0; font-size: 11px; font-weight: 700; color: var(--accent-fg); background: var(--accent); border-radius: 999px; padding: 1px 8px; margin-left: 8px; }
  .ws-badge { display: inline-flex; align-items: center; gap: 4px; max-width: 110px; }
  .ws-badge span:last-child { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .work-banner { display: flex; align-items: center; gap: 8px; max-width: 860px; width: 100%; margin: 0 auto; padding: 10px 54px 0 24px; box-sizing: border-box; font-size: 12.5px; color: var(--muted-2); }
  .work-banner .work-badge { margin-left: 0; }
  .ctx-wrap { position: relative; margin-left: auto; display: inline-flex; cursor: default; }
  .ctx-popup { display: none; position: absolute; right: 0; top: calc(100% + 8px); background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 10px 14px; z-index: 30; min-width: 170px; }
  .ctx-wrap:hover .ctx-popup, .ctx-wrap:focus-within .ctx-popup { display: block; }
  .ctx-row { display: flex; justify-content: space-between; gap: 24px; font-size: 13px; color: var(--text); padding: 2px 0; font-variant-numeric: tabular-nums; }
  .ctx-row span:first-child { color: var(--muted-2); }
  .goal-strip { display: flex; align-items: center; gap: 8px; }
  .goal-wrap { position: relative; max-width: 860px; width: 100%; margin: 0 auto; padding: 6px 54px 0 24px; box-sizing: border-box; font-size: 12.5px; color: var(--muted-2); }
  .goal-popup { position: absolute; top: calc(100% + 6px); left: 24px; right: 54px; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 10px; z-index: 30; display: flex; flex-direction: column; gap: 8px; }
  .goal-edit-row { display: flex; gap: 8px; }
  .goal-edit-row .pull-input { flex: 1; min-width: 0; }
  .goal-actions-row { display: flex; gap: 8px; }
  .goal-badge { flex-shrink: 0; font-size: 11px; font-weight: 700; color: var(--accent); border: 1px solid var(--accent); border-radius: 999px; padding: 0 8px; }
  button.goal-badge { background: transparent; cursor: pointer; font-family: inherit; }
  button.goal-badge:hover { background: color-mix(in srgb, var(--accent) 12%, transparent); }
  button.goal-badge.goal-dim { color: var(--text); border-color: var(--border-strong); }
  button.goal-badge.goal-dim:hover { background: var(--hover); }
  .goal-text { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--text-dim); }
  .goal-state { flex-shrink: 0; font-size: 11px; color: var(--muted-2); }
  .compact-div { display: flex; align-items: center; gap: 12px; margin: 16px 0 10px; color: var(--muted-2); font-size: 12.5px; cursor: help; }
  .compact-div::before, .compact-div::after { content: ""; flex: 1; height: 1px; background: var(--border-2); }
  .compact-div > span { white-space: nowrap; }
  .work-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .work-composer-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .work-composer-bar .work-badge { margin-left: 0; }
  .work-composer-bar .workspace-input { flex: 1; min-width: 0; }
  .explorer-toggle-btn { position: absolute; top: 10px; right: 12px; z-index: 7; background: var(--bg); }
  .explorer-toggle-btn[aria-pressed="true"] { color: var(--accent); }
  .explorer { width: 300px; flex: 0 0 auto; border-left: 1px solid var(--border); background: var(--bg); display: flex; flex-direction: column; min-height: 0; min-width: 0; position: relative; }
  .explorer.hidden { display: none; }
  .explorer.resizing { user-select: none; }
  .exp-resize { position: absolute; left: -4px; top: 0; bottom: 0; width: 9px; cursor: ew-resize; z-index: 5; touch-action: none; }
  .exp-resize::after { content: ""; position: absolute; left: 4px; top: 0; bottom: 0; width: 1px; background: transparent; transition: background .15s; }
  .exp-resize:hover::after, .explorer.resizing .exp-resize::after { background: var(--accent); }
  .exp-resize:focus-visible { outline: 1.5px solid var(--accent); outline-offset: -1.5px; }
  .exp-btn { width: 26px; height: 26px; }
  .side-tabs { position: relative; display: flex; align-items: center; gap: 4px; padding: 10px 8px 8px 10px; border-bottom: 1px solid var(--border); }
  .side-tab { display: flex; align-items: center; gap: 2px; min-width: 0; max-width: 150px; border: none; background: transparent; border-radius: 9px; padding: 3px 3px 3px 8px; cursor: pointer; color: var(--muted-2); }
  .side-tab:hover { background: var(--hover-3); }
  .side-tab.sel { background: var(--hover-3); color: var(--text); }
  .side-tab-label { display: flex; align-items: center; gap: 6px; min-width: 0; border: none; background: transparent; color: inherit; font: inherit; font-size: 13px; padding: 2px 0; cursor: pointer; }
  .side-tab-title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .side-tab-x { flex-shrink: 0; border: none; background: transparent; color: var(--faint); font-size: 14px; line-height: 1; width: 20px; height: 20px; border-radius: 6px; cursor: pointer; padding: 0; }
  .side-tab-x:hover { background: var(--hover); color: var(--text); }
  .side-add { width: 26px; height: 26px; flex-shrink: 0; }
  .new-tab-menu { position: absolute; left: 8px; top: calc(100% - 2px); z-index: 42; min-width: 160px; }
  .new-tab-menu .ctx-item { display: flex; align-items: center; gap: 9px; }
  .side-view { flex: 1; display: flex; flex-direction: column; min-height: 0; min-width: 0; }
  .side-view.hidden { display: none; }
  .term-wrap { flex: 1; min-height: 0; padding: 8px 4px 8px 10px; background: #f4f4f4; }
  :global(html[data-theme="dark"]) .term-wrap { background: #161616; }
  .explorer-search { display: flex; align-items: center; gap: 7px; margin: 10px 10px 8px 10px; padding: 7px 6px 7px 10px; border: 1px solid var(--border); border-radius: 10px; background: var(--card); color: var(--muted); }
  .explorer-search-input { flex: 1; min-width: 0; border: none; outline: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding: 0; }
  .explorer-list { flex: 1; overflow-y: auto; overflow-x: hidden; padding: 0 6px 10px 6px; min-height: 0; }
  .exp-row { display: flex; align-items: center; gap: 7px; width: 100%; box-sizing: border-box; border: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding-top: 5px; padding-bottom: 5px; padding-right: 8px; border-radius: 8px; cursor: pointer; text-align: left; min-width: 0; }
  .exp-row:hover { background: var(--hover-3); }
  .exp-rename { flex: 1; min-width: 0; font: inherit; font-size: 12.5px; color: var(--text); background: var(--bg); border: 1.5px solid var(--accent); border-radius: 6px; padding: 2px 6px; outline: none; }
  .exp-chev { flex-shrink: 0; width: 12px; font-size: 10px; color: var(--muted); }
  .exp-icon { flex-shrink: 0; color: var(--muted-2); }
  .exp-dir .exp-icon { color: var(--accent); }
  .exp-name { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .exp-file .exp-name { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .exp-size { flex-shrink: 0; font-size: 11px; color: var(--faint); }
  .exp-empty { padding: 18px 14px; font-size: 13px; color: var(--muted); text-align: center; }
  .explorer-toast { position: absolute; left: 12px; right: 12px; bottom: 44px; z-index: 8; background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; box-shadow: 0 8px 24px rgba(0,0,0,.18); padding: 8px 12px; font-size: 12.5px; color: var(--text); pointer-events: none; }
  .mcp-btn { position: relative; }
  .mcp-dot { position: absolute; top: -4px; right: -4px; min-width: 15px; height: 15px; padding: 0 3px; box-sizing: border-box; border-radius: 999px; background: var(--accent); color: var(--accent-fg); font-size: 10px; font-weight: 700; line-height: 15px; text-align: center; }
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
  .mcp-row { display: flex; align-items: center; gap: 10px; width: 100%; box-sizing: border-box; border: none; background: transparent; color: var(--text); font: inherit; font-size: 15.5px; padding: 5px 8px; border-radius: 8px; cursor: pointer; text-align: left; }
  .mcp-row.sel { background: var(--accent); color: var(--accent-fg); }
  .mcp-row.sel .mcp-status, .mcp-row.sel .mcp-tools-n, .mcp-row.sel .mcp-mini { color: inherit; opacity: .9; }
  .mcp-name { font-weight: 700; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mcp-tools-n { font-size: 12px; color: var(--muted); white-space: nowrap; }
  .mcp-status { margin-left: auto; white-space: nowrap; font-size: 14.5px; }
  .mcp-status.connected { color: var(--accent); }
  .mcp-row.sel .mcp-status.connected { color: inherit; }
  .mcp-status.disabled { color: var(--muted); }
  .mcp-status.error { color: #d9732b; }
  .mcp-status.idle { color: var(--muted); }
  .mcp-row-actions { display: flex; align-items: center; gap: 2px; }
  .mcp-mini { border: none; background: transparent; color: var(--faint); font-size: 13px; line-height: 1; width: 22px; height: 22px; border-radius: 6px; cursor: pointer; padding: 0; }
  .mcp-mini:hover { background: rgba(127,127,127,.25); color: var(--text); }
  .mcp-row.sel .mcp-mini:hover { background: rgba(255,255,255,.25); color: inherit; }
  .mcp-mini.danger { width: auto; padding: 0 6px; font-size: 12px; }
  .mcp-row-error { font-size: 12px; color: #d9732b; padding: 0 8px 6px 8px; overflow-wrap: anywhere; white-space: pre-wrap; }
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

  @media (max-width: 700px) {
    .messages { padding: 20px 12px 8px 12px; }
    .composer-wrap { padding: 8px 12px 14px 12px; }
    .suggest-grid { grid-template-columns: 1fr; }
    .empty-title { font-size: 20px; margin-bottom: 20px; }
    .model-pill { max-width: 150px; }
    .model-drop { width: 300px; max-width: calc(100vw - 48px); }
  .collapsed-rail [data-testid='settings-btn-collapsed'] { display: none; }
    .sidebar { position: fixed; left: 0; top: 0; bottom: 0; z-index: 60; background: var(--bg); box-shadow: 8px 0 30px rgba(0,0,0,.15); }
    .explorer { position: fixed; right: 0; top: 0; bottom: 0; z-index: 60; background: var(--bg); box-shadow: -8px 0 30px rgba(0,0,0,.15); }
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
