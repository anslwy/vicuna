<script lang="ts">
  import { onMount, onDestroy, tick, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import { listModels, listRunning, loadModelIntoMemory, unloadModelFromMemory, pullModel, deleteModel, chatStream, generateStream, setOllamaBase, setServerOllama, ollamaBase as activeOllamaBase, isCloudModel, serverVersion, startServer as startOllamaServer, stopServer as stopOllamaServer, quitApp, setTrayStatus, isTauriRuntime, modelContextLength, modelThinking, modelFamily, type ChatMsg, type RunningModel } from "$lib/ollama";
  import { shouldWarnOllamaUnavailable } from "$lib/ollamaWarning";
  import { newSessionStart, readLastMode, writeLastMode } from "$lib/newSessionMode";
  import { notifyTurnEnd, notifyAgentAsk, getNotifyEnabled, setNotifyEnabled, getNotifySound, setNotifySound, getNotifySoundChoice, setNotifySoundChoice } from "$lib/notify";
  import { unlockChime, playChime, CHIME_OPTIONS, DEFAULT_CHIME } from "$lib/chime";
  import { renderMarkdown } from "$lib/markdown";
  import "katex/dist/katex.min.css";
  import { MODEL_CATALOG, CATALOG_UPDATED, type CatalogCat, type CatalogEntry } from "$lib/catalog";
  import { refreshCatalog, type CatalogOverride } from "$lib/library";
  import {
    initStorage,
    serverSync,
    adoptServerChat,
    normalizeServerChat,
    loadServerChatList,
    serverSaveRow,
    serverSavesIdle,
    loadChats,
    saveChats,
    loadModel,
    saveModel,
    loadCatalog,
    saveCatalog,
    loadPrefs,
    savePrefs,
    readLocalPrefs,
    readStartupPrefs,
    normalizeStoredWorkspace,
    writePendingStream,
    readPendingStream,
    clearPendingStream,
    clearPendingStreamFor,
    readDraft,
    writeDraft,
    clearDraft,
    loadActiveChat,
    saveActiveChat,
    chatActivity,
    DEFAULT_STOP_WORDS,
    type Chat
  } from "$lib/store";
  import { isAgentQuestion, toggleChoice, buildAskAnswer, laterAnswerMessage } from "$lib/askCard";
  import { workerDoneMessage, sessionListText, tailText, parseWorkerDone, demoteOtherLeads, sessionPermissions, type SessionRow } from "$lib/leadSessions";
  import { mcpWakeMessage, parseBgDoneMessage, parseMcpWakeMessage } from "$lib/backgroundRun";
  import { SCHEDULE_KEY, SCHEDULE_OPEN_NOTE, SCHEDULE_SERVER_NOTE, scheduleDiff, resolveWakeTime, wakeMessage, schedulePresets, parseCustomTime, customTimeError, defaultCustomTime, dateInputValue, timeInputValue, formatWhen, formatClock, sortScheduled, dueItems, markMissed, parseScheduled, previewText, trayScheduleLines, scheduledCountLabel, type ScheduledItem } from "$lib/schedule";
  import { createTurnReplyRoute } from "$lib/turnReply";
  import { stopReconcileAction, STOP_RECONCILE_DELAYS_MS } from "$lib/stopReconcile";
  import { isPluginEngine, pluginHarnessName, pluginEngineIcon } from "$lib/pluginHarness";
  import { sessionPick, missingHarnessReason, handoffAt, labelHarness, workspaceHandoffLabels, latestSummaryIndex, canClearContext, isAssistantReply, undoMessageRange } from "$lib/sessionEngine";
  import { formatToolCallHead, formatToolCallTail, formatToolResult, formatAskRecord, formatDisplayNote, listWorkTree, collectDropEntries, fileToBase64, stageUploadFile, renameWorkFile, deleteWorkFile, openWorkPathExternal, spawnTerm, writeTerm, resizeTerm, killTerm, completeMentionText, estimateWorkTokens, startWorkAgentTurn, stopWorkAgentTurn, workTurnLive, liveWorkTurns, attachWorkAgentTurn, attachWorkAgentSnapshot, queueWorkAgentMsg, removeWorkAgentMsg, answerWorkAgentAsk, ASK_LATER, workEngineInfo as fetchWorkEngineInfo, workAccountSupport, addWorkAccount, renameWorkAccount, removeWorkAccount, workSendGate, workReplyModel, listWorkModels, setWorkModel, selectWorkEngine, setWorkApproval, engineId, engineLabel, workEngineChoices, workEngineMenuItems, opencodeModelChoices, fetchWorkUsage, usageMeterTone, usageTabs, usageHoverRows, usageGaugeFrac, USAGE_POLL_INTERVAL_MS, formatUsageAge, isUsageStale, usagePollOrder, parseUsageCache, type WorkTreeEntry, type WorkAgentEvent, type WorkEngineInfo, type OpencodeModels, type WorkUsageReport } from "$lib/work";
  import Tooltip from "$lib/Tooltip.svelte";
  import GitPanel from "$lib/GitPanel.svelte";
  import { baseEngineId, changesCliAccount, type WorkAccountInfo } from "$lib/workGate";
  import { BridgeChannel, backendBaseUrl, invokeOrBridge, bridgeAvailable, isRemoteServer, isServerBacked, activeServerId, switchServer, listServers, connectServer, touchServer, removeServer, renameServer, serverLbProxyUrl, searchServerChats, openChatInWindow, type ServerInfo, type ServerChatHit } from "$lib/bridge";
  import { extractSessionId, sessionLinkAction } from "$lib/deepLink";
  import { orderPaletteRows, remoteQueryFor, sessionIdIn, showHostLabels, describeServerSearchError, mergeLocalServerHits } from "$lib/palette";
  import { isNewerRelease, pickAssetForPlatform, releaseTimestamp, currentPlatform, formatAssetSize, formatBuildDate, type ReleaseInfo } from "$lib/update";
  import UpdatePromptModal from "$lib/UpdatePromptModal.svelte";
  import FolderPickerModal from "$lib/FolderPickerModal.svelte";
  import TelegramModal from "$lib/TelegramModal.svelte";
  import BridgeModal from "$lib/BridgeModal.svelte";
  import PluginsPanel from "$lib/PluginsPanel.svelte";
  import SkillsModal from "$lib/SkillsModal.svelte";
  import EngineIcon from "$lib/EngineIcon.svelte";
  import UsageModal from "$lib/UsageModal.svelte";
  import { harnessUsageUrl } from "$lib/harnessUsageLinks";
  import ServerDialogModal from "$lib/ServerDialogModal.svelte";
  import GotoPaletteModal from "$lib/GotoPaletteModal.svelte";
  import ScheduleModals from "$lib/ScheduleModals.svelte";
  import McpPopup from "$lib/McpPopup.svelte";
  import SettingsModal from "$lib/SettingsModal.svelte";
  import SidePanel, { type SideTab } from "$lib/SidePanel.svelte";
  import ConfirmModals from "$lib/ConfirmModals.svelte";
  import {
    ACCENTS,
    formatDuration,
    formatPullRate,
    formatLiveElapsed,
    cmpVersion,
    defaultServerName,
    stopWordsToText,
    textToStopWords,
    capLevel,
    isMlxVariant,
    formatSize,
    stripHarnessMarkup,
    toHistory,
    compactedHistory,
    displayWorkspace,
    workspaceBase,
    searchSnippet,
    highlightHtml
  } from "$lib/chatFormat";
  import { enabledSkillCount, listSkills, newSkillSelection, removeSkill, resolveSkills, saveSkill, saveSkillDefaults, skillSwitches, type SkillsEnabledMap, type SkillSummary } from "$lib/skills";
  import { parsePanelRequest, handlePanelRequest, panelUrl, THEME_VARS, type PanelHost, type PanelTheme, type PanelContext } from "$lib/pluginBridge";
  import { allThemes, browserThemeParams, themeKey, themeProperties, themeAccent, selectedAccent, reconcileActive, readActiveTheme, resolveTerminalColors, writeActiveTheme, readAccentMuted, writeAccentMuted, safeColor, type ActivePluginTheme, type InstalledPlugin } from "$lib/pluginTheme";
  import { telegramConnLabel, telegramConnState, telegramConnTitle, type TelegramConnState, type TelegramStatusSnapshot } from "$lib/telegram";
  import { bridgeConnLabel, bridgeConnState, bridgeConnTitle, bridgeDisplayName, safeIconPath, type BridgeStatusSnapshot } from "$lib/bridgePlugin";
  import { hideSplash } from "$lib/splash";
  import {
    APP_UPDATE_RELEASES_URL,
    createReleaseNotesPreview,
    fetchGithubReleaseNotesMetadata,
    formatAppUpdateInstallError,
    loadAppUpdatePreferenceFromStorage,
    normalizeUpdateErrorMessage,
    persistAppUpdatePreference,
    type AppUpdateInfo,
    type AppUpdateStatus
  } from "$lib/appUpdate";
  import { CONTINUE_MODES, buildRawContinuePrompt, isRunawayRepetition, normalizeContinueMode, normalizePromptStyle, promptStyleForFamily, stripRunawayTail, useRawContinue, type ContinueMode, type PromptStyle } from "$lib/continue";
  import { listMcpServers, saveMcpDefaults, resolveMcpEnabled, isMcpEnabled, toggleMcpEnabled, isFixedMcp, saveMcpServer, deleteMcpServer, connectServer as connectMcpBackend, disconnectServer as disconnectMcpBackend, listServerTools as listMcpServerTools, describeServer as describeMcpServer, validateServer as validateMcpServer, parseStringMap as parseMcpStringMap, sanitizeToolName, signInServer as signInMcpBackend, signOutServer as signOutMcpBackend, loginPresent as loginPresentBackend, isAuthError, type McpEnabledMap, type McpServerConfig, type McpTool } from "$lib/mcp";
  import { BUILT_IN_MCPS, builtinToConfig, type BuiltinMcp } from "$lib/mcp-builtins";
  import { cliStatus, cliInstall, cliUninstall, focusAppWindow, extractOpenPath, type CliStatus } from "$lib/cli";
  import { LB_URL, isBrowserMcpId, browserStatus, browserStart, browserRestart, browserTakeover, browserEnsureEngine, formatDownloadProgress, type BrowserStatus, type EngineDownloadProgress } from "$lib/browser";
  import { browserPollAction, browserPollRevived, browserButtonAction } from "$lib/browserPoll";
  import { ALL_FILTER, NO_WORKSPACE_FILTER, NO_WORKSPACE_SENTINEL, isNoWorkspacePath, resolveDraftWorkspace, folderOptions, resolveSessionFilter, filterChats, groupByDate, workspaceOfFolderKey, workspaceFilterKey, folderOptionKey, visibleChats, isPinnedRow, showWorkspaceBadge } from "$lib/sidebar";
  import {
    captureOpenToolDetails,
    captureViewAnchor,
    followAfterScroll,
    restoreOpenToolDetails,
    restoreViewAnchor,
    scrollKeyDirection,
    upwardScrollInput,
    type OpenDetailsSnapshot,
    type ViewAnchor
  } from "$lib/scrollFollow";
  import { fetchOpenRouterModels, testOpenRouterKey, readOpenRouterCache, writeOpenRouterCache, filterOpenRouterModels, formatContextLength, formatModelPricing, formatCacheAge, formatUsd, isFreeModel, supportsVision, supportsTools, maskKey, setOpenRouterApiKey, isOpenRouterSelection, openRouterModelId, openRouterSelection, displayOpenRouterId, findOpenRouterModel, OPENROUTER_FILTERS, type OpenRouterFilter, type OpenRouterKeyInfo, type OpenRouterModel } from "$lib/openrouter";
  import { OLLAMA_ICON_PATH, OPENROUTER_ICON_PATH, CLAUDE_ICON_PATH, OPENCODE_ICON_PATH, OPENCODE_ICON_INNER_PATH, CODEX_ICON_OUTLINE_PATH, CODEX_ICON_PROMPT_PATH, FX_ICON_PATH, AGY_ICON_PATH, PI_ICON_PATH } from "$lib/modelIcons";
  import pkg from "../../package.json";
  import type { Terminal as XTerm, ITheme as XTermTheme } from "xterm";
  import type { FitAddon as XFitAddon } from "@xterm/addon-fit";

  let { sessionId = null }: { sessionId: string | null } = $props();

  let chats = $state<Chat[]>([]);
  let activeId = $state<string | null>(null);
  /** chats loaded: every effect/gate that reads the session list waits on it */
  let ready = $state(false);
  /** the window shell can draw (prefs applied). Set before the session list
   *  arrives: a remote server's full history can take a minute on a slow
   *  link, and the window used to stay blank (white once the splash's 15 s
   *  backstop removed it) until it landed. */
  let uiReady = $state(false);
  /** the session list arrived (or failed): hides the sidebar loading row */
  let chatsLoaded = $state(false);
  // fade out the launch splash (src/app.html) once real content can show
  $effect(() => {
    if (uiReady || ready) hideSplash();
  });
  /** shown while the session list is still loading (seconds, ticks) */
  let loadingSince = $state(0);
  let loadingTick = $state(0);
  $effect(() => {
    if (chatsLoaded || !uiReady) return;
    const t = setInterval(() => (loadingTick = Date.now()), 1000);
    return () => clearInterval(t);
  });
  const loadingSecs = $derived(loadingSince ? Math.max(0, Math.floor(((loadingTick || loadingSince) - loadingSince) / 1000)) : 0);
  /** sends would race the incoming list (it replaces `chats` wholesale) */
  function blockedWhileLoading(): boolean {
    if (ready) return false;
    flashNotice("Sessions are still loading. Try again in a moment.");
    return true;
  }
  const initialPrefs = typeof window !== "undefined" ? readStartupPrefs() : { theme: "light" as const, accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "", zoom: 1, defaultWorkspace: "", thinkLevel: "auto", continueMode: "raw" as const, openrouterApiKey: "", openrouterAdded: [] as string[], sessionFilter: "all" };
  // the chat/Work transports read the key from the openrouter module
  setOpenRouterApiKey(initialPrefs.openrouterApiKey ?? "");
  let sidebarOpen = $state(initialPrefs.sidebarOpen);
  let defaultWorkspace = $state(initialPrefs.defaultWorkspace ?? "");
  /** mode + workspace picker for the session being started (home or empty draft) */
  // starts on the mode last picked (remembered across launches, see newSessionMode.ts)
  let pendingMode = $state<"chat" | "work">(typeof window !== "undefined" ? readLastMode(localStorage) : "chat");
  let pendingWorkspace = $state(resolveDraftWorkspace(initialPrefs.defaultWorkspace));
  const continueWorkspace = $derived(defaultWorkspace || (isNoWorkspacePath(pendingWorkspace) ? "" : pendingWorkspace));
  let workspaceError = $state<string | null>(null);
  /** true while the in-thread workspace input is focused: keeps the picker
   *  mounted while typing (it hides once a workspace is set) */
  let workspaceEditing = $state(false);
  /** browser-only workspace picker popup (plain browsers have no native
   *  folder dialog — and no window.prompt: headless browsers auto-dismiss
   *  it. Draft commits on Enter, discards on Esc/backdrop). */
  let workspacePickerOpen = $state(false);
  let workspaceDraft = $state("");
  let workspacePickerInput = $state<HTMLInputElement | null>(null);
  $effect(() => {
    if (workspacePickerOpen) workspacePickerInput?.focus();
  });
  /** visible "command running" indicator while a work tool executes.
   *  Per session (chatId → label): several sessions can run at once, and a
   *  background turn must never overwrite the open session's indicator. */
  let toolStatuses = $state<Record<string, string>>({});
  function setToolStatus(chatId: string, label: string | null) {
    if (label == null) {
      if (!(chatId in toolStatuses)) return;
      const { [chatId]: _, ...rest } = toolStatuses;
      toolStatuses = rest;
    } else {
      if (toolStatuses[chatId] === label) return;
      toolStatuses = { ...toolStatuses, [chatId]: label };
    }
  }
  /** live multiple-choice question from a work agent turn, per session. The
   *  turn blocks until the user answers; each session keeps its own card so
   *  concurrent turns don't overwrite each other's question. */
  interface PendingAsk {
    chatId: string;
    index: number;
    question: string;
    options: string[];
    /** multi-select question (vicuna-mcp ask_user `multiple`) */
    multi: boolean;
    /** the labels picked so far in a multi-select question */
    picked: string[];
    /** agent question (vicuna-mcp ask_user): choices prefill the answer box
     *  instead of answering, and the box takes pasted images. Permission
     *  asks (Allow/Deny) still answer on click. */
    agentQuestion: boolean;
    /** the answer box's draft text */
    text: string;
    /** images pasted into the answer box (data URLs) */
    images: string[];
    /** Other… opened the box on a permission ask */
    otherOpen: boolean;
    resolve: (answer: string, outcome: "answer" | "dismiss" | "cancel" | "later", display?: string) => void;
  }
  let pendingAsks = $state<Record<string, PendingAsk>>({});
  /** the open session's ask card (null when it has none pending) */
  const pendingAsk = $derived(activeId != null ? (pendingAsks[activeId] ?? null) : null);
  /** agent questions the user put off with "Answer later": the ask_user call
   *  already returned, so the answer goes to the session as a message */
  interface LaterAsk {
    id: string;
    chatId: string;
    index: number;
    question: string;
    options: string[];
    multi: boolean;
    picked: string[];
    text: string;
    images: string[];
  }
  let laterAsks = $state<LaterAsk[]>([]);
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
  /** transient side-panel notice (files, terminal, browser actions):
   *  floats at the bottom of the right panel without replacing tab content */
  let expNotice = $state<string | null>(null);
  let expNoticeTimer: ReturnType<typeof setTimeout> | null = null;

  function flashExpNotice(msg: string, ttl = 4500) {
    expNotice = msg;
    if (expNoticeTimer) clearTimeout(expNoticeTimer);
    expNoticeTimer = setTimeout(() => {
      expNotice = null;
      expNoticeTimer = null;
    }, ttl);
  }
  /** explorer panel width (drag the left edge; in-memory only) */
  let explorerWidth = $state(300);
  let expResizing = $state(false);
  /** side-panel tabs: one Files view max, one Browser max, one Git max, any number of terminals */
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
  /** terminal-style prompt history: ArrowUp/Down walks this session's sent
   *  user messages; `historyDraft` is the in-progress text to restore on the
   *  way back down. `historyIndex` is null while not browsing. */
  let historyIndex = $state<number | null>(null);
  let historyDraft = "";
  let models = $state<string[]>(["gemma4:31b-cloud"]);
  let selectedModel = $state("gemma4:31b-cloud");
  let modelOpen = $state(false);
  let engineMenuOpen = $state(false);
  $effect(() => {
    if (!modelOpen) engineMenuOpen = false;
  });
  let modelQuery = $state("");
  /** sessions with a live chat-completion stream. Per session (not one global
   *  flag) so one thread working never blocks starting or using another:
   *  gates check the OPEN session only, while the sidebar/tray aggregate. */
  let streamingIds = $state<string[]>([]);
  /** wall-clock start per live chat stream: sidebar hover shows elapsed time */
  let streamingStartedAt = $state<Record<string, number>>({});
  /** any chat stream live (sidebar shimmer clock + tray aggregate) */
  const streaming = $derived(streamingIds.length > 0);
  /** true for the whole Work turn, including tool dispatch + the wait for the
   *  next round's first token — `streaming` only covers a single request */
  let workRunningIds = $state<string[]>([]);
  /** manual compaction in flight (/compact): blocks sends + edits like a turn */
  let compactingIds = $state<string[]>([]);
  /** any Work turn live (sidebar shimmer clock + tray aggregate) */
  const workRunning = $derived(workRunningIds.length > 0);
  /** wall-clock start per live Work turn: sidebar hover shows elapsed time */
  let workStartedAt = $state<Record<string, number>>({});
  /** ticking clock for live elapsed labels: advances once a second while a
   *  turn is live so sidebar hover tooltips stay current */
  let now = $state(Date.now());
  /** sessions the user asked to stop (Stop / double-Esc / chat deleted):
   *  per session so stopping one turn never aborts another */
  const workStopIds = new Set<string>();
  /** sessions whose turn was interrupted by "Send now" (a queued chip or a
   *  finished background job): settle restarts with the queue instead of
   *  handing it back to the composer */
  const sendNowIds = new Set<string>();
  // turn-finished OS notifications: chat turns the user stopped / that
  // errored, and a short debounce so a tool loop's back-to-back streams
  // notify once
  const chatStopIds = new Set<string>();
  const chatErrorIds = new Set<string>();
  const notifyTimers = new Map<string, ReturnType<typeof setTimeout>>();
  let notifyTurns = $state(true);
  let notifySound = $state(true);
  let notifySoundChoice = $state<string>(DEFAULT_CHIME);
  function lastReply(id: string): string {
    const c = chats.find((x) => x.id === id);
    return c?.messages.findLast(isAssistantReply)?.content ?? "";
  }
  let workTurnSeq = 0;
  /** current frontend turn number per session: a stale turn's late events are
   *  dropped by comparing against this (a single global slot would invalidate
   *  every other live turn the moment a second one starts) */
  const workTurnSeqByChat = new Map<string, number>();
  /** backend turn id of each live Rust agent loop — needed for
   *  stop/steer/ask-answer routing while a turn runs */
  const workTurnIds = new Map<string, string>();
  /** this window's server records Work turns itself (server_turns.rs): it
   *  writes their transcript, runs follow-ups and sends scheduled messages,
   *  so the window skips saving live turns and adopts the server's copy */
  let serverPersist = $state(false);
  let serverSessionTools = $state(false);
  let serverChatTurns = $state(false);
  const scheduleNote = $derived(serverPersist ? SCHEDULE_SERVER_NOTE : SCHEDULE_OPEN_NOTE);
  /** per session: settle its current Work turn as stopped, from outside the
   *  turn's closure (the stop reconciliation below uses it when the
   *  backend's `done` never arrived) */
  const workForceSettle = new Map<string, () => void>();
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
    return activeChat ?? { mcpEnabled: homeMcpEnabled };
  }

  async function newMcpEnabled(draft?: McpEnabledMap): Promise<McpEnabledMap> {
    const defaults = await listMcpServers();
    return resolveMcpEnabled(defaults, { mcpEnabled: draft });
  }

  async function setMcpForSession(sid: string | null, id: string, enabled: boolean) {
    if (sid === null) {
      homeMcpEnabled = toggleMcpEnabled([], { mcpEnabled: homeMcpEnabled }, id, enabled);
      return true;
    }
    const updated = await serverAction("session_update", { id: sid, mcpToggle: { id, enabled } });
    if (updated !== false) return updated !== null;
    const chat = chats.find((c) => c.id === sid);
    if (!chat) return false;
    chat.mcpEnabled = toggleMcpEnabled(mcpServers, chat, id, enabled);
    save();
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
    const sid = activeId;
    try {
      mcpServers = await saveMcpServer(builtinToConfig(b));
      if (!await setMcpForSession(sid, b.name, true)) return;
    } catch (e: any) {
      mcpBuiltinError = e?.message ?? String(e);
      return;
    } finally {
      mcpUnmarkBusy(`builtin:${b.key}`);
    }
    if (sid === activeId) await connectMcpServer(b.name);
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
        await ensureBrowserDaemon();
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
    const sid = activeId;
    const enable = !isMcpEnabled(cfg, currentMcpSelection());
    mcpMarkBusy(id);
    try {
      if (!await setMcpForSession(sid, id, enable)) return;
    } finally {
      mcpUnmarkBusy(id);
    }
    if (enable && sid === activeId && isMcpEnabled(cfg, currentMcpSelection()) && !mcpLive[id]?.connected) {
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
      if (onServer) {
        mcpOpen = false;
        explorerOpen = true;
        openSideBrowser();
        await ensureBrowserDaemon();
      }
      const info = await signInMcpBackend(cfg, onServer);
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
      if (onServer) flashExpNotice(msg);
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
    const sid = activeId;
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

  /** `live-browser:open-external` from the Browser-tab iframe: lb's own
   *  target=_blank links die silently in desktop webviews (no new-window
   *  implementation), so when framed lb posts out instead and the app opens
   *  the URL natively. Strict loopback-origin + shape check. */
  function onBrowserFrameMessage(e: MessageEvent) {
    const base = onServer ? browserProxyUrl : (browserDaemon?.url ?? LB_URL);
    if (!base) return;
    try {
      if (e.origin !== new URL(base).origin) return;
    } catch {
      return;
    }
    const d = e.data as { type?: unknown; url?: unknown } | null;
    if (!d || d.type !== "live-browser:open-external" || typeof d.url !== "string") return;
    if (!/^https?:\/\//.test(d.url)) return;
    void openExternal(d.url);
  }

  /** Open a URL outside the app: system browser on desktop (Tauri opener),
   *  new tab in plain browsers. Never navigates the app webview itself. */
  async function openExternal(url: string): Promise<void> {
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

  /** Open the token generation page (github-only PAT prompt). */
  async function openTokenPage(e: MouseEvent) {
    e.preventDefault();
    await openExternal("https://github.com/settings/tokens/new");
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

  /** prompts typed while a work turn is live: they join the context at the
   *  next round (after the tool results), never as a parallel turn */
  let queuedMsgs = $state<{ chatId: string; id: string; msg: ChatMsg }[]>([]);
  /** first Esc arms interrupt, a second Esc within 2.5s stops the agent */
  let escArmed = $state(false);
  let escTimer: ReturnType<typeof setTimeout> | null = null;
  /** sessions with a Continue append in flight (drives the thinking shimmer) */
  let continuingIds = $state<string[]>([]);
  const continuing = $derived(activeId != null && continuingIds.includes(activeId));
  /** picked or pasted image attachments as data URLs (shown as thumbnails, sent as base64) */
  let attachImages = $state<string[]>([]);
  /** composer file attachments (any type): staged to ~/vicuna/uploads/<session>/ on send */
  let attachFiles = $state<{ id: number; name: string; size: number; file: File }[]>([]);
  let attachFileSeq = 0;
  let error = $state<string | null>(null);
  /** auto-clear delay for the error box (ms): real failures linger long
   *  enough to read, then clear — nothing squats until restart anymore.
   *  Command outcomes use the separate neutral `notice` toast below. */
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
  /** Transient command feedback (undo/redo/compact): a neutral toast that
   *  clears itself. These are outcomes, not failures — they must never use
   *  the red error box and must never linger until dismissed. */
  let notice = $state<string | null>(null);
  let noticeTimer: ReturnType<typeof setTimeout> | null = null;
  function flashNotice(text: string, ttl = 4000) {
    notice = text;
    if (noticeTimer) {
      clearTimeout(noticeTimer);
      noticeTimer = null;
    }
    noticeTimer = setTimeout(() => {
      noticeTimer = null;
      if (notice === text) notice = null;
    }, ttl);
  }
  /** one AbortController per live chat stream (Stop / delete aborts only its
   *  own session's request, never a turn running in another session) */
  const aborters = new Map<string, AbortController>();
  let messagesEl = $state<HTMLDivElement | null>(null);
  let composerEl = $state<HTMLTextAreaElement | null>(null);
  let composerBackdrop = $state<HTMLDivElement | null>(null);
  let fileInput = $state<HTMLInputElement | null>(null);
  let editingIndex = $state<number | null>(null);
  let editDraft = $state("");
  let editImages = $state<string[]>([]);
  let copiedKey = $state<string | null>(null);
  let isPinned = $state(true);
  let scrollFrame = 0;
  /** viewport anchor held across a streamed DOM write while the reader is paused */
  let viewAnchor: ViewAnchor | null = null;
  /** open <details class="tool-result"> cards held across a DOM write */
  let openDetailsSnapshot: OpenDetailsSnapshot = [];
  /** incremented to invalidate any in-flight releaseReaderPlace() on user scroll input */
  let scrollRestoreGen = 0;
  /** true while a paused-reader restore is in flight so momentary collapse cannot flip isPinned */
  let scrollRestoring = false;
  let scrollRestoreRaf = 0;
  let draggingScrollbar = false;
  let touchScrollY: number | null = null;

  function cancelScrollRestore() {
    scrollRestoreGen++;
    viewAnchor = null;
    openDetailsSnapshot = [];
    if (scrollRestoreRaf) {
      cancelAnimationFrame(scrollRestoreRaf);
      scrollRestoreRaf = 0;
    }
    scrollRestoring = false;
  }

  function hasMessageSelection() {
    const sel = window.getSelection();
    return !!(sel && !sel.isCollapsed && messagesEl?.contains(sel.anchorNode));
  }

  // Code blocks and other nested scroll areas should consume their own input
  // without pausing follow on the surrounding transcript.
  function nestedScrollerConsumes(target: EventTarget | null, deltaY: number) {
    let el = target instanceof Element ? target : null;
    while (el && el !== messagesEl) {
      if (el.scrollHeight > el.clientHeight && /^(auto|scroll)$/.test(getComputedStyle(el).overflowY)) {
        if (deltaY < 0 ? el.scrollTop > 0 : el.scrollTop + el.clientHeight < el.scrollHeight) return true;
      }
      el = el.parentElement;
    }
    return false;
  }

  function pauseForScrollInput(deltaY: number, target: EventTarget | null) {
    if (!messagesEl || nestedScrollerConsumes(target, deltaY)) return;
    if (deltaY !== 0) cancelScrollRestore();
    if (upwardScrollInput(deltaY, messagesEl.scrollTop)) {
      isPinned = false;
    }
  }

  function onMessagesWheel(e: WheelEvent) {
    if (!e.ctrlKey) pauseForScrollInput(e.deltaY, e.target);
  }

  function onMessagesTouchStart(e: TouchEvent) {
    touchScrollY = e.touches.length === 1 ? e.touches[0].clientY : null;
  }

  function onMessagesTouchMove(e: TouchEvent) {
    const y = e.touches.length === 1 ? e.touches[0].clientY : null;
    if (y != null && touchScrollY != null) pauseForScrollInput(touchScrollY - y, e.target);
    touchScrollY = y;
  }

  function onMessagesPointerDown(e: PointerEvent) {
    if (!messagesEl || e.pointerType !== "mouse" || e.button !== 0 || e.target !== messagesEl) return;
    const rect = messagesEl.getBoundingClientRect();
    // Overlay scrollbars have no layout width. Reserve their edge too.
    const gutter = Math.max(12, messagesEl.offsetWidth - messagesEl.clientWidth);
    if (e.clientX < rect.right - gutter) return;
    cancelScrollRestore();
    draggingScrollbar = true;
    isPinned = false;
  }

  function onMessagesPointerUp() {
    if (!draggingScrollbar) return;
    draggingScrollbar = false;
    onMessagesScroll();
  }

  function onMessagesKeyDown(e: KeyboardEvent) {
    if (!messagesEl || e.defaultPrevented) return;
    const target = e.target instanceof Element ? e.target : null;
    if (target?.closest("input, textarea, select, button, a, summary, [contenteditable]:not([contenteditable=false]), [role=textbox]")) return;
    if (target !== document.body && target !== document.documentElement && !messagesEl.contains(target)) return;
    const direction = scrollKeyDirection(e);
    if (direction && !nestedScrollerConsumes(target, direction)) {
      pauseForScrollInput(direction, target);
    }
  }

  function onMessagesScroll() {
    if (!messagesEl) return;
    // Ignore scroll events fired by a momentary DOM collapse or our own
    // anchor restore while releaseReaderPlace() is in flight — otherwise a
    // collapsed answer can make scrollHeight - clientHeight - scrollTop <= 2
    // and falsely flip isPinned back to true.
    if (scrollRestoring) return;
    // WebKit may reset scrollTop during a streamed {@html} replacement,
    // focus loss, or layout correction. A scroll event alone is NOT proof
    // the reader scrolled up. Only the input handlers above may unpin.
    isPinned = followAfterScroll(isPinned, messagesEl, draggingScrollbar);
    if (isPinned) scrollBottom();
  }

  function jumpToBottom() {
    cancelScrollRestore();
    isPinned = true;
    scrollBottom();
  }

  // A paused reader must not be moved by the stream. Auto-follow is not the
  // only thing that moves the viewport: when the answer above grows, Chromium
  // still anchors an overflowing scroller (despite `overflow-anchor: none`) and
  // pushes the text down; when `scrollHeight` collapses — an `{@html}`
  // replacement tears down the last message's `.answer` or the thinking block
  // collapses — scrollTop is clamped and the view jumps back up toward the top
  // of the last message. Neither produces a scroll event the app could tell
  // apart from a real gesture, so record open tool cards + what is on screen
  // around the write and put them back.
  function holdReaderPlace() {
    if (!messagesEl) return false;
    openDetailsSnapshot = captureOpenToolDetails(messagesEl);
    // a pinned view is owned by scrollBottom(); a gesture or a selection during
    // the stream clears the anchor so the reader is never fought
    if (isPinned || draggingScrollbar || hasMessageSelection()) {
      viewAnchor = null;
      return openDetailsSnapshot.length > 0;
    }
    viewAnchor = captureViewAnchor(messagesEl, messagesEl);
    return viewAnchor != null || openDetailsSnapshot.length > 0;
  }

  /** Re-apply open tool cards and the anchor recorded by holdReaderPlace. */
  async function releaseReaderPlace() {
    const anchor = viewAnchor;
    const details = openDetailsSnapshot;
    viewAnchor = null;
    openDetailsSnapshot = [];
    if ((!anchor && details.length === 0) || !messagesEl) return;
    const gen = ++scrollRestoreGen;
    if (scrollRestoreRaf) {
      cancelAnimationFrame(scrollRestoreRaf);
      scrollRestoreRaf = 0;
    }
    if (anchor) scrollRestoring = true;
    // Let Svelte commit the new {@html} markdown before restoring open cards
    // and the paused viewport position.
    await tick();
    if (gen !== scrollRestoreGen || !messagesEl) {
      if (gen === scrollRestoreGen) scrollRestoring = false;
      return;
    }
    if (details.length > 0) restoreOpenToolDetails(messagesEl, details);
    if (!anchor || isPinned || draggingScrollbar || hasMessageSelection()) {
      scrollRestoring = false;
      return;
    }
    restoreViewAnchor(messagesEl, anchor);
    // One animation frame later, WebKit/Chromium post-layout clamping or
    // scroll-anchoring adjustments have landed; re-apply once if needed.
    scrollRestoreRaf = requestAnimationFrame(() => {
      scrollRestoreRaf = 0;
      if (gen !== scrollRestoreGen || !messagesEl) {
        if (gen === scrollRestoreGen) scrollRestoring = false;
        return;
      }
      if (!isPinned && !draggingScrollbar && !hasMessageSelection()) {
        restoreViewAnchor(messagesEl, anchor);
      }
      scrollRestoring = false;
    });
  }

  /** Run a message-list state mutation while preserving a paused reader's
   *  scroll position and any open tool-result <details> cards when `chatId`
   *  is the currently open session. */
  function withPreservedScroll(chatId: string, mutate: () => void) {
    const isOpen = chatId === activeId;
    const held = isOpen && holdReaderPlace();
    mutate();
    if (isOpen) scrollBottom();
    if (held) void releaseReaderPlace();
  }

  // The send/stop/queue buttons are an {#if} swap: starting or stopping a
  // turn DESTROYS the focused node when it was mouse-clicked. Safari then
  // drops focus to <body> and scrolls the thread to the top, permanently
  // unpinning auto-follow (Chromium leaves the viewport alone). Park focus
  // in the composer — which is never destroyed — before the swap happens.
  // Moving focus must not itself scroll the transcript or the page.
  function parkComposerFocus() {
    const ae = document.activeElement;
    if (
      ae instanceof HTMLButtonElement &&
      ["send-btn", "stop-btn", "queue-btn"].includes(ae.dataset.testid ?? "")
    ) {
      composerEl?.focus({ preventScroll: true });
    }
  }

  // opencode-style run stats: "27m 48s", "8.1s" under a minute
  const activeChat = $derived(chats.find((c) => c.id === activeId) ?? null);
  const activeSummaryIndex = $derived(latestSummaryIndex(activeChat?.messages ?? []));
  // composer reflects the starter toggle while starting (home or empty draft);
  // a thread with messages keeps its own mode
  const composerMode = $derived(
    !activeChat || activeChat.messages.length === 0 ? pendingMode : (activeChat.mode ?? "chat")
  );
  const queuedForActive = $derived(queuedMsgs.filter((q) => q.chatId === activeId));
  /** per-session liveness: several sessions can stream/work/compact at once */
  function isChatStreaming(id: string): boolean {
    return streamingIds.includes(id);
  }
  function isWorkRunning(id: string): boolean {
    return workRunningIds.includes(id);
  }
  function isCompacting(id: string): boolean {
    return compactingIds.includes(id);
  }
  function isSessionBusy(id: string | null): boolean {
    if (id == null) return false;
    return isChatStreaming(id) || isWorkRunning(id) || isCompacting(id);
  }
  function markStreaming(id: string) {
    clearTimeout(notifyTimers.get(id));
    notifyTimers.delete(id);
    if (!streamingIds.includes(id)) streamingIds = [...streamingIds, id];
    streamingStartedAt = { ...streamingStartedAt, [id]: Date.now() };
  }
  function unmarkStreaming(id: string) {
    if (!streamingIds.includes(id)) return;
    streamingIds = streamingIds.filter((x) => x !== id);
    clearTimeout(notifyTimers.get(id));
    notifyTimers.set(id, setTimeout(() => {
      notifyTimers.delete(id);
      if (isSessionBusy(id)) { chatStopIds.delete(id); chatErrorIds.delete(id); return; }
      const userStopped = chatStopIds.delete(id);
      const end = chatErrorIds.delete(id) ? "error" : "done";
      void notifyTurnEnd({ title: chats.find((x) => x.id === id)?.title, mode: "chat", end, reply: lastReply(id), userStopped });
    }, 1500));
    const { [id]: _, ...rest } = streamingStartedAt;
    streamingStartedAt = rest;
  }
  /** `startedAt`: the turn's real start when reattaching to one the server
   *  kept running (the sidebar clock must not restart at the reopen) */
  function markWorkRunning(id: string, startedAt?: number) {
    if (!workRunningIds.includes(id)) workRunningIds = [...workRunningIds, id];
    workStartedAt = { ...workStartedAt, [id]: startedAt && startedAt <= Date.now() ? startedAt : Date.now() };
  }
  function unmarkWorkRunning(id: string) {
    if (!workRunningIds.includes(id)) return;
    workRunningIds = workRunningIds.filter((x) => x !== id);
    const { [id]: _, ...rest } = workStartedAt;
    workStartedAt = rest;
  }
  function markCompacting(id: string) {
    if (!compactingIds.includes(id)) compactingIds = [...compactingIds, id];
  }
  function unmarkCompacting(id: string) {
    if (!compactingIds.includes(id)) return;
    compactingIds = compactingIds.filter((x) => x !== id);
  }
  /** the OPEN session has a live turn (single request, Work agent turn, or
   *  compaction). Every edit/send/delete gate reads this, so a turn running
   *  in another session never blocks the one being used. */
  /** sessions the server reports a live Work turn for, before this window
   *  has attached to it: shown as running at once (the attach waits on a
   *  snapshot, and a running worker used to look stopped meanwhile) */
  let liveHintIds = $state<string[]>([]);
  /** the open session is a list copy still being swapped for its full copy */
  const activeLoading = $derived(!!activeChat?.partial);
  const busy = $derived(isSessionBusy(activeId) || activeLoading || (activeId != null && liveHintIds.includes(activeId)));
  const activeStreaming = $derived(activeId != null && streamingIds.includes(activeId));
  const activeWorkRunning = $derived(activeId != null && workRunningIds.includes(activeId));
  /** sidebar shimmer: a session with a live chat stream or work turn */
  function isSessionStreaming(id: string): boolean {
    return isChatStreaming(id) || isWorkRunning(id) || liveHintIds.includes(id);
  }
  // live elapsed ("32m 25s", "1h 32m", "3d 2h") since the task/query started
  /** elapsed ms for a live session (work turn start wins between rounds);
   *  null when the session isn't live. Reads `now` so hover tooltips tick. */
  function liveElapsedMs(id: string): number | null {
    if (isWorkRunning(id) && workStartedAt[id] != null) return now - workStartedAt[id];
    if (isChatStreaming(id) && streamingStartedAt[id] != null) return now - streamingStartedAt[id];
    return null;
  }
  /** sidebar hover tooltip for a live session ("Working · 32m 25s") */
  function liveTitle(id: string): string | undefined {
    const ms = liveElapsedMs(id);
    if (ms == null) return undefined;
    const kind = isWorkRunning(id) ? "Working" : "Streaming";
    return `${kind} · ${formatLiveElapsed(ms)}`;
  }
  // advance the live clock while a turn runs (or a browser start is in
  // flight) so elapsed tooltips/labels stay current
  $effect(() => {
    if (!streaming && !workRunning && !browserStarting) return;
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
  let lastTraySchedKey = "";
  let lastTrayRunning = false;
  let lastTraySentAt = 0;
  $effect(() => {
    if (!isTauriRuntime()) return;
    // minute-bucket tick so long static turns still refresh, without per-second churn
    const minuteBucket = Math.floor(now / 60000);
    void minuteBucket;
    const starts: number[] = [];
    const ids = new Set<string>();
    for (const sid of streamingIds) {
      ids.add(sid);
      if (streamingStartedAt[sid] != null) starts.push(streamingStartedAt[sid]);
    }
    for (const sid of workRunningIds) {
      ids.add(sid);
      if (workStartedAt[sid] != null) starts.push(workStartedAt[sid]);
    }
    const n = ids.size;
    const at = Date.now();
    const oldest = starts.length ? Math.min(...starts) : null;
    const running = n > 0;
    const status = !running
      ? "No running sessions"
      : `${n} running work session${n === 1 ? "" : "s"}`;
    const elapsed = !running ? null : formatLiveElapsed(at - (oldest ?? at));
    // scheduled-messages section: header row + one row per message. Its
    // changes are rare (schedule/send/delete), so they bypass the throttle.
    const schedLines = scheduled.length
      ? [scheduledCountLabel(scheduled.length), ...trayScheduleLines(scheduled, at)]
      : [];
    const schedKey = schedLines.join("\n");
    const key = `${status}\n${elapsed ?? ""}`;
    if (key === lastTrayKey && schedKey === lastTraySchedKey) return;
    if (schedKey === lastTraySchedKey && running && lastTrayRunning && at - lastTraySentAt < 60000) return;
    lastTrayKey = key;
    lastTraySchedKey = schedKey;
    lastTrayRunning = running;
    lastTraySentAt = at;
    // the tray belongs to the main window: server windows would fight it
    if (!isRemoteServer()) void setTrayStatus(status, elapsed, schedLines, scheduled.length);
  });
  /** Quit from the UI. With scheduled messages pending it asks first (they
   *  are only sent while Vicuna is open); `force` is the prompt's own
   *  "Quit anyway". Tray Quit / Cmd+Q reach the same prompt through the
   *  backend's `vicuna-quit-requested` event. */
  async function quitVicuna(force = false) {
    if (!force && scheduled.length > 0) {
      settingsOpen = false;
      pluginsOpen = false;
      quitPromptOpen = true;
      return;
    }
    try {
      await quitApp();
    } catch (e: any) {
      error = e?.message ?? String(e);
    }
  }

  // ---------- scheduled sends (Work mode) ----------
  // Messages wait here (localStorage) and a timer in the running app sends
  // them, so they only go out while Vicuna is open. Rules: $lib/schedule.
  let scheduled = $state<ScheduledItem[]>([]);
  const scheduledSorted = $derived(sortScheduled(scheduled));
  /** split-button menu above the send button */
  let scheduleMenuOpen = $state(false);
  /** sidebar "Scheduled" list */
  let scheduleListOpen = $state(false);
  /** "quit with scheduled messages?" prompt */
  let quitPromptOpen = $state(false);
  /** custom-time dialog; `rescheduleId` set = moving an existing item */
  let scheduleCustom = $state<{ date: string; time: string; rescheduleId: string | null; error: string | null } | null>(null);
  /** clock for the schedule UI (preset times, Today/Tomorrow labels) */
  let scheduleNow = $state(Date.now());
  const scheduleTz = (() => {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || "";
    } catch {
      return "";
    }
  })();
  /** ids being sent right now (a slow workspace check must not double-send) */
  const scheduleSending = new Set<string>();

  /** server mode: id → JSON of each scheduled item the server has */
  const schedSent = new Map<string, string>();
  let schedSync: Promise<void> = Promise.resolve();
  function persistScheduled() {
    if (serverPersist) {
      // the server owns the list (and sends the messages): push the diff
      const { put, remove } = scheduleDiff(schedSent, scheduled);
      for (const it of put) schedSent.set(it.id, JSON.stringify(it));
      for (const id of remove) schedSent.delete(id);
      if (!put.length && !remove.length) return;
      schedSync = schedSync.then(async () => {
        try {
          if (put.length) await invokeOrBridge("schedule_put", { items: put, tzOffsetMin: -new Date().getTimezoneOffset() });
          if (remove.length) await invokeOrBridge("schedule_remove", { ids: remove });
        } catch (e: any) {
          error = `Could not save scheduled messages on the server: ${typeof e === "string" ? e : (e?.message ?? String(e))}`;
        }
      });
      return;
    }
    try {
      localStorage.setItem(scheduleStoreKey(), JSON.stringify(scheduled));
    } catch {
      error = "Could not save scheduled messages (storage is full). They will be lost when Vicuna closes.";
    }
  }
  function removeScheduled(id: string) {
    scheduled = scheduled.filter((x) => x.id !== id);
    persistScheduled();
  }
  function scheduleTarget(it: ScheduledItem): string {
    const live = it.chatId ? chats.find((c) => c.id === it.chatId) : undefined;
    return live ? live.title : `New session in ${workspaceBase(it.workspace)}`;
  }
  function toggleScheduleMenu() {
    scheduleNow = Date.now();
    scheduleMenuOpen = !scheduleMenuOpen;
  }
  function openScheduleCustom(rescheduleId: string | null = null) {
    scheduleMenuOpen = false;
    scheduleNow = Date.now();
    const base = defaultCustomTime(scheduleNow);
    scheduleCustom = { date: dateInputValue(base), time: timeInputValue(base), rescheduleId, error: null };
  }
  async function confirmScheduleCustom() {
    const c = scheduleCustom;
    if (!c) return;
    const at = parseCustomTime(c.date, c.time);
    const err = customTimeError(at, Date.now());
    if (err || at == null) {
      scheduleCustom = { ...c, error: err };
      return;
    }
    scheduleCustom = null;
    if (c.rescheduleId) {
      scheduled = scheduled.map((x) => {
        if (x.id !== c.rescheduleId) return x;
        const { missed: _m, error: _e, ...rest } = x;
        return { ...rest, at };
      });
      persistScheduled();
      flashNotice(`Rescheduled for ${formatWhen(at, Date.now())}.`);
    } else {
      await scheduleSend(at);
    }
  }

  /** Schedule the composer's message for `at` instead of sending it now. */
  async function scheduleSend(at: number) {
    scheduleMenuOpen = false;
    if (blockedWhileLoading()) return;
    const text = input.trim();
    const images = attachImages.length ? [...attachImages] : undefined;
    if (!text && !images) return;
    if (attachFiles.length) {
      error = "Scheduled messages can't carry attached files. Remove them, or send now.";
      return;
    }
    if (customTimeError(at, Date.now())) {
      error = "Pick a time in the future.";
      return;
    }
    if (workspacePickerOpen) await commitWorkspacePicker();
    const chat = chats.find((c) => c.id === activeId);
    // an empty session is discarded on navigation, so only a real thread is
    // a target; otherwise the message starts a new session in the workspace
    const thread = chat && chat.messages.length > 0 ? chat : null;
    const rawWs = thread ? (thread.workspace ?? pendingWorkspace) : pendingWorkspace;
    if (!rawWs?.trim()) {
      workspaceError = "Pick a workspace folder first.";
      error = "Work mode needs a workspace folder. Set one, then schedule.";
      return;
    }
    let ws: string;
    try {
      ws = isNoWorkspacePath(rawWs)
        ? await ensureScratchWorkspace(thread ? thread.id : crypto.randomUUID())
        : await canonicalWorkspace(rawWs);
    } catch (e: any) {
      const msg = typeof e === "string" ? e : (e?.message ?? String(e));
      workspaceError = msg;
      error = `Workspace invalid: ${msg}`;
      return;
    }
    scheduled = [
      ...scheduled,
      {
        id: crypto.randomUUID(),
        chatId: thread ? thread.id : null,
        title: thread ? thread.title : "",
        workspace: ws,
        text,
        ...(images ? { images } : {}),
        at,
        createdAt: Date.now()
      }
    ];
    persistScheduled();
    input = "";
    historyIndex = null;
    historyDraft = "";
    mention = null;
    clearDraft(null);
    attachImages = [];
    error = null;
    flashNotice(`Scheduled for ${formatWhen(at, Date.now())}. ${scheduleNote}`, 7000);
  }

  /** Send a scheduled message: into its session's running turn (queued, like
   *  typing while it runs), as a new turn of that session, or as a new
   *  session when the target is gone. A failure keeps the message in the
   *  list (flagged) so nothing is lost. */
  /** server mode: reload the server's Scheduled list (it delivers them) */
  async function refreshServerSchedule() {
    await schedSync;
    try {
      const list = await invokeOrBridge<ScheduledItem[]>("schedule_list", {});
      if (!Array.isArray(list)) return;
      schedSent.clear();
      for (const it of list) schedSent.set(it.id, JSON.stringify(it));
      if (JSON.stringify(list) !== JSON.stringify(scheduled)) scheduled = list;
    } catch {
      /* server unreachable: keep the list shown */
    }
  }

  async function deliverScheduled(id: string) {
    const item = scheduled.find((x) => x.id === id);
    if (!item || scheduleSending.has(id)) return;
    if (serverPersist) {
      // Send now: due immediately, the server's timer sends it within seconds
      scheduled = scheduled.map((x) => {
        if (x.id !== id) return x;
        const { missed: _m, error: _e, ...rest } = x;
        return { ...rest, at: Date.now() };
      });
      persistScheduled();
      flashNotice("Sending on the server…");
      return;
    }
    scheduleSending.add(id);
    try {
      const find = () => {
        const c = item.chatId ? chats.find((x) => x.id === item.chatId) : undefined;
        return c && (c.mode ?? "chat") === "work" ? c : undefined;
      };
      const msg: ChatMsg = { role: "user", content: item.text, ...(item.images?.length ? { images: [...item.images] } : {}) };
      let chat = find();
      // a compaction or a plain stream holds the session: try again next tick
      if (chat && (isCompacting(chat.id) || isChatStreaming(chat.id))) return;
      let ws = chat?.workspace ?? item.workspace;
      if (!chat || !isWorkRunning(chat.id)) {
        const gate = workSendGate(workEngineInfo);
        if (gate.blocked) throw new Error(gate.reason ?? "The Work engine is not available.");
        ws = await canonicalWorkspace(ws);
        // the await may have outlived the item or the session's idle state
        if (!scheduled.some((x) => x.id === id)) return;
        chat = find();
        if (chat && (isCompacting(chat.id) || isChatStreaming(chat.id))) return;
      }
      if (chat && isWorkRunning(chat.id)) {
        const qid = crypto.randomUUID();
        queuedMsgs = [...queuedMsgs, { chatId: chat.id, id: qid, msg }];
        const turnId = workTurnIds.get(chat.id);
        if (turnId) void queueWorkAgentMsg(turnId, qid, msg).catch(() => {});
        removeScheduled(id);
        flashNotice(`Scheduled message queued in "${chat.title}".`);
        return;
      }
      const titleSrc = item.text || "Image";
      const title = titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : "");
      const reply: ChatMsg = { role: "assistant", content: "", thinking: "", model: selectedModel };
      if (!chat) {
        // complete before it enters reactive state (see send())
        chat = {
          id: crypto.randomUUID(),
          title,
          createdAt: Date.now(),
          updatedAt: Date.now(),
          messages: [msg, reply],
          mode: "work",
          mcpEnabled: await newMcpEnabled(),
          skillsEnabled: await newSessionSkills(),
          workspace: ws
        };
        chats = [chat, ...chats];
      } else {
        chat.updatedAt = Date.now();
        if (!chat.workspace) chat.workspace = ws;
        chat.messages = [...chat.messages, msg, reply];
        chats = [chat, ...chats.filter((x) => x.id !== chat!.id)];
      }
      save();
      removeScheduled(id);
      flashNotice(`Scheduled message sent to "${chat.title}".`);
      if (chat.id === activeId) {
        isPinned = true;
        scrollBottom();
      }
      void runWorkTurn(chat, chat.messages.length - 1, ws);
    } catch (e: any) {
      const msg = typeof e === "string" ? e : (e?.message ?? String(e));
      scheduled = scheduled.map((x) => (x.id === id ? { ...x, missed: true, error: msg } : x));
      persistScheduled();
      error = `Scheduled message not sent: ${msg}`;
    } finally {
      scheduleSending.delete(id);
    }
  }
  /** Send a message to a Work session from outside the composer (a finished
   *  background job, a late ask answer): queued into its live turn, else a
   *  new background turn. Returns false while the session is busy with a
   *  compaction or plain stream (the caller retries) or gone. */
  async function sendToWorkSession(chatId: string, msg: ChatMsg, opts: { now?: boolean } = {}): Promise<boolean> {
    const chat = chats.find((x) => x.id === chatId);
    if (!chat || (chat.mode ?? "chat") !== "work" || !chat.workspace) return false;
    if (isCompacting(chat.id) || isChatStreaming(chat.id)) return false;
    if (isWorkRunning(chat.id)) {
      const qid = crypto.randomUUID();
      queuedMsgs = [...queuedMsgs, { chatId: chat.id, id: qid, msg }];
      const turnId = workTurnIds.get(chat.id);
      if (opts.now) sendQueuedNow(qid);
      else if (turnId) void queueWorkAgentMsg(turnId, qid, msg).catch(() => {});
      return true;
    }
    const gate = workSendGate(workEngineInfo);
    if (gate.blocked && !chat.engine) throw new Error(gate.reason ?? "The Work engine is not available.");
    const ws = await canonicalWorkspace(chat.workspace);
    await ensureChatFull(chat);
    // the await may have outlived the session or its idle state
    if (!chats.some((x) => x.id === chatId) || isWorkRunning(chatId) || isCompacting(chatId) || isChatStreaming(chatId)) return false;
    const reply: ChatMsg = { role: "assistant", content: "", thinking: "", model: selectedModel };
    chat.updatedAt = Date.now();
    chat.messages = [...chat.messages, msg, reply];
    chats = [chat, ...chats.filter((x) => x.id !== chat.id)];
    save();
    if (chat.id === activeId) {
      isPinned = true;
      scrollBottom();
    }
    void runWorkTurn(chat, chat.messages.length - 1, ws);
    return true;
  }

  // ---- lead sessions (vicuna-mcp manage_sessions) ----
  /** workers whose next turn end goes back to their lead */
  const reportIds = new Set<string>();

  function reportToLead(chatId: string, stopped: boolean) {
    if (!reportIds.has(chatId)) return;
    reportIds.delete(chatId);
    const w = chats.find((x) => x.id === chatId);
    const lead = w?.leadId ? chats.find((x) => x.id === w.leadId) : undefined;
    if (!w || !lead) return;
    const msg: ChatMsg = { role: "user", content: workerDoneMessage({ id: w.id, title: w.title, engine: w.engine || engineId(workEngineInfo), model: w.engine ? (w.workModel ?? null) : workModel }, lastReply(chatId) ?? "", stopped) };
    const trySend = (n: number) =>
      void sendToWorkSession(lead.id, msg)
        .then((ok) => {
          if (!ok && n < 30) setTimeout(() => trySend(n + 1), 2000);
        })
        .catch((e: any) => {
          error = `Worker report not sent to the lead: ${typeof e === "string" ? e : (e?.message ?? String(e))}`;
        });
    trySend(0);
  }

  function sessionRow(c: Chat, leadId: string): SessionRow {
    return {
      id: c.id,
      title: c.title,
      status: isWorkRunning(c.id) ? "running" : isCompacting(c.id) ? "compacting" : "idle",
      engine: c.engine || engineId(workEngineInfo),
      model: c.engine ? (c.workModel ?? null) : workModel,
      workspace: c.workspace ?? null,
      role: c.id === leadId ? "lead" : c.leadId === leadId ? "worker" : "other",
      updatedAt: chatActivity(c),
      archived: !!c.archived
    };
  }

  /** a lead asking to delete a session waits on this confirm card */
  let leadDeleteAsk = $state<{ title: string; leadTitle: string; resolve: (yes: boolean) => void } | null>(null);

  /** perform a lead's manage_sessions call; the reply is the tool result */
  async function handleSessionControl(leadId: string, leadWs: string, action: string, args: Record<string, any>): Promise<{ ok: boolean; text?: string; error?: string }> {
    const str = (k: string) => (typeof args[k] === "string" ? (args[k] as string).trim() : "");
    const callerIsLead = chats.find((x) => x.id === leadId)?.role === "lead";
    const report = callerIsLead && args.report !== false;
    const target = () => {
      const id = str("session_id");
      if (!id) throw new Error("`session_id` is required (see `list`).");
      if (id === leadId) throw new Error("That is your own session.");
      const c = chats.find((x) => x.id === id);
      if (!c) throw new Error(`No session ${id}.`);
      return c;
    };
    const engines = workEngineChoices(workEngineInfo).map((e) => e.id);
    switch (action) {
      case "permissions": {
        if (!serverPersist) throw new Error("Session permissions are available only for sessions persisted by the Vicuna server.");
        const c = target();
        if (typeof args.read !== "boolean" && typeof args.allow_message !== "boolean") {
          return { ok: true, text: JSON.stringify({ session_id: c.id, ...sessionPermissions(c) }) };
        }
        const change: { read?: boolean; message?: boolean } = {};
        if (typeof args.read === "boolean") change.read = args.read;
        if (typeof args.allow_message === "boolean") change.message = args.allow_message;
        const updated = await serverAction("session_update", { id: c.id, permissions: change });
        if (!updated) throw new Error("Could not save session permissions.");
        const current: Chat = updated;
        return { ok: true, text: JSON.stringify({ session_id: current.id, ...sessionPermissions(current) }) };
      }
      case "list": {
        const rows = chats.filter((c) => (c.mode ?? "chat") === "work" && c.messages.length > 0).slice(0, 40).map((c) => sessionRow(c, leadId));
        return { ok: true, text: sessionListText(rows, engines, engineId(workEngineInfo)) };
      }
      case "status": {
        const c = target();
        const tail = tailText(lastReply(c.id) ?? "", 3000);
        return { ok: true, text: `${sessionListText([sessionRow(c, leadId)], engines, engineId(workEngineInfo)).split("\n").slice(-1)[0]}\nLast reply (tail):\n${tail || "(none yet)"}` };
      }
      case "create": {
        const engine = str("engine");
        if (engine && engines.length && !engines.includes(engine)) throw new Error(`Engine "${engine}" is not installed. Installed: ${engines.join(", ")}.`);
        const ws = await canonicalWorkspace(str("workspace") || leadWs);
        const message = str("message");
        const title = (str("title") || message || "Worker").slice(0, 40);
        const msgs: ChatMsg[] = message ? [{ role: "user", content: message }, { role: "assistant", content: "", thinking: "", model: selectedModel }] : [];
        // complete before it enters reactive state (see send())
        const c: Chat = {
          id: crypto.randomUUID(),
          title,
          createdAt: Date.now(),
          updatedAt: Date.now(),
          messages: msgs,
          mode: "work",
          mcpEnabled: await newMcpEnabled(),
          skillsEnabled: await newSessionSkills(),
          workspace: ws,
          engine: engine || engineId(workEngineInfo),
          workModel: str("model") || null,
          leadId
        };
        chats = [c, ...chats];
        save();
        if (message) {
          if (report) reportIds.add(c.id);
          void runWorkTurn(c, c.messages.length - 1, ws);
        }
        return { ok: true, text: `Created "${c.title}" (session_id=${c.id}, engine=${c.engine}, model=${c.workModel ?? "default"}, workspace=${ws})${message ? "; its turn has started" + (report ? " and its reply will reach you when it ends" : "") : ""}.` };
      }
      case "send": {
        const c = target();
        const message = str("message");
        if (!message) throw new Error("`message` is required.");
        const running = isWorkRunning(c.id);
        const ok = await sendToWorkSession(c.id, { role: "user", content: message });
        if (!ok) throw new Error(`"${c.title}" is busy (compacting or streaming) or has no workspace; try again shortly.`);
        if (report) reportIds.add(c.id);
        const acknowledgement = running ? `Queued into the running turn of "${c.title}".` : `Started a turn in "${c.title}".`;
        return { ok: true, text: callerIsLead ? acknowledgement : `${acknowledgement} No reply will be sent back to this non-lead caller.` };
      }
      case "stop": {
        const c = target();
        if (!isWorkRunning(c.id)) return { ok: true, text: `"${c.title}" was not running.` };
        stopSession(c.id);
        return { ok: true, text: `Stopped "${c.title}".` };
      }
      case "archive":
      case "unarchive": {
        // archiving only hides the session in the sidebar: a running turn
        // keeps going and its messages/engine session/jobs stay intact
        const c = target();
        const on = action === "archive";
        if (on && c.role === "lead") throw new Error("Refusing to archive a lead session.");
        if (!!c.archived === on) return { ok: true, text: `"${c.title}" was already ${on ? "archived" : "not archived"}.` };
        setArchived(c.id, on);
        return { ok: true, text: on ? `Archived "${c.title}"${isWorkRunning(c.id) ? " (its running turn keeps going)" : ""}. It stays fully intact; unarchive brings it back.` : `Unarchived "${c.title}".` };
      }
      case "set_workspace": {
        const raw = str("workspace");
        if (!raw) throw new Error("`workspace` (an absolute folder) is required.");
        const id = str("session_id") || leadId;
        const c = chats.find((x) => x.id === id);
        if (!c) throw new Error(`No session ${id}.`);
        const own = id === leadId;
        if (!own && isWorkRunning(c.id)) throw new Error(`"${c.title}" is running a turn; stop it or wait, then try again.`);
        const ws = await canonicalWorkspace(raw);
        const prev = c.workspace ?? null;
        if (prev === ws) return { ok: true, text: `"${c.title}" is already in ${ws}.` };
        c.workspace = ws;
        c.mode = "work";
        c.updatedAt = Date.now();
        save();
        return { ok: true, text: `Moved "${c.title}" from ${prev ?? "(none)"} to ${ws}.${own ? " This turn keeps running in the old folder; your next turn starts in the new one (with a fresh engine session, so use read_session if you need earlier context)." : ""}` };
      }
      case "delete": {
        const c = target();
        if (c.role === "lead") throw new Error("Refusing to delete a lead session.");
        const lead = chats.find((x) => x.id === leadId);
        const yes = await new Promise<boolean>((resolve) => {
          leadDeleteAsk?.resolve(false);
          leadDeleteAsk = { title: c.title, leadTitle: lead?.title ?? "The lead", resolve };
        });
        leadDeleteAsk = null;
        if (!yes) return { ok: false, error: `The user declined deleting "${c.title}". Do not retry unless they ask.` };
        if (!chats.some((x) => x.id === c.id)) return { ok: true, text: `"${c.title}" was already gone.` };
        if (isWorkRunning(c.id)) stopSession(c.id);
        reportIds.delete(c.id);
        deleteChat(c.id);
        return { ok: true, text: `Deleted "${c.title}".` };
      }
    }
    throw new Error(`Unknown action ${action}.`);
  }

  const pendingMcpWakes = new Map<string, Array<{ name: string; tools: number | null }>>();
  /** send a question answered after "Answer later" */
  async function sendLaterAsk(id: string) {
    const a = laterAsks.find((x) => x.id === id);
    if (!a) return;
    const m = laterAnswerMessage(a.question, a);
    if (!m) return;
    const msg: ChatMsg = { role: "user", content: m.text, ...(m.images.length ? { images: m.images } : {}) };
    try {
      const sent = await sendToWorkSession(a.chatId, msg);
      if (!sent) {
        error = "The session is busy right now; try again in a moment.";
        return;
      }
      laterAsks = laterAsks.filter((x) => x.id !== id);
    } catch (e: any) {
      error = `Answer not sent: ${typeof e === "string" ? e : (e?.message ?? String(e))}`;
    }
  }

  function scheduleTick() {
    if (!ready) return;
    if (serverPersist) {
      // the server sends scheduled messages and may start turns on its own
      // (follow-ups, worker reports, wake-ups): follow them and its list
      void reattachLiveWorkTurns();
      void refreshServerSchedule();
      return;
    }
    const at = Date.now();
    // keep Today/Tomorrow labels and the tray rows honest across midnight
    if (scheduled.length && Math.floor(at / 60000) !== Math.floor(scheduleNow / 60000)) scheduleNow = at;
    for (const it of dueItems(scheduled, at)) void deliverScheduled(it.id);
  }
  type UsageCacheEntry = { report: WorkUsageReport | null; error: string | null; at: number };
  const USAGE_CACHE_KEY = "vicuna-usage-cache";
  let usageOpen = $state(false);
  let usageEngine = $state("agy");
  let usageNoEngineError = $state<string | null>(null);
  /** Unified per-engine usage snapshot shared by the header badge, hover popover, and modal. */
  let usageStore = $state<Record<string, UsageCacheEntry>>({});
  /** Per-engine in-flight flags so hover, background poll, and modal refreshes deduplicate. */
  let usageFetching = $state<Record<string, boolean>>({});
  /** Ticks every 30s so "Updated Xm ago" labels stay fresh without extra fetches. */
  let usageNow = $state(Date.now());
  const usageInFlight = new Map<string, Promise<void>>();
  let usagePollRunning = false;

  const usageEntry = $derived(usageStore[usageEngine] ?? null);
  const usageReport = $derived(usageEntry?.report ?? null);
  const usageError = $derived(usageNoEngineError ?? usageEntry?.error ?? null);
  const usageUpdatedAt = $derived(usageEntry?.at ?? null);
  const usageLoading = $derived(!!usageFetching[usageEngine]);

  function persistUsageStore() {
    try {
      const toSave: Record<string, { report: WorkUsageReport; at: number }> = {};
      for (const [k, v] of Object.entries(usageStore)) {
        if (v.report) toSave[k] = { report: v.report, at: v.at };
      }
      localStorage.setItem(USAGE_CACHE_KEY, JSON.stringify(toSave));
    } catch {}
  }

  function hydrateUsageStore() {
    try {
      const loaded = parseUsageCache<WorkUsageReport>(localStorage.getItem(USAGE_CACHE_KEY));
      if (Object.keys(loaded).length) usageStore = loaded;
    } catch {}
  }

  function fetchUsageEngine(engine: string, opts?: { force?: boolean; background?: boolean }): Promise<void> {
    if (!engine) return Promise.resolve();
    const force = opts?.force ?? false;
    const background = opts?.background ?? false;
    const existing = usageStore[engine];
    if (!force && existing?.report && !isUsageStale(existing.at, Date.now(), USAGE_POLL_INTERVAL_MS)) {
      return Promise.resolve();
    }
    const inflight = usageInFlight.get(engine);
    if (inflight) return inflight;

    usageFetching = { ...usageFetching, [engine]: true };
    const p = (async () => {
      try {
        const res = await fetchWorkUsage(engine);
        const now = Date.now();
        const prev = usageStore[engine];
        if (!res.error) {
          usageStore = { ...usageStore, [engine]: { report: res, error: null, at: now } };
          persistUsageStore();
        } else if (background && prev?.report) {
          // keep last good snapshot on transient background poll failures
        } else {
          usageStore = {
            ...usageStore,
            [engine]: { report: prev?.report ?? null, error: res.error, at: prev?.report ? prev.at : now },
          };
        }
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        const prev = usageStore[engine];
        if (!(background && prev?.report)) {
          usageStore = {
            ...usageStore,
            [engine]: { report: prev?.report ?? null, error: msg, at: prev?.report ? prev.at : Date.now() },
          };
        }
      } finally {
        usageInFlight.delete(engine);
        const { [engine]: _, ...rest } = usageFetching;
        usageFetching = rest;
        usageNow = Date.now();
      }
    })();
    usageInFlight.set(engine, p);
    return p;
  }

  async function pollUsageBackground() {
    if (usagePollRunning) return;
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
    const order = usagePollOrder(workEngines, engineId(workEngineInfo));
    if (!order.length) return;
    usagePollRunning = true;
    try {
      for (const eng of order) {
        if (typeof document !== "undefined" && document.visibilityState === "hidden") break;
        await fetchUsageEngine(eng, { background: true });
      }
    } finally {
      usagePollRunning = false;
    }
  }

  function primeUsageHover() {
    usageNow = Date.now();
    const engine = usageHoverEngine;
    if (!engine) return;
    void fetchUsageEngine(engine, { background: true });
  }

  function closeUsageModal() {
    usageOpen = false;
  }

  function selectUsageTab(engine: string) {
    usageNoEngineError = null;
    usageEngine = engine;
    usageNow = Date.now();
    void fetchUsageEngine(engine, { background: true });
  }

  async function openUsageModal(engine?: string) {
    usageOpen = true;
    usageNoEngineError = null;
    usageNow = Date.now();
    if (!engine && !workEngineInfo) await refreshWorkEngine();
    if (!usageOpen) return;
    const target = engine ?? usageTabs(workEngines, engineId(workEngineInfo)).initial;
    if (!target) {
      usageNoEngineError = "Usage is available for Claude Code, Codex and Antigravity; none of them is installed.";
      return;
    }
    usageEngine = target;
    void fetchUsageEngine(target, { background: true });
  }

  $effect(() => {
    const key = usagePollOrder(workEngines, engineId(workEngineInfo)).join(",");
    if (!key) return;
    void pollUsageBackground();
  });

  /** Esc closes the top schedule/usage overlay before any other handler sees it */
  function onScheduleEscape(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (scheduleCustom) scheduleCustom = null;
    else if (quitPromptOpen) quitPromptOpen = false;
    else if (usageOpen) closeUsageModal();
    else if (scheduleListOpen) scheduleListOpen = false;
    else if (scheduleMenuOpen) scheduleMenuOpen = false;
    else return;
    e.preventDefault();
    e.stopPropagation();
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
    if (!serverSessionTools) void saveChats(chats);
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
    homeMcpEnabled = undefined;
    homeSkillsEnabled = undefined;
    // navigating never touches live turns: other sessions keep streaming or
    // working in the background (sidebar shimmer shows them)
    // the composer box is global: New never touches the textbox.
    // No placeholder session is created: the sidebar stays unchanged and
    // the session only appears once the first message is sent (send()
    // builds it inline with the first message as its title).
    if (!serverSessionTools && chats.some((c) => c.messages.length === 0)) {
      chats = chats.filter((c) => c.messages.length > 0);
      save();
    }
    // New follows the open session's mode (and a Work session's folder);
    // from home it uses the last-picked mode
    const start = newSessionStart(activeChat, readLastMode(localStorage));
    pendingMode = start.mode;
    activeId = null;
    if (sessionFilter === NO_WORKSPACE_FILTER) {
      pendingWorkspace = NO_WORKSPACE_SENTINEL;
    } else {
      const scopeWs = workspaceOfFolderKey(sessionFilter);
      pendingWorkspace = scopeWs ?? resolveDraftWorkspace(start.workspace || pendingWorkspace || defaultWorkspace);
    }
    isPinned = true;
    editingIndex = null;
    editDraft = "";
    error = null;
    void goto("/");
  }

  function selectChat(id: string) {
    if (id === activeId) return;
    // navigating never aborts live turns (see newChat)
    editingIndex = null;
    editDraft = "";
    copiedKey = null;
    pendingMsgDelete = null;
    dropActiveDraft(id);
    activeId = id;
    isPinned = true;
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
      pendingWorkspace = resolveDraftWorkspace(c.workspace || defaultWorkspace);
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
    // external nav also leaves live turns alone (see newChat)
    editingIndex = null;
    editDraft = "";
    copiedKey = null;
    pendingMsgDelete = null;
    dropActiveDraft(sid);
    if (sid && chats.some((c) => c.id === sid)) {
      activeId = sid;
      isPinned = true;
    } else if (!sid) {
      activeId = null;
    } else {
      void goto("/");
    }
  });

  function deleteChat(id: string) {
    // stop only THIS session's live turn — other sessions keep running
    if (pendingAsks[id]) cancelPendingAsk(id, "(the chat was deleted before answering)");
    // its put-off questions go with it
    laterAsks = laterAsks.filter((a) => a.chatId !== id);
    const turnId = workTurnIds.get(id);
    if (turnId) {
      workStopIds.add(id);
      void stopWorkAgentTurn(turnId).catch(() => {});
      workTurnIds.delete(id);
    }
    workTurnSeqByChat.delete(id);
    aborters.get(id)?.abort();
    aborters.delete(id);
    serverChatFeeds.get(id)?.close();
    // drop any prompts queued for the deleted session (backend-confirmed or
    // not — their turn is gone)
    if (queuedMsgs.some((q) => q.chatId === id)) {
      queuedMsgs = queuedMsgs.filter((q) => q.chatId !== id);
    }
    // messages scheduled into the deleted session go with it
    if (scheduled.some((x) => x.chatId === id)) {
      scheduled = scheduled.filter((x) => x.chatId !== id);
      persistScheduled();
    }
    unmarkStreaming(id);
    unmarkWorkRunning(id);
    unmarkCompacting(id);
    setToolStatus(id, null);
    ctxMenu = null;
    pendingDeleteId = null;
    const remaining = chats.filter((c) => c.id !== id);
    chats = remaining;
    // The server action stops the turn and drops the scheduled sends.
    if (serverPersist) {
      void serverSavesIdle()
        .then(() => invokeOrBridge("session_delete", { id }))
        .catch(async (e: unknown) => {
          if (serverSessionTools) {
            const raw = await invokeOrBridge<unknown>("chats_get", { id }).catch(() => null);
            if (raw && !chats.some((c) => c.id === id)) adoptChatTurn(raw);
          }
          error = e instanceof Error ? e.message : String(e);
        });
    }
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
  let clearContextChatId = $state<string | null>(null);
  let gotoOpen = $state(false);
  let gotoInput = $state("");
  let gotoError = $state<string | null>(null);
  let gotoSel = $state(0);
  let settingsOpen = $state(false);
  let settingsTab = $state<"general" | "work" | "models" | "openrouter" | "chat">("general");
  let pluginsOpen = $state(false);
  const CLI_INSTALL_CMD = "curl -fsSL https://github.com/anslwy/vicuna/releases/latest/download/install-cli.sh | sh";
  let cliCopied = $state(false);
  async function copyCliInstall() {
    try {
      await navigator.clipboard.writeText(CLI_INSTALL_CMD);
      cliCopied = true;
      setTimeout(() => (cliCopied = false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  }
  /** Vicuna CLI launcher status (desktop only, loaded when Settings opens) */
  let cliState = $state<CliStatus | null>(null);
  let cliOp = $state<null | "load" | "install" | "uninstall">(null);
  let cliError = $state<string | null>(null);
  /** last install/uninstall outcome line (e.g. where it landed, PATH note) */
  let cliNote = $state<string | null>(null);
  /** In-app updates (Settings → App updates): current build age vs the
   *  rolling `latest` prerelease; Update downloads the platform installer
   *  to ~/Downloads and opens it. Works in Tauri and plain-browser-with-
   *  bridge (see src/lib/update.ts + bridge). */
  let buildInfo = $state<{ version: string; builtAtUnix: number } | null>(null);
  let workEngineInfo = $state<WorkEngineInfo | null>(null);

  /** Work mode = the user's own agent CLI: opencode (over ACP), Claude
   *  Code (its headless stream-json mode), Codex (codex app-server) or
   *  Antigravity (agy acp),
   *  nothing else. Detection re-runs at startup,
   *  on window focus, on Settings → Refresh and when the model menu opens,
   *  so installing an engine while Vicuna runs is picked up. */
  let workEngineChecked = $state(false);
  let workEnginePicking = $state(false);
  let engineRefreshSeq = 0;
  async function refreshWorkEngine() {
    if (workEnginePicking) return;
    const seq = ++engineRefreshSeq;
    try {
      const prev = workEngineInfo ? engineId(workEngineInfo) : null;
      const next = await fetchWorkEngineInfo();
      if (seq !== engineRefreshSeq) return;
      applyWorkEngine(next, prev);
      void loadWorkAccountSupport();
    } catch {
      if (seq === engineRefreshSeq) workEngineInfo = null;
    } finally {
      if (seq === engineRefreshSeq) workEngineChecked = true;
    }
  }
  /** Adopt fresh detection; a different engine brings its own model list
   *  and saved pick. */
  function applyWorkEngine(next: WorkEngineInfo, prev: string | null) {
    workEngineInfo = next;
    if (prev !== null && prev !== engineId(next)) {
      ocModels = null;
      ocModelsError = null;
      workModel = next.model;
    } else if (next.installed && workModel === null) {
      workModel = next.model;
    }
  }
  /** Engine picker (composer model menu + Settings): lists every harness,
   *  disabling ones that are not installed. */
  async function pickWorkEngine(id: string) {
    if (workEnginePicking || (workEngineInfo && engineId(workEngineInfo) === id && !sessionEngineMissing)) return;
    const targetChat = activeId;
    const prev = workEngineInfo ? engineId(workEngineInfo) : null;
    ++engineRefreshSeq;
    ++syncSeq;
    workEnginePicking = true;
    try {
      const next = await selectWorkEngine(id);
      if (activeId !== targetChat) return;
      if (engineId(next) !== id) throw new Error(`Could not select ${id}.`);
      applyWorkEngine(next, prev);
      sessionEngineMissing = null;
      // The session's persisted pick must land before it can send again.
      await pinActivePick();
      if (modelOpen) void refreshOpencodeModels();
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      workEnginePicking = false;
    }
  }
  // ---- Work account picker ----
  let accountEngines = $state<string[]>([]);
  let accountAddFor = $state<string | null>(null);
  let accountAddName = $state("");
  let accountBusy = $state(false);
  let accountError = $state<string | null>(null);
  let accountRemoveArmed = $state<string | null>(null);
  let accountSignInFor = $state<WorkAccountInfo | null>(null);
  let accountSignInCopied = $state(false);
  async function addWorkAccountRow() {
    const engine = accountAddFor;
    const label = accountAddName.trim();
    if (!engine || !label || accountBusy) return;
    accountBusy = true;
    accountError = null;
    try {
      const acct = await addWorkAccount(engine, label);
      accountAddFor = null;
      accountAddName = "";
      await refreshWorkEngine();
      accountSignInCopied = false;
      accountSignInFor = acct.signedIn ? null : acct;
    } catch (e: any) {
      accountError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      accountBusy = false;
    }
  }
  let accountRenameFor = $state<string | null>(null);
  let accountRenameName = $state("");
  function startWorkAccountRename(acct: WorkAccountInfo) {
    accountAddFor = null;
    accountError = null;
    accountRenameName = acct.label;
    accountRenameFor = accountRenameFor === acct.engine ? null : acct.engine;
  }
  /** Changes only the label: the account's folder and login stay, so no new sign-in. */
  async function renameWorkAccountRow() {
    const account = workEngineList.find((e) => e.id === accountRenameFor)?.account;
    const label = accountRenameName.trim();
    if (!account || !label || accountBusy) return;
    accountBusy = true;
    accountError = null;
    try {
      const acct = await renameWorkAccount(baseEngineId(account.engine), account.id, label);
      accountRenameFor = null;
      if (accountSignInFor?.engine === account.engine) accountSignInFor = acct;
      await refreshWorkEngine();
    } catch (e: any) {
      accountError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      accountBusy = false;
    }
  }
  async function removeWorkAccountRow(account: WorkAccountInfo) {
    const id = account.engine;
    if (accountRemoveArmed !== id) {
      accountRemoveArmed = id;
      setTimeout(() => { if (accountRemoveArmed === id) accountRemoveArmed = null; }, 6000);
      return;
    }
    accountRemoveArmed = null;
    try {
      await removeWorkAccount(baseEngineId(account.engine), account.id);
      if (accountSignInFor?.engine === id) accountSignInFor = null;
      await refreshWorkEngine();
    } catch (e: any) {
      accountError = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }
  /** Keep the sign-in card visible when it opens under an account row in the scrolling engine list. */
  function revealCard(node: HTMLElement) {
    requestAnimationFrame(() => node.scrollIntoView({ block: "nearest", behavior: "smooth" }));
  }

  let accountSignInCopiedTimer: ReturnType<typeof setTimeout> | undefined;
  async function copyWorkAccountSignIn() {
    if (!accountSignInFor) return;
    try {
      await navigator.clipboard.writeText(accountSignInFor.signInCommand);
      accountSignInCopied = true;
      clearTimeout(accountSignInCopiedTimer);
      accountSignInCopiedTimer = setTimeout(() => (accountSignInCopied = false), 2000);
    } catch {
      accountError = "Could not copy: select the command and copy it by hand.";
    }
  }
  async function checkWorkAccountSignIn() {
    const want = accountSignInFor?.engine;
    await refreshWorkEngine();
    const row = workEngineInfo?.engines?.find((e) => e.id === want);
    if (row?.account?.signedIn) {
      accountSignInFor = null;
      accountError = null;
      void pickWorkEngine(row.id);
    } else {
      accountError = "Not signed in yet: finish the sign-in in the terminal, then try again.";
    }
  }
  async function loadWorkAccountSupport() {
    try {
      accountEngines = await workAccountSupport();
    } catch {
      accountEngines = [];
    }
  }
  async function toggleWorkApproval(ask: boolean) {
    try {
      const prev = workEngineInfo ? engineId(workEngineInfo) : null;
      applyWorkEngine(await setWorkApproval(ask), prev);
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }
  /** the open session stored a harness that can't be selected (uninstalled
   *  plugin): Send is blocked until the user picks another one */
  let sessionEngineMissing = $state<string | null>(null);
  /** Write the picker's current harness + model into the open Work session
   *  (server-side `chats.extra`), so the header, picker, footer and the
   *  session's next turn all agree. */
  async function pinActivePick() {
    const c = activeChat;
    if (!c || composerMode !== "work" || !workEngineInfo) return;
    const engine = engineId(workEngineInfo);
    const model = workModel;
    if (c.engine === engine && (c.workModel ?? null) === model) return;
    c.engine = engine;
    c.workModel = model;
    if (serverPersist && c.messages.length > 0) await serverAction("session_update", { id: c.id, engine, workModel: model });
    else save();
  }
  /** Show the open session's own harness + model in the header and picker:
   *  a session that stored one runs on it, so the view follows it. */
  let syncSeq = 0;
  $effect(() => {
    if (workEnginePicking) return;
    const c = activeChat;
    const info = workEngineInfo;
    if (!c || !info || composerMode !== "work") return;
    const pick = sessionPick(c, { engine: engineId(info), model: untrack(() => workModel) });
    if (!pick.own) { sessionEngineMissing = null; return; }
    if (pick.engine !== engineId(info)) {
      const seq = ++syncSeq;
      const prev = engineId(info);
      void untrack(() => selectWorkEngine(pick.engine)).then((next) => {
        if (seq !== syncSeq || activeId !== c.id) return;
        // the backend fell back to another harness: this session's is gone
        if (engineId(next) !== pick.engine) { sessionEngineMissing = pick.engine; return; }
        sessionEngineMissing = null;
        applyWorkEngine(next, prev);
        workModel = pick.model;
        ocModels = null;
      }).catch(() => {
        if (seq === syncSeq && activeId === c.id) sessionEngineMissing = pick.engine;
      });
      return;
    }
    sessionEngineMissing = null;
    if (untrack(() => workModel) !== pick.model) workModel = pick.model;
  });
  const workEngineLabel = $derived(engineLabel(workEngineInfo));
  /** Short engine name for "<name> default" ("muse" for plugin:muse-acp/muse). */
  const workAccountLabel = $derived(workEngineInfo?.engines?.find((e) => e.id === engineId(workEngineInfo))?.account?.label ?? null);
  const workEngineBase = $derived(baseEngineId(engineId(workEngineInfo)));
  const workEngineName = $derived(isPluginEngine(workEngineBase) ? pluginHarnessName(workEngineBase) : workEngineBase);
  const workEngineShort = $derived(workAccountLabel ? `${workEngineName} · ${workAccountLabel}` : workEngineName);
  const workEngines = $derived(workEngineChoices(workEngineInfo));
  const workEngineList = $derived(workEngineMenuItems(workEngineInfo));
  const enginesWithAccounts = $derived(new Set(workEngineList.filter((e) => e.account).map((e) => baseEngineId(e.id))));
  /** quota harness the header meter icon summarizes (null hides the icon) */
  const usageHoverEngine = $derived(usageTabs(workEngines, engineId(workEngineInfo)).initial);
  /** Send gate: no engine → Work send disabled with the install hint. */
  const workGate = $derived(
    workEnginePicking
      ? { blocked: true, reason: "Switching Work engine…" }
      : sessionEngineMissing
      ? { blocked: true, reason: missingHarnessReason(sessionEngineMissing) }
      : workEngineChecked && workEngineInfo === null
      ? { blocked: true, reason: "Could not check for the Work engine (backend unavailable)." }
      : workSendGate(workEngineInfo)
  );
  /** opencode's own model list (`opencode models`) for the Work picker. */
  let ocModels = $state<OpencodeModels | null>(null);
  let modelsLoadingFor = $state<string | null>(null);
  const ocModelsLoading = $derived(modelsLoadingFor === engineId(workEngineInfo));
  let modelFetchSeq = 0;
  let ocModelsError = $state<string | null>(null);
  /** Work model pick, persisted backend-side (null = opencode's default). */
  let workModel = $state<string | null>(null);
  async function refreshOpencodeModels() {
    if (!workEngineInfo?.installed || ocModelsLoading) return;
    const eng = engineId(workEngineInfo);
    const seq = ++modelFetchSeq;
    modelsLoadingFor = eng;
    ocModelsError = null;
    try {
      const list = await listWorkModels(eng);
      if (seq !== modelFetchSeq || engineId(workEngineInfo) !== eng) return;
      ocModels = list;
      // a session with its own pick keeps it; the list's current is the global one
      const own = activeChat?.engine ? activeChat : null;
      workModel = own ? (own.workModel ?? null) : list.current;
    } catch (e: any) {
      if (seq === modelFetchSeq && engineId(workEngineInfo) === eng) {
        ocModelsError = typeof e === "string" ? e : (e?.message ?? String(e));
      }
    } finally {
      if (seq === modelFetchSeq) modelsLoadingFor = null;
    }
  }
  async function pickWorkModel(id: string | null) {
    workModel = id;
    modelOpen = false;
    error = null;
    void pinActivePick();
    await warnIfOllamaUnavailable(id?.startsWith("ollama/") ?? false);
    try {
      await setWorkModel(engineId(workEngineInfo), id);
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }
  let updateRelease = $state<ReleaseInfo | null>(null);
  let updateOp = $state<null | "check" | "download">(null);
  let updateError = $state<string | null>(null);
  let updateNote = $state<string | null>(null);
  let downloadedPath = $state<string | null>(null);

  // --- Signed in-app updates (Tauri updater; same flow as cojudge's
  // src/routes/+page.svelte: auto-check on startup + manual check +
  // download/install + relaunch). The manifest is latest.json on the newest
  // stable GitHub release (tauri.conf.json → plugins.updater.endpoints).
  // Plain-browser/bridge mode has no updater and keeps the download flow above.
  let showUpdatePrompt = $state(false);
  let availableAppUpdate = $state<AppUpdateInfo | null>(null);
  let appUpdateStatus = $state<AppUpdateStatus>("idle");
  let appUpdateError = $state<string | null>(null);
  let appUpdateProgress = $state<{ downloadedBytes: number; totalBytes: number | null } | null>(null);
  let skippedAppUpdateVersion = $state<string | null>(null);
  let autoCheckAppUpdatesEnabled = $state(true);
  let suppressAutoUpdatePromptUntilNextCheck = false;
  /** the plugin's Update handle (not reactive: it owns a native resource) */
  let updateObject: {
    version: string;
    currentVersion?: string;
    downloadAndInstall: (cb?: (event: unknown) => void) => Promise<void>;
  } | null = null;
  const appUpdateLabel = $derived(
    appUpdateStatus === "checking"
      ? "Checking for updates…"
      : appUpdateStatus === "installing"
        ? "Installing update…"
        : appUpdateStatus === "installed"
          ? "Restart to apply update"
          : appUpdateStatus === "available" && availableAppUpdate
            ? `Update to v${availableAppUpdate.version}`
            : "Check for updates"
  );

  function initializeAppUpdatePreference() {
    const stored = loadAppUpdatePreferenceFromStorage();
    skippedAppUpdateVersion = stored.skippedVersion;
    autoCheckAppUpdatesEnabled = stored.autoCheckEnabled;
  }

  function setAutoCheckAppUpdates(enabled: boolean) {
    autoCheckAppUpdatesEnabled = enabled;
    try {
      persistAppUpdatePreference({ skippedVersion: skippedAppUpdateVersion, autoCheckEnabled: enabled });
    } catch {
      /* storage unavailable: keep the in-memory choice */
    }
  }

  function closeUpdatePrompt() {
    showUpdatePrompt = false;
  }

  function skipAvailableAppUpdateVersion() {
    const version = availableAppUpdate?.version?.trim();
    if (version) {
      skippedAppUpdateVersion = version;
      try {
        persistAppUpdatePreference({ skippedVersion: version, autoCheckEnabled: autoCheckAppUpdatesEnabled });
      } catch {
        /* storage unavailable */
      }
    }
    suppressAutoUpdatePromptUntilNextCheck = false;
    closeUpdatePrompt();
  }

  function remindAboutAppUpdateLater() {
    suppressAutoUpdatePromptUntilNextCheck = true;
    closeUpdatePrompt();
  }

  async function triggerAutomaticAppUpdateCheck() {
    if (typeof navigator !== "undefined" && !navigator.onLine) return;
    if (!autoCheckAppUpdatesEnabled) return;
    if (appUpdateStatus === "checking" || appUpdateStatus === "installing" || appUpdateStatus === "installed") return;
    const suppressPrompt = suppressAutoUpdatePromptUntilNextCheck;
    suppressAutoUpdatePromptUntilNextCheck = false;
    await checkForAppUpdates({ source: "automatic", suppressAutoPrompt: suppressPrompt });
  }

  async function resolveAppUpdateReleaseNotes(update: {
    body?: string | null;
  }): Promise<{ notesPreview: string | null; releaseNotesUrl: string }> {
    const fallback = createReleaseNotesPreview(update.body ?? null);
    try {
      return await fetchGithubReleaseNotesMetadata();
    } catch (err) {
      console.warn("Failed to fetch GitHub release notes preview:", err);
      return { notesPreview: fallback, releaseNotesUrl: APP_UPDATE_RELEASES_URL };
    }
  }

  async function checkForAppUpdates(
    options: { source?: "manual" | "automatic"; suppressAutoPrompt?: boolean } = {}
  ) {
    const source = options.source ?? "manual";
    const suppressAutoPrompt = options.suppressAutoPrompt ?? false;
    if (!isTauriRuntime()) {
      void openExternal(APP_UPDATE_RELEASES_URL);
      return;
    }
    if (appUpdateStatus === "checking" || appUpdateStatus === "installing") return;

    const previousAvailable = availableAppUpdate;
    const previousStatus = appUpdateStatus;
    const previousError = appUpdateError;
    const previousUpdateObject = updateObject;

    appUpdateStatus = "checking";
    appUpdateError = null;
    appUpdateProgress = null;
    availableAppUpdate = null;
    updateObject = null;

    try {
      const { check } = await import("@tauri-apps/plugin-updater");
      const update = await check();
      if (update) {
        const rawTarget = (update as unknown as { rawJson?: { target?: unknown } }).rawJson?.target;
        const releaseNotes = await resolveAppUpdateReleaseNotes({
          body: (update as unknown as { body?: string | null }).body ?? null
        });
        availableAppUpdate = {
          version: update.version,
          currentVersion: update.currentVersion || "",
          notes: releaseNotes.notesPreview,
          publishedAt: (update as unknown as { date?: string | null }).date ?? null,
          target: typeof rawTarget === "string" ? rawTarget : "",
          releaseNotesUrl: releaseNotes.releaseNotesUrl
        };
        updateObject = update as unknown as typeof updateObject;
        appUpdateStatus = "available";
        if (source === "manual") {
          showUpdatePrompt = true;
        } else if (!suppressAutoPrompt && update.version !== skippedAppUpdateVersion) {
          showUpdatePrompt = true;
        } else {
          closeUpdatePrompt();
        }
      } else {
        appUpdateStatus = "up_to_date";
        closeUpdatePrompt();
        if (source === "manual") flashNotice("Vicuna is already up to date.");
      }
    } catch (err) {
      console.error("Failed to check for app updates:", err);
      if (source === "automatic") {
        // background checks never surface errors (offline, no release yet…)
        availableAppUpdate = previousAvailable;
        appUpdateStatus = previousStatus;
        appUpdateError = previousError;
        updateObject = previousUpdateObject;
        return;
      }
      availableAppUpdate = null;
      updateObject = null;
      appUpdateStatus = "error";
      appUpdateError = normalizeUpdateErrorMessage(err);
      showUpdatePrompt = false;
    }
  }

  async function installAppUpdate() {
    if (appUpdateStatus === "installing" || appUpdateStatus === "installed") return;
    // The modal can outlive the in-memory updater handle: re-check instead
    // of silently doing nothing.
    if (!updateObject) {
      appUpdateError = null;
      await checkForAppUpdates({ source: "manual" });
      if (!updateObject || appUpdateStatus !== "available") return;
    }
    if (appUpdateStatus !== "available" && appUpdateStatus !== "error") return;
    const handle = updateObject;
    if (!handle) return;
    appUpdateStatus = "installing";
    appUpdateError = null;
    appUpdateProgress = { downloadedBytes: 0, totalBytes: null };
    // Keep the dialog visible so progress is seen; it stays dismissible and
    // the install continues in the background if hidden.
    showUpdatePrompt = true;
    try {
      await handle.downloadAndInstall((event) => {
        const name = (event as { event?: string })?.event;
        const data = (event as { data?: { chunkLength?: unknown; contentLength?: unknown } })?.data ?? {};
        if (name === "Started") {
          const total = typeof data.contentLength === "number" ? data.contentLength : null;
          appUpdateProgress = { downloadedBytes: 0, totalBytes: total };
        } else if (name === "Progress") {
          const chunk = typeof data.chunkLength === "number" ? data.chunkLength : 0;
          const prev = appUpdateProgress ?? { downloadedBytes: 0, totalBytes: null };
          appUpdateProgress = { downloadedBytes: prev.downloadedBytes + chunk, totalBytes: prev.totalBytes };
        }
      });
      appUpdateProgress = null;
      appUpdateStatus = "installed";
    } catch (err) {
      console.error("Failed to install app update:", err);
      appUpdateProgress = null;
      appUpdateStatus = "error";
      appUpdateError = formatAppUpdateInstallError(err);
    }
  }

  async function restartToApplyUpdate() {
    try {
      const { relaunch } = await import("@tauri-apps/plugin-process");
      await relaunch();
    } catch (err) {
      console.error("Failed to restart after installing update:", err);
      appUpdateError = normalizeUpdateErrorMessage(err);
    }
  }

  /** Settings → App updates button: reopen a known update (incl. one that
   *  is installing/installed/failed) instead of starting a conflicting check. */
  async function openAppUpdate() {
    if (
      availableAppUpdate &&
      (appUpdateStatus === "available" ||
        appUpdateStatus === "installing" ||
        appUpdateStatus === "installed" ||
        appUpdateStatus === "error")
    ) {
      showUpdatePrompt = true;
      return;
    }
    await checkForAppUpdates({ source: "manual" });
  }
  /** update errors fade like the main error box (a manual × also exists);
   *  same plain-timer shape as `errorTimer` above */
  let updateErrorTimer: ReturnType<typeof setTimeout> | null = null;
  $effect(() => {
    const msg = updateError;
    if (updateErrorTimer) {
      clearTimeout(updateErrorTimer);
      updateErrorTimer = null;
    }
    if (!msg) return;
    updateErrorTimer = setTimeout(() => {
      updateErrorTimer = null;
      if (updateError === msg) updateError = null;
    }, 12000);
  });
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

  // ── OpenRouter (Settings → OpenRouter) ─────────────────────────────
  // The key lives in prefs; the catalog is fetched at runtime (the public
  // endpoint is CORS-enabled) and cached in localStorage. Chat + Work both
  // run through OpenRouter selections (`openrouter:` ids).
  let openrouterKey = $state(initialPrefs.openrouterApiKey ?? "");
  let openrouterKeyDraft = $state(initialPrefs.openrouterApiKey ?? "");
  let openrouterKeyVisible = $state(false);
  /** Once a key is saved the full form collapses (frees room for the model
   *  list); "Change" reveals it again. Always open when no key is set. */
  let openrouterKeyEditing = $state(false);
  let openrouterKeyBusy = $state(false);
  let openrouterKeyInfo = $state<OpenRouterKeyInfo | null>(null);
  let openrouterKeyError = $state<string | null>(null);
  let openrouterModels = $state<OpenRouterModel[]>([]);
  let openrouterUpdated = $state("");
  let openrouterLoading = $state(false);
  let openrouterError = $state<string | null>(null);
  let openrouterQuery = $state("");
  let openrouterCat = $state<OpenRouterFilter>("added");
  const openrouterKeyDirty = $derived(openrouterKeyDraft.trim() !== openrouterKey);
  /** Curated ids: only these appear in the model picker and under the
   *  Added chip — the live catalog is 400+ entries. Any other chip or a
   *  search query browses the full catalog for discovery. */
  let openrouterAdded = $state<string[]>([...(initialPrefs.openrouterAdded ?? [])]);
  /** Sidebar scope ("all", "chat", or a workspace root). Resolved against the
   *  live folders below, so a stale value can never blank the list. */
  let sessionFilterRaw = $state<string>(initialPrefs.sessionFilter ?? ALL_FILTER);
  const openrouterAddedSet = $derived(new Set(openrouterAdded));
  const openrouterBrowsing = $derived(openrouterCat !== "added" || openrouterQuery.trim() !== "");
  const filteredOpenRouterModels = $derived.by(() => {
    const base =
      openrouterCat === "added" && openrouterQuery.trim() === ""
        ? openrouterModels.filter((m) => openrouterAddedSet.has(m.id))
        : openrouterModels;
    return filterOpenRouterModels(base, openrouterQuery, openrouterCat);
  });

  function toggleOpenRouterAdded(id: string) {
    openrouterAdded = openrouterAddedSet.has(id)
      ? openrouterAdded.filter((x) => x !== id)
      : [...openrouterAdded, id];
    persistPrefs();
  }

  function openOpenRouterTab() {
    settingsTab = "openrouter";
    // Verify a saved key when the tab opens. Success sticks (no network per
    // click); a failed check retries next time, which helps after the account
    // or network is fixed.
    if (openrouterKey && !openrouterKeyInfo && !openrouterKeyBusy) void verifyOpenRouterKey();
  }

  /** Persist the trimmed key and verify it immediately. */
  async function saveOpenRouterKey() {
    const next = openrouterKeyDraft.trim();
    const changed = next !== openrouterKey;
    openrouterKey = next;
    openrouterKeyDraft = next;
    openrouterKeyInfo = null;
    openrouterKeyError = null;
    setOpenRouterApiKey(next);
    if (changed) persistPrefs();
    if (!next) {
      openrouterKeyVisible = false;
      return;
    }
    await verifyOpenRouterKey();
    if (!openrouterKeyError) openrouterKeyEditing = false;
  }

  async function verifyOpenRouterKey() {
    if (openrouterKeyBusy) return;
    openrouterKeyError = null;
    if (!openrouterKey) {
      openrouterKeyError = "Enter an API key first.";
      return;
    }
    openrouterKeyBusy = true;
    try {
      openrouterKeyInfo = await testOpenRouterKey(openrouterKey);
    } catch (e: any) {
      openrouterKeyInfo = null;
      openrouterKeyError = e?.message ?? String(e);
    } finally {
      openrouterKeyBusy = false;
    }
  }

  function clearOpenRouterKey() {
    openrouterKey = "";
    openrouterKeyDraft = "";
    openrouterKeyVisible = false;
    openrouterKeyEditing = false;
    openrouterKeyInfo = null;
    openrouterKeyError = null;
    setOpenRouterApiKey("");
    persistPrefs();
  }

  async function fetchOpenRouterCatalog() {
    if (openrouterLoading) return;
    openrouterLoading = true;
    openrouterError = null;
    try {
      const list = await fetchOpenRouterModels(openrouterKey);
      openrouterModels = list;
      openrouterUpdated = writeOpenRouterCache(list);
    } catch (e: any) {
      openrouterError = e?.message ?? String(e);
    } finally {
      openrouterLoading = false;
    }
  }

  function openRouterUsageText(info: OpenRouterKeyInfo): string {
    const bits: string[] = [];
    if (info.usage !== null) bits.push(`usage ${formatUsd(info.usage)}`);
    if (info.limit !== null && info.limit > 0) {
      bits.push(info.limitRemaining !== null ? `${formatUsd(info.limitRemaining)} of ${formatUsd(info.limit)} left` : `limit ${formatUsd(info.limit)}`);
    }
    return bits.join(" · ");
  }

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

  // ---- remote server (sidebar server menu; src/lib/bridge.ts + src-tauri/src/servers.rs) ----
  // Picking a server reloads the window onto it: every backend call, the
  // chat list and Work turns then live on that machine. Constant per load.
  const onServer = isRemoteServer();
  const activeServer = activeServerId();
  /** `?chat=<id>`: this window was opened on a session from the Cmd+P palette
   *  (a server window gets the session in its URL; read before navigation
   *  rewrites it). */
  const launchChatId: string | null = (() => {
    try {
      const id = new URLSearchParams(window.location.search).get("chat");
      return id && /^[A-Za-z0-9_-]+$/.test(id) ? id : null;
    } catch {
      return null;
    }
  })();
  let srvList = $state<ServerInfo[]>([]);
  let srvMenuOpen = $state(false);
  let srvDialogOpen = $state(false);
  let srvUrlDraft = $state("");
  let srvTokenDraft = $state("");
  let srvNameDraft = $state("");
  let srvNameTouched = $state(false);
  let srvBusy = $state(false);
  let srvErr = $state<string | null>(null);
  /** chats could not be loaded from the active server */
  let srvLoadError = $state<string | null>(null);
  let srvCopied = $state<string | null>(null);
  const srvCurrent = $derived(srvList.find((x) => x.id === activeServer) ?? null);
  const srvLabel = $derived(onServer ? (srvCurrent?.name ?? "Remote server") : "This computer");

  /** this app's version vs. the remote `vicuna serve` it talks to */
  const clientVersion: string = pkg.version;
  let srvVersion = $state<string | null>(null);
  let srvLatest = $state<string | null>(null);
  let srvCanUpgrade = $state(false);
  let srvUpgrading = $state(false);
  let srvVersionChecked = false;
  const srvVersionSkew = $derived(!!srvVersion && srvVersion !== clientVersion);
  const srvUpgradeAvailable = $derived(!!srvLatest && !!srvVersion && cmpVersion(srvLatest, srvVersion) > 0);

  async function loadServerVersion() {
    const info = await invokeOrBridge<{ version?: string; canUpgrade?: boolean }>("server_info", {}).catch(() => null);
    srvVersion = info?.version ?? null;
    srvCanUpgrade = !!info?.canUpgrade;
    if (!srvCanUpgrade) return;
    const chk = await invokeOrBridge<{ latest?: string }>("server_upgrade_check", {}).catch(() => null);
    srvLatest = chk?.latest ?? null;
  }

  $effect(() => {
    if (onServer && uiReady && !srvVersionChecked) {
      srvVersionChecked = true;
      void loadServerVersion();
    }
  });

  /** live progress of a server upgrade (`server_upgrade_status`) */
  let srvUpgradeStage = $state<string | null>(null);
  let srvUpgradePct = $state<number | null>(null);
  let srvUpgradeDetail = $state<string | null>(null);

  async function upgradeServer() {
    if (srvUpgrading || !srvLatest) return;
    const from = srvVersion;
    srvUpgrading = true;
    srvUpgradeStage = "Starting upgrade…";
    srvUpgradePct = null;
    srvUpgradeDetail = null;
    type St = { running?: boolean; failed?: boolean; error?: string; stage?: string | null; step?: number; steps?: number; percent?: number | null; detail?: string | null };
    try {
      await invokeOrBridge("server_upgrade", {});
      // ~3 minutes: progress from the upgrade log, then the restart
      for (let i = 0; i < 180; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        const st = await invokeOrBridge<St>("server_upgrade_status", {}).catch(() => null);
        if (st?.failed) throw new Error(st.error || "upgrade failed");
        if (st?.stage && st.running !== false) {
          srvUpgradeStage = st.steps && st.step ? `${st.stage} (${st.step}/${st.steps})` : st.stage;
          srvUpgradePct = st.percent ?? null;
          srvUpgradeDetail = st.detail ?? null;
          continue;
        }
        // upgrader finished (or the server is down / restarted): wait for the new version
        srvUpgradeStage = "Restarting the server…";
        srvUpgradePct = null;
        srvUpgradeDetail = null;
        const info = await invokeOrBridge<{ version?: string }>("server_info", {}).catch(() => null);
        if (info?.version && info.version !== from) {
          srvVersion = info.version;
          flashNotice(`${srvLabel} is now on v${info.version}.`);
          return;
        }
      }
      error = `${srvLabel} did not come back on a new version. Check ~/vicuna/server/upgrade.log there.`;
    } catch (e) {
      error = `Server upgrade failed: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      srvUpgrading = false;
      srvUpgradeStage = null;
      srvUpgradePct = null;
      srvUpgradeDetail = null;
    }
  }

  /** scheduled sends target chat ids, which belong to one backend: keep a
   *  separate list per server so nothing fires against the wrong machine */
  function scheduleStoreKey(): string {
    return activeServer ? `${SCHEDULE_KEY}:server:${activeServer}` : SCHEDULE_KEY;
  }

  async function refreshServers() {
    try {
      srvList = await listServers();
    } catch {
      srvList = [];
    }
  }

  function toggleServerMenu() {
    srvMenuOpen = !srvMenuOpen;
    srvMenuRenaming = false;
    if (srvMenuOpen) void refreshServers();
  }

  function openServerDialog() {
    srvMenuOpen = false;
    srvMenuRenaming = false;
    srvEditingId = null;
    srvEditDraft = "";
    srvErr = null;
    srvUrlDraft = "";
    srvTokenDraft = "";
    srvNameDraft = "";
    srvNameTouched = false;
    srvDialogOpen = true;
    void refreshServers();
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-url-input]")?.focus());
  }

  function fillRecentServer(sv: ServerInfo) {
    srvUrlDraft = sv.url;
    srvTokenDraft = "";
    srvNameDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
    srvNameTouched = true;
  }

  /** Open the server's own window (or focus it), or focus the main window
   *  for This computer. This window keeps running whatever it shows. */
  async function requestServerSwitch(id: string | null) {
    srvMenuOpen = false;
    if (id === activeServer) return;
    try {
      if (id) await touchServer(id).catch(() => {});
      await switchServer(id, srvList.find((x) => x.id === id)?.name);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function submitServer() {
    if (srvBusy || !srvUrlDraft.trim()) return;
    srvBusy = true;
    srvErr = null;
    try {
      const sv = await connectServer(
        srvUrlDraft.trim(),
        srvTokenDraft.trim(),
        srvNameTouched ? srvNameDraft.trim() : null
      );
      srvDialogOpen = false;
      await refreshServers();
      await switchServer(sv.id, sv.name);
    } catch (e) {
      srvErr = e instanceof Error ? e.message : String(e);
    } finally {
      srvBusy = false;
    }
  }

  let srvEditingId = $state<string | null>(null);
  let srvEditDraft = $state("");
  let srvRenaming = $state(false);
  let srvMenuRenaming = $state(false);

  function startRenameServer(sv: ServerInfo) {
    srvEditingId = sv.id;
    srvEditDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-rename-input]")?.focus());
  }

  function cancelRenameServer() {
    srvEditingId = null;
    srvEditDraft = "";
  }

  async function saveRenameServer() {
    if (srvRenaming || !srvEditingId) return;
    srvRenaming = true;
    srvErr = null;
    try {
      const sv = await renameServer(srvEditingId, srvEditDraft.trim());
      srvEditingId = null;
      srvEditDraft = "";
      if (srvUrlDraft.trim() === sv.url) {
        srvNameDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
      }
      await refreshServers();
    } catch (e) {
      srvErr = e instanceof Error ? e.message : String(e);
    } finally {
      srvRenaming = false;
    }
  }

  function startMenuRenameServer() {
    if (!srvCurrent) return;
    srvEditDraft = srvCurrent.name === defaultServerName(srvCurrent.url) ? "" : srvCurrent.name;
    srvMenuRenaming = true;
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-menu-rename-input]")?.focus());
  }

  async function saveMenuRenameServer() {
    if (srvRenaming || !srvCurrent) return;
    srvRenaming = true;
    try {
      await renameServer(srvCurrent.id, srvEditDraft.trim());
      srvMenuRenaming = false;
      srvMenuOpen = false;
      await refreshServers();
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      srvRenaming = false;
    }
  }

  async function forgetServer(id: string) {
    if (srvEditingId === id) cancelRenameServer();
    await removeServer(id).catch(() => {});
    await refreshServers();
    // removed the server this window shows: back to the main window, close this one
    if (id === activeServer) {
      await switchServer(null).catch(() => {});
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      void getCurrentWindow().close();
    }
  }

  const SERVER_SETUP_CMD = "curl -fsSL https://github.com/anslwy/vicuna/releases/latest/download/install-cli.sh | sh\nvicuna serve --tailscale";
  let srvCmdCopied = $state(false);
  async function copyServerSetup() {
    try {
      await navigator.clipboard.writeText(SERVER_SETUP_CMD);
      srvCmdCopied = true;
      setTimeout(() => (srvCmdCopied = false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  }

  async function copyServerUrl(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      srvCopied = url;
      setTimeout(() => {
        if (srvCopied === url) srvCopied = null;
      }, 1500);
    } catch {
      /* clipboard unavailable */
    }
  }

  async function openSettings() {
    pluginsOpen = false;
    settingsTab = "general";
    settingsOpen = true;
    void refreshCliStatus();
    void refreshBuildInfo();
    void refreshWorkEngine();
    void refreshBrowserEngine();
  }

  function openPlugins() {
    settingsOpen = false;
    pluginsOpen = true;
  }

  /** Browser engine row (Settings → Browser engine): which `lb` runs and
   *  its version, from the backend status. Silent on failure. */
  async function refreshBrowserEngine() {
    browserDaemon = await browserStatus().catch(() => null);
  }

  /** Read this build's version + baked timestamp (silent unless it fails:
   *  the row shows "unavailable" instead). */
  async function refreshBuildInfo() {
    try {
      buildInfo = await invokeOrBridge<{ version: string; builtAtUnix: number }>("app_build_info", {});
    } catch {
      buildInfo = null;
    }
  }

  /** Fetch the rolling `latest` prerelease and decide by date. */
  async function checkForUpdates() {
    if (updateOp) return;
    updateOp = "check";
    updateError = null;
    updateNote = null;
    downloadedPath = null;
    try {
      updateRelease = await invokeOrBridge<ReleaseInfo>("github_latest_release", {});
    } catch (e: any) {
      updateError = typeof e === "string" ? e : (e?.message ?? String(e));
      updateRelease = null;
    } finally {
      updateOp = null;
    }
  }

  /** Download the platform installer to ~/Downloads and open it for the
   *  user to run (unsigned builds can't self-replace, so the last step is
   *  manual). Desktop opens via the opener plugin; plain browsers go
   *  through the loopback bridge's system opener. */
  async function applyUpdate() {
    if (updateOp || !updateRelease) return;
    const asset = pickAssetForPlatform(updateRelease.assets, currentPlatform());
    if (!asset) {
      updateError = `No installer for this platform in ${updateRelease.tag}. See the release page.`;
      return;
    }
    updateOp = "download";
    updateError = null;
    updateNote = null;
    try {
      const path = await invokeOrBridge<string>("github_download_asset", {
        url: asset.url,
        fileName: asset.name
      });
      downloadedPath = path;
      try {
        if (isTauriRuntime()) {
          const { openPath } = await import("@tauri-apps/plugin-opener");
          await openPath(path);
        } else {
          await invokeOrBridge("bridge_open_path", { path });
        }
        updateNote = `Downloaded to ${path} — the installer should be opening now.`;
      } catch {
        updateNote = `Downloaded to ${path} — open it to install.`;
      }
    } catch (e: any) {
      updateError = typeof e === "string" ? e : (e?.message ?? String(e));
    } finally {
      updateOp = null;
    }
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
      } else if (models.length > 0 && !models.includes(selectedModel) && !isOpenRouterSelection(selectedModel)) {
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
      if (!models.includes(selectedModel) && models.length > 0 && !isOpenRouterSelection(selectedModel)) {
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
  let continueMode = $state<ContinueMode>(initialPrefs.continueMode);

  // plugin themes (Plugins modal): the active one is cached whole in
  // localStorage per host (src/lib/pluginTheme.ts) and overrides the built-in palette.
  let plugins = $state<InstalledPlugin[]>([]);
  let pluginTheme = $state<ActivePluginTheme | null>(typeof window !== "undefined" ? readActiveTheme(localStorage, activeServerId()) : null);
  /** The user picked a built-in swatch over the active theme's own accent
   *  (key of that theme; cleared whenever the theme changes). */
  let pluginAccentMutedFor = $state<string | null>(typeof window !== "undefined" ? readAccentMuted(localStorage, activeServerId()) : null);
  let pluginThemeProps: string[] = [];
  const pluginThemeChoices = $derived(allThemes(plugins));
  /** The effective light/dark base (a plugin theme brings its own). */
  const themeBase = $derived<"light" | "dark">(pluginTheme?.base ?? theme);
  const pluginAccent = $derived(themeAccent(pluginTheme));
  const pluginAccentMuted = $derived(!!pluginTheme && pluginAccentMutedFor === pluginTheme.key);
  /** Swatch row selection: "theme" while the theme's own accent is in use. */
  const accentSel = $derived(selectedAccent(pluginTheme, pluginAccentMuted, accent));

  function applyAppearance() {
    const root = document.documentElement;
    for (const k of pluginThemeProps) root.style.removeProperty(k);
    pluginThemeProps = [];
    root.dataset.theme = themeBase;
    const a = ACCENTS[accent] ?? ACCENTS.orange;
    const v = themeBase === "dark" ? a.dark : a.light;
    root.style.setProperty("--accent", v.bg);
    root.style.setProperty("--accent-hover", v.hover);
    root.style.setProperty("--accent-fg", v.fg);
    root.style.setProperty("--msg-font-size", `${fontSize}px`);
    if (pluginTheme) {
      for (const [k, val] of Object.entries(themeProperties(pluginTheme, !pluginAccentMuted))) {
        root.style.setProperty(k, val);
        pluginThemeProps.push(k);
      }
    }
  }

  $effect(() => {
    themeBase;
    accent;
    fontSize;
    pluginTheme;
    pluginAccentMuted;
    applyAppearance();
  });

  function setAccentMuted(key: string | null) {
    pluginAccentMutedFor = key;
    writeAccentMuted(localStorage, key, activeServerId());
  }

  function setPluginTheme(t: ActivePluginTheme | null) {
    if (t?.key !== pluginTheme?.key) setAccentMuted(null);
    pluginTheme = t;
    writeActiveTheme(localStorage, t, activeServerId());
  }

  function usePluginTheme(pluginId: string, themeId: string) {
    const t = pluginThemeChoices.find((c) => c.key === themeKey(pluginId, themeId));
    if (t) setPluginTheme(t);
  }

  /** Fresh list from the server: refresh the cached active theme (upgrade)
   *  or drop it when its plugin was uninstalled. */
  function onPluginsChanged(list: InstalledPlugin[]) {
    plugins = list;
    const next = reconcileActive(pluginTheme, list);
    if (JSON.stringify(next) !== JSON.stringify(pluginTheme)) setPluginTheme(next);
  }

  async function loadPlugins() {
    try {
      onPluginsChanged(await invokeOrBridge<InstalledPlugin[]>("plugin_list", {}));
    } catch {
      // no server (plain browser without a bridge): keep the cached theme
    }
    try {
      backendBase = await backendBaseUrl();
    } catch {
      backendBase = null;
    }
  }

  // ---- plugin side panels (sandboxed iframes + src/lib/pluginBridge.ts) ----
  /** base URL the panel iframes load from (this window's backend) */
  let backendBase = $state<string | null>(null);
  const pluginPanels = $derived(
    plugins.flatMap((p) =>
      (p.panels ?? []).map((pn) => ({
        key: `plugin:${p.id}:${pn.id}`,
        pluginId: p.id,
        pluginName: p.name,
        title: pn.title,
        // the version + install time re-key the iframe so an upgrade reloads it
        rev: `${p.version}:${p.installedAt}`,
        url: panelUrl(backendBase, p.panelBase, pn.entry),
        icon: pn.icon ? panelUrl(backendBase, p.panelBase, pn.icon) : null
      }))
    )
  );
  /** live panel iframes by tab key, to match message sources and push events */
  const panelFrames = new Map<string, { el: HTMLIFrameElement; pluginId: string }>();

  function openSidePanel(key: string) {
    newTabMenu = false;
    const pn = pluginPanels.find((x) => x.key === key);
    if (!pn) return;
    if (!sideTabs.some((t) => t.key === key)) {
      const [, , panelId] = key.split(":");
      sideTabs = [...sideTabs, { key, kind: "plugin", title: pn.title, pluginId: pn.pluginId, panelId }];
    }
    explorerOpen = true;
    activeSideTab = key;
  }

  /** an uninstalled plugin's (or removed panel's) tabs close with it */
  $effect(() => {
    const live = new Set(pluginPanels.map((p) => p.key));
    const stale = sideTabs.filter((t) => t.kind === "plugin" && !live.has(t.key));
    if (stale.length && ready) for (const t of stale) closeSideTab(t.key);
  });

  function panelTheme(): PanelTheme {
    const cs = getComputedStyle(document.documentElement);
    const vars: Record<string, string> = {};
    for (const v of THEME_VARS) {
      const val = cs.getPropertyValue(v).trim();
      if (val) vars[v] = val;
    }
    vars["--font-family"] = getComputedStyle(document.body).fontFamily;
    return { base: themeBase, vars };
  }

  function panelContext(): PanelContext {
    const c = activeChat;
    return {
      sessionId: c?.id ?? null,
      title: c?.title ?? null,
      workspace: (c?.workspace || explorerRoot.trim()) || null,
      mode: c ? (c.mode === "work" ? "work" : "chat") : null
    };
  }

  function panelHost(pluginId: string): PanelHost {
    return {
      theme: panelTheme,
      context: panelContext,
      // the plugin id comes from the frame that asked, never the message
      storage: (cmd, key, value) => invokeOrBridge(cmd, { id: pluginId, key, value })
    };
  }

  function onPanelMessage(e: MessageEvent) {
    for (const f of panelFrames.values()) {
      if (!f.el.contentWindow || e.source !== f.el.contentWindow) continue;
      const req = parsePanelRequest(e.data);
      if (!req) return;
      const target = f.el.contentWindow;
      void handlePanelRequest(req, panelHost(f.pluginId)).then((reply) => target.postMessage(reply, "*"));
      return;
    }
  }

  function postToPanels(event: "theme" | "context", data: unknown) {
    // opaque-origin frames can't be targeted by origin; the window is the target
    for (const f of panelFrames.values()) f.el.contentWindow?.postMessage({ vicuna: 1, event, data }, "*");
  }

  /** Svelte action: track a panel iframe while it is mounted. */
  function panelFrame(el: HTMLIFrameElement, arg: { key: string; pluginId: string }) {
    panelFrames.set(arg.key, { el, pluginId: arg.pluginId });
    return {
      destroy() {
        if (panelFrames.get(arg.key)?.el === el) panelFrames.delete(arg.key);
      }
    };
  }

  $effect(() => {
    themeBase;
    accent;
    fontSize;
    pluginTheme;
    pluginAccentMuted;
    // after applyAppearance has written the variables
    void tick().then(() => postToPanels("theme", panelTheme()));
  });

  $effect(() => {
    const ctx = { id: activeChat?.id, title: activeChat?.title, ws: activeChat?.workspace, root: explorerRoot, mode: activeChat?.mode };
    void ctx;
    void tick().then(() => postToPanels("context", panelContext()));
  });

  function setTheme(t: "light" | "dark") {
    theme = t;
    setPluginTheme(null);
    persistPrefs();
  }

  function setAccent(a: string) {
    if (a === "theme") {
      setAccentMuted(null);
      return;
    }
    if (!ACCENTS[a]) return;
    accent = a;
    // picking a built-in swatch overrides the active theme's own accent
    if (pluginTheme && pluginAccent) setAccentMuted(pluginTheme.key);
    persistPrefs();
  }

  function setFontSize(n: number) {
    fontSize = Math.min(35, Math.max(12, Math.round(n)));
    persistPrefs();
  }

  function setStopWords(text: string) {
    stopWordsText = text;
    stopWords = textToStopWords(text);
    persistPrefs();
  }

  function setContinueMode(m: ContinueMode) {
    continueMode = normalizeContinueMode(m);
    persistPrefs();
  }

  let lastDefaultWorkspace = "";
  let defaultWorkspaceWrite = Promise.resolve();

  function persistPrefs() {
    savePrefs({ theme, accent, fontSize, sidebarOpen, stopWords, ollamaBase, zoom: zoomLevel, defaultWorkspace, thinkLevel, continueMode, openrouterApiKey: openrouterKey, openrouterAdded, sessionFilter: sessionFilterRaw });
    if (!serverSessionTools || lastDefaultWorkspace === defaultWorkspace) return;
    const prev = lastDefaultWorkspace;
    const workspace = defaultWorkspace;
    lastDefaultWorkspace = workspace;
    defaultWorkspaceWrite = defaultWorkspaceWrite.then(async () => {
      try {
        await invokeOrBridge("work_default_workspace", { workspace });
      } catch (e) {
        if (lastDefaultWorkspace === workspace) lastDefaultWorkspace = prev;
        console.error("Could not save the server workspace default", e);
      }
    });
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
    // fresh host list: a pasted link is looked up on every connected server
    void refreshServers();
  }

  function closeGoto() {
    gotoOpen = false;
    gotoInput = "";
    gotoError = null;
    gotoSel = 0;
    if (remoteTimer) {
      clearTimeout(remoteTimer);
      remoteTimer = null;
    }
    remoteKey = "";
    remotePending = null;
    remoteResolve?.();
    remoteResolve = null;
    remoteHits = [];
    remoteSearching = false;
    remoteFailures = [];
    remoteNotices = [];
    localServerHits = [];
    localServerSearching = false;
  }

  // search modal: filter sessions by title, message text, or id substring.
  // A pasted session id/link resolves directly via gotoIdHit AND is looked up
  // on every connected server (searchServerChats), so a link that belongs to
  // another machine still lands in the palette, labelled with its host.
  const searchQuery = $derived(gotoInput.trim());
  const searchQueryLow = $derived(searchQuery.toLowerCase());
  const gotoSessionId = $derived(sessionIdIn(gotoInput));
  const gotoIdHit = $derived.by((): { id: string; chat: Chat | null } | null => {
    if (!gotoSessionId) return null;
    const id = gotoSessionId;
    return { id, chat: chats.find((c) => c.id.toLowerCase() === id) ?? null };
  });
  const searchResults = $derived.by((): Chat[] => {
    const q = searchQueryLow;
    if (!q) return chats.filter((c) => c.messages.length > 0).slice(0, 8);
    return chats.filter((c) => {
      if (c.messages.length === 0) return false;
      if (c.title.toLowerCase().includes(q)) return true;
      if (c.id.toLowerCase().includes(q)) return true;
      return c.messages.some((m) => !m.contextCleared && m.content.toLowerCase().includes(q));
    }).slice(0, 20);
  });
  /** connected hosts whose chats are NOT loaded in this window */
  const remoteTargets = $derived(srvList.filter((s) => s.id !== activeServer));
  const hasPartialChats = $derived(isServerBacked() && chats.some((c) => c.partial));
  type RemoteHit = ServerChatHit & { serverId: string; serverName: string };
  let localServerHits = $state<ServerChatHit[]>([]);
  let localServerSearching = $state(false);
  let remoteHits = $state<RemoteHit[]>([]);
  let remoteSearching = $state(false);
  let remoteFailures = $state<{ serverName: string; message: string }[]>([]);
  let remoteNotices = $state<string[]>([]);
  let remoteKey = "";
  let remoteTimer: ReturnType<typeof setTimeout> | null = null;
  let remotePending: Promise<void> | null = null;
  let remoteResolve: (() => void) | null = null;
  type SearchItem =
    | { kind: "new" }
    | { kind: "chat"; chat: Chat; snippet?: string }
    | { kind: "remote"; hit: RemoteHit };
  const searchItems = $derived.by((): SearchItem[] => {
    const rows: SearchItem[] = [];
    const seen = new Set<string>();
    // a pasted session link surfaces its local session first, even though the
    // raw link text matches nothing
    if (gotoIdHit?.chat) {
      rows.push({ kind: "chat", chat: gotoIdHit.chat });
      seen.add(gotoIdHit.chat.id);
    }
    const merged: Array<{ chat: Chat; snippet?: string }> = searchQuery
      ? mergeLocalServerHits(searchResults, localServerHits, chats, 20)
      : searchResults.map((chat) => ({ chat }));
    for (const m of merged) {
      if (seen.has(m.chat.id)) continue;
      seen.add(m.chat.id);
      rows.push(m.snippet ? { kind: "chat", chat: m.chat, snippet: m.snippet } : { kind: "chat", chat: m.chat });
    }
    for (const h of remoteHits) rows.push({ kind: "remote", hit: h });
    // matches first: the keyboard cursor starts at 0, so a query needs no
    // arrow-key walk (paste a link → Enter)
    return orderPaletteRows(rows, searchQuery);
  });
  const searchShowHosts = $derived(showHostLabels(onServer, remoteHits.length));

  // ask the current server (when some sessions are still `partial` previews)
  // AND any other connected hosts on every query change (debounced), and by
  // session id alone when a link was pasted into the box
  $effect(() => {
    if (!gotoOpen) return;
    const { ids, text } = remoteQueryFor(searchQuery, gotoSessionId);
    const targets = remoteTargets;
    const searchHere = hasPartialChats;
    const key = `${ids.join(",")}|${text}|${targets.map((t) => t.id).join(",")}|${searchHere ? 1 : 0}`;
    if (key === remoteKey) return;
    remoteKey = key;
    if (remoteTimer) {
      clearTimeout(remoteTimer);
      remoteTimer = null;
    }
    // a superseded lookup answers at once so an Enter waiting on it moves on
    remoteResolve?.();
    remoteResolve = null;
    localServerHits = [];
    localServerSearching = false;
    remoteHits = [];
    remoteSearching = false;
    remoteFailures = [];
    remoteNotices = [];
    remotePending = null;
    if ((!targets.length && !searchHere) || (!ids.length && !text)) return;
    remoteSearching = targets.length > 0;
    localServerSearching = searchHere;
    const wait = ids.length ? 0 : 200;
    remotePending = new Promise<void>((resolve) => {
      remoteResolve = resolve;
      remoteTimer = setTimeout(async () => {
        const failures: { serverName: string; message: string }[] = [];
        const notices: string[] = [];
        const localPromise = searchHere
          ? invokeOrBridge<ServerChatHit[]>("server_search_chats", { query: text, ids, limit: 20 })
              .then((res) => (Array.isArray(res) ? res : []))
              .catch(() => [] as ServerChatHit[])
          : Promise.resolve([] as ServerChatHit[]);
        const remotePromise = Promise.all(
          targets.map(async (sv) => {
            try {
              const { hits, degraded } = await searchServerChats(sv.id, text, ids, 6);
              // an older `vicuna serve` has no server_search_chats; the list
              // search above still answers, so say how the rows were found
              if (degraded) notices.push(sv.name);
              return hits.map((h) => ({ ...h, serverId: sv.id, serverName: sv.name }));
            } catch (e: any) {
              // shown under the rows: a silent empty result is indistinguishable
              // from "this session is nowhere"
              failures.push({
                serverName: sv.name,
                message: describeServerSearchError(typeof e === "string" ? e : (e?.message ?? String(e)))
              });
              return [] as RemoteHit[];
            }
          })
        );
        const [localFound, found] = await Promise.all([localPromise, remotePromise]);
        remoteTimer = null;
        remoteResolve = null;
        if (remoteKey !== key) {
          resolve();
          return;
        }
        localServerHits = localFound;
        localServerSearching = false;
        remoteHits = found.flat();
        remoteFailures = failures;
        remoteNotices = notices;
        remoteSearching = false;
        resolve();
      }, wait);
    });
  });

  function openSearchItem(it: SearchItem) {
    if (it.kind === "remote") {
      void openRemoteHit(it.hit);
      return;
    }
    closeGoto();
    if (it.kind === "new") newChat();
    else selectChat(it.chat.id);
  }

  /** A hit on another host: raise that host's window on the session. The
   *  palette's own window keeps whatever it was doing. */
  async function openRemoteHit(hit: RemoteHit) {
    closeGoto();
    try {
      await openChatInWindow(hit.serverId, hit.id);
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }

  async function submitSearch() {
    // a pasted link may still be resolving on the connected servers
    if (remotePending) {
      try {
        await remotePending;
      } catch {
        /* an offline host just contributes no rows */
      }
    }
    if (gotoIdHit && !gotoIdHit.chat && !remoteHits.some((h) => h.id === gotoIdHit.id)) {
      if (remoteFailures.length) {
        gotoError = `No session with that link here — ${remoteFailures.map((f) => f.serverName).join(", ")} couldn't be searched.`;
      } else {
        gotoError = remoteTargets.length
          ? "No session with that link here or on your connected servers."
          : "No chat found for that session.";
      }
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
      void submitSearch();
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
      x: Math.min(e.clientX, window.innerWidth - 280),
      y: Math.max(8, Math.min(e.clientY, window.innerHeight - 360))
    };
    if ((chats.find((x) => x.id === id)?.mode ?? "chat") === "work") {
      void refreshTelegramStatus();
    }
  }

  /** Folder pill inside the session menu ("Continue in Work" row): change
   *  the last-used work folder before forking. Desktop keeps the menu open
   *  (native dialog is OS-modal) so Continue is one more click; plain
   *  browsers have no folder dialog, so an inline path form opens inside
   *  the menu instead — the pick persists to defaultWorkspace. */
  async function pickContinueWorkspace(e?: MouseEvent) {
    e?.stopPropagation();
    e?.preventDefault();
    // the native dialog browses THIS machine; a server's folders need the
    // in-app picker (or, with no backend to list folders, a typed path)
    const hint = continueWorkspace.trim();
    if (!isTauriRuntime() || onServer) {
      if (await canBrowseFolders()) {
        openFolderPicker(hint, "continue");
        return;
      }
      workspaceDraft = hint;
      workspaceError = null;
      workspacePickerOpen = true;
      return;
    }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: hint && !(isWindows && hint === "/") ? hint : undefined
      });
      if (typeof picked !== "string" || !picked) return; // cancelled — menu stays open
      pendingWorkspace = picked;
      workspaceError = await validateWorkspace(picked);
      if (!workspaceError) {
        defaultWorkspace = picked;
        persistPrefs();
      } else {
        error = workspaceError;
      }
    } catch (err: any) {
      error = typeof err === "string" ? err : (err?.message ?? String(err));
    }
  }

  /** Browser equivalent of the session-menu folder dialog: validates the
   *  typed path and makes it the work folder for "Continue in Work". Never
   *  touches the active session's own workspace (the menu may target any
   *  chat in the sidebar). */
  async function commitContinueWorkspace() {
    const picked = workspaceDraft.trim();
    if (!picked) {
      workspacePickerOpen = false;
      return;
    }
    const err = await validateWorkspace(picked);
    if (err) {
      workspaceError = err;
      return;
    }
    workspaceError = null;
    pendingWorkspace = picked;
    defaultWorkspace = picked;
    persistPrefs();
    workspacePickerOpen = false;
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
  /** a session handed to this (already loaded) window by the search palette */
  let pendingOpenChatId: string | null = null;
  function handleDeepLinkUrls(urls: string[] | null) {
    if (!urls?.length) return;
    const id = extractSessionId(urls);
    if (id) {
      // decided only once chats have loaded (flushPendingDeepLinks)
      if (!ready) pendingDeepLinkId = id;
      else void openSessionLink(id);
      return;
    }
    const path = extractOpenPath(urls);
    if (!path) return;
    if (ready) void openWorkspacePath(path);
    else pendingDeepLinkPath = path;
  }

  /** Open a `vicuna://s/<id>` session: this computer's list first, then
   *  every connected server (the session may live on a remote host, whose
   *  window is raised on it). */
  async function openSessionLink(id: string) {
    if (srvList.length === 0) await refreshServers();
    const action = sessionLinkAction(id, {
      ready,
      localIds: chats.map((c) => c.id),
      serverCount: remoteTargets.length
    });
    if (action.kind === "stash") {
      pendingDeepLinkId = id;
      return;
    }
    if (action.kind === "open-local") {
      // navigate and let the route effect activate it (#29)
      isPinned = true;
      void goto(`/s/${id}`);
      return;
    }
    if (action.kind === "search-remote") {
      for (const sv of remoteTargets) {
        try {
          const { hits } = await searchServerChats(sv.id, "", [id], 1);
          if (hits.some((h) => h.id === id)) {
            await openChatInWindow(sv.id, id);
            return;
          }
        } catch {
          // unreachable server: try the next one
        }
      }
    }
    flashNotice("That session isn't on this computer or any connected server.");
  }

  /** Run links stashed before chats loaded (cold start, slow server). */
  function flushPendingDeepLinks() {
    if (pendingDeepLinkId) {
      const id = pendingDeepLinkId;
      pendingDeepLinkId = null;
      void openSessionLink(id);
    }
    if (pendingDeepLinkPath) {
      const p = pendingDeepLinkPath;
      pendingDeepLinkPath = null;
      void openWorkspacePath(p);
    }
  }

  /** Open a folder from the `vicuna` CLI (`vicuna://open?path=…`): validate
   *  it, make it the workspace, and raise a work session rooted there
   *  (reusing an empty one when there is one). Never disturbs other
   *  sessions' streams. */
  async function openWorkspacePath(raw: string) {
    const p = raw.trim();
    if (!p || !isTauriRuntime()) return;
    if (onServer) {
      // `vicuna <path>` names a folder on THIS computer
      flashNotice(`Switch to This computer to open ${p}: Vicuna is connected to ${srvLabel}.`);
      return;
    }
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
    syncScopeToWorkspace(canon);
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
    if (!serverSessionTools && chats.some((c) => c.messages.length === 0)) {
      chats = chats.filter((c) => c.messages.length > 0);
    }
    const chat: Chat = {
      id: crypto.randomUUID(),
      title: workspaceBase(canon).slice(0, 40) || "New session",
      createdAt: Date.now(),
      updatedAt: Date.now(),
      messages: [],
      mode: "work",
      mcpEnabled: serverSessionTools ? undefined : await newMcpEnabled(),
      skillsEnabled: serverSessionTools ? undefined : await newSessionSkills(),
      workspace: canon
    };
    if (serverSessionTools) {
      const created = await serverAction("session_create", {
        id: chat.id, title: chat.title, mode: chat.mode, workspace: chat.workspace
      });
      if (!created) return;
    } else {
      chats = [chat, ...chats];
    }
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

  async function commitRename() {
    if (renamingId == null) return;
    const id = renamingId;
    const text = renameDraft.trim();
    renamingId = null;
    if (!text) return;
    const c = chats.find((x) => x.id === id);
    if (!c) return;
    if ((await serverAction("session_update", { id, title: text })) !== false) return;
    c.title = text;
    chats = [...chats];
    save();
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
    if (!serverSessionTools && c && c.messages.length === 0) {
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
    if (target.role === "user" && chat.messages[i + 1]?.role === "assistant" && !chat.messages[i + 1].contextCleared) {
      count = 2;
    } else if (target.role === "assistant" && i > 0 && chat.messages[i - 1]?.role === "user") {
      from = i - 1;
      count = 2;
    }
    if (editingIndex != null && editingIndex >= from) cancelEdit();
    copiedKey = null;
    void serverAction("message_delete", { id: chat.id, index: i }).then((r) => {
      if (r !== false) return;
      chat.messages = [...chat.messages.slice(0, from), ...chat.messages.slice(from + count)];
      chats = [...chats];
      save();
    });
  }

  function toggleModel() {
    modelOpen = !modelOpen;
    if (modelOpen) modelQuery = "";
    if (modelOpen && composerMode === "work") {
      // Work models come from opencode itself: re-detect, then re-list
      void refreshWorkEngine().then(() => refreshOpencodeModels());
      return;
    }
    if (modelOpen) void refreshRunners();
  }
  /** Work picker rows (opencode's list, saved pick kept visible). */
  const workModelChoices = $derived(opencodeModelChoices(ocModels, workModel, modelQuery));
  // re-detect opencode whenever the window regains focus (installed in a
  // terminal while Vicuna was open → Work unblocks without a restart)
  $effect(() => {
    if (typeof window === "undefined") return;
    const onFocus = () => void refreshWorkEngine();
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  });

  // reasoning effort, next to the model picker: string think levels are
  // per-model (gpt-oss low/med/high, qwen3.8 xhigh, deepseek max…), read
  // from /api/show. The pill only appears for models reporting levels;
  // boolean-only and non-thinking models keep today's think:true default.
  let thinkLevel = $state(initialPrefs.thinkLevel ?? "auto");
  let thinkLevels = $state<string[]>([]);
  let thinkDefault = $state<string | null>(null);
  let thinkCanDisable = $state(false);
  let effortOpen = $state(false);

  async function refreshThinking() {
    const m = selectedModel;
    if (!m) {
      thinkLevels = [];
      thinkDefault = null;
      thinkCanDisable = false;
      return;
    }
    if (isOpenRouterSelection(m)) {
      // OpenRouter exposes reasoning as a model capability, not a level list.
      // Effort strings are only offered when the model advertises support;
      // OpenRouter normalizes/falls back otherwise.
      const om = findOpenRouterModel(openrouterModels, m);
      if (selectedModel !== m) return;
      thinkLevels = om?.reasoning && om.supportedParameters.includes("reasoning_effort") ? ["low", "medium", "high"] : [];
      thinkDefault = null;
      thinkCanDisable = Boolean(om?.reasoning);
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
    if (!ready) return;
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

  /** human-readable model name, Ollama-style everywhere: OpenRouter picks
   *  show the bare provider id (`org/model…`, `openrouter:` prefix and the
   *  registry's `~` rolling-pointer mark stripped) instead of the catalog
   *  marketing name — the provider icon already says which backend it is,
   *  and ids stay precise across vendors. */
  function modelLabel(sel: string): string {
    if (!sel) return "";
    if (isOpenRouterSelection(sel)) return displayOpenRouterId(openRouterModelId(sel));
    const engine = sel.split("/", 1)[0];
    const account = workEngineList.find((e) => e.id === engine && e.account);
    return account ? account.label + sel.slice(engine.length) : sel;
  }

  interface ModelChoice {
    id: string;
    name: string;
    sub: string;
    provider: "ollama" | "openrouter";
  }

  // model menu: installed Ollama tags first (as before), then the added
  // OpenRouter models under `openrouter:` ids. Rows show the bare provider
  // id exactly like Ollama tags (no marketing-name line); search still
  // matches the id and the catalog display name so both "claude" and
  // "anthropic/..." find the same rows. The current selection always stays
  // visible (even if removed) so a persisted pick never strands the pill.
  const filteredModelChoices = $derived.by(() => {
    const q = modelQuery.trim().toLowerCase();
    const out: ModelChoice[] = [];
    for (const m of models) {
      if (!q || m.toLowerCase().includes(q)) out.push({ id: m, name: m, sub: "", provider: "ollama" });
    }
    for (const om of openrouterModels) {
      const sel = openRouterSelection(om.id);
      if (!openrouterAddedSet.has(om.id) && sel !== selectedModel) continue;
      if (q && !om.id.toLowerCase().includes(q) && !om.name.toLowerCase().includes(q)) continue;
      out.push({ id: sel, name: displayOpenRouterId(om.id), sub: "", provider: "openrouter" });
    }
    // loaded models pin to the top (selected first): clicking Load closes
    // the menu, and the model should be right there — not lost in the long
    // list — when it reopens. Stable otherwise, so search order is steady.
    const rank = (c: ModelChoice) =>
      c.id === selectedModel ? 0 : c.provider === "ollama" && runningNames.has(c.id) ? 1 : 2;
    return out
      .map((c, i) => ({ c, i }))
      .sort((a, b) => rank(a.c) - rank(b.c) || a.i - b.i)
      .map((x) => x.c);
  });

  const runningNames = $derived(new Set(runners.map((r) => r.name)));

  // manage tab: current chat's model first, then loaded models, then the rest
  const sortedManageModels = $derived.by(() => {
    const running = new Set(runners.map((r) => r.name));
    const rank = (m: string) => (m === selectedModel ? 0 : running.has(m) ? 1 : 2);
    return models.map((m, i) => ({ m, i })).sort((a, b) => rank(a.m) - rank(b.m) || a.i - b.i).map((x) => x.m);
  });

  // discover tab: curated registry catalog with client-side search + filters
  const BROWSE_CATS = ["all", "chat", "code", "reasoning", "vision", "embedding", "small", "mlx"] as const;
  /** variant tags ending in -mlx are Apple-silicon builds (see refresh-catalog.py) */
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

  // The sidebar is scoped by where a session lives (plain chat vs. a work
  // workspace), so every chip offered here comes from the sessions
  // themselves. Order by last user-message time, not creation: a reply to an
  // old session brings it back to the top (and into Today).
  const orderedChats = $derived([...chats].sort((a, b) => chatActivity(b) - chatActivity(a)));
  // When the user explicitly switches workspace (or enables No workspace) while
  // scoped (not "all"), keep that entry visible in Scope even with 0 sessions.
  const activeScopeWorkspace = $derived.by(() => {
    if (sessionFilterRaw === ALL_FILTER) return null;
    const composerWs = (activeChat && activeChat.messages.length > 0 && activeChat.mode === "work"
      ? (activeChat.workspace ?? pendingWorkspace)
      : pendingWorkspace).trim();
    if (sessionFilterRaw === NO_WORKSPACE_FILTER) {
      return isNoWorkspacePath(composerWs) ? NO_WORKSPACE_SENTINEL : null;
    }
    const filterWs = workspaceOfFolderKey(sessionFilterRaw);
    if (!filterWs) return null;
    return workspaceFilterKey(composerWs) === folderOptionKey(filterWs) ? filterWs : null;
  });
  const SHOW_ARCHIVED_KEY = "vicuna-show-archived";
  let showArchived = $state(typeof localStorage !== "undefined" && localStorage.getItem(SHOW_ARCHIVED_KEY) === "1");
  // archived sessions leave Pinned (the pin flag is kept for unarchive) and
  // are hidden unless "Show archived" is on — the open one always stays
  const listedChats = $derived(visibleChats(orderedChats, showArchived, activeId));
  // Scope offers only folders with a listed session: with archived hidden, a
  // folder whose sessions are all archived drops out (counts match the list)
  const folderRows = $derived(folderOptions(listedChats, activeScopeWorkspace));
  const sessionFilter = $derived(resolveSessionFilter(sessionFilterRaw, folderRows));
  // full path of the chosen scope: a native <select> can only tooltip the
  // control, not each option, so the closed state carries the path instead
  const scopeTitle = $derived(folderRows.find((f) => f.key === sessionFilter)?.title ?? "");
  // pinned sessions sit above the date groups whatever the scope (the lead
  // stays in view); they are not repeated below
  const archivedCount = $derived(chats.filter((c) => c.archived && c.messages.length > 0).length);
  const pinnedChats = $derived(listedChats.filter((c) => isPinnedRow(c)));
  const grouped = $derived(groupByDate(filterChats(listedChats.filter((c) => !isPinnedRow(c)), sessionFilter), Date.now()));

  const ctxChat = $derived(ctxMenu ? chats.find((x) => x.id === ctxMenu!.chatId) : undefined);

  function toggleShowArchived() {
    showArchived = !showArchived;
    try {
      localStorage.setItem(SHOW_ARCHIVED_KEY, showArchived ? "1" : "0");
    } catch {}
  }

  /** Database-changing operations are server actions (`vicuna api list`):
   *  a server-backed window runs the action and adopts the server's copy
   *  (its new revision becomes this window's baseline, so no generic save
   *  re-sends it). Returns `false` when this window has no recording server
   *  (the token-less dev bridge), so callers keep their old local path; null
   *  when the action failed (the error is shown). */
  async function serverAction(action: string, args: Record<string, unknown>): Promise<Chat | null | false> {
    if (!serverPersist) return false;
    await serverSavesIdle();
    try {
      const raw = await invokeOrBridge<any>(action, args);
      if (!raw || typeof raw.id !== "string") return null;
      const fresh = adoptServerChat(raw);
      if (action === "session_update" && (serverChatFeeds.has(fresh.id) || workRunningIds.includes(fresh.id))) {
        const live = chats.find((c) => c.id === fresh.id);
        if (live) fresh.messages = live.messages;
      }
      chats = chats.some((x) => x.id === fresh.id) ? chats.map((x) => (x.id === fresh.id ? fresh : x)) : [fresh, ...chats];
      // hand back the REACTIVE copy: writes through the raw object would be
      // invisible to the UI (#16) — a regenerate placeholder once was
      return chats.find((x) => x.id === fresh.id) ?? null;
    } catch (e: any) {
      error = typeof e === "string" ? e : (e?.message ?? String(e));
      return null;
    }
  }

  /** Archive hides a session from the sidebar; it does not stop its turn or
   *  touch its messages, engine session or background jobs. */
  async function setArchived(id: string, on: boolean) {
    const c = chats.find((x) => x.id === id);
    ctxMenu = null;
    if (!c) return;
    if ((await serverAction("session_update", { id, archived: on })) !== false) return;
    c.archived = on;
    chats = [...chats];
    save();
  }

  async function togglePin(id: string) {
    const c = chats.find((x) => x.id === id);
    ctxMenu = null;
    if (!c) return;
    if ((await serverAction("session_update", { id, pinned: !c.pinned })) !== false) return;
    c.pinned = !c.pinned;
    chats = [...chats];
    save();
  }

  /** the lead may manage other sessions; making one the lead also pins it */
  // Per-session Telegram bridge dialog; the session it was opened for
  let telegramFor = $state<string | null>(null);
  // Per-session Telegram statuses keyed by session ID (for sidebar badges & context menu dots)
  let telegramStatuses = $state<Record<string, TelegramStatusSnapshot>>({});
  function telegramStateFor(id: string | undefined): TelegramConnState {
    return telegramConnState(id ? telegramStatuses[id] : null);
  }
  async function refreshTelegramStatus() {
    try {
      const res = await invokeOrBridge<{ bridges?: Record<string, TelegramStatusSnapshot> }>("telegram_list", {});
      telegramStatuses = res?.bridges ?? {};
    } catch {
      // Fallback for older servers that only expose single-lead telegram_status
      try {
        const lead = chats.find((c) => c.role === "lead");
        if (!lead) {
          telegramStatuses = {};
          return;
        }
        const s = await invokeOrBridge<TelegramStatusSnapshot>("telegram_status", {});
        telegramStatuses = s?.configured ? { [lead.id]: s } : {};
      } catch {
        telegramStatuses = {};
      }
    }
  }
  // Per-session bridge plugin dialog & statuses
  const bridgePlugins = $derived(plugins.filter((p) => !!p.bridge));
  let bridgeModal = $state<{ pluginId: string; sessionId: string } | null>(null);
  let bridgeStatuses = $state<Record<string, Record<string, BridgeStatusSnapshot>>>({});
  async function refreshBridgeStatuses() {
    if (bridgePlugins.length === 0) {
      bridgeStatuses = {};
      return;
    }
    try {
      const res = await invokeOrBridge<{ plugins?: Record<string, Record<string, BridgeStatusSnapshot>> }>("bridge_list", {});
      bridgeStatuses = res?.plugins ?? {};
    } catch {
      bridgeStatuses = {};
    }
  }
  $effect(() => {
    if (!ready) return;
    void refreshTelegramStatus();
    const t = setInterval(refreshTelegramStatus, 15000);
    return () => clearInterval(t);
  });
  $effect(() => {
    if (!ready) return;
    void bridgePlugins.length;
    void refreshBridgeStatuses();
    if (bridgePlugins.length === 0) return;
    const t = setInterval(refreshBridgeStatuses, 15000);
    return () => clearInterval(t);
  });

  /** Make/Replace lead prompt (explainer + optional Telegram setup). */
  let leadPrompt = $state<{ id: string; prevTitle: string | null } | null>(null);
  let permissionsFor = $state<string | null>(null);
  let permissionBusy = $state(false);
  $effect(() => {
    const sessionId = permissionsFor;
    if (!sessionId) return;
    const onEscape = (event: KeyboardEvent) => {
      if (event.key !== "Escape") return;
      event.preventDefault();
      event.stopImmediatePropagation();
      permissionsFor = null;
    };
    window.addEventListener("keydown", onEscape, true);
    void tick().then(() => {
      if (permissionsFor === sessionId) document.querySelector<HTMLInputElement>('[data-testid="permission-read"]')?.focus();
    });
    return () => window.removeEventListener("keydown", onEscape, true);
  });
  async function setSessionPermission(key: "read" | "message", value: boolean) {
    const id = permissionsFor;
    if (!id || permissionBusy) return;
    if (!serverPersist) {
      error = "Permissions require server-backed session persistence.";
      return;
    }
    const c = chats.find((x) => x.id === id);
    if (!c) return;
    permissionBusy = true;
    const r = await serverAction("session_update", { id, permissions: { [key]: value } });
    if (r === false) error = "Permissions could not be enforced because this session is not stored by the Vicuna server.";
    permissionBusy = false;
  }

  async function toggleLead(id: string) {
    let c = chats.find((x) => x.id === id);
    ctxMenu = null;
    if (!c) return;
    const makingLead = c.role !== "lead";
    const prevLeadIds = makingLead ? chats.filter((x) => x.id !== id && x.role === "lead").map((x) => x.id) : [];
    const r = await serverAction("session_update", { id, role: makingLead ? "lead" : null });
    if (r === null) return;
    if (r) {
      c = r;
      if (makingLead && prevLeadIds.length > 0) {
        for (const prevId of prevLeadIds) {
          const raw = await invokeOrBridge<any>("chats_get", { id: prevId }).catch(() => null);
          if (raw && typeof raw.id === "string") {
            const fresh = adoptServerChat(raw);
            if (serverChatFeeds.has(fresh.id) || workRunningIds.includes(fresh.id)) {
              const live = chats.find((x) => x.id === fresh.id);
              if (live) fresh.messages = live.messages;
            }
            chats = chats.map((x) => (x.id === fresh.id ? fresh : x));
          } else {
            const prev = chats.find((x) => x.id === prevId);
            if (prev) prev.role = null;
            chats = [...chats];
          }
        }
      }
    } else {
      if (c.role === "lead") {
        c.role = null;
      } else {
        demoteOtherLeads(chats, c.id);
        c.role = "lead";
        c.pinned = true;
      }
      chats = [...chats];
      save();
    }
    flashNotice(c.role === "lead" ? `"${c.title}" is now the lead: from its next turn it can create, message, stop and remove sessions.` : `"${c.title}" is no longer the lead.`);
  }

  /** Keep the sidebar scope synced with an explicit workspace change,
   *  unless the user has chosen "All". */
  function syncScopeToWorkspace(workspace: string) {
    if (sessionFilter === ALL_FILTER) return;
    const key = workspaceFilterKey(workspace);
    if (key) sessionFilterRaw = key;
  }

  /** Choose the sidebar scope. Picking a workspace also makes it where New
   *  starts: the last-used work folder and the composer's pending workspace
   *  both follow the dropdown, so a new session lands in the scope you are
   *  looking at instead of whichever folder was used last. */
  function setSessionFilter(key: string) {
    sessionFilterRaw = key;
    if (key === NO_WORKSPACE_FILTER) {
      pendingWorkspace = NO_WORKSPACE_SENTINEL;
      pendingMode = "work";
      workspaceError = null;
      const c = activeChat;
      if (c && c.messages.length === 0) {
        c.mode = "work";
        c.workspace = NO_WORKSPACE_SENTINEL;
      }
      persistPrefs();
      return;
    }
    const ws = workspaceOfFolderKey(key);
    if (ws !== null) {
      defaultWorkspace = ws;
      pendingWorkspace = ws;
      pendingMode = "work";
      workspaceError = null;
      const c = activeChat;
      if (c && c.messages.length === 0) {
        c.mode = "work";
        c.workspace = ws;
      }
    }
    persistPrefs();
  }

  const ollamaUnavailableMessage = () => serverChatTurns
    ? "Ollama isn't reachable on the Vicuna server. Start Ollama there to use this model."
    : `Ollama isn't reachable at ${activeOllamaBase()}. Start Ollama to use this model.`;

  async function warnIfOllamaUnavailable(isOllamaModel: boolean): Promise<boolean> {
    if (!isOllamaModel) return false;
    try {
      await serverVersion();
      return false;
    } catch {
      if (shouldWarnOllamaUnavailable(isOllamaModel, false)) error = ollamaUnavailableMessage();
      return true;
    }
  }

  async function pickChatModel(id: string) {
    selectedModel = id;
    modelOpen = false;
    error = null;
    save();
    await warnIfOllamaUnavailable(!isOpenRouterSelection(id));
  }

  async function refreshModels() {
    try {
      const tags = await listModels();
      models = tags.map((t) => t.name);
      if (models.length > 0 && !models.includes(selectedModel) && !isOpenRouterSelection(selectedModel)) selectedModel = models[0];
    } catch {
      // Ollama is optional; the picker can still show catalog models as unavailable.
    }
  }

  function scrollBottom() {
    // Coalesce token/layout/scroll notifications into one frame. Check
    // selection and pin state INSIDE it: a gesture can arrive after scheduling.
    if (scrollFrame) return;
    scrollFrame = requestAnimationFrame(() => {
      scrollFrame = 0;
      if (!messagesEl || !isPinned || draggingScrollbar || hasMessageSelection()) return;
      const bottom = Math.max(0, messagesEl.scrollHeight - messagesEl.clientHeight);
      if (Math.abs(messagesEl.scrollTop - bottom) > 1) messagesEl.scrollTop = bottom;
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
    if (!pendingDeleteId && pendingMsgDelete == null && !gotoOpen && !settingsOpen && !pluginsOpen && !lightboxSrc && !pendingSaveConfirm) return;
    const h = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        pendingDeleteId = null;
        pendingMsgDelete = null;
        closeGoto();
        settingsOpen = false;
        pluginsOpen = false;
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
      if (gotoOpen || settingsOpen || pluginsOpen || pendingDeleteId || pendingMsgDelete != null || modelOpen || lightboxSrc || pendingSaveConfirm) return;
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
        if (settingsOpen || pluginsOpen || pendingDeleteId || pendingMsgDelete != null || pendingSaveConfirm || lightboxSrc || modelOpen) return;
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
      // external links in message prose (agent answers, pasted URLs): open
      // in the system browser / a new tab instead of navigating the app
      // webview itself (which strands the session with no way back). Links
      // win over inline-code copy when code sits inside a link.
      const a = t?.closest?.("a[href]") as HTMLAnchorElement | null;
      if (a) {
        const href = a.getAttribute("href") ?? "";
        if (/^(https?:\/\/|mailto:)/i.test(href)) {
          e.preventDefault();
          void openExternal(href);
          return;
        }
        // internal/app links (/#… routes, vicuna:// deep links): leave to
        // the router and deep-link handlers below
      }
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
    // middle-clicks bypass `click`: route those the same way
    const onAux = (e: MouseEvent) => {
      if (e.button !== 1) return;
      const t = e.target as HTMLElement | null;
      const a = t?.closest?.("a[href]") as HTMLAnchorElement | null;
      const href = a?.getAttribute("href") ?? "";
      if (a && /^(https?:\/\/|mailto:)/i.test(href)) {
        e.preventDefault();
        void openExternal(href);
      }
    };
    el.addEventListener("auxclick", onAux);
    return () => {
      el.removeEventListener("click", onClick);
      el.removeEventListener("auxclick", onAux);
    };
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

  /** Copy xterm's model selection. Prefer the synchronous copy command:
   *  xterm's own `copy` listener fills clipboardData, and WebView2 may deny
   *  the async Clipboard API on the custom scheme. */
  function copyTermSelection(term: XTerm) {
    const text = term.getSelection();
    if (!text) return;
    let ok = false;
    try {
      ok = document.execCommand("copy");
    } catch {
      ok = false;
    }
    if (!ok) void copyText(text);
  }

  /** terminal right-click menu (see the xterm `contextmenu` listener) */
  let termCtx = $state.raw<{ x: number; y: number; term: XTerm; sel: string } | null>(null);

  function termMenu(action: "copy" | "paste" | "selectAll" | "clear") {
    const c = termCtx;
    termCtx = null;
    if (!c) return;
    const { term } = c;
    if (action === "copy") {
      void copyText(c.sel).then((ok) => flashExpNotice(ok ? "Copied" : "Copy failed (clipboard unavailable)", 1800));
    } else if (action === "paste") {
      const clip = navigator.clipboard;
      if (!clip?.readText) {
        flashExpNotice("Paste isn't available here — use ⌘V / Ctrl+Shift+V.");
      } else {
        clip
          .readText()
          .then((text) => {
            if (text) term.paste(text);
          })
          .catch(() => flashExpNotice("Paste isn't available here — use ⌘V / Ctrl+Shift+V."));
      }
    } else if (action === "selectAll") {
      term.selectAll();
    } else {
      term.clear();
    }
    term.focus();
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

  // Regenerating drops everything after the message. Regenerating the last
  // message is lossless; anywhere earlier asks first, and offers a fork so
  // the original thread (and its progress) survives the edit. Save (both
  // boxes) only rewrites that message in history — no confirm, no
  // truncation, no model reply. Assistant edits never regenerate, so their
  // box keeps Save / Fork & Save.
  let pendingSaveConfirm = $state(false);
  let pendingSaveAction = $state<"regen" | "save">("regen");
  function saveOrConfirm() {
    const c = activeChat;
    if (!c || editingIndex == null || activeStreaming) return;
    if (editingIndex >= c.messages.length - 1) {
      void saveEdit();
    } else {
      pendingSaveAction = c.messages[editingIndex]?.role === "user" ? "regen" : "save";
      pendingSaveConfirm = true;
    }
  }
  // History-only Save never regenerates, so it applies directly with no
  // confirm — it rewrites just that message and leaves everything after it.
  function saveOnlyOrConfirm() {
    const c = activeChat;
    if (!c || editingIndex == null || activeStreaming) return;
    void saveEditOnly();
  }
  function cancelSaveConfirm() {
    pendingSaveConfirm = false;
  }
  function confirmSaveHere() {
    pendingSaveConfirm = false;
    if (pendingSaveAction === "save") void saveEditOnly();
    else void saveEdit();
  }
  async function forkAndSave() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || editingIndex == null || busy) return;
    const i = editingIndex;
    const draft = editDraft;
    const imgs = [...editImages];
    if (serverChatTurns && chat.mode !== "work" && chat.messages[i]?.role === "user") {
      const forkId = crypto.randomUUID();
      await startServerChat(chat.id, { operation: "fork_edit", index: i, content: draft, images: imgs, forkId }, forkId, () => {
        cancelEdit();
        if (activeId === chat.id) {
          activeId = forkId;
          pendingMode = "chat";
          void goto(`/s/${forkId}`);
        }
      });
      return;
    }
    if (!(await forkAt(i))) return;
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

  // History-only save: rewrite just the edited message, keep every other
  // message in place — no truncation, no model reply. Then persist.
  async function saveEditOnly() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || editingIndex == null || activeStreaming || activeWorkRunning) return;
    const text = editDraft.trim();
    const images = editImages.length ? [...editImages] : undefined;
    if (!text && !images) return;
    const target = chat.messages[editingIndex];
    if (!target) {
      cancelEdit();
      return;
    }
    const r = await serverAction("message_edit", { id: chat.id, index: editingIndex, content: text, images: images ?? null });
    if (r) {
      cancelEdit();
      isPinned = true;
      scrollBottom();
      return;
    }
    if (r === null) return;
    const updated = [...chat.messages];
    if (target.role === "assistant") {
      updated[editingIndex] = { ...target, content: text, images };
    } else {
      updated[editingIndex] = { role: "user", content: text, ...(images ? { images } : {}) };
      chat.updatedAt = Date.now();
      if (text && (chat.title === "New session" || chat.title === "New Session")) {
        chat.title = text.slice(0, 32) + (text.length > 32 ? ".." : "");
      }
    }
    chat.messages = updated;
    chats = [chat, ...chats.filter((x) => x.id !== chat.id)];
    cancelEdit();
    isPinned = true;
    save();
    scrollBottom();
  }

  async function saveEdit() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || editingIndex == null || activeStreaming || activeWorkRunning) return;
    const trimmed = editDraft.trim();
    const images = editImages.length ? [...editImages] : undefined;
    if (!trimmed && !images) return;
    const target = chat.messages[editingIndex];
    if (!target) {
      cancelEdit();
      return;
    }
    // replies keep the exact text: trailing whitespace is meaningful when
    // continuing ("Yo! How's " must not collapse into "Yo! How's")
    const text = target.role === "assistant" ? editDraft : trimmed;
    if (serverChatTurns && chat.mode !== "work" && target.role === "user") {
      await startServerChat(chat.id, { operation: "edit", index: editingIndex, content: text, images }, chat.id, cancelEdit);
      return;
    }
    const edited = await serverAction("message_edit", { id: chat.id, index: editingIndex, content: text, images: images ?? null, truncate: true });
    if (edited === null) return;
    if (edited) {
      cancelEdit();
      isPinned = true;
      if (target.role === "assistant") {
        scrollBottom();
        return;
      }
      await regenerateAfterEdit(edited);
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
    // re-engaging the thread re-times it like a fresh reply
    chat.updatedAt = Date.now();
    chats = [chat, ...chats.filter((x) => x.id !== chat.id)];
    if (text && (chat.title === "New session" || chat.title === "New Session")) {
      chat.title = text.slice(0, 32) + (text.length > 32 ? ".." : "");
    }
    cancelEdit();
    await regenerateAfterEdit(chat);
  }

  /** After a user-message edit (history already truncated to it): run a
   *  fresh reply — the agent for Work threads, a plain stream for Chat. */
  async function regenerateAfterEdit(chat: Chat) {
    if ((chat.mode ?? "chat") === "work") {
      // work threads re-run the agent, not a plain reply (mirrors regenerate)
      if (!chat.workspace?.trim()) {
        error = "Work mode needs a workspace folder — pick one with the folder pill, then regenerate.";
        return;
      }
      chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
      chats = [...chats];
      await runWorkTurn(chat, chat.messages.length - 1, chat.workspace, [], { freshSession: true });
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
      const target = editingIndex != null ? activeChat?.messages[editingIndex] : undefined;
      if (target?.role === "assistant") saveOnlyOrConfirm();
      else saveOrConfirm();
    }
  }

  async function forkAt(i: number): Promise<boolean> {
    const c = chats.find((x) => x.id === activeId);
    // busy is the open session's: forking a live session is blocked, but a
    // turn running elsewhere never blocks forking this one (and forking must
    // never abort that other turn)
    if (!c || busy) return false;
    const made = await serverAction("session_fork", { id: c.id, at: i });
    if (made === null) return false;
    const fork: Chat = made || {
      id: crypto.randomUUID(),
      title: `${c.title} (fork)`,
      createdAt: Date.now(),
      updatedAt: Date.now(),
      messages: JSON.parse(JSON.stringify(c.messages.slice(0, i + 1))),
      forkedFrom: { id: c.id, title: c.title },
      forkFromIndex: i,
      mode: c.mode ?? "chat",
      mcpEnabled: c.mode === "work" ? await newMcpEnabled() : undefined,
      skillsEnabled: c.mode === "work" ? await newSessionSkills() : undefined,
      workspace: c.workspace ?? null
    };
    if (!made) chats = [fork, ...chats];
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
    if (!made) save();
    void goto(`/s/${fork.id}`);
    return true;
  }

  type ChatTurnSnapshot = {
    id: string;
    turnId: string;
    chat: unknown;
    startedMs: number;
    index: number;
    continuing: boolean;
    imagesStripped: boolean;
  };
  type ChatTurnEvent =
    | { type: "token" | "think" | "replace"; text: string }
    | { type: "done" | "images_stripped" }
    | { type: "error"; message: string };
  type ChatTurnOperation =
    | { operation: "send"; content: string; images?: string[]; titleSource: string }
    | { operation: "continue" | "regenerate"; index: number }
    | { operation: "edit"; index: number; content: string; images?: string[] }
    | { operation: "fork_edit"; index: number; content: string; images: string[]; forkId: string };

  const serverChatFeeds = new Map<string, { stop(): void; close(): void; reconcile(turnId: string | undefined): void }>();
  let startingChatFromHome: string | null = null;

  function adoptChatTurn(raw: unknown) {
    const fresh = adoptServerChat(raw);
    chats = chats.some((c) => c.id === fresh.id)
      ? chats.map((c) => c.id === fresh.id ? fresh : c)
      : [fresh, ...chats];
  }

  async function startServerChat(
    source: string,
    operation: ChatTurnOperation,
    id = source,
    onStarted?: () => void,
    options: { model: string; think?: boolean | string; continueMode: ContinueMode; stop: string[] } = {
      model: selectedModel, think: effectiveThink(), continueMode, stop: stopWords
    }
  ) {
    if (serverChatFeeds.has(id)) return;
    error = null;
    const chat = chats.find((c) => c.id === source);
    const baseRev = chat ? serverSaveRow(chat)?.baseRev : undefined;
    await followServerChat(id, async (onEvent) => {
      await serverSavesIdle();
      return await invokeOrBridge<ChatTurnSnapshot>("chat_turn_start", {
        id: source,
        baseRev: operation.operation === "send" && chat ? serverSaveRow(chat)?.baseRev : baseRev,
        ...operation,
        ...options,
        ...(isOpenRouterSelection(options.model) ? { openRouterApiKey: openrouterKey } : {}),
        onEvent
      });
    }, onStarted ?? (() => {}), !isOpenRouterSelection(options.model));
  }

  async function followServerChat(
    id: string,
    subscribe: (onEvent: ReturnType<BridgeChannel["marker"]>) => Promise<ChatTurnSnapshot | null>,
    onStarted?: () => void,
    isOllama = false
  ) {
    if (serverChatFeeds.has(id)) return;
    const channel = new BridgeChannel();
    let snapshot: ChatTurnSnapshot | null = null;
    let stopped = false;
    let closed = false;
    let pendingText = "";
    let pendingThinking = "";
    let replacement: string | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let earlyEvents: ChatTurnEvent[] = [];
    let finish!: (event: ChatTurnEvent) => void;
    const settled = new Promise<ChatTurnEvent>((resolve) => { finish = resolve; });

    const stop = () => {
      stopped = true;
      if (snapshot) {
        void invokeOrBridge("chat_turn_stop", { id, turnId: snapshot.turnId }).catch((e: Error) => {
          if (activeId === id) error = e.message;
        });
      }
    };
    const close = () => {
      closed = true;
      channel.stop();
      clearTimeout(timer);
      finish({ type: "done" });
    };
    let missing = false;
    function reconcile(turnId: string | undefined) {
      if (!snapshot || closed) return;
      if (turnId === snapshot.turnId) {
        missing = false;
      } else if (missing) {
        // The window can sleep longer than the server keeps terminal events.
        // Give a normal Done/Error time to arrive before fetching the transcript.
        receive({ type: "done" });
      } else {
        missing = true;
      }
    }
    serverChatFeeds.set(id, { stop, close, reconcile });

    function flush() {
      clearTimeout(timer);
      timer = undefined;
      if (closed || !snapshot || (!pendingText && !pendingThinking && replacement === undefined)) return;
      const { index } = snapshot;
      const c = chats.find((chat) => chat.id === id);
      const msg = c?.messages[index];
      if (!c || !msg) return;
      const isOpen = activeId === id;
      const keep = isOpen ? captureStreamSel(index) : null;
      withPreservedScroll(id, () => {
        c.messages[index] = {
          ...msg,
          content: (replacement ?? msg.content) + pendingText,
          thinking: (msg.thinking ?? "") + pendingThinking
        };
      });
      pendingText = "";
      pendingThinking = "";
      replacement = undefined;
      if (keep) void tick().then(() => restoreStreamSel(index, keep));
    }
    function receive(event: ChatTurnEvent) {
      if (event.type === "done" || event.type === "error") {
        flush();
        channel.stop();
        finish(event);
        return;
      }
      if (event.type === "images_stripped") {
        if (activeId === id) noteImagesStripped();
        return;
      }
      if (event.type === "replace") {
        replacement = event.text;
        pendingText = "";
      } else if (event.type === "think") {
        pendingThinking += event.text;
      } else if (event.type === "token") {
        pendingText += event.text;
      }
      timer ??= setTimeout(flush, 120);
    }
    channel.onmessage = (event: ChatTurnEvent) => {
      if (snapshot) receive(event);
      else earlyEvents.push(event);
    };
    let started = false;
    try {
      snapshot = await subscribe(channel.marker());
      if (closed || !snapshot) return;
      started = true;
      adoptChatTurn(snapshot.chat);
      markStreaming(id);
      streamingStartedAt = { ...streamingStartedAt, [id]: snapshot.startedMs };
      if (snapshot.continuing) continuingIds = [...continuingIds, id];
      onStarted?.();
      if (activeId === id) {
        if (onStarted) isPinned = true;
        if (snapshot.imagesStripped) noteImagesStripped();
        scrollBottom();
      }
      if (stopped) stop();
      for (const event of earlyEvents) receive(event);
      earlyEvents = [];
      const end = await settled;
      if (closed) return;
      await reloadServerChatTurn(id);
      if (end.type === "error") throw new Error(end.message);
    } catch (e) {
      if (closed) return;
      if (started) chatErrorIds.add(id);
      const message = e instanceof Error ? e.message : String(e);
      if (message.includes("SESSION_CHANGED")) await reloadServerChatTurn(id);
      if (activeId === id || !started) {
        if (!started || !await warnIfOllamaUnavailable(isOllama)) error = message;
      }
    } finally {
      close();
      if (serverChatFeeds.get(id)?.close === close) serverChatFeeds.delete(id);
      continuingIds = continuingIds.filter((x) => x !== id);
      if (started) unmarkStreaming(id);
    }
  }

  async function reloadServerChatTurn(id: string) {
    const raw = await invokeOrBridge<unknown>("chats_get", { id }).catch(() => null);
    if (raw && chats.some((c) => c.id === id) && !unloading) adoptChatTurn(raw);
  }

  async function reattachServerChatTurns() {
    const live = await invokeOrBridge<Record<string, string>>("chat_turn_live", {}).catch(() => null);
    if (!live) return;
    for (const [id, feed] of serverChatFeeds) feed.reconcile(live[id]);
    for (const id of Object.keys(live)) {
      void followServerChat(id, async (onEvent) => {
        const snapshot = await invokeOrBridge<ChatTurnSnapshot | null>("chat_turn_attach", { id, onEvent });
        if (!snapshot) await reloadServerChatTurn(id);
        return snapshot;
      });
    }
  }

  async function streamInto(chat: Chat, index: number, history: ChatMsg[], removeIfEmpty: boolean, opts: { think?: boolean | string; temperature?: number; generatePrompt?: string; rawStyle?: PromptStyle; stop?: string[]; mode?: "fresh" | "continue"; tools?: import("$lib/ollama").OllamaToolDef[]; onImagesStripped?: () => void; pinFollow?: boolean } = {}): Promise<{ text: string; toolCalls: import("$lib/ollama").OllamaToolCall[] }> {
    markStreaming(chat.id);
    const ctrl = new AbortController();
    aborters.set(chat.id, ctrl);
    let produced = false;
    let generatedText = "";
    // follow the new output — unless this is a mid-turn work round or the
    // wrap-up generation: re-pinning there would yank a user who scrolled
    // up to read earlier tool output, on every single round. A background
    // session's stream never touches the open view's pin/scroll either.
    if ((opts.pinFollow ?? true) && chat.id === activeId) {
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
      stop: opts.stop,
      rawStyle: opts.rawStyle
    });
    save();
    if (chat.id === activeId) scrollBottom();
    // raw Continue can overrun the turn on some builds (see
    // isRunawayRepetition): remember the partial so a cut keeps only the
    // clean continuation
    const startContent = chats.find((x) => x.id === chat.id)?.messages[index]?.content ?? "";
    let runawayCut = false;
    // the continuation continues the existing text: drop a duplicated
    // boundary space so "How's " + " it" doesn't render as a double space
    let firstTok = true;
    const wallStart = Date.now();
    let runEvalCount = 0;
    let runEvalDurationNs = 0;
    let runCost: number | null = null;
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
      // selection + scroll belong to the open view: a background session's
      // tokens update state but never touch them
      const isOpen = chat.id === activeId;
      const keep = isOpen ? captureStreamSel(index) : null;
      withPreservedScroll(chat.id, () => {
        const c = chats.find((x) => x.id === chat.id);
        if (c && c.messages[index]) {
          const m = c.messages[index];
          c.messages[index] = { role: "assistant", content: m.content + tok, thinking: (m.thinking ?? "") + th, thinkingOpen: m.thinkingOpen, model: m.model ?? selectedModel, genMs: m.genMs, genTps: m.genTps, genCost: m.genCost };
          chats = [...chats];
        }
      });
      if (keep) void tick().then(() => restoreStreamSel(index, keep));
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
      if (toolStatuses[chat.id] === "Preparing tool call…") setToolStatus(chat.id, null);
    }
    function armStall() {
      clearStall();
      if (!opts.tools) return;
      stallTimer = setTimeout(() => {
        stallTimer = null;
        if (!pendingTok && !pendingTh && !toolStatuses[chat.id]) {
          setToolStatus(chat.id, "Preparing tool call…");
        }
      }, 1000);
    }
    try {
      const onTok = (tok: string) => {
        produced = true;
        if (firstTok) {
          firstTok = false;
          if (/[ \t]$/.test(startContent)) tok = tok.replace(/^[ \t]+/, "");
        }
        generatedText += tok;
        pendingTok += tok;
        scheduleFlush();
        dropStallHint();
        armStall();
        // raw Continue can overrun a finished turn into a short phrase
        // repeated forever (verified on MLX gemma4 builds); cut the stream
        // instead of persisting kilobytes of "owns owns owns"
        if (opts.generatePrompt !== undefined && !runawayCut && isRunawayRepetition(generatedText)) {
          runawayCut = true;
          ctrl.abort();
        }
      };
      const onTh = (tok: string) => {
        produced = true;
        pendingTh += tok;
        scheduleFlush();
        dropStallHint();
        armStall();
      };
      if (opts.generatePrompt !== undefined) {
        const res = await generateStream(selectedModel, opts.generatePrompt, onTok, ctrl.signal, undefined, 0, { temperature: opts.temperature, stop: opts.stop });
        runEvalCount = res.evalCount;
        runEvalDurationNs = res.evalDurationNs;
      } else {
        const res = await chatStream(selectedModel, history, onTok, ctrl.signal, onTh, 0, { think, temperature: opts.temperature, stop: opts.stop, tools: opts.tools, onImagesStripped: opts.onImagesStripped ?? noteImagesStripped });
        runEvalCount = res.evalCount;
        runEvalDurationNs = res.evalDurationNs;
        runCost = res.cost ?? null;
        runToolCalls = res.toolCalls ?? [];
      }
    } catch (err: any) {
      if (err?.name === "AbortError") {
        // keep partial
      } else {
        if (!await warnIfOllamaUnavailable(!isOpenRouterSelection(selectedModel))) {
          error = err?.message ?? String(err);
        }
        chatErrorIds.add(chat.id);
        const c = chats.find((x) => x.id === chat.id);
        if (c && removeIfEmpty && !produced) {
          c.messages = c.messages.filter((_, j) => j !== index);
          chats = [...chats];
        }
      }
    } finally {
      unmarkStreaming(chat.id);
      if (aborters.get(chat.id) === ctrl) aborters.delete(chat.id);
      clearStall();
      dropStallHint();
      // drop any scheduled flush and push the tail synchronously: the final
      // content must be in state before stats/resume logic runs below
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      flushTokens();
      // a cut raw continuation keeps only the text before the repeated run
      if (runawayCut) {
        generatedText = stripRunawayTail(generatedText);
        const cc = chats.find((x) => x.id === chat.id);
        const cm = cc?.messages[index];
        if (cc && cm && cm.role === "assistant") {
          cc.messages[index] = { ...cm, content: startContent + generatedText };
          chats = [...chats];
        }
      }
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
        const prevCost = m.genCost ?? 0;
        const addCost = runCost ?? 0;
        const nextCost = prevCost + addCost > 0 ? prevCost + addCost : undefined;
        c.messages[index] = {
          ...m,
          genMs: (m.genMs ?? 0) + wallMs,
          genTps: tps,
          ...(nextCost !== undefined ? { genCost: nextCost } : {})
        };
        chats = [...chats];
      }
      // user stop / natural settle clears the marker; a refresh/close
      // abort keeps it so the next load resumes the turn. Only this
      // session's own marker is cleared — never a concurrent turn's.
      if (!unloading) clearPendingStreamFor(chat.id);
      save();
      if (chat.id === activeId) scrollBottom();
      return { text: generatedText, toolCalls: runToolCalls };
    }
  }

  async function continueFrom(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || busy || serverChatFeeds.has(chat.id)) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    // Continue generates with the selected model: cloud models can't append
    // (raw /api/generate returns empty, native starts a new turn), so refuse
    // both routes here even if this is called outside the disabled button.
    // OpenRouter selections have no /api/generate path either — chat and Work
    // are the supported transports there.
    if (isCloudModel(selectedModel) || isOpenRouterSelection(selectedModel)) return;
    if (serverChatTurns && chat.mode !== "work") {
      await startServerChat(chat.id, { operation: "continue", index: i });
      return;
    }
    error = null;
    const history: ChatMsg[] = compactedHistory(chat.messages.slice(0, i + 1));
    // Two ways to extend a reply. Native /api/chat resends the partial as
    // the last assistant message — works for models that append, but gemma's
    // chat template closes the assistant turn, so the model starts a fresh
    // greeting instead. Raw /api/generate prefills the family's own prompt
    // format, ending inside the model turn, so the model carries on exactly
    // where the text stopped. Raw needs a known format and can't carry
    // images; everything else falls back to native.
    let style: PromptStyle | null = null;
    if (continueMode !== "native" && !history.some((m) => m.images?.length)) {
      style = promptStyleForFamily(await modelFamily(selectedModel));
      if (!useRawContinue(continueMode, style)) style = null;
    }
    const raw = style ? buildRawContinuePrompt(history, style) : null;
    if (!continuingIds.includes(chat.id)) continuingIds = [...continuingIds, chat.id];
    try {
      if (raw && style) {
        await streamInto(chat, i, history, false, {
          generatePrompt: raw.prompt,
          rawStyle: style,
          think: false,
          temperature: 0.2,
          stop: [...stopWords, ...raw.stop],
          mode: "continue"
        });
      } else {
        // Native reflection: no re-thinking and low temperature so it appends
        // instead of planning a new turn. Custom stop words end generation if
        // it tries to start one ("User:" etc.).
        await streamInto(chat, i, history, false, { think: false, temperature: 0.2, stop: stopWords, mode: "continue" });
      }
    } finally {
      continuingIds = continuingIds.filter((x) => x !== chat.id);
    }
  }

  async function regenerate(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || busy || serverChatFeeds.has(chat.id)) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    error = null;
    cancelEdit();
    if (serverChatTurns && chat.mode !== "work") {
      await startServerChat(chat.id, { operation: "regenerate", index: i });
      return;
    }
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
      await runWorkTurn(chat, chat.messages.length - 1, chat.workspace, [], { freshSession: true });
      return;
    }
    await streamInto(chat, chat.messages.length - 1, freshHistory, true);
  }

  /** /compact: append a flagged summary of the model-visible transcript.
   *  The UI keeps every message; only future model context is trimmed. */
  async function compactSession() {
    const chat = chats.find((c) => c.id === activeId);
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
    if (serverSessionTools) {
      setToolStatus(chat.id, "Compacting context…");
      try {
        await runServerSessionTool("session_compact", chat, {
          model: selectedModel,
          ...(isOpenRouterSelection(selectedModel) ? { openRouterApiKey: openrouterKey } : {})
        });
      } finally {
        setToolStatus(chat.id, null);
      }
      return;
    }
    // model-visible transcript (drops anything already compacted): the UI
    // keeps every message, only the model's context is trimmed
    const visible = compactedHistory(chat.messages);
    if (!visible.length) {
      flashNotice("There are no messages in the current context.");
      return;
    }
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
    markCompacting(chat.id);
    error = null;
    setToolStatus(chat.id, "Compacting context…");
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
      unmarkCompacting(chat.id);
      setToolStatus(chat.id, null);
    }
    if (!summary.trim()) return;
    const c = chats.find((x) => x.id === chat.id);
    // messages moved under us (edit/delete raced the summary): bail instead
    // of flagging a shifted transcript
    if (!c || c.messages.length !== startLen) {
      flashNotice("Session changed during compaction — try again.");
      return;
    }
    // Keep older summaries marked so handoff detection can skip them.
    c.messages = [
      ...c.messages,
      { role: "assistant", content: summary.trim(), model: selectedModel, summary: true }
    ];
    chats = [...chats];
    save();
    scrollBottom();
  }

  async function clearContextSession() {
    const chat = chats.find((c) => c.id === clearContextChatId);
    clearContextChatId = null;
    if (!clearPossible(chat)) return;
    if (serverSessionTools) {
      await runServerSessionTool("session_clear_context", chat);
      return;
    }
    chat.messages = [...chat.messages, { role: "assistant", content: "", contextCleared: true }];
    dropUndoStash(chat.id);
    chats = [...chats];
    save();
    scrollBottom();
  }

  async function send() {
    if (blockedWhileLoading()) return;
    if (activeChat?.partial) {
      await ensureChatFull(activeChat);
      if (activeChat?.partial) return;
    }
    const text = input.trim();
    if (!text && attachImages.length === 0 && attachFiles.length === 0) return;
    // only THIS session's own live turn blocks its composer: a turn running
    // in another session never stops this one from sending (each session runs
    // its own stream/work turn concurrently)
    if (isCompacting(activeId ?? "") || (activeId === null && startingChatFromHome)) return;
    // Work mode needs the opencode CLI (no fallback engine); a live turn can
    // still take queued messages
    if (composerMode === "work" && workGate.blocked && !activeWorkRunning) return;
    // keep focus off the send/queue button: the streaming swap destroys it,
    // and Safari scrolls the thread to the top on the resulting focus loss
    parkComposerFocus();
    // an open workspace draft never outlives the send: committing first
    // treats typed-then-sent as picked (the native dialog is modal, so
    // desktop can never send mid-pick either)
    if (workspacePickerOpen) await commitWorkspacePicker();
    if (activeStreaming || activeWorkRunning) {
      // work turns stay interactive: a prompt typed mid-run queues for the
      // next round instead of being dropped or starting a parallel turn
      const running = chats.find((c) => c.id === activeId);
      if (activeWorkRunning && (running?.mode ?? "chat") === "work") {
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
    writeLastMode(localStorage, effectiveMode);
    if (serverChatTurns && effectiveMode === "chat") {
      const fromHome = !chat;
      if (fromHome && startingChatFromHome) return;
      if (fromHome) startingChatFromHome = stagedId;
      try {
        const titleSource = text || (images ? "Image" : (firstUploadName ?? "New session"));
        await startServerChat(stagedId, { operation: "send", content: fullText, images, titleSource }, stagedId, () => {
          if (startingChatFromHome === stagedId) startingChatFromHome = null;
          if (input.trim() === text && activeId === sentFromId) clearSentComposer();
          if (fromHome && activeId === sentFromId) {
            activeId = stagedId;
            void saveActiveChat(stagedId);
            void goto(`/s/${stagedId}`);
          }
          const sent = chats.find((c) => c.id === stagedId);
          if (sent) chats = [sent, ...chats.filter((c) => c.id !== stagedId)];
        });
      } finally {
        if (startingChatFromHome === stagedId) startingChatFromHome = null;
      }
      return;
    }
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
        workRoot = isNoWorkspacePath(rawWs)
          ? await ensureScratchWorkspace(stagedId)
          : await canonicalWorkspace(rawWs);
      } catch (e: any) {
        const msg = typeof e === "string" ? e : (e?.message ?? String(e));
        workspaceError = msg;
        error = `Workspace invalid: ${msg}`;
        return;
      }
      workspaceError = null;
      if (isNoWorkspacePath(rawWs)) {
        pendingWorkspace = NO_WORKSPACE_SENTINEL;
      } else {
        defaultWorkspace = workRoot;
        pendingWorkspace = workRoot;
      }
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
        updatedAt: Date.now(),
        messages: [
          { role: "user", content: fullText, ...(images ? { images } : {}) },
          { role: "assistant", content: "", thinking: "", model: selectedModel }
        ],
        mode: effectiveMode,
        mcpEnabled: effectiveMode === "work" ? await newMcpEnabled(homeMcpEnabled) : undefined,
        skillsEnabled: effectiveMode === "work" ? await newSessionSkills(homeSkillsEnabled) : undefined,
        workspace: effectiveMode === "work" ? workRoot : null
      };
      chats = [chat, ...chats];
      if (effectiveMode === "work") {
        homeMcpEnabled = undefined;
        homeSkillsEnabled = undefined;
      }
      activeId = chat.id;
      isPinned = true;
      freshChat = true;
      save();
    } else {
      if (chat.messages.length === 0) {
        if (chat.skillsEnabled === undefined && effectiveMode === "work") {
          chat.skillsEnabled = await newSessionSkills();
        }
        chat.mode = effectiveMode;
        if (effectiveMode === "work") chat.workspace = workRoot;
      }
      // a new user message re-times the session: sidebar Today/Yesterday
      // and ordering follow this, not creation
      chat.updatedAt = Date.now();
      chat.messages = [...chat.messages, { role: "user", content: fullText, ...(images ? { images } : {}) }];
      if (chat.title === "New session" || chat.title === "New Session") {
        const titleSrc = text || (images ? "Image" : (firstUploadName ?? "New session"));
        chat.title = titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : "");
      }
      chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
      // most-recently-active first, so the just-messaged session jumps to top
      chats = [chat, ...chats.filter((x) => x.id !== chat!.id)];
    }
    clearSentComposer();
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

  function clearSentComposer() {
    input = "";
    // a send ends any history walk: the next ArrowUp starts from the newest
    historyIndex = null;
    historyDraft = "";
    mention = null;
    clearDraft(null);
    attachImages = [];
    attachFiles = [];
  }

  function queuePrompt(text: string, images?: string[]) {
    const chatId = activeId;
    if (!chatId || (!text && !images?.length)) return;
    const id = crypto.randomUUID();
    const msg: ChatMsg = { role: "user", content: text, ...(images ? { images } : {}) };
    queuedMsgs = [...queuedMsgs, { chatId, id, msg }];
    const turnId = workTurnIds.get(chatId);
    if (turnId) void queueWorkAgentMsg(turnId, id, msg).catch(() => {});
    input = "";
    historyIndex = null;
    historyDraft = "";
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

  function unqueue(id: string) {
    const removed = queuedMsgs.filter((q) => q.id === id);
    queuedMsgs = queuedMsgs.filter((q) => q.id !== id);
    for (const q of removed) {
      const turnId = workTurnIds.get(q.chatId);
      if (turnId) void removeWorkAgentMsg(turnId, q.id).catch(() => {});
    }
  }

  /** Interrupt the session's live turn and start a new one right away with
   *  this message (plus anything else queued there, in order). The backend
   *  never drains its queue after a stop, so settle's leftovers carry them. */
  function sendQueuedNow(id: string) {
    const q = queuedMsgs.find((x) => x.id === id);
    if (!q) return;
    const chatId = q.chatId;
    if (!isWorkRunning(chatId)) {
      // the turn already ended but this chip stayed behind (a superseded or
      // reattached turn's settle never took it): returning here made the
      // button do nothing. Send it as a fresh turn instead.
      queuedMsgs = queuedMsgs.filter((x) => x.id !== id);
      void sendToWorkSession(chatId, q.msg)
        .then((ok) => {
          if (!ok) {
            queuedMsgs = [...queuedMsgs, q];
            error = "Couldn't send now: the session is busy or no longer a Work session.";
          }
        })
        .catch((e: any) => {
          queuedMsgs = [...queuedMsgs, q];
          error = `Couldn't send now: ${typeof e === "string" ? e : (e?.message ?? String(e))}`;
        });
      return;
    }
    if (sendNowIds.has(chatId)) return; // already interrupting
    // stopping the engine can take a while (opencode's cancel grace): say so
    flashNotice("Interrupting the agent to send this now…");
    // the urgent one goes first
    queuedMsgs = [q, ...queuedMsgs.filter((x) => x.id !== id)];
    const turnId = workTurnIds.get(chatId);
    for (const x of queuedMsgs) {
      if (x.chatId === chatId && turnId) void removeWorkAgentMsg(turnId, x.id).catch(() => {});
    }
    sendNowIds.add(chatId);
    workStopIds.add(chatId);
    if (turnId) void stopWorkAgentTurn(turnId).catch(() => {});
    cancelPendingAsk(chatId, "(interrupted by a message sent now)");
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
      gitRefreshTick++; // renames / deletes / imports change git status too
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
    if (!el || !isTauriRuntime() || onServer) return;
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
   *  otherwise empty composer (typing "/usr/…" filters, anything else hides). */
  const SLASH_COMMANDS = [
    { name: "compact", desc: "Summarize older messages into one note to free context" },
    { name: "clear", desc: "Clear the agent's context" },
    { name: "undo", desc: "Remove the last turn and revert its file changes" },
    { name: "redo", desc: "Re-apply the turn most recently undone" }
  ];
  type SlashItem = { name: string; desc: string; disabled?: boolean };
  let slash = $state<{ query: string; items: SlashItem[]; index: number } | null>(null);
  /** message pairs removed by /undo, newest last, per session — /redo
   *  re-inserts them while the transcript still matches (afterLen). */
  let undoStashes = $state<Record<string, { messages: ChatMsg[]; index: number; afterLen: number }[]>>({});

  /** recompute the slash menu from the input (input/click/keyup/blur) */
  function refreshSlashMenu() {
    // work-only: slash commands steer the agent, meaningless in chat
    if (composerMode !== "work" || !input.startsWith("/") || input.includes("\n") || busy) {
      slash = null;
      return;
    }
    const q = input.slice(1).trim().toLowerCase();
    const head = q.split(/\s+/)[0] ?? "";
    const chat = chats.find((c) => c.id === activeId) ?? null;
    // unavailable commands stay visible but greyed out (undo/redo state)
    const items: SlashItem[] = SLASH_COMMANDS.filter((c) => !head || c.name.startsWith(head)).map((c) => ({
      ...c,
      disabled:
        c.name === "clear" ? !clearPossible(chat) :
        c.name === "undo" ? !undoPossible(chat) :
        c.name === "redo" ? !redoPossible(chat) :
        false
    }));
    if (!items.length) {
      slash = null;
      return;
    }
    // open on the first runnable row (a greyed /redo shouldn't be preselected)
    const firstEnabled = items.findIndex((it) => !it.disabled);
    slash = { query: q, items, index: firstEnabled < 0 ? 0 : firstEnabled };
  }

  async function runServerSessionTool(action: "session_compact" | "session_clear_context" | "session_undo" | "session_redo", chat: Chat, args: Record<string, unknown> = {}) {
    if (isCompacting(chat.id)) return;
    markCompacting(chat.id);
    error = null;
    try {
      const result = await invokeOrBridge<{ chat?: unknown; notice?: string }>(action, {
        id: chat.id, baseRev: serverSaveRow(chat)?.baseRev, ...args
      });
      if (result.chat && chats.some((c) => c.id === chat.id)) {
        adoptChatTurn(result.chat);
        if (activeId === chat.id) {
          scrollBottom();
          if (action !== "session_compact") void loadExplorer().catch(() => {});
        }
      }
      if (result.notice && activeId === chat.id) flashNotice(result.notice);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      if (error.includes("SESSION_CHANGED")) await reloadServerChatTurn(chat.id);
    } finally {
      unmarkCompacting(chat.id);
    }
  }

  /** /undo: remove the trailing clear marker or drop the last pair and revert its
   *  file writes (backend pops one undo frame onto its redo stack). Work-only,
   *  idle only — like /compact. Shell (`exec`) side effects are reported,
   *  never auto-reverted. The removed pair is stashed so /redo can restore
   *  it in place. */
  async function undoSession() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat) {
      flashNotice("/undo needs a Work session — open one first.");
      return;
    }
    if ((chat.mode ?? "chat") !== "work") {
      flashNotice("/undo is only available in Work sessions.");
      return;
    }
    if (busy) return;
    if (serverSessionTools) {
      await runServerSessionTool("session_undo", chat);
      return;
    }
    const range = undoMessageRange(chat.messages);
    if (!range) {
      flashNotice("Nothing to undo — no completed turn.");
      return;
    }
    const { index, count } = range;
    const removed = chat.messages.slice(index, index + count);
    const afterLen = chat.messages.length - count;
    const contextOnly = removed[0].contextCleared;
    let undoMsg = "Restored context.";
    if (!contextOnly) {
      try {
        undoMsg = await invokeOrBridge<string>("work_undo", { sessionId: chat.id });
      } catch (e: any) {
        const msg = typeof e === "string" ? e : (e?.message ?? String(e));
        if (!msg.includes("Nothing to undo")) {
          error = `Undo failed: ${msg}`;
          return;
        }
        undoMsg = "Undone last turn — no file changes to revert.";
      }
    }
    const c = chats.find((x) => x.id === chat.id);
    if (!c) return;
    c.messages = [...c.messages.slice(0, index), ...c.messages.slice(index + count)];
    c.updatedAt = Date.now();
    chats = [...chats];
    // stash for /redo: only valid while the transcript still matches the
    // shape undo left behind (any new/edited message greys redo out)
    const stack = undoStashes[chat.id] ?? [];
    undoStashes = {
      ...undoStashes,
      [chat.id]: [...stack.slice(-19), { messages: removed, index, afterLen }]
    };
    save();
    scrollBottom();
    if (!contextOnly) void loadExplorer().catch(() => {});
    flashNotice(undoMsg);
  }

  /** /redo: re-insert the pair /undo removed and re-apply that turn's file
   *  writes (backend pops its redo frame). Greys out when impossible. */
  async function redoSession() {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat) {
      flashNotice("/redo needs a Work session — open one first.");
      return;
    }
    if ((chat.mode ?? "chat") !== "work") {
      flashNotice("/redo is only available in Work sessions.");
      return;
    }
    if (busy || !redoPossible(chat)) return;
    if (serverSessionTools) {
      await runServerSessionTool("session_redo", chat);
      return;
    }
    const stack = undoStashes[chat.id] ?? [];
    const top = stack[stack.length - 1];
    if (!top) return;
    const contextOnly = top.messages[0].contextCleared;
    let redoMsg = "Cleared context.";
    if (!contextOnly) {
      try {
        redoMsg = await invokeOrBridge<string>("work_redo", { sessionId: chat.id });
      } catch (e: any) {
        const msg = typeof e === "string" ? e : (e?.message ?? String(e));
        // messages come from the frontend stash, so a missing backend frame
        // (e.g. the turn ran before this build) is not a failure
        if (!msg.includes("Nothing to redo")) {
          error = `Redo failed: ${msg}`;
          return;
        }
        redoMsg = "Redone last turn — no file changes to replay.";
      }
    }
    const c = chats.find((x) => x.id === chat.id);
    if (!c || c.messages.length !== top.afterLen) {
      flashNotice("Transcript changed since the undo — redo is no longer possible.");
      return;
    }
    c.messages = [
      ...c.messages.slice(0, top.index),
      ...top.messages,
      ...c.messages.slice(top.index)
    ];
    c.updatedAt = Date.now();
    chats = [...chats];
    undoStashes = { ...undoStashes, [chat.id]: stack.slice(0, -1) };
    save();
    scrollBottom();
    if (!contextOnly) void loadExplorer().catch(() => {});
    flashNotice(redoMsg);
  }

  function clearPossible(chat: Chat | null | undefined): chat is Chat {
    return !!chat && chat.mode === "work" && !busy && !isCompacting(chat.id) && canClearContext(chat.messages);
  }

  function undoPossible(chat: Chat | null): boolean {
    return !!chat && chat.mode === "work" && !busy && undoMessageRange(chat.messages) !== null;
  }

  /** /redo availability: something was undone and the transcript is
   *  unchanged since (a new message or edit greys it out). */
  function redoPossible(chat: Chat | null): boolean {
    if (!chat || (chat.mode ?? "chat") !== "work" || busy) return false;
    if (serverSessionTools) return chat.canRedo === true;
    const stack = undoStashes[chat.id];
    const top = stack?.[stack.length - 1];
    return !!top && chat.messages.length === top.afterLen;
  }

  /** a new agent turn invalidates the redo history (backend clears its
   *  stack in undo_begin_turn; drop the message stash to match) */
  function dropUndoStash(chatId: string) {
    if (!(chatId in undoStashes)) return;
    const { [chatId]: _, ...rest } = undoStashes;
    undoStashes = rest;
  }

  /** run the highlighted slash command, then clear the composer */
  function runSlash() {
    if (blockedWhileLoading()) {
      slash = null;
      return;
    }
    const item = slash?.items[slash.index];
    const cmd = item?.name;
    slash = null;
    if (!cmd || item?.disabled) return;
    input = "";
    historyIndex = null;
    historyDraft = "";
    clearDraft(null);
    autosizeComposer();
    composerEl?.focus();
    if (cmd === "clear" && clearPossible(activeChat)) clearContextChatId = activeChat.id;
    if (cmd === "compact") void compactSession();
    if (cmd === "undo") void undoSession();
    if (cmd === "redo") void redoSession();
  }

  /** context window of the selected model for the usage ring (cached per
   *  model via modelContextLength; 32768 fallback until it resolves) */
  let ctxWindow = $state(32768);
  async function refreshCtxWindow() {
    const m = selectedModel;
    if (isOpenRouterSelection(m)) {
      const om = findOpenRouterModel(openrouterModels, m);
      if (selectedModel !== m) return;
      if (om?.contextLength) ctxWindow = om.contextLength;
      return;
    }
    const len = await modelContextLength(m).catch(() => null);
    if (selectedModel !== m) return;
    if (len) ctxWindow = len;
  }
  $effect(() => {
    if (!ready) return;
    selectedModel;
    openrouterModels;
    void refreshCtxWindow();
  });

  /** usage for the Work header ring: system prompt + send-shaped history
   *  over the model's window — the same fraction the auto-compact watches,
   *  so the ring hitting ~70% explains a compaction */
  const workCtx = $derived.by(() => {
    const c = activeChat;
    if (!c || (c.mode ?? "chat") !== "work") return null;
    const used = estimateWorkTokens(compactedHistory(c.messages));
    const win = ctxWindow > 0 ? ctxWindow : 32768;
    return { used, win, frac: Math.min(1, used / win) };
  });

  /** total OpenRouter spend for the open Work session: sum of per-message
   *  `genCost` (USD, authoritative `usage.cost` per round). Ollama messages
   *  carry no cost, so they contribute nothing. */
  const workSpent = $derived.by(() => {
    const c = activeChat;
    if (!c || (c.mode ?? "chat") !== "work") return 0;
    let total = 0;
    for (const m of c.messages) {
      if (typeof m.genCost === "number" && Number.isFinite(m.genCost)) total += m.genCost;
    }
    return total;
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

  // in-app code editor (CodeMirror, lazy-loaded on first open so the initial
  // bundle stays lean, like xterm). Click, double-click and menu → Open all
  // land here for files; folders keep opening in the OS.
  let codeEditorPath = $state<string | null>(null);
  let codeEditorRoot = $state("");
  let CodeEditorComp = $state<any>(null);

  async function openInEditor(path: string) {
    if (codeEditorPath === path) return; // dblclick's second click
    try {
      if (!CodeEditorComp) CodeEditorComp = (await import("$lib/CodeEditorModal.svelte")).default;
      codeEditorRoot = explorerRoot.trim();
      codeEditorPath = path;
    } catch (e: any) {
      flashExpNotice(`Couldn't load the editor: ${e?.message ?? String(e)}`);
    }
  }

  function closeCodeEditor() {
    codeEditorPath = null;
  }

  let skillsOpen = $state(false);
  let skillDefinitions = $state<SkillSummary[]>([]);
  let skillsLoaded = $state(false);
  let homeSkillsEnabled = $state<SkillsEnabledMap>();
  const skillsList = $derived(activeChat
    ? resolveSkills(skillDefinitions, activeChat.skillsEnabled)
    : resolveSkills(skillDefinitions, newSkillSelection(skillDefinitions, homeSkillsEnabled)));
  const skillsEnabledCount = $derived(enabledSkillCount(skillsList));

  async function refreshSkills() {
    try {
      skillDefinitions = await listSkills();
      skillsLoaded = true;
    } catch {
      // Keep the last catalog while the backend reconnects.
    }
  }

  async function newSessionSkills(draft?: SkillsEnabledMap) {
    try {
      return newSkillSelection(await listSkills(), draft);
    } catch {
      return skillsLoaded ? newSkillSelection(skillDefinitions, draft) : undefined;
    }
  }

  async function setSkillForSession(sid: string | null, slug: string, enabled: boolean) {
    if (sid === null) {
      homeSkillsEnabled = { ...homeSkillsEnabled, [slug]: enabled };
      return;
    }
    const updated = await serverAction("session_update", { id: sid, skillToggle: { slug, enabled } });
    if (updated) return;
    if (updated === null) throw new Error("Could not update the session's skill.");
    const chat = chats.find((item) => item.id === sid);
    if (!chat) throw new Error("Work session no longer exists.");
    chat.skillsEnabled = { ...(chat.skillsEnabled ?? skillSwitches(skillDefinitions)), [slug]: enabled };
    save();
  }

  async function setSkillsAsDefault() {
    const defaults = skillSwitches(skillsList);
    const previous = skillSwitches(skillDefinitions);
    await saveSkillDefaults(defaults);
    // Legacy sessions were using the old defaults. The server freezes these
    // in its database, and the dev bridge keeps its copies in the window.
    for (const chat of chats) {
      if (chat.mode === "work" && chat.skillsEnabled === undefined) chat.skillsEnabled = previous;
    }
    if (!serverPersist) save();
    await refreshSkills();
  }

  function forgetSkillSwitch(slug: string) {
    if (homeSkillsEnabled) delete homeSkillsEnabled[slug];
    for (const chat of chats) {
      if (chat.skillsEnabled) delete chat.skillsEnabled[slug];
    }
    if (!serverPersist) save();
  }

  onMount(() => {
    void refreshSkills();
  });

  function openSkillsModal() {
    skillsOpen = true;
    void refreshSkills();
  }

  async function openSkillInEditor(skill: SkillSummary) {
    skillsOpen = false;
    await openInEditor(skill.path);
  }

  async function expOpenFile() {
    const p = expCtx;
    closeExpCtx();
    if (!p) return;
    if (p.isDir) await openPathExternal(p.path);
    else await openInEditor(p.path);
  }

  async function expOpenExternal() {
    const p = expCtx;
    closeExpCtx();
    if (p) await openPathExternal(p.path);
  }

  /** OS default app, sandboxed backend-side (work_open_path) */
  async function openPathExternal(path: string) {
    if (onServer) {
      flashExpNotice("That file is on the server; open it in the editor instead.");
      return;
    }
    if (!isTauriRuntime()) {
      flashExpNotice("Opening in the default app needs the desktop app.");
      return;
    }
    try {
      await openWorkPathExternal(explorerRoot.trim(), path);
    } catch (e: any) {
      flashExpNotice(`Couldn't open: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  /** Server window: copy the file from the server into this computer's ~/Downloads, then reveal it. */
  async function expDownload() {
    const p = expCtx;
    closeExpCtx();
    if (!p || !activeServer || !isTauriRuntime()) return;
    flashExpNotice(`Downloading ${p.path.split("/").pop()}…`);
    try {
      const { invoke } = await import("@tauri-apps/api/core");
      const dest = await invoke<string>("server_download_file", {
        id: activeServer,
        root: explorerRoot.trim(),
        path: p.path,
      });
      flashExpNotice(`Saved to ${dest}`);
      try {
        const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
        await revealItemInDir(dest);
      } catch {}
    } catch (e: any) {
      flashExpNotice(`Couldn't download: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  async function expReveal() {
    const p = expCtx;
    closeExpCtx();
    if (!p || !isTauriRuntime()) return;
    if (onServer) {
      flashExpNotice("That file is on the server, not this computer.");
      return;
    }
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

  /** side-panel tabs: focus the Git view, creating it if closed */
  function openSideGit() {
    newTabMenu = false;
    if (!sideTabs.some((t) => t.kind === "git")) {
      sideTabs = [...sideTabs, { key: "git", kind: "git", title: "Git" }];
    }
    activeSideTab = "git";
  }

  /** bumped whenever files may have changed, so a visible Git tab re-reads status */
  let gitRefreshTick = $state(0);

  async function revealWorkPath(rel: string) {
    if (onServer) {
      flashExpNotice("That file is on the server, not this computer.");
      return;
    }
    if (!isTauriRuntime()) {
      flashExpNotice("Reveal needs the desktop app.");
      return;
    }
    try {
      const { revealItemInDir } = await import("@tauri-apps/plugin-opener");
      await revealItemInDir(absExpPath(rel));
    } catch (e: any) {
      flashExpNotice(`Couldn't reveal: ${typeof e === "string" ? e : (e?.message ?? String(e))}`);
    }
  }

  // ---- Browser tab (shared `lb` daemon: iframe view + `lb mcp` agent tools) ----
  // The tab is a view onto the daemon, not its owner: closing the tab leaves
  // the daemon (and the agent's MCP connection) running. The only tab chrome
  // is a stale-engine strip (owned daemon predates the on-disk bundle):
  // in-place recover can't fix that, so it offers a full restart instead.
  let browserDaemon = $state<BrowserStatus | null>(null);
  let browserStarting = $state(false);
  let browserError = $state<string | null>(null);
  /** starting-view label: plain starts vs button restarts/takeovers. */
  let browserStartLabel = $state("Starting browser…");
  /** live CloakBrowser download progress while an engine fetch is in flight. */
  let browserDownload = $state<EngineDownloadProgress | null>(null);
  /** wall-clock start of the in-flight start drive: the starting view shows
   *  elapsed seconds so a slow Windows first launch never reads as frozen. */
  let browserStartedAt = $state<number | null>(null);
  /** iframe key: bumped whenever the daemon behind a live frame is (re)started,
   *  so the frame re-navigates instead of sitting on a failed navigation. */
  let browserFrameRev = $state(0);
  let browserProxyUrl = $state<string | null>(null);
  /** lb UI url + Vicuna theme/palette/accent: lb reads `?theme=`/`?accent=`/
   *  `?accentfg=` + palette vars (`bg`, `fg`, `card`, `border`, …) pre-paint
   *  (older lb builds ignore unknown params and fall back to their saved theme
   *  + built-in greens). Derived from `themeBase`/`pluginTheme`/`accent`, so
   *  appearance flips re-navigate the frame into the matching lb look. */
  const browserFrameSrc = $derived.by(() => {
    const base = onServer ? browserProxyUrl : (browserDaemon?.url ?? LB_URL);
    if (!base) return "about:blank";
    const a = ACCENTS[accent] ?? ACCENTS.orange;
    const v = pluginAccent && !pluginAccentMuted ? pluginAccent : themeBase === "dark" ? a.dark : a.light;
    return `${base}/?${browserThemeParams(themeBase, pluginTheme, v).toString()}`;
  });

  /** status → start when down. Throws on failure (callers surface it).
   *  Concurrent callers share one in-flight drive (tab-open + visible-tab
   *  ensure + poll can all fire together) instead of stacking spawns. */
  let browserDrive: Promise<BrowserStatus> | null = null;
  function ensureBrowserDaemon(): Promise<BrowserStatus> {
    browserDrive ??= driveBrowserDaemon().finally(() => {
      browserDrive = null;
    });
    return browserDrive;
  }
  /** one-time CloakBrowser fetch with live progress polling (non-blocking on
   *  new backends so remote servers never hit HTTP timeouts during a slow
   *  ~200 MB download; falls back gracefully on older backends that ignore
   *  `background: true`). Best-effort: a failed/blocked download must not
   *  abort the start, since the backend falls back to a system Chromium. */
  async function prewarmBrowserEngine(existing?: EngineDownloadProgress | null) {
    const active = (d?: EngineDownloadProgress | null) =>
      d != null && (d.phase === "downloading" || d.phase === "extracting");
    browserDownload = active(existing) ? existing! : null;
    browserStartLabel = browserDownload
      ? formatDownloadProgress(browserDownload)
      : "Downloading browser engine (one-time setup)…";
    try {
      let poll = active(existing);
      if (!poll) {
        const info = await browserEnsureEngine(true);
        poll = !!info.downloading;
      }
      if (poll) {
        const deadline = Date.now() + 15 * 60 * 1000;
        while (Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, 1000));
          const st = await browserStatus();
          if (st.running) {
            browserDownload = null;
            return;
          }
          const d = st.download ?? null;
          if (!active(d)) {
            browserDownload = null;
            return;
          }
          browserDownload = d;
          browserStartLabel = formatDownloadProgress(d!);
        }
      }
    } catch {
      /* fall through: the start path falls back to a system Chromium */
    } finally {
      browserDownload = null;
    }
  }

  /** Seconds the start drive has been running (0 while idle). */
  const browserStartSeconds = $derived.by(() => {
    if (!browserStarting || browserStartedAt == null) return 0;
    return Math.max(0, Math.floor((now - browserStartedAt) / 1000));
  });
  /** Starting label + elapsed tick ("Starting browser… (12s)"). */
  const browserStartDisplay = $derived.by(() => {
    return browserStartSeconds >= 3
      ? `${browserStartLabel} (${browserStartSeconds}s)`
      : browserStartLabel;
  });

  /** Reject `p` after `ms` so a blocked backend call can never leave the tab
   *  spinning forever (Windows antivirus/SmartScreen can stall a first launch
   *  far past every backend timeout). The backend attempt keeps running and a
   *  later poll adopts the daemon if it did come up. */
  function withBrowserTimeout<T>(p: Promise<T>, ms: number, message: string): Promise<T> {
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(message)), ms);
      p.then(
        (v) => {
          clearTimeout(timer);
          resolve(v);
        },
        (e) => {
          clearTimeout(timer);
          reject(e);
        }
      );
    });
  }

  const BROWSER_START_TIMEOUT_MS = 4 * 60 * 1000;
  const BROWSER_START_TIMEOUT_MESSAGE =
    "The browser engine did not respond within 4 minutes. On Windows this is usually antivirus blocking the engine binary — click Retry, or set VICUNA_BROWSER_CHROME to a Chrome/Edge path and relaunch Vicuna.";

  async function driveBrowserDaemon(): Promise<BrowserStatus> {
    browserStartLabel = "Starting browser…";
    browserStarting = true;
    browserStartedAt = Date.now();
    browserError = null;
    try {
      let st = await browserStatus();
      if (!st.running) {
        await prewarmBrowserEngine(st.download);
        browserStartedAt = Date.now();
        browserStartLabel = "Starting browser…";
        st = await withBrowserTimeout(
          browserStart(),
          BROWSER_START_TIMEOUT_MS,
          BROWSER_START_TIMEOUT_MESSAGE
        );
      }
      if (onServer && !browserProxyUrl) {
        const sid = activeServerId();
        if (sid) browserProxyUrl = await serverLbProxyUrl(sid);
      }
      browserDaemon = st;
      return st;
    } catch (e: any) {
      // plain browser with no bridge: still allow viewing a manually-run
      // daemon (`lb web --port 18765` in a terminal) via a direct probe
      if (!isTauriRuntime()) {
        try {
          const r = await fetch(`${LB_URL}/api/health`);
          const h = await r.json();
          if (h?.ok === true && typeof h?.probe === "string") {
            browserDaemon = {
              running: true,
              owned: false,
              engine_stale: false,
              url: LB_URL,
              health: { browser_alive: !!h.browser_alive, probe: h.probe, tabs: h.tabs ?? 0 }
            };
            return browserDaemon;
          }
        } catch {
          /* fall through to the backend error below */
        }
      }
      browserError = e?.message ?? String(e);
      browserDaemon = null;
      throw e;
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }

  /** full stop + start of an owned daemon (stale engine / wedged
   *  browser). Refuses adopted daemons — the backend owns that refusal. */
  async function restartBrowserDaemon() {
    // swap the view instantly: the stop + start takes seconds, and the
    // stale frame meanwhile reads as "nothing happens"
    browserStartLabel = "Restarting browser…";
    browserDaemon = null;
    browserStarting = true;
    browserStartedAt = Date.now();
    browserError = null;
    try {
      await prewarmBrowserEngine();
      browserStartLabel = "Restarting browser…";
      browserDaemon = await withBrowserTimeout(
        browserRestart(),
        BROWSER_START_TIMEOUT_MS,
        BROWSER_START_TIMEOUT_MESSAGE
      );
      browserFrameRev += 1;
    } catch (e: any) {
      browserError = e?.message ?? String(e);
      browserDaemon = null;
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }

  /** two-step arm for adopted takeovers (first click explains + arms,
   *  second within 5s executes) — killing a foreign session must stay a
   *  deliberate act, never a stray click. */
  let browserKillArmed = $state(false);
  let browserKillTimer: ReturnType<typeof setTimeout> | null = null;

  /** tab-bar button: `lb start` when down, stop + start when owned,
   *  confirmed kill + start when adopted. Failures flash a transient
   *  notice instead of replacing a (possibly healthy) frame. */
  async function restartBrowserFromButton() {
    if (browserStarting) return;
    let st: BrowserStatus | null = null;
    try {
      st = await browserStatus();
    } catch {
      /* fall through to ensure on last-known state */
    }
    const action = browserButtonAction(st ?? browserDaemon);
    if (action === "start") {
      // instant view swap (a stale running frame would sit blank meanwhile)
      browserStartLabel = "Starting browser…";
      browserDaemon = null;
      const ok = await ensureBrowserDaemon().then(
        () => true,
        () => false
      );
      if (ok) browserFrameRev += 1;
      return;
    }
    if (action === "restart") {
      browserStartLabel = "Restarting browser…";
      browserDaemon = null;
      browserStarting = true;
      browserStartedAt = Date.now();
      try {
        await prewarmBrowserEngine();
        browserStartLabel = "Restarting browser…";
        browserDaemon = await withBrowserTimeout(
          browserRestart(),
          BROWSER_START_TIMEOUT_MS,
          BROWSER_START_TIMEOUT_MESSAGE
        );
        browserError = null;
        browserFrameRev += 1;
      } catch (e: any) {
        flashExpNotice(e?.message ?? String(e));
        // restore truth instead of guessing: stop may have died mid-flight
        browserDaemon = await browserStatus().catch(() => null);
      } finally {
        browserStarting = false;
        browserStartedAt = null;
      }
      return;
    }
    if (!browserKillArmed) {
      browserKillArmed = true;
      flashExpNotice(
        "Browser was started outside Vicuna — click again to force-stop it and start Vicuna's own (its tabs will be lost)."
      );
      if (browserKillTimer) clearTimeout(browserKillTimer);
      browserKillTimer = setTimeout(() => {
        browserKillArmed = false;
        browserKillTimer = null;
      }, 5000);
      return;
    }
    if (browserKillTimer) {
      clearTimeout(browserKillTimer);
      browserKillTimer = null;
    }
    browserKillArmed = false;
    browserStartLabel = "Taking over browser…";
    browserDaemon = null;
    browserStarting = true;
    browserStartedAt = Date.now();
    try {
      // pre-warm BEFORE the kill: a failed download must not cost their tabs
      // (backend takeover ensures pre-kill too — this is the honest label)
      await prewarmBrowserEngine();
      browserStartLabel = "Taking over browser…";
      browserDaemon = await withBrowserTimeout(
        browserTakeover(),
        BROWSER_START_TIMEOUT_MS,
        BROWSER_START_TIMEOUT_MESSAGE
      );
      browserError = null;
      browserFrameRev += 1;
    } catch (e: any) {
      flashExpNotice(e?.message ?? String(e));
      browserDaemon = await browserStatus().catch(() => null);
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }

  /** one poll tick for a visible running tab (see browserPollAction). */
  async function pollBrowserDaemon() {
    if (browserStarting) return;
    let st: BrowserStatus | null = null;
    try {
      st = await browserStatus();
    } catch {
      /* backend unreachable: keep the frame, retry next tick */
    }
    const action = browserPollAction(browserDaemon, st);
    if (action === "update" && st) browserDaemon = st;
    else if (action === "ensure") {
      const ok = await ensureBrowserDaemon().then(
        () => true,
        () => false
      );
      if (browserPollRevived(action, ok)) browserFrameRev += 1;
    }
  }

  // A visible Browser tab must converge to running: the daemon can die
  // under a live tab (crash, kill, sleep) leaving a dead iframe with no
  // recourse, so re-validate on every view and poll while up. Never
  // revives while hidden; errors stay user-driven (Retry owns them).
  let browserPoll: ReturnType<typeof setInterval> | null = null;
  const browserTabVisible = $derived(
    sideTabs.find((t) => t.key === activeSideTab)?.kind === "browser"
  );
  $effect(() => {
    const visible = browserTabVisible;
    const running = browserDaemon?.running === true;
    if (visible && !running && !browserStarting && !browserError) {
      void ensureBrowserDaemon().catch(() => {});
    }
    if (visible && running && !browserPoll) {
      browserPoll = setInterval(() => void pollBrowserDaemon(), 5000);
    } else if ((!visible || !running) && browserPoll) {
      clearInterval(browserPoll);
      browserPoll = null;
    }
  });

  /** side-panel tabs: focus the Browser view, creating it if closed */
  function openSideBrowser() {
    newTabMenu = false;
    const existing = sideTabs.find((t) => t.kind === "browser");
    if (existing) {
      activeSideTab = existing.key;
    } else {
      sideTabs = [...sideTabs, { key: "browser", kind: "browser", title: "Browser" }];
      activeSideTab = "browser";
    }
    void ensureBrowserDaemon().catch(() => {});
  }

  function termTheme(): XTermTheme {
    // Full 16-color ANSI palettes: PowerShell / PSReadLine assume a dark
    // console (yellow prompt segments, white text), so the defaults that
    // ship with xterm go unreadable on a light background. Both palettes
    // follow the active app/plugin theme's bg/fg while remapping every ANSI
    // slot to stay legible.
    const chrome = resolveTerminalColors(themeBase, pluginTheme);
    if (themeBase === "dark") {
      return {
        ...chrome,
        selectionBackground: "rgba(255,255,255,.25)",
        selectionInactiveBackground: "rgba(255,255,255,.16)",
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
      ...chrome,
      selectionBackground: "rgba(0,0,0,.20)",
      selectionInactiveBackground: "rgba(0,0,0,.12)",
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
        v.term.options.theme = t;
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

  /** open a Terminal tab rooted at the workspace (desktop or dev bridge) */
  async function openSideTerminal() {
    newTabMenu = false;
    const key = `term-${sideTabSeq++}`;
    sideTabs = [...sideTabs, { key, kind: "terminal", title: explorerRootBase || "Terminal" }];
    activeSideTab = key;
    await tick();
    if (!isTauriRuntime() && !(await bridgeAvailable())) {
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
        scrollback: 5000,
        windowsPty: /win/i.test(navigator.userAgent ?? "") ? { backend: "conpty" } : undefined
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      fit.fit();
      // xterm leaves clipboard chords to the embedder. macOS WKWebView fires
      // the browser copy event for ⌘C, but WebView2 does not, so a selection
      // there could not be copied and Ctrl+C interrupted the shell instead.
      term.attachCustomKeyEventHandler((ev) => {
        if (ev.type !== "keydown") return true;
        const key = ev.key.toLowerCase();
        // Windows Terminal's select-all chord: a no-drag escape hatch.
        if (!isMac && ev.ctrlKey && ev.shiftKey && key === "a") {
          term.selectAll();
          return false;
        }
        if (key !== "c" || !term.hasSelection()) return true;
        const copyChord = isMac ? ev.metaKey && !ev.ctrlKey : ev.ctrlKey && !ev.altKey;
        if (!copyChord) return true;
        // Returning false also keeps xterm from sending ETX, which sent SIGINT
        // to the shell and cleared the selection on Windows.
        copyTermSelection(term);
        return false;
      });
      // Tauri disables the native WebView context menu, so right-click opens
      // our own (Copy / Paste / Select All / Clear). It used to copy silently
      // on right-click, with no sign anything happened.
      el.addEventListener("contextmenu", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        termCtx = {
          x: Math.max(8, Math.min(ev.clientX, window.innerWidth - 200)),
          y: Math.max(8, Math.min(ev.clientY, window.innerHeight - 180)),
          term,
          // read now: focus leaves the terminal once the menu is clicked
          sel: term.getSelection()
        };
      });
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

  // refresh the tree + MCP popup when an agent turn settles (files may
  // have changed, and the agent may have added/removed MCP servers)
  let wasWorkRunning = false;
  $effect(() => {
    if (wasWorkRunning && !workRunning) {
      if (explorerOpen && !explorerLoading) void loadExplorer();
      gitRefreshTick++;
      void syncMcpServersAfterTurn();
    }
    wasWorkRunning = workRunning;
  });

  /** work mode: Esc once arms the interrupt, Esc again within 2.5s stops it.
   *  Only the open session's own turn is interruptible here — a background
   *  turn keeps running until its own session stops it. */
  function onEscape() {
    if (!busy) return false;
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
  function answerAsk(value: string, display?: string) {
    const id = activeId;
    const a = id != null ? pendingAsks[id] : undefined;
    if (!id || !a) return;
    const { [id]: _, ...rest } = pendingAsks;
    pendingAsks = rest;
    a.resolve(value, "answer", display);
    scrollBottom();
  }

  /** a choice on an agent question: copy it into the answer box to edit,
   *  caret at the end — sending stays a separate, deliberate step */
  function prefillAsk(card: { text: string }, opt: string, from: HTMLElement) {
    card.text = opt;
    // the box of the card that was clicked (live and put-off cards coexist)
    const box = from.closest(".ask-card");
    requestAnimationFrame(() => {
      const el = box?.querySelector<HTMLTextAreaElement>("textarea.ask-input");
      if (!el) return;
      el.focus();
      el.setSelectionRange(el.value.length, el.value.length);
    });
  }

  /** send the agent-question answer box (text, multi-select picks, images) */
  function sendAskBox(card: PendingAsk) {
    const r = buildAskAnswer(card);
    if (r) answerAsk(r.answer, r.display);
  }

  /** images pasted into the ask answer box attach as thumbnails (same
   *  downscale as the composer); text pastes stay native */
  async function onAskPaste(e: ClipboardEvent, card: { images: string[] }) {
    const files = pastedImageFiles(e);
    if (!files.length) return;
    e.preventDefault();
    for (const f of files) {
      try {
        card.images = [...card.images, await imageFileToDataUrl(f)];
      } catch {
        error = `Couldn't read pasted image (${f.name || f.type || "unknown type"})`;
      }
    }
  }

  /** "Answer later": the agent stops waiting (it continues or ends its turn)
   *  and the card stays here, detached; sending it later delivers the answer
   *  to the session as a message */
  function answerLater() {
    const id = activeId;
    const a = id != null ? pendingAsks[id] : undefined;
    if (!id || !a || !a.agentQuestion) return;
    const { [id]: _, ...rest } = pendingAsks;
    pendingAsks = rest;
    laterAsks = [
      ...laterAsks,
      {
        id: crypto.randomUUID(),
        chatId: id,
        index: a.index,
        question: a.question,
        options: [...a.options],
        multi: a.multi,
        picked: [...a.picked],
        text: a.text,
        images: [...a.images]
      }
    ];
    a.resolve(ASK_LATER, "later");
  }

  /** user dismissed the question without answering: tell the model so it
   *  stops waiting and doesn't re-ask */
  function dismissAsk() {
    const id = activeId;
    const a = id != null ? pendingAsks[id] : undefined;
    if (!id || !a) return;
    const { [id]: _, ...rest } = pendingAsks;
    pendingAsks = rest;
    a.resolve(
      "User dismissed the question without answering. Do not ask it again — continue with your best judgment or explain what you need.",
      "dismiss"
    );
    scrollBottom();
  }

  /** dismiss a session's pending ask without an answer (stop / chat deleted)
   *  so the awaiting work loop can unwind instead of hanging forever */
  function cancelPendingAsk(chatId: string, reason: string) {
    const a = pendingAsks[chatId];
    if (!a) return;
    const { [chatId]: _, ...rest } = pendingAsks;
    pendingAsks = rest;
    a.resolve(reason, "cancel");
  }

  /** stop the OPEN session's live turn only — background sessions keep
   *  running (each has its own Stop while open, and its own sidebar row) */
  function stop() {
    const id = activeId;
    // same destroyed-node focus loss as send(): the stop button unmounts at
    // settle, so park focus first or Safari jumps to the top on Stop too
    parkComposerFocus();
    if (id == null) return;
    stopSession(id);
  }

  /** stop one session's live turn (Stop button, or a lead's manage_sessions) */
  function stopSession(id: string) {
    const chatFeed = serverChatFeeds.get(id);
    if (chatFeed) {
      chatStopIds.add(id);
      chatFeed.stop();
      return;
    }
    if (isWorkRunning(id)) workStopIds.add(id);
    const turnId = workTurnIds.get(id);
    if (turnId) {
      void stopWorkAgentTurn(turnId).catch(() => {});
      reconcileStoppedTurn(id, turnId);
    }
    cancelPendingAsk(id, "(the user interrupted before answering)");
    if (aborters.has(id)) chatStopIds.add(id);
    aborters.get(id)?.abort();
  }

  /** After a Stop, make the window agree with the backend: if the stopped
   *  turn is over there but this window still shows it running (its `done`
   *  event got lost), settle it here. Never touches a newer turn. */
  function reconcileStoppedTurn(id: string, turnId: string, step = 0) {
    const delay = STOP_RECONCILE_DELAYS_MS[step];
    if (delay == null) return;
    setTimeout(async () => {
      if (!isWorkRunning(id) || workTurnIds.get(id) !== turnId) return;
      const backendLive = await workTurnLive(turnId);
      const action = stopReconcileAction({ running: isWorkRunning(id), currentTurnId: workTurnIds.get(id), stoppedTurnId: turnId, backendLive });
      if (action === "clear") workForceSettle.get(id)?.();
      else if (action === "retry") reconcileStoppedTurn(id, turnId, step + 1);
    }, delay);
  }

  /** Terminal-style prompt history for the open session: the sent user
   *  messages, newest first. Empty threads have no history. */
  function promptHistory(): string[] {
    const c = chats.find((x) => x.id === activeId);
    if (!c) return [];
    const out: string[] = [];
    for (const m of c.messages) {
      if (m.role === "user" && m.content.trim()) out.push(m.content);
    }
    return out.reverse();
  }

  /** Move one step through the prompt history (dir -1 = older/ArrowUp,
   *  +1 = newer/ArrowDown; the list is newest-first). Reaching past the
   *  newest restores the draft the user was typing. Returns false when
   *  there was nothing to do (let the caret move). */
  function historyStep(dir: -1 | 1): boolean {
    const hist = promptHistory();
    if (!hist.length) return false;
    if (historyIndex == null) {
      if (dir > 0) return false; // ArrowDown with nothing recalled: plain caret
      historyDraft = input;
      historyIndex = 0;
    } else {
      // newest-first list: older entries live at larger indexes
      const next = historyIndex - dir;
      if (next < 0) {
        // stepped past the newest recalled entry: hand the draft back
        input = historyDraft;
        historyIndex = null;
        historyDraft = "";
        historyAfterFill();
        return true;
      }
      if (next >= hist.length) return true; // already at the oldest entry
      historyIndex = next;
    }
    input = hist[historyIndex];
    historyAfterFill();
    return true;
  }

  /** caret to the end + autosize after a history fill (no input event fires
   *  for a programmatic value change, so do the upkeep by hand) */
  function historyAfterFill() {
    autosizeComposer();
    requestAnimationFrame(() => {
      const el = composerEl;
      if (el) {
        el.setSelectionRange(input.length, input.length);
        el.focus();
      }
      syncBackdropScroll();
      refreshMentionMenu();
    });
  }

  // switching sessions starts a fresh history walk (the box is shared, but
  // each session walks its own sent messages)
  $effect(() => {
    activeId;
    historyIndex = null;
    historyDraft = "";
  });

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
        // greyed rows are skipped: arrows only land on runnable commands
        const dir = e.key === "ArrowDown" ? 1 : -1;
        const n = slash.items.length;
        let next = slash.index;
        for (let step = 0; step < n; step++) {
          next = (next + dir + n) % n;
          if (!slash.items[next].disabled) break;
        }
        slash.index = next;
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        popupKeyHandled = true;
        runSlash();
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
    // terminal-style prompt history: ArrowUp recalls the last sent message
    // (even over typed text, as long as the caret is on the first line),
    // ArrowUp again walks older, ArrowDown returns toward the draft. While
    // browsing, Down on the last line steps past the newest entry and hands
    // the draft back. Collapsed carets only, so in-message navigation wins.
    if ((e.key === "ArrowUp" || e.key === "ArrowDown") && noMod && !e.shiftKey) {
      const el = composerEl;
      const collapsed = !el || el.selectionStart === el.selectionEnd;
      if (collapsed) {
        const caret = el?.selectionStart ?? input.length;
        const firstLine = !input.slice(0, caret).includes("\n");
        const lastLine = !input.slice(caret).includes("\n");
        const up = e.key === "ArrowUp";
        const browsing = historyIndex != null;
        const canRecall = up && (browsing ? firstLine : input.trim() === "" || firstLine);
        const canForward = !up && browsing && lastLine;
        if ((canRecall || canForward) && historyStep(up ? -1 : 1)) {
          e.preventDefault();
          return;
        }
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

  function noteImagesStripped() {
    error = `Note: ${selectedModel} doesn't support images, so the attached picture was not sent.`;
  }

  const isWindows = typeof navigator !== "undefined" && /win/i.test(navigator.userAgent ?? "");

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

  async function ensureScratchWorkspace(sessionId: string): Promise<string> {
    return await invokeOrBridge<string>("work_ensure_scratch_workspace", { sessionId });
  }

  let wsMenuOpen = $state(false);

  /** Switch to No Workspace mode: uses an empty per-session scratch folder
   *  under ~/vicuna/scratch/<session-id> so Work engines run without touching
   *  any existing repository. */
  async function selectNoWorkspace() {
    wsMenuOpen = false;
    workspaceError = null;
    const workspaceChat = activeChat;
    if (composerMode === "work" && workspaceChat && workspaceChat.messages.length > 0) {
      try {
        const scratch = await ensureScratchWorkspace(workspaceChat.id);
        workspaceChat.workspace = scratch;
        await persistSessionWorkspace(workspaceChat, scratch);
      } catch (e: any) {
        workspaceError = typeof e === "string" ? e : (e?.message ?? String(e));
        error = workspaceError;
        return;
      }
    } else if (workspaceChat && workspaceChat.messages.length === 0) {
      workspaceChat.mode = "work";
      workspaceChat.workspace = NO_WORKSPACE_SENTINEL;
    }
    pendingMode = "work";
    pendingWorkspace = NO_WORKSPACE_SENTINEL;
    syncScopeToWorkspace(NO_WORKSPACE_SENTINEL);
    persistPrefs();
  }

  /** Workspace pill (composer bar, work contexts): native folder picker on
   *  desktop; inline popup form in plain browsers (no FS access for a
   *  dialog, and no window.prompt — it auto-dismisses headlessly). */
  async function pickWorkspace() {
    const rawHint = (composerMode === "work" ? (activeChat?.workspace ?? pendingWorkspace) : pendingWorkspace).trim();
    const pickerHint = isNoWorkspacePath(rawHint) ? (isNoWorkspacePath(defaultWorkspace) ? "" : defaultWorkspace.trim()) : rawHint;
    if (!isTauriRuntime() || onServer) {
      if (await canBrowseFolders()) {
        openFolderPicker(pickerHint, "composer");
        return;
      }
      const el = document.querySelector<HTMLInputElement>("[data-testid=workspace-input-active]");
      if (el) {
        el.focus();
        el.scrollIntoView({ block: "nearest" });
        return;
      }
      workspaceDraft = pickerHint;
      workspaceError = null;
      workspacePickerOpen = true;
      return;
    }
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      // "/" is the Unix fresh-install default, not a valid Windows path —
      // never hand it to the native dialog as a default folder.
      const hint = pickerHint || (isNoWorkspacePath(defaultWorkspace) ? "" : defaultWorkspace.trim());
      const picked = await open({
        directory: true,
        multiple: false,
        defaultPath: hint && !(isWindows && hint === "/") ? hint : undefined
      });
      if (typeof picked !== "string" || !picked) return; // cancelled
      const workspaceChat = activeChat;
      pendingWorkspace = picked;
      if (composerMode === "work" && activeChat) activeChat.workspace = picked;
      workspaceError = await validateWorkspace(picked);
      if (!workspaceError) {
        await persistSessionWorkspace(workspaceChat, picked);
        defaultWorkspace = picked;
        syncScopeToWorkspace(picked);
        persistPrefs();
      }
    } catch (e: any) {
      workspaceError = typeof e === "string" ? e : (e?.message ?? String(e));
    }
  }

  // ---- in-app folder picker (FolderPickerModal): remote servers + bridged
  // browsers, where the native dialog can't browse the backend's folders
  let folderPicker = $state<{ start: string; target: "composer" | "continue" } | null>(null);
  let folderPickError = $state<string | null>(null);

  async function canBrowseFolders(): Promise<boolean> {
    if (onServer) return true;
    return !isTauriRuntime() && (await bridgeAvailable());
  }

  function openFolderPicker(start: string, target: "composer" | "continue") {
    folderPickError = null;
    workspaceError = null;
    folderPicker = { start, target };
  }

  /** Same validate + assign flow as the typed-path popups; the modal stays
   *  open with the error when the folder isn't a usable workspace. */
  async function onFolderPicked(path: string) {
    const target = folderPicker?.target;
    // validate BEFORE assigning: the commit helpers set the pending
    // workspace first, so a rejected pick (e.g. "/") would otherwise stick
    // to the composer even after the modal is cancelled
    const err = await validateWorkspace(path);
    if (err) {
      folderPickError = err;
      return;
    }
    workspaceDraft = path;
    if (target === "continue") await commitContinueWorkspace();
    else await commitWorkspacePicker();
    workspacePickerOpen = false;
    if (workspaceError) {
      folderPickError = workspaceError;
      return;
    }
    folderPicker = null;
  }

  /** Discard the browser workspace-picker draft (Esc always cancels). */
  function closeWorkspacePicker() {
    workspacePickerOpen = false;
  }

  /** Backdrop click: commit a typed path, discard when empty. Split from
   *  Esc (which cancels outright) so clicking Send right after typing
   *  keeps the path instead of dropping it. */
  function dismissWorkspacePicker() {
    void commitWorkspacePicker();
  }

  /** Commit the browser workspace-picker draft: same assignment + validate
   *  + persist-default flow as the desktop picker (empty draft = cancel).
   *  The popup stays open on validation failure so the error is visible
   *  next to the path it rejects. */
  async function persistSessionWorkspace(chat: Chat | undefined | null, workspace: string) {
    if (!serverSessionTools || chat?.mode !== "work" || serverSaveRow(chat)?.baseRev == null) return;
    await serverAction("session_update", { id: chat.id, workspace });
  }

  async function commitWorkspacePicker() {
    const picked = workspaceDraft.trim();
    if (!picked) {
      workspacePickerOpen = false;
      return;
    }
    const workspaceChat = activeChat;
    pendingWorkspace = picked;
    if (composerMode === "work" && activeChat) activeChat.workspace = picked || null;
    workspaceError = await validateWorkspace(picked);
    if (!workspaceError) {
      await persistSessionWorkspace(workspaceChat, picked);
      defaultWorkspace = picked;
      syncScopeToWorkspace(picked);
      persistPrefs();
      workspacePickerOpen = false;
    }
  }

  /** Work-mode agent loop, owned by the Rust backend (work_agent.rs).
   *  Each round streams narration text into the live assistant message and
   *  returns `tool_calls`; the backend executes them, and the frontend
   *  mirrors the semantic event stream below into the bubble (collapsed
   *  markdown results for persistence, ask cards, steering bubbles) exactly
   *  like the old in-thread loop did. Settle paths (leftovers, follow-up
   *  turns and stats) are unchanged. */
  /** Follow Work turns the server is still running for sessions this window
   *  shows (the window that started them closed or reloaded). Without this
   *  the turn ran on unseen, its output was never saved, and the next send
   *  started a second engine process on the same session. */
  let reattaching = false;
  async function reattachLiveWorkTurns() {
    if (serverPersist) {
      // one pass at a time (the 5 s tick also calls this)
      if (reattaching) return;
      reattaching = true;
      try {
        await reattachRecordedTurns();
      } finally {
        reattaching = false;
      }
      return;
    }
    const live = await liveWorkTurns();
    for (const [id, t] of Object.entries(live)) {
      const c = chats.find((x) => x.id === id);
      if (!c || c.mode !== "work" || workTurnIds.has(id) || workRunningIds.includes(id)) continue;
      // its live bubble is the trailing assistant message; a user message
      // last means the placeholder never got saved, so add it back
      let idx = c.messages.length - 1;
      if (idx < 0) continue;
      if (c.messages[idx].role !== "assistant") {
        c.messages = [...c.messages, { role: "assistant", content: "" }];
        idx = c.messages.length - 1;
        chats = [...chats];
      }
      void runWorkTurn(c, idx, c.workspace ?? "", [], { attach: t });
    }
  }

  /** Server mode: follow every turn the server is running for this
   *  window's sessions, from the server's own recording (the transcript in
   *  the snapshot already holds every event up to now, so nothing doubles).
   *  Sessions the server created on its own (a scheduled send into a new
   *  session) are fetched first. */
  async function reattachRecordedTurns() {
    // which turns are live comes first and is cheap: those sessions show as
    // running right away, before the list refresh and the attach
    const livePromise = liveWorkTurns();
    const chatTurns = serverChatTurns ? reattachServerChatTurns() : Promise.resolve();
    const live = await livePromise;
    liveHintIds = Object.keys(live).filter((id) => !workRunningIds.includes(id));
    try {
      await serverSavesIdle();
      const rows = await loadServerChatList().catch(() => []);
      for (const raw of rows) {
        const local = chats.find((c) => c.id === raw.id);
        if (workRunningIds.includes(raw.id) || serverChatFeeds.has(raw.id)) continue;
        const base = local ? serverSaveRow(local)?.baseRev : null;
        if (!local || (typeof raw.rev === "number" && typeof base === "number" && raw.rev > base)) {
          if (local && !local.partial && raw.partial) {
            // a full copy here stays full: fetch the new revision whole
            const full = await invokeOrBridge<any>("chats_get", { id: raw.id }).catch(() => null);
            if (!full || workRunningIds.includes(raw.id) || serverChatFeeds.has(raw.id)) continue;
            const fresh = adoptServerChat(full);
            withPreservedScroll(fresh.id, () => {
              chats = chats.map((c) => c.id === fresh.id ? fresh : c);
            });
          } else {
            const fresh = adoptServerChat(raw);
            if (local) {
              withPreservedScroll(fresh.id, () => {
                chats = chats.map((c) => c.id === fresh.id ? fresh : c);
              });
            } else {
              chats = [fresh, ...chats];
            }
          }
        }
      }
      await chatTurns;
      await attachLiveWorkTurns(live);
    } finally {
      liveHintIds = [];
    }
  }

  async function attachLiveWorkTurns(live: Awaited<ReturnType<typeof liveWorkTurns>>) {
    for (const id of Object.keys(live)) {
      if (workTurnIds.has(id) || workRunningIds.includes(id)) continue;
      if (!chats.some((x) => x.id === id)) {
        const raw = await invokeOrBridge<any>("chats_get", { id }).catch(() => null);
        if (!raw || chats.some((x) => x.id === id)) continue;
        chats = [adoptServerChat(raw), ...chats];
      }
      const snap = await attachWorkAgentSnapshot(id);
      const c = chats.find((x) => x.id === id);
      if (!snap?.snapshot || !c || c.mode !== "work" || workTurnIds.has(id) || workRunningIds.includes(id)) continue;
      await ensureChatFull(c);
      if (workTurnIds.has(id) || workRunningIds.includes(id)) continue;
      c.messages = normalizeServerChat({ id, messages: snap.snapshot.messages }).messages;
      chats = [...chats];
      void runWorkTurn(c, snap.snapshot.index, c.workspace ?? "", [], { attach: snap });
    }
  }

  /** Server mode: swap a list copy (`partial`, message previews only) for
   *  the full session, in place so callers holding the object keep it.
   *  Messages added locally past the server's copy are kept. */
  const fullLoads = new Map<string, Promise<void>>();
  let hoverPrefetchTimer: ReturnType<typeof setTimeout> | null = null;
  let warmCancelled = false;
  let warmStarted = false;
  const WARM_RECENT_COUNT = 12;

  function ensureChatFull(chat: Chat | null | undefined, opts: { silent?: boolean } = {}): Promise<void> {
    if (!chat?.partial) return Promise.resolve();
    const id = chat.id;
    let p = fullLoads.get(id);
    if (!p) {
      p = (async () => {
        const raw = await invokeOrBridge<any>("chats_get", { id }).catch(() => null);
        const local = chats.find((x) => x.id === id);
        if (!local?.partial) return;
        if (!raw || typeof raw.id !== "string") {
          if (!opts.silent) error = "Couldn't load this session from the server.";
          return;
        }
        const fresh = adoptServerChat(raw);
        const messages = [...fresh.messages, ...local.messages.slice(fresh.messages.length)];
        Object.assign(local, fresh, { messages });
        delete local.partial;
        chats = [...chats];
      })().finally(() => fullLoads.delete(id));
      fullLoads.set(id, p);
    }
    return p;
  }

  function scheduleHoverPrefetch(chat: Chat) {
    if (!chat.partial) return;
    if (hoverPrefetchTimer) clearTimeout(hoverPrefetchTimer);
    hoverPrefetchTimer = setTimeout(() => {
      hoverPrefetchTimer = null;
      if (chat.partial) void ensureChatFull(chat, { silent: true });
    }, 80);
  }

  function cancelHoverPrefetch() {
    if (hoverPrefetchTimer) {
      clearTimeout(hoverPrefetchTimer);
      hoverPrefetchTimer = null;
    }
  }

  async function warmRecentPartialChats() {
    if (!isServerBacked() || warmStarted || warmCancelled) return;
    warmStarted = true;
    // Wait for the active session (if any) to hydrate first so user-visible
    // content always gets first priority on the connection.
    if (activeChat?.partial) await ensureChatFull(activeChat);
    const targets = chats
      .filter((c) => c.partial && !c.archived)
      .slice(0, WARM_RECENT_COUNT)
      .map((c) => c.id);
    for (const id of targets) {
      if (warmCancelled) return;
      // Pause briefly if an explicit user load is in flight.
      while (fullLoads.size > 0 && !warmCancelled) {
        await new Promise((r) => setTimeout(r, 80));
      }
      if (warmCancelled) return;
      const c = chats.find((x) => x.id === id);
      if (c?.partial) {
        await ensureChatFull(c, { silent: true });
        await new Promise((r) => setTimeout(r, 40));
      }
    }
  }

  $effect(() => {
    if (ready && activeChat?.partial) void ensureChatFull(activeChat);
  });
  $effect(() => {
    if (ready && isServerBacked() && !warmStarted) void warmRecentPartialChats();
  });

  /** Server mode: take the server's transcript of a session (after its turn
   *  ended, or when a save was refused). Local metadata (title, pins) stays;
   *  if it differs, the next save sends it on top of the adopted revision. */
  async function refreshServerChat(id: string) {
    const raw = await invokeOrBridge<any>("chats_get", { id }).catch(() => null);
    if (!raw) return;
    const local = chats.find((x) => x.id === id);
    const fresh = adoptServerChat(raw);
    if (!local) return;
    withPreservedScroll(id, () => {
      if (serverSessionTools) Object.assign(local, fresh);
      else {
        local.messages = fresh.messages;
        if ((fresh.updatedAt ?? 0) > (local.updatedAt ?? 0)) local.updatedAt = fresh.updatedAt;
      }
      chats = [...chats];
    });
    save();
  }

  async function runWorkTurn(chat: Chat, index: number, workspace: string, seedContext: ChatMsg[] = [], opts: { freshSession?: boolean; retried?: boolean; attach?: { turnId: string; channel: string; startedMs?: number; snapshot?: { messages: any[]; index: number; pendingHead: { name: string; args: Record<string, any> } | null; pendingAsk: WorkAgentEvent | null; pendingAskRecords?: string[] } } } = {}) {
    // reply label: the opencode model this turn runs on (refined by the
    // backend's "model" event), never the Chat-mode model
    // a session pinned to its own harness (lead-created workers) runs on it;
    // otherwise the global pick
    const own = chats.find((x) => x.id === chat.id) ?? chat;
    const turnEngine = own.engine || engineId(workEngineInfo);
    const turnPick: string | null = own.engine ? (own.workModel ?? null) : workModel;
    const priorMessages = own.messages.slice(0, index);
    const replyAt = priorMessages.findLastIndex(isAssistantReply);
    const clearAt = priorMessages.findLastIndex((m) => m.contextCleared);
    const freshSession = opts.freshSession || clearAt > replyAt || changesCliAccount(turnEngine, priorMessages[replyAt]?.model);
    // first turn of a session without its own harness: pin the last-used pick
    // so the session never silently changes agent later
    if (!own.engine) {
      own.engine = turnEngine;
      own.workModel = turnPick;
    }
    let turnModel = workReplyModel(null, turnPick, turnEngine);
    const turnWorkspace = workspace.trim() || undefined;
    {
      const c0 = chats.find((x) => x.id === chat.id);
      const m0 = c0?.messages[index];
      // a server snapshot already carries the label the server recorded
      if (c0 && m0 && m0.role === "assistant" && !opts.attach?.snapshot) {
        c0.messages[index] = { ...m0, model: turnModel, ...(turnWorkspace ? { workspace: turnWorkspace } : {}) };
        chats = [...chats];
      }
    }
    const turnId = ++workTurnSeq;
    workTurnSeqByChat.set(chat.id, turnId);
    // a turn is live while it is still its session's newest: late events
    // from a superseded turn (or a deleted session) are dropped
    const isCurrentTurn = () => workTurnSeqByChat.get(chat.id) === turnId;
    // view updates belong to the open session: a background turn updates
    // state but never scrolls the thread being read
    const scrollIfOpen = () => {
      if (chat.id === activeId) scrollBottom();
    };
    // No Tauri gate here: plain browsers drive the same flow through the
    // loopback bridge (startWorkAgentTurn throws the desktop/bridge error
    // when neither is present, and the catch below surfaces it).
    markWorkRunning(chat.id, opts.attach?.startedMs);
    workStopIds.delete(chat.id);
    // a new turn invalidates /redo (backend clears its stack in undo_begin_turn)
    if (!opts.attach) dropUndoStash(chat.id);
    let curIndex = index;
    // in-flight tool head (for say/ask/done display) + the ask answer the
    // frontend itself sent (the record mirrors it once the result lands)
    let pendingHead: { name: string; args: Record<string, any> } | null = null;
    // ask-log records answered while a tool-result <details> card is still open
    // (the ask_user tool call itself): flushed right after </details> on toolend
    let pendingAskRecords: string[] = [];
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
      const isOpen = chat.id === activeId;
      const keep = isOpen ? captureStreamSel(curIndex) : null;
      withPreservedScroll(chat.id, () => {
        const c = chats.find((x) => x.id === chat.id);
        if (c && c.messages[curIndex]) {
          const m = c.messages[curIndex];
          c.messages[curIndex] = { role: "assistant", content: m.content + tok, thinking: (m.thinking ?? "") + th, thinkingOpen: m.thinkingOpen, model: turnModel, ...(m.workspace || turnWorkspace ? { workspace: m.workspace ?? turnWorkspace } : {}), genMs: m.genMs, genTps: m.genTps, genCost: m.genCost };
          chats = [...chats];
        }
      });
      if (keep) void tick().then(() => restoreStreamSel(curIndex, keep));
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
      if (toolStatuses[chat.id] === "Preparing tool call…") setToolStatus(chat.id, null);
    }
    function armStall() {
      clearStall();
      stallTimer = setTimeout(() => {
        stallTimer = null;
        if (!pendingTok && !pendingTh && !toolStatuses[chat.id]) {
          setToolStatus(chat.id, "Preparing tool call…");
        }
      }, 1000);
    }
    const live = () => chats.find((x) => x.id === chat.id);
    // buffered tokens land before anything appended out of band (a tool
    // card head, a note), or text streamed just before a tool call ends up
    // inside that call's card
    function flushNow() {
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      flushTokens();
    }
    function appendContent(s: string) {
      flushNow();
      withPreservedScroll(chat.id, () => {
        const c = live();
        if (!c?.messages[curIndex]) return;
        const m = c.messages[curIndex];
        c.messages[curIndex] = { ...m, content: m.content + s };
        chats = [...chats];
      });
    }
    // native history: proper tool roles for the model; the display message
    // holds narration + markdown results for persistence. seedContext carries
    // hidden context (e.g. a continued chat transcript) never shown here.
    const baseHistory = compactedHistory(live()?.messages.slice(0, index) ?? []);
    let seed = clearAt >= 0 ? [] : seedContext;
    if (clearAt < 0 && !seed.length && chat.forkedFrom && chat.forkFromIndex == null) {
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
    // visible history: opencode keeps its own session context; a fresh
    // session (fork/restart/regenerate) gets this as a preamble backend-side.
    // opencode brings its own tools; enabled MCP servers are handed to it in
    // session/new by the backend.
    const native: ChatMsg[] = [...seed, ...baseHistory];
    const think = effectiveThink();

    // The channel can deliver before start returns its id. Keep its origin
    // independent of the mutable per-chat map (settle/replacement clears it).
    const replyRoute = createTurnReplyRoute();
    const replyToTurn = (askId: string, answer: string, record?: { display: string; dismissed: boolean }, question?: string) => {
      void replyRoute.send(askId, answer, answerWorkAgentAsk, record).catch((e) => {
        // the engine gave up on the question (agy times MCP calls out) and the
        // turn has since ended: deliver the answer as a normal message
        if (question && record && !record.dismissed && /stopped waiting for this answer/.test(String(e))) {
          let text = record.display;
          let images: string[] = [];
          try {
            const env = JSON.parse(answer)?.vicunaAskAnswer;
            if (env) { text = env.text ?? text; images = env.images ?? []; }
          } catch { /* plain-text answer */ }
          const m = laterAnswerMessage(question, { text, images, multi: false, picked: [] });
          if (m) {
            const msg: ChatMsg = { role: "user", content: m.text, ...(m.images.length ? { images: m.images } : {}) };
            void sendToWorkSession(chat.id, msg)
              .then((sent) => { if (!sent) error = "The agent stopped waiting for your answer; the session is busy, so send it again in a moment."; })
              .catch((err) => { error = `Answer not sent: ${String(err)}`; });
            return;
          }
        }
        console.error("Work reply delivery failed", { chatId: chat.id, askId, error: e });
        error = `Could not deliver the Work reply: ${String(e)}. The action may have run; check before retrying.`;
      });
    };

    async function settle(stopped: boolean) {
      if (flushTimer) {
        clearTimeout(flushTimer);
        flushTimer = null;
      }
      flushTokens();
      clearStall();
      dropStallHint();
      // the turn is over: a card still up (the engine dropped the call) can
      // no longer be answered
      if (isCurrentTurn() && pendingAsks[chat.id]) cancelPendingAsk(chat.id, "(the turn ended before an answer)");
      if (!unloading) clearPendingStreamFor(chat.id);
      // server mode: the server wrote the whole turn (and its final copy is
      // in before this terminal event was released); adopt it
      if (serverPersist && !unloading) await refreshServerChat(chat.id);
      save();
      if (escTimer) {
        clearTimeout(escTimer);
        escTimer = null;
      }
      escArmed = false;
      // prompts that arrived after the last round: continue the agent with them
      // (an aborted turn returns them to the composer instead)
      // "Send now" stops the turn too, but its queue must run, not return
      const sendNow = sendNowIds.delete(chat.id);
      const aborted = !sendNow && (workStopIds.has(chat.id) || stopped);
      const leftovers = isCurrentTurn() ? takeQueued(chat.id) : [];
      if (leftovers.length && serverPersist && !aborted && !sendNow) {
        // the server already started the next turn with them (it holds the
        // same queue); follow it instead of starting a second one
        setTimeout(() => void reattachLiveWorkTurns(), 300);
      } else if (leftovers.length) {
        if (aborted) {
          if (activeId === chat.id) {
            input = [input.trim(), ...leftovers.map((q) => q.content.trim())].filter(Boolean).join("\n\n");
            attachImages = [...attachImages, ...leftovers.flatMap((q) => q.images ?? [])];
          }
        } else {
          const c = live();
          if (c) {
            c.updatedAt = Date.now();
            for (const q of leftovers) {
              c.messages = [...c.messages, q, { role: "assistant", content: "", thinking: "", model: turnModel }];
            }
            chats = [c, ...chats.filter((x) => x.id !== c.id)];
            save();
            // hand off to a fresh turn; the inner call clears this
            // session's running mark when it settles
            await runWorkTurn(chat, c.messages.length - 1, workspace);
            return;
          }
        }
      }
      // natural end or abort: no step cap — the backend loop already exits
      // when the model makes no calls, repeats finished work, or stops.
      // Only the current turn clears its session (a superseded turn's late
      // settle must never wipe the newer turn's state).
      if (isCurrentTurn()) {
        const userStopped = workStopIds.has(chat.id);
        void notifyTurnEnd({ title: live()?.title, mode: "work", end: "done", reply: lastReply(chat.id), userStopped });
        unmarkWorkRunning(chat.id);
        // server mode: no longer server-owned, so local edits made during the
        // turn (an auto-title) can go up on top of the adopted revision
        if (serverPersist) save();
        workStopIds.delete(chat.id);
        workTurnSeqByChat.delete(chat.id);
        workTurnIds.delete(chat.id);
        workForceSettle.delete(chat.id);
        reportToLead(chat.id, userStopped);
        const wakeServers = pendingMcpWakes.get(chat.id);
        pendingMcpWakes.delete(chat.id);
        if (wakeServers?.length && !userStopped) {
          void sendToWorkSession(chat.id, {
            role: "user",
            content: mcpWakeMessage(wakeServers),
          });
        }
      }
    }

    // stop reconciliation: settles this turn when the backend says it is
    // over but its `done` never arrived (a late `done` is then dropped by
    // handleEvent's isCurrentTurn guard, so it can't settle twice)
    workForceSettle.set(chat.id, () => {
      if (isCurrentTurn()) void settle(true);
    });

    function handleEvent(ev: WorkAgentEvent) {
      if (!isCurrentTurn()) return;
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
          if (ev.label) setToolStatus(chat.id, ev.label);
          else setToolStatus(chat.id, null);
          break;
        case "roundstart":
          // a generation round is in flight: persist the resume marker (same
          // shape the old per-round streamInto wrote) so a refresh resumes it
          // `work` keeps initApp from "resuming" it through the tool-less Chat
          // path with the Chat model (that once replaced opus's real reply)
          writePendingStream({ chatId: chat.id, index: curIndex, mode: ev.mode as "fresh" | "continue", model: selectedModel, think, work: true });
          save();
          armStall();
          break;
        case "model": {
          turnModel = workReplyModel(ev.model, turnPick, turnEngine);
          const t = live();
          const m = t?.messages[curIndex];
          if (t && m && m.role === "assistant" && m.model !== turnModel) {
            t.messages[curIndex] = { ...m, model: turnModel };
            chats = [...chats];
            saveSoon();
          }
          break;
        }
        case "roundend": {
          clearStall();
          dropStallHint();
          if (!unloading) clearPendingStreamFor(chat.id);
          // per-round stats accumulate onto the live bubble (same math the
          // old streamInto settle used: wall time always, server eval rate
          // when present, eval tokens over wall time for cloud APIs)
          const t = live();
          const m = t?.messages[curIndex];
          if (t && m && m.role === "assistant") {
            const prevCost = m.genCost ?? 0;
            const addCost = ev.gen_cost ?? 0;
            const nextCost = prevCost + addCost > 0 ? prevCost + addCost : undefined;
            t.messages[curIndex] = { ...m, genMs: (m.genMs ?? 0) + ev.gen_ms, genTps: ev.gen_tps ?? m.genTps, ...(nextCost !== undefined ? { genCost: nextCost } : {}) };
            chats = [...chats];
          }
          save();
          break;
        }
        case "displaynote":
          appendContent(formatDisplayNote(ev.text, !!pendingHead && pendingHead.name !== "ask" && pendingHead.name !== "say"));
          scrollIfOpen();
          saveSoon();
          break;
        case "queuedrained": {
          // steering prompts the backend just consumed: render each as a
          // user bubble followed by a fresh assistant bubble, exactly like
          // the old loop did at the top of the next round
          confirmQueuedDrained(ev.ids);
          const t = live();
          if (t) {
            t.updatedAt = Date.now();
            for (const q of ev.messages) {
              t.messages = [
                ...t.messages,
                { role: "user", content: q.content, ...(q.images ? { images: q.images } : {}) },
                { role: "assistant", content: "", thinking: "", model: turnModel, ...(turnWorkspace ? { workspace: turnWorkspace } : {}) }
              ];
            }
            chats = [t, ...chats.filter((x) => x.id !== t.id)];
            curIndex = t.messages.length - 1;
            scrollIfOpen();
            saveSoon();
          }
          break;
        }
        case "skillschanged": {
          const target = chats.find((item) => item.id === ev.session_id);
          if (target && ev.enabled) {
            target.skillsEnabled = ev.enabled;
            if (!serverPersist) save();
          }
          if (ev.removed) forgetSkillSwitch(ev.removed);
          void refreshSkills();
          break;
        }
        case "mcpchanged": {
          const current = live();
          if (current && ev.enabled) {
            current.mcpEnabled = ev.enabled;
            if (!serverPersist) save();
          }
          if (ev.wake && !serverPersist) {
            const [name, tools] = ev.wake;
            const list = pendingMcpWakes.get(chat.id) ?? [];
            if (!list.some((item) => item.name === name)) list.push({ name, tools });
            pendingMcpWakes.set(chat.id, list);
          }
          void syncMcpServersAfterTurn();
          break;
        }
        case "toolstart":
          clearStall();
          dropStallHint();
          pendingHead = { name: ev.name, args: ev.args };
          // `ask` (live card) and `say` (normal prose) own their display
          if (ev.name === "ask" || ev.name === "say") break;
          appendContent(formatToolCallHead(ev.name, ev.args));
          scrollIfOpen();
          break;
        case "toolend": {
          flushNow();
          const head = pendingHead;
          pendingHead = null;
          withPreservedScroll(chat.id, () => {
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
                const askTail = pendingAskRecords.length ? pendingAskRecords.splice(0).join("") : "";
                t.messages[curIndex] = { ...m, content: m.content + formatToolCallTail(ev.output) + askTail };
                chats = [...chats];
              }
            }
          });
          saveSoon();
          break;
        }
        case "bgcompleted": {
          withPreservedScroll(chat.id, () => {
            const t = live();
            if (t?.messages[curIndex]) {
              const m = t.messages[curIndex];
              t.messages[curIndex] = {
                ...m,
                content: m.content + formatToolResult("exec", { command: `${ev.command} (${ev.job_id})` }, ev.output)
              };
              chats = [...chats];
            }
          });
          saveSoon();
          break;
        }
        case "contextcompacted":
          appendContent(`\n${ev.note}\n`);
          scrollIfOpen();
          saveSoon();
          break;
        case "schedulewakeup": {
          // vicuna-mcp schedule_wakeup: resolve in the user's timezone, add a
          // Scheduled send for this session (same delivery, list, tray and
          // cancel as one the user scheduled), answer the waiting tool call
          const r = resolveWakeTime({ at: ev.at, inMinutes: ev.in_minutes }, Date.now());
          let reply: { ok: boolean; when?: string; error?: string };
          if ("error" in r) {
            reply = { ok: false, error: r.error };
          } else {
            const c = chats.find((x) => x.id === chat.id);
            scheduled = [
              ...scheduled,
              {
                id: crypto.randomUUID(),
                chatId: chat.id,
                title: c?.title ?? chat.title,
                workspace,
                text: wakeMessage(ev.message),
                at: r.at,
                createdAt: Date.now()
              }
            ];
            persistScheduled();
            const tz = Intl.DateTimeFormat().resolvedOptions().timeZone;
            const abs = new Date(r.at).toLocaleString(undefined, { weekday: "short", month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
            const mins = Math.round((r.at - Date.now()) / 60000);
            reply = { ok: true, when: `${abs} ${tz} (in ${mins >= 120 ? `${Math.round(mins / 60)} h` : `${mins} min`})` };
          }
          replyToTurn(ev.req_id, JSON.stringify(reply));
          break;
        }
        case "sessioncontrol": {
          void handleSessionControl(chat.id, workspace, ev.action, ev.args ?? {})
            .catch((e: any) => ({ ok: false, error: typeof e === "string" ? e : (e?.message ?? String(e)) }))
            .then((reply) => {
              replyToTurn(ev.req_id, JSON.stringify(reply));
            });
          break;
        }
        case "askrequest":
          // the agent is now blocked on the user: if the window is hidden or
          // in the background, chime + OS notification so they come back
          void notifyAgentAsk(ev.question);
          askOutcome = null;
          askText = "";
          pendingAsks = {
            ...pendingAsks,
            [chat.id]: {
              chatId: chat.id,
              index: curIndex,
              question: ev.question,
              options: ev.options,
              multi: !!ev.multi,
              picked: [],
              agentQuestion: isAgentQuestion(ev.ask_id),
              text: "",
              images: [],
              otherOpen: false,
              resolve: (answer, outcome, display) => {
                // cancel (Stop / chat deleted) needs no backend call: the stop
                // path already unblocked the waiting tool with no record
                if (outcome === "cancel") return;
                if (outcome === "later") {
                  // the card lives on in laterAsks; the tool call returns now
                  replyToTurn(ev.ask_id, ASK_LATER);
                  return;
                }
                askOutcome = outcome;
                askText = answer;
                // vicuna-mcp questions: record the Q/A in the transcript here
                // (permission asks get the backend's Approved/Denied note). When
                // a tool-result <details> card is open (the ask_user call
                // itself), defer the record until toolend closes </details>,
                // otherwise it lands inside the collapsed card and disappears.
                const t = live();
                if (isAgentQuestion(ev.ask_id) && t?.messages[curIndex]) {
                  const rec = formatAskRecord(ev.question, display ?? answer, outcome === "dismiss");
                  const inCard = !!pendingHead && pendingHead.name !== "ask" && pendingHead.name !== "say";
                  if (inCard) {
                    pendingAskRecords.push(rec);
                  } else {
                    withPreservedScroll(chat.id, () => {
                      const m = t.messages[curIndex];
                      t.messages[curIndex] = { ...m, content: m.content + rec };
                      chats = [...chats];
                    });
                    saveSoon();
                  }
                }
                // a server recording the turn writes the same record
                replyToTurn(ev.ask_id, answer, isAgentQuestion(ev.ask_id) ? { display: display ?? answer, dismissed: outcome === "dismiss" } : undefined, ev.question);
                scrollIfOpen();
              }
            }
          };
          requestAnimationFrame(() => scrollIfOpen());
          break;
        case "done":
          setToolStatus(chat.id, null);
          void settle(ev.stopped);
          break;
        case "error":
          void warnIfOllamaUnavailable(workModel?.startsWith("ollama/") ?? false).then((warned) => {
            if (!warned) error = ev.message;
          });
          setToolStatus(chat.id, null);
          void settle(false);
          break;
      }
    }

    let agentTurn: string;
    if (opts.attach) {
      // the server kept this turn running while no window followed it: keep
      // polling its channel (the queue held every event since then — or, from
      // a server snapshot, every event the snapshot doesn't already hold)
      const snap = opts.attach.snapshot;
      if (snap) {
        curIndex = snap.index;
        pendingHead = snap.pendingHead;
        pendingAskRecords = [...(snap.pendingAskRecords ?? [])];
      }
      replyRoute.bind(opts.attach.turnId);
      attachWorkAgentTurn(opts.attach.channel, (ev) => handleEvent(ev));
      workTurnIds.set(chat.id, opts.attach.turnId);
      // the agent was already waiting on an ask card when this window came
      if (snap?.pendingAsk) handleEvent(snap.pendingAsk);
      return;
    }
    // server mode: hand the server the session as it stands now (it writes it
    // and records the turn) — after queued saves land, so its revision is
    // current. A worker report owed to a lead becomes the server's job too.
    let persist: { chat: Record<string, unknown>; index: number } | null = null;
    let reportTo: string | null = null;
    if (serverPersist) {
      await serverSavesIdle();
      const row = serverSaveRow(live() ?? chat);
      if (row) persist = { chat: row, index: curIndex };
      const lead = live()?.leadId;
      if (reportIds.has(chat.id) && lead) {
        reportIds.delete(chat.id);
        reportTo = lead;
      }
    }
    try {
      agentTurn = await startWorkAgentTurn({
        persist,
        reportTo,
        engine: turnEngine,
        model: turnPick ?? "",
        lead: own.role === "lead",
        mcpEnabled: (live() ?? chat).mcpEnabled,
        skillsEnabled: (live() ?? chat).skillsEnabled,
        mcpOnly: (live() ?? chat).mcpOnly,
        freshSession,
        workspace,
        messages: native,
        sessionId: chat.id,
        onEvent: (ev) => handleEvent(ev)
      });
    } catch (e: any) {
      replyRoute.bind(null);
      // the server has a newer copy than this window (a turn it recorded, or
      // another window's save): take its transcript, put this message back on
      // top and start once more — never drop what the user just sent
      const msg = typeof e === "string" ? e : (e?.message ?? String(e));
      const c = live();
      const sent = c?.messages[index - 1];
      if (persist && !opts.retried && msg.startsWith("SESSION_CHANGED") && c && sent?.role === "user" && isCurrentTurn()) {
        const raw = await invokeOrBridge<any>("chats_get", { id: chat.id }).catch(() => null);
        if (raw && isCurrentTurn()) {
          const fresh = adoptServerChat(raw);
          // the server's copy wins whole (title, pins, …: a rename made
          // elsewhere must not be overwritten by this window's stale row)
          Object.assign(c, fresh, { messages: [...fresh.messages, sent, { role: "assistant", content: "", thinking: "", model: turnModel }] });
          chats = [...chats];
          if (reportTo) reportIds.add(chat.id);
          await runWorkTurn(chat, c.messages.length - 1, workspace, seedContext, { ...opts, retried: true });
          return;
        }
      }
      if (!await warnIfOllamaUnavailable(workModel?.startsWith("ollama/") ?? false)) {
        error = typeof e === "string" ? e : (e?.message ?? String(e));
      }
      if (reportTo) reportIds.add(chat.id);
      await settle(true);
      return;
    }
    replyRoute.bind(agentTurn);
    if (isCurrentTurn()) {
      workTurnIds.set(chat.id, agentTurn);
    } else {
      // the session went away (deleted/stopped) while the turn was starting:
      // stop the orphan so it doesn't run detached with no UI
      void stopWorkAgentTurn(agentTurn).catch(() => {});
    }
  }

  /** "Continue in Work": fork a chat transcript into a new work session rooted
   *  at the default (last-used) workspace. The thread shows only
   *  "Continue from chat [link]" followed by the assistant reply — the source
   *  transcript travels as hidden seed context, never as a visible bubble. */
  async function continueInWork(id: string) {
    const src = chats.find((c) => c.id === id);
    // only the source session's own live turn blocks the fork — and forking
    // never aborts anything (the source keeps running in the background)
    if (!src || isSessionBusy(id)) return;
    await ensureChatFull(src);
    if (src.partial) return;
    ctxMenu = null;
    workspacePickerOpen = false;
    const ws = continueWorkspace;
    const seedContext: ChatMsg[] = [
      ...compactedHistory(src.messages.slice(-20)),
      { role: "user", content: ws ? `Continue the above in the workspace (${ws}).` : "Continue the above in a workspace." }
    ];
    const fork: Chat = {
      id: crypto.randomUUID(),
      title: `${src.title} (work)`.slice(0, 40),
      createdAt: Date.now(),
      updatedAt: Date.now(),
      messages: [
        { role: "assistant", content: "", thinking: "", model: selectedModel }
      ],
      mode: "work",
      mcpEnabled: await newMcpEnabled(),
      skillsEnabled: await newSessionSkills(),
      workspace: ws || null,
      forkedFrom: { id: src.id, title: src.title },
      forkFromIndex: null
    };
    if (serverSessionTools && !ws) {
      const created = await serverAction("session_create", {
        id: fork.id, title: fork.title, mode: fork.mode,
        forkedFromId: src.id, placeholderModel: selectedModel
      });
      if (!created) return;
    } else {
      chats = [fork, ...chats];
    }
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
      if (termCtx) {
        termCtx.term.focus();
        termCtx = null;
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
      if (settingsOpen || pluginsOpen || skillsOpen || gotoOpen || lightboxSrc || editingIndex != null || renamingId || pendingDeleteId || modelOpen || codeEditorPath) return;
      if (!activeWorkRunning) return;
      if (onEscape()) e.preventDefault();
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  });

  function setSidebar(open: boolean) {
    sidebarOpen = open;
    srvMenuOpen = false;
    persistPrefs();
  }

  onMount(() => {
    notifyTurns = getNotifyEnabled();
    notifySound = getNotifySound();
    notifySoundChoice = getNotifySoundChoice();
    // audio contexts start suspended until a gesture; unlock on the first one
    // so a chime can play later while the window is in the background
    const unlock = () => { unlockChime(); window.removeEventListener("pointerdown", unlock); window.removeEventListener("keydown", unlock); };
    window.addEventListener("pointerdown", unlock);
    window.addEventListener("keydown", unlock);
    window.addEventListener("beforeunload", handleUnload);
    window.addEventListener("message", onBrowserFrameMessage);
    window.addEventListener("message", onPanelMessage);
    window.addEventListener("pointerup", onMessagesPointerUp);
    window.addEventListener("pointercancel", onMessagesPointerUp);
    window.addEventListener("keydown", onMessagesKeyDown);
    // scheduled sends: anything already due was missed while Vicuna was
    // closed, so it waits for a decision instead of firing late
    {
      let stored: string | null = null;
      try {
        stored = localStorage.getItem(scheduleStoreKey());
      } catch {}
      const loaded = markMissed(parseScheduled(stored), Date.now());
      if (loaded.length) {
        scheduled = loaded;
        persistScheduled();
        if (loaded.some((x) => x.missed)) scheduleListOpen = true;
      }
    }
    const scheduleTimer = setInterval(scheduleTick, 5000);
    hydrateUsageStore();
    const usagePollTimer = setInterval(() => void pollUsageBackground(), USAGE_POLL_INTERVAL_MS);
    const usageTickTimer = setInterval(() => { usageNow = Date.now(); }, 30_000);
    const onUsageVisibility = () => {
      if (document.visibilityState === "visible") {
        usageNow = Date.now();
        void pollUsageBackground();
      }
    };
    document.addEventListener("visibilitychange", onUsageVisibility);
    window.addEventListener("keydown", onScheduleEscape, true);
    // tray Quit / Cmd+Q with scheduled messages pending: the backend shows
    // the window and asks here instead of quitting
    let unlistenQuit: (() => void) | null = null;
    if (isTauriRuntime()) {
      void import("@tauri-apps/api/event")
        .then(async ({ listen }) => {
          unlistenQuit = await listen("vicuna-quit-requested", () => {
            if (isRemoteServer()) return; // the main window owns the quit prompt
            if (scheduled.length > 0) quitPromptOpen = true;
            else void quitVicuna(true);
          });

        })
        .catch(() => {});
    }
    void initApp();
    void loadPlugins();
    void refreshServers();
    // Automatic update detection on startup (desktop only), same as cojudge.
    initializeAppUpdatePreference();
    const updateCheckTimer = isTauriRuntime() && !isRemoteServer()
      ? setTimeout(() => void triggerAutomaticAppUpdateCheck(), 2500)
      : null;
    // deep links (Copy Link in desktop mode) arrive here, both at launch
    // (getCurrent) and while running (onOpenUrl, e.g. link opened in Safari)
    let unlistenDeepLink: (() => void) | null = null;
    // links name sessions/folders on THIS computer: the main window owns them
    if (isTauriRuntime() && !isRemoteServer()) {
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
    // the Cmd+P palette in another window can hand this one a session
    // (open_chat_window → emit_to; a fresh window gets ?chat= instead)
    let unlistenOpenChat: (() => void) | null = null;
    let unlistenServersChanged: (() => void) | null = null;
    if (isTauriRuntime()) {
      void import("@tauri-apps/api/event")
        .then(({ listen }) => {
          void listen<{ chatId?: string }>("vicuna-open-chat", (ev) => {
            const id = ev.payload?.chatId;
            if (!id) return;
            if (!ready) {
              pendingOpenChatId = id;
              return;
            }
            if (chats.some((c) => c.id === id)) selectChat(id);
            else flashNotice("That session isn't loaded in this window.");
          }).then((u) => {
            unlistenOpenChat = u;
          });
          void listen("vicuna-servers-changed", () => {
            void refreshServers();
          }).then((u) => {
            unlistenServersChanged = u;
          });
        })
        .catch(() => {});
    }
    return () => {
      window.removeEventListener("beforeunload", handleUnload);
      window.removeEventListener("message", onBrowserFrameMessage);
      window.removeEventListener("message", onPanelMessage);
      window.removeEventListener("pointerup", onMessagesPointerUp);
      window.removeEventListener("pointercancel", onMessagesPointerUp);
      window.removeEventListener("keydown", onMessagesKeyDown);
      unlistenDeepLink?.();
      unlistenOpenChat?.();
      unlistenServersChanged?.();
      unlistenQuit?.();
      clearTimeout(mcpDefaultTimer);
      clearInterval(scheduleTimer);
      clearInterval(usagePollTimer);
      clearInterval(usageTickTimer);
      document.removeEventListener("visibilitychange", onUsageVisibility);
      window.removeEventListener("keydown", onScheduleEscape, true);
      if (updateCheckTimer) clearTimeout(updateCheckTimer);
    };
  });

  onDestroy(() => {
    warmCancelled = true;
    cancelHoverPrefetch();
    if (scrollFrame) cancelAnimationFrame(scrollFrame);
    for (const c of aborters.values()) c.abort();
    for (const feed of serverChatFeeds.values()) feed.close();
  });

  /** Server mode with a recording server: wire the store's save hooks and
   *  move the Scheduled list onto the server. */
  async function installServerSync() {
    // a live server turn is written by the server alone
    serverSync.skip = (id) => serverPersist && (workRunningIds.includes(id) || serverChatFeeds.has(id));
    // a refused save: the server has a newer copy (it recorded a turn while
    // this window slept, or another window saved) — show that one
    serverSync.onConflict = (fresh) => {
      const byId = new Map(fresh.filter((c) => !isSessionBusy(c.id)).map((c) => [c.id, c]));
      if (!byId.size) return;
      chats = chats.map((c) => byId.get(c.id) ?? c);
      flashNotice("A session changed on the server; showing its latest version.");
    };
    // scheduled sends kept in this window's storage by older builds go up once
    try {
      const local = parseScheduled(localStorage.getItem(scheduleStoreKey()));
      if (local.length) {
        await invokeOrBridge("schedule_put", { items: local, tzOffsetMin: -new Date().getTimezoneOffset() });
        localStorage.removeItem(scheduleStoreKey());
      }
    } catch {
      /* keep them local; the server list below still loads */
    }
    await refreshServerSchedule();
  }

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
      defaultWorkspace = normalizeStoredWorkspace(prefs.defaultWorkspace);
      pendingWorkspace = resolveDraftWorkspace(defaultWorkspace);
      zoomLevel = prefs.zoom;
      if (isTauriRuntime() && zoomLevel !== 1) void applyZoom(zoomLevel);
      thinkLevel = prefs.thinkLevel ?? "auto";
      continueMode = normalizeContinueMode(prefs.continueMode);
      openrouterKey = prefs.openrouterApiKey ?? "";
      openrouterKeyDraft = openrouterKey;
      setOpenRouterApiKey(openrouterKey);
      openrouterAdded = [...(prefs.openrouterAdded ?? [])];
      sessionFilterRaw = prefs.sessionFilter ?? ALL_FILTER;
      // narrow screens start collapsed regardless of the saved preference
      sidebarOpen = window.innerWidth < 700 ? false : prefs.sidebarOpen;
      // restore the unsent composer text (one global box, not per session;
      // local storage) before drawing, so typing while sessions load is kept
      input = readDraft(null);
      // draw the window now; the session list follows
      loadingSince = Date.now();
      uiReady = true;
      try {
        chats = await loadChats();
      } catch (e) {
        if (!isServerBacked()) throw e;
        chats = [];
        srvLoadError = e instanceof Error ? e.message : String(e);
      } finally {
        chatsLoaded = true;
      }
      if (isServerBacked() && !srvLoadError) {
        const info = await invokeOrBridge<{ persist?: boolean; chatTurns?: boolean; sessionTools?: boolean }>("server_info", {}).catch(() => null);
        serverPersist = info?.persist === true;
        serverChatTurns = info?.chatTurns === true;
        serverSessionTools = info?.sessionTools === true;
        if (serverSessionTools) {
          const workspace = await invokeOrBridge<string | null>("work_default_workspace", {}).catch(() => null);
          if (workspace) {
            defaultWorkspace = normalizeStoredWorkspace(workspace);
            pendingWorkspace = resolveDraftWorkspace(defaultWorkspace);
          }
          lastDefaultWorkspace = workspace ?? "";
        }
        setServerOllama(serverChatTurns);
        if (serverPersist) await installServerSync();
      }
      // Keep the composer workspace synced with a non-"all" workspace scope on
      // startup (even if server work_default_workspace or prefs drifted).
      const resolvedStartupScope = resolveSessionFilter(sessionFilterRaw, folderOptions(chats));
      if (resolvedStartupScope === NO_WORKSPACE_FILTER) {
        pendingWorkspace = NO_WORKSPACE_SENTINEL;
      } else {
        const startupScopeWs = workspaceOfFolderKey(resolvedStartupScope);
        if (startupScopeWs !== null) {
          pendingWorkspace = startupScopeWs;
          defaultWorkspace = startupScopeWs;
        }
      }
      // reopen where the user left off (saved on every navigation); the
      // draft below then restores their unsent text into the composer
      const savedActive = await loadActiveChat();
      // empty drafts are never listed: drop any persisted from older
      // builds so a stale "New session" row can't linger in the sidebar
      if (!serverSessionTools && chats.some((c) => c.messages.length === 0)) {
        chats = chats.filter((c) => c.messages.length > 0);
        save();
      }
      if (launchChatId && chats.some((c) => c.id === launchChatId)) {
        // opened from the Cmd+P palette on another host: this session wins
        // over the last one (navigate and let the route effect activate it)
        void goto(`/s/${launchChatId}`);
      } else if (sessionId) {
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
      // the palette handed this window a session before chats finished loading
      if (pendingOpenChatId) {
        const id = pendingOpenChatId;
        pendingOpenChatId = null;
        if (chats.some((c) => c.id === id)) void goto(`/s/${id}`);
      }
      const savedCatalog = await loadCatalog();
      if (savedCatalog) {
        catalog = savedCatalog.entries;
        catalogUpdated = savedCatalog.updated;
      }
      const cachedOpenRouter = readOpenRouterCache();
      if (cachedOpenRouter) {
        openrouterModels = cachedOpenRouter.models;
        openrouterUpdated = cachedOpenRouter.updated;
      }
      const m = await loadModel();
      if (m) selectedModel = m;
      await loadMcpServers();
    } catch (e) {
      console.error("storage load failed:", e);
    } finally {
      uiReady = true;
      chatsLoaded = true;
      ready = true;
    }
    // vicuna:// links that arrived before chats loaded (cold start: the
    // getCurrent() answer can land at any point during initApp)
    flushPendingDeepLinks();
    refreshModels();
    void refreshWorkEngine();
    // resume a stream interrupted by a refresh: the partial tokens persisted
    // via the regular save, the pending marker tells us how to continue it
    // (user-stopped streams clear the marker, so they never resume here)
    let pending = readPendingStream();
    if (pending && (pending.work || chats.find((c) => c.id === pending!.chatId)?.mode === "work")) {
      // a Work turn's marker: the Chat resume below would wipe the agent's
      // partial reply and regenerate it with the Chat model and no tools.
      // Work turns live on the backend; a still-running one is reattached
      // just below, a finished one keeps whatever was saved.
      clearPendingStream();
      pending = null;
    }
    // reattach first: a restored run strip needs the live turn's bubble
    void reattachLiveWorkTurns();
    if (pending && (!sessionId || sessionId === pending.chatId)) {
      const pc = chats.find((c) => c.id === pending!.chatId);
      const pm = pc?.messages[pending.index];
      if (pc && pm && pm.role === "assistant") {
        activeId = pc.id;
        if (pending.model) {
          selectedModel = pending.model;
          void saveModel(pending.model);
        }
        error = null;
        if (serverChatTurns && pc.mode !== "work") {
          clearPendingStreamFor(pc.id);
          await startServerChat(pc.id, {
            operation: pending.mode === "continue" ? "continue" : "regenerate",
            index: pending.index
          }, pc.id, () => {}, {
            model: pending.model || selectedModel,
            think: pending.think,
            continueMode: pending.rawStyle ? "raw" : "native",
            stop: pending.stop ?? []
          });
          return;
        }
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
        if (pending.mode === "continue" && !continuingIds.includes(pc.id)) continuingIds = [...continuingIds, pc.id];
        // raw Continue resumes through /api/generate: rebuild the prefilled
        // prompt from the persisted partial (the marker carries the style
        // because the family lookup could differ offline)
        const resumeStyle = normalizePromptStyle(pending.rawStyle);
        const rawResume = resumeStyle ? buildRawContinuePrompt(resumeHistory, resumeStyle) : null;
        try {
          await streamInto(pc, pending.index, resumeHistory, false, {
            think: pending.think,
            temperature: pending.temperature,
            stop: rawResume ? [...new Set([...(pending.stop ?? []), ...rawResume.stop])] : pending.stop,
            mode: pending.mode,
            generatePrompt: rawResume?.prompt,
            rawStyle: pending.rawStyle
          });
        } finally {
          continuingIds = continuingIds.filter((x) => x !== pc.id);
        }
      } else {
        clearPendingStream();
      }
    }
  }
</script>

{#if uiReady}
<!-- agent question body (vicuna-mcp ask_user): choices prefill the answer
     box (never answer directly), the box takes pasted images; shared by the
     live card and the put-off "Answer later" cards -->
{#snippet agentAskBody(card: { options: string[]; multi: boolean; picked: string[]; text: string; images: string[] }, tid: string, onSend: () => void, onDismiss: () => void, onLater: (() => void) | null)}
  {@const canSend = buildAskAnswer(card) != null}
  <div class="ask-options">
    {#each card.options as opt, oi}
      {#if card.multi}
        <button
          class="ask-option ask-option-multi"
          class:picked={card.picked.includes(opt)}
          data-testid="{tid}-option-{oi}"
          aria-pressed={card.picked.includes(opt)}
          onclick={() => (card.picked = toggleChoice(card.options, card.picked, opt))}
        ><span class="ask-check" aria-hidden="true">{card.picked.includes(opt) ? "✓" : ""}</span>{opt}</button>
      {:else}
        <button
          class="ask-option"
          class:picked={card.text.trim() === opt}
          data-testid="{tid}-option-{oi}"
          title="Use this answer (you can edit it before sending)"
          onclick={(e) => prefillAsk(card, opt, e.currentTarget)}
        >{opt}</button>
      {/if}
    {/each}
    <div class="ask-compose">
      {#if card.images.length}
        <div class="image-strip ask-image-strip" data-testid="{tid}-image-strip">
          {#each card.images as img, k}
            <div class="image-chip">
              <button class="image-open" aria-label="View attached image {k + 1} fullscreen" onclick={() => openLightbox(img)}>
                <img src={img} alt="" />
              </button>
              <button class="image-remove" aria-label="remove image {k + 1}" data-testid="{tid}-image-remove-{k}" onclick={() => { card.images = card.images.filter((_, j) => j !== k); }}>×</button>
            </div>
          {/each}
        </div>
      {/if}
      <div class="ask-other">
        <textarea
          class="ask-input"
          data-testid="{tid}-input"
          rows={Math.min(8, Math.max(1, card.text.split("\n").length))}
          bind:value={card.text}
          placeholder={card.multi
            ? "Add a note or paste an image (optional)…"
            : card.options.length
              ? "Pick a choice to edit it, or type your answer…"
              : "Type your answer or paste an image…"}
          aria-label="Your answer"
          onpaste={(e) => onAskPaste(e, card)}
          onkeydown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
              e.preventDefault();
              if (canSend) onSend();
            } else if (e.key === "Escape") {
              // leave the box without arming double-Esc interrupt
              e.preventDefault();
              e.stopPropagation();
              (e.currentTarget as HTMLTextAreaElement).blur();
            }
          }}
        ></textarea>
        <button class="ask-send" data-testid={card.multi ? `${tid}-submit` : `${tid}-send`} disabled={!canSend} onclick={onSend}>
          {card.multi ? `Submit${card.picked.length ? ` (${card.picked.length})` : ""}` : "Send"}
        </button>
        <button class="ask-dismiss" type="button" data-testid="{tid}-dismiss" onclick={onDismiss}>Dismiss</button>
      </div>
      {#if onLater}
        <button
          class="ask-later"
          type="button"
          data-testid="{tid}-later"
          title="The agent stops waiting and keeps going (or ends its turn); send your answer whenever you're ready"
          onclick={onLater}
        >Answer later</button>
      {/if}
    </div>
  </div>
{/snippet}
{#snippet scheduleCaret(empty: boolean)}
  <button class="send send-caret" data-testid="schedule-btn" aria-label="Schedule message" aria-haspopup="menu" aria-expanded={scheduleMenuOpen} title="Schedule for later" disabled={empty} onclick={toggleScheduleMenu}>
    <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="m4 6 4 4 4-4"/></svg>
  </button>
  {#if scheduleMenuOpen}
    <button class="sched-backdrop" data-testid="schedule-backdrop" aria-label="Close schedule menu" onclick={() => (scheduleMenuOpen = false)} tabindex="-1"></button>
    <div class="sched-menu" data-testid="schedule-menu" role="menu" aria-label="Schedule message">
      <div class="sched-menu-head">Schedule message</div>
      {#each schedulePresets(scheduleNow) as p (p.id)}
        <button class="ctx-item sched-opt" role="menuitem" data-testid="schedule-preset-{p.id}" onclick={() => void scheduleSend(Date.now() + (p.at - scheduleNow))}>
          <span>{p.label}</span><span class="sched-opt-at">{formatClock(p.at)}</span>
        </button>
      {/each}
      <div class="sched-menu-sep"></div>
      <button class="ctx-item sched-opt" role="menuitem" data-testid="schedule-custom" onclick={() => openScheduleCustom()}>Custom time</button>
      <div class="sched-menu-note">{scheduleNote}</div>
    </div>
  {/if}
{/snippet}
{#snippet engineIcon(id: string)}
  <EngineIcon {id} {workEngineList} />
{/snippet}
{#snippet serverMenu()}
  <button class="ctx-backdrop" data-testid="server-menu-backdrop" aria-label="Close menu" tabindex="-1" onclick={() => { srvMenuOpen = false; srvMenuRenaming = false; }}></button>
  <div class="srv-menu" data-testid="server-menu" role="menu">
    <div class="srv-menu-label">Recents</div>
    <button class="srv-item" data-testid="server-item-local" role="menuitem" onclick={() => requestServerSwitch(null)}>
      <span class="srv-check">{#if !onServer}✓{/if}</span><span class="srv-item-name">This computer</span>
    </button>
    {#each srvList as sv (sv.id)}
      <button class="srv-item" data-testid="server-item" role="menuitem" title={sv.url} onclick={() => requestServerSwitch(sv.id)}>
        <span class="srv-check">{#if sv.id === activeServer}✓{/if}</span><span class="srv-item-name">{sv.name}</span>
      </button>
    {/each}
    <div class="srv-sep"></div>
    {#if onServer && srvVersion}
      <div class="srv-version" data-testid="server-version">
        <span>Server v{srvVersion} · App v{clientVersion}</span>
        {#if srvVersionSkew && !srvUpgradeAvailable}<span class="srv-skew" title="The server and this app run different versions">differs</span>{/if}
      </div>
      {#if srvUpgradeAvailable && srvCanUpgrade}
        <button class="srv-item srv-upgrade" data-testid="server-upgrade" role="menuitem" disabled={srvUpgrading} onclick={upgradeServer}>
          {#if srvUpgrading}
            <span class="srv-check"><span class="srv-spin" aria-hidden="true"></span></span>
            <span class="srv-item-name srv-upgrade-prog" data-testid="server-upgrade-progress">
              <span class="shimmer">{srvUpgradeStage ?? "Upgrading server…"}</span>
              {#if srvUpgradePct != null}<span class="srv-upgrade-bar"><span style="width:{srvUpgradePct}%"></span></span>{/if}
              {#if srvUpgradeDetail}<span class="srv-upgrade-detail">{srvUpgradeDetail}</span>{/if}
            </span>
          {:else}
            <span class="srv-check">↑</span><span class="srv-item-name">Upgrade server to v{srvLatest}</span>
          {/if}
        </button>
      {/if}
      <div class="srv-sep"></div>
    {/if}
    {#if onServer && srvCurrent}
      {#if srvMenuRenaming}
        <div class="srv-menu-rename-row" data-testid="server-menu-rename-form">
          <input class="srv-menu-rename-input" data-testid="server-menu-rename-input" placeholder={defaultServerName(srvCurrent.url)} autocomplete="off" spellcheck="false"
            bind:value={srvEditDraft} disabled={srvRenaming}
            onkeydown={(e) => { if (e.key === "Enter") void saveMenuRenameServer(); else if (e.key === "Escape") srvMenuRenaming = false; }} />
          <button class="srv-menu-rename-ok" data-testid="server-menu-rename-save" aria-label="Save name" title="Save" disabled={srvRenaming} onclick={() => void saveMenuRenameServer()}>✓</button>
        </div>
      {:else}
        <button class="srv-item" data-testid="server-menu-rename" role="menuitem" onclick={startMenuRenameServer}>
          <span class="srv-check">✎</span><span class="srv-item-name">Rename server…</span>
        </button>
      {/if}
    {/if}
    <button class="srv-item srv-connect" data-testid="server-connect-open" role="menuitem" onclick={openServerDialog}>
      <span class="srv-check">+</span><span class="srv-item-name">Connect to new server…</span>
    </button>
  </div>
{/snippet}

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
        <button class="goto" data-testid="scheduled-btn" title="Scheduled messages" onclick={() => { scheduleNow = Date.now(); scheduleListOpen = true; }}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/></svg>
          <span>Scheduled</span>
          {#if scheduled.length > 0}
            <span class="sched-count {scheduled.some((x) => x.missed) ? 'missed' : ''}" data-testid="scheduled-count">{scheduled.length}</span>
          {/if}
        </button>
      </div>

      <div class="scope-row">
        <label class="scope-label" for="scope-select">Scope</label>
        <select
          id="scope-select"
          class="scope-select {sessionFilter !== ALL_FILTER ? 'scoped' : ''}"
          data-testid="scope-select"
          aria-label="Scope sessions"
          title={scopeTitle || "Every session"}
          value={sessionFilter}
          onchange={(e) => setSessionFilter(e.currentTarget.value)}
        >
          {#each folderRows as f (f.key)}
            <option value={f.key}>{f.label} · {f.count}</option>
          {/each}
        </select>
        <button
          class="archive-toggle {showArchived ? 'on' : ''}"
          data-testid="show-archived-toggle"
          aria-pressed={showArchived}
          title={showArchived ? "Hide archived sessions" : "Show archived sessions"}
          onclick={toggleShowArchived}
        >
          <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="4" rx="1"/><path d="M5 8v11a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1V8M10 12h4"/></svg>
          {#if archivedCount > 0}<span class="archive-count">{archivedCount}</span>{/if}
        </button>
      </div>

      <div class="history">
        {#snippet chatList(items: Chat[])}
          {#each items as c (c.id)}
            {@const live = isSessionStreaming(c.id)}
            <button
              class="chat-item {c.id === activeId ? 'active' : ''} {live ? 'streaming' : ''} {c.archived ? 'archived' : ''} {c.role === 'lead' ? 'lead-row' : ''}"
              data-testid="chat-item"
              data-archived={c.archived ? "true" : null}
              data-streaming={live ? "true" : null}
              title={live ? liveTitle(c.id) : c.role === "lead" ? c.title : undefined}
              onclick={() => selectChat(c.id)}
              onpointerdown={() => { if (c.partial) void ensureChatFull(c, { silent: true }); }}
              onpointerenter={() => scheduleHoverPrefetch(c)}
              onpointerleave={cancelHoverPrefetch}
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
                {#if c.role === "lead"}
                  <span class="work-badge lead-badge" data-testid="lead-badge" title="Lead session: can create, message, stop and remove other sessions">Lead</span>
                {:else}
                  <span class="title {live ? 'shimmer' : ''}">{c.title}</span>
                {/if}
                {@const liveMs = live ? liveElapsedMs(c.id) : null}
                {#if liveMs != null}<span class="live-elapsed" data-testid="live-elapsed">{formatLiveElapsed(liveMs)}</span>{/if}
                {#if c.archived}<span class="work-badge archived-badge" data-testid="archived-badge" title="Archived: hidden unless Show archived is on">Archived</span>{/if}
                {#if c.mode === "work"}
                  {@const tgSnap = telegramStatuses[c.id]}
                  {@const tgState = telegramConnState(tgSnap)}
                  {#if tgState !== "unconfigured"}
                    <span
                      class="tg-row-badge {tgState}"
                      data-testid="telegram-row-badge"
                      data-status={tgState}
                      title={telegramConnTitle(tgSnap)}
                      aria-label={telegramConnTitle(tgSnap)}
                    >
                      <svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d="M9.78 18.65l.28-4.23 7.68-6.92c.34-.31-.07-.46-.52-.19L7.74 13.3 3.64 12c-.88-.25-.89-.86.2-1.3l15.97-6.16c.73-.33 1.43.18 1.15 1.3l-2.72 12.81c-.19.91-.74 1.13-1.5.71L12.6 16.3l-1.99 1.93c-.23.23-.42.42-.83.42z"/></svg>
                    </span>
                  {/if}
                  {#each bridgePlugins as bp (bp.id)}
                    {@const bSnap = bridgeStatuses[bp.id]?.[c.id]}
                    {@const bState = bridgeConnState(bSnap)}
                    {#if bState !== "unconfigured"}
                      {@const bName = bridgeDisplayName(bp)}
                      <span
                        class="tg-row-badge {bState}"
                        data-testid="bridge-row-badge-{bp.id}"
                        data-status={bState}
                        title={bridgeConnTitle(bName, bSnap)}
                        aria-label={bridgeConnTitle(bName, bSnap)}
                      >
                        <svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path d={safeIconPath(bp.bridge?.iconPath)}/></svg>
                      </span>
                    {/if}
                  {/each}
                  {#if showWorkspaceBadge(c, sessionFilter)}
                    {@const wsIsNone = isNoWorkspacePath(c.workspace)}
                    {@const wsBase = wsIsNone ? "" : workspaceBase(c.workspace ?? "")}
                    {#if wsIsNone}
                      <span class="work-badge ws-badge" data-testid="ws-badge" title="No workspace — uses a temporary empty scratch folder if needed">
                        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M5.6 5.6l12.8 12.8"/></svg>
                        <span>No workspace</span>
                      </span>
                    {:else if wsBase}
                      <span class="work-badge ws-badge" data-testid="ws-badge" title={displayWorkspace(c.workspace ?? "")}>
                        <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                        <span>{wsBase}</span>
                      </span>
                    {:else}
                      <span class="work-badge" title="Work session">Work</span>
                    {/if}
                  {/if}
                {/if}
              {/if}
            </button>
          {/each}
        {/snippet}
        {#if pinnedChats.length > 0}
          <div class="group-label" data-testid="pinned-label">Pinned</div>
          <div class="pinned-list" data-testid="pinned-list">{@render chatList(pinnedChats)}</div>
        {/if}
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
        {#if !chatsLoaded}
          <div class="skel-list" data-testid="sessions-loading" role="status" aria-label={`Loading sessions${onServer ? ` from ${srvLabel}` : ""}`}>
            <div class="skel skel-label"></div>
            {#each [78, 62, 88, 54, 70, 46, 66] as w, k (k)}
              <div class="skel-row"><div class="skel" style="width: {w}%"></div></div>
            {/each}
            {#if loadingSecs >= 5}
              <div class="skel-note">{onServer ? `Loading from ${srvLabel}` : "Loading"} · {loadingSecs}s</div>
            {/if}
          </div>
        {:else if grouped.today.length + grouped.yesterday.length + grouped.week.length + grouped.older.length === 0 && pinnedChats.length === 0}
          <div class="group-label">{sessionFilter === ALL_FILTER ? "Today" : "No matches"}</div>
          <div class="empty-hint">{sessionFilter === ALL_FILTER ? "No chats yet" : "No sessions in this scope yet"}</div>
        {/if}
      </div>
      <div class="side-footer">
        {#if isTauriRuntime()}
          <div class="srv-pick">
            {#if srvMenuOpen}
              {@render serverMenu()}
            {/if}
            <button class="goto srv-btn {onServer ? 'remote' : ''}" data-testid="server-btn" title={onServer ? (srvCurrent?.url ?? "") : "Sessions and agents run on this computer"} aria-haspopup="menu" aria-expanded={srvMenuOpen} onclick={toggleServerMenu}>
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/></svg>
              <span class="srv-name">{srvLabel}</span>
              {#if onServer && (srvVersionSkew || srvUpgradeAvailable)}<span class="srv-dot" data-testid="server-version-dot" title={srvUpgradeAvailable ? `Server update available: v${srvLatest}` : `Server v${srvVersion}, app v${clientVersion}`}></span>{/if}
              <svg class="srv-chev" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{#if srvMenuOpen}<path d="m6 15 6-6 6 6"/>{:else}<path d="m6 9 6 6 6-6"/>{/if}</svg>
            </button>
          </div>
        {/if}
        <button class="goto" data-testid="plugins-btn" aria-label="Plugins" title="Plugins" onclick={openPlugins}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9 3v4M15 3v4M6 7h12v4a6 6 0 0 1-12 0z"/><path d="M12 17v4"/></svg>
          <span>Plugins</span>
        </button>
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
          onclick={() => {
            ctxMenu = null;
            workspacePickerOpen = false;
          }}
          oncontextmenu={(e) => {
            e.preventDefault();
            ctxMenu = null;
            workspacePickerOpen = false;
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
          <button class="ctx-item" data-testid="pin-item" onclick={() => togglePin(ctxMenu!.chatId)}>{ctxChat?.pinned ? "Unpin" : "Pin"}</button>
          <button class="ctx-item" data-testid="archive-item" onclick={() => setArchived(ctxMenu!.chatId, !ctxChat?.archived)}>{ctxChat?.archived ? "Unarchive" : "Archive"}</button>
          <div class="ctx-sep" role="separator"></div>
          {#if (ctxChat?.mode ?? "chat") === "work"}
            {#if serverPersist}
              <button class="ctx-item" data-testid="permissions-item" onclick={() => { permissionsFor = ctxMenu!.chatId; ctxMenu = null; }}>Permissions…</button>
            {/if}
            {@const tgSnap = ctxChat ? telegramStatuses[ctxChat.id] : null}
            {@const tgState = telegramStateFor(ctxChat?.id)}
            {#if ctxChat?.role === "lead"}
              <button class="ctx-item" data-testid="lead-item" onclick={() => toggleLead(ctxMenu!.chatId)}>Remove lead</button>
            {:else}
              {@const hasLead = chats.some((x) => x.role === "lead" && x.id !== ctxMenu!.chatId)}
              <button
                class="ctx-item"
                data-testid="lead-item"
                onclick={() => { leadPrompt = { id: ctxMenu!.chatId, prevTitle: chats.find((x) => x.role === "lead" && x.id !== ctxMenu!.chatId)?.title ?? null }; ctxMenu = null; }}
              >{hasLead ? "Replace lead" : "Make lead"}</button>
            {/if}
            <button
              class="ctx-item ctx-telegram"
              data-testid="telegram-item"
              title={telegramConnTitle(tgSnap)}
              onclick={() => { telegramFor = ctxMenu!.chatId; ctxMenu = null; }}
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
              {@const bColor = safeColor(bp.bridge?.color) ? bp.bridge!.color! : "var(--accent)"}
              <button
                class="ctx-item ctx-telegram"
                data-testid="bridge-item-{bp.id}"
                title={bridgeConnTitle(bName, bSnap)}
                onclick={() => { bridgeModal = { pluginId: bp.id, sessionId: ctxMenu!.chatId }; ctxMenu = null; }}
              >
                <span class="ctx-tg-icon" style:color={bColor} aria-hidden="true"><svg width="11" height="11" viewBox="0 0 24 24" fill="currentColor"><path d={safeIconPath(bp.bridge?.iconPath)}/></svg></span>
                <span class="ctx-tg-label">{bName}</span>
                {#if bSnap?.botUsername}<span class="ctx-tg-bot">@{bSnap.botUsername}</span>{/if}
                <span class="ctx-tg-dot {bState}" data-testid="bridge-status-dot-{bp.id}" data-status={bState} aria-label={bridgeConnLabel(bName, bState)}></span>
              </button>
            {/each}
          {/if}
          <div class="ctx-continue-row">
            <button class="ctx-item ctx-continue" data-testid="continue-work-item" onclick={() => continueInWork(ctxMenu!.chatId)}>Continue in Work</button>
            <button
              class="ctx-ws-pill"
              data-testid="continue-work-workspace"
              title={defaultWorkspace ? `Work folder: ${displayWorkspace(defaultWorkspace)} — click to change` : "Pick a work folder"}
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
                    workspacePickerOpen = false;
                  }
                }}
              />
              {#if workspaceError}
                <div class="goto-error" data-testid="continue-work-workspace-error">{workspaceError}</div>
              {/if}
            </form>
          {/if}
          <div class="ctx-sep" role="separator"></div>
          <button class="ctx-item danger" data-testid="delete-item" onclick={() => requestDelete(ctxMenu!.chatId)}>Delete</button>
        </div>
      {/if}
    </aside>
  {/if}

  <main class="main">
    {#if srvLoadError}
      <div class="srv-strip" data-testid="server-load-error" role="alert">
        <span>Can't load sessions from {srvLabel}: {srvLoadError}</span>
        <button class="mini-btn" onclick={() => window.location.reload()}>Retry</button>
        {#if onServer}<button class="mini-btn" onclick={() => void switchServer(null)}>Use this computer</button>{/if}
      </div>
    {/if}
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
        <div class="collapsed-rail-foot">
          {#if isTauriRuntime()}
            <div class="srv-pick">
              {#if srvMenuOpen}
                {@render serverMenu()}
              {/if}
              <button class="icon-btn srv-icon-btn {onServer ? 'remote' : ''}" data-testid="server-btn-collapsed" aria-label={onServer ? `Server: ${srvLabel}` : "Server: This computer"} title={onServer ? (srvCurrent?.url ? `${srvLabel} (${srvCurrent.url})` : srvLabel) : "Sessions and agents run on this computer"} aria-haspopup="menu" aria-expanded={srvMenuOpen} onclick={toggleServerMenu}>
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><rect x="3" y="4" width="18" height="7" rx="2"/><rect x="3" y="13" width="18" height="7" rx="2"/><path d="M7 7.5h.01M7 16.5h.01"/></svg>
                {#if onServer && (srvVersionSkew || srvUpgradeAvailable)}<span class="srv-dot" data-testid="server-version-dot" title={srvUpgradeAvailable ? `Server update available: v${srvLatest}` : `Server v${srvVersion}, app v${clientVersion}`}></span>{/if}
              </button>
            </div>
          {/if}
          <button class="icon-btn" data-testid="plugins-btn-collapsed" aria-label="Open plugins" title="Plugins" onclick={openPlugins}>
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M9 3v4M15 3v4M6 7h12v4a6 6 0 0 1-12 0z"/><path d="M12 17v4"/></svg>
          </button>
          <button class="icon-btn" data-testid="settings-btn-collapsed" aria-label="Open settings" title="Settings" onclick={openSettings}>
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path fill="currentColor" stroke="none" d="M12 21a8.985 8.985 0 0 1-1.755-.173 1 1 0 0 1-.791-.813l-.273-1.606a6.933 6.933 0 0 1-1.32-.762l-1.527.566a1 1 0 0 1-1.1-.278 8.977 8.977 0 0 1-1.756-3.041 1 1 0 0 1 .31-1.092l1.254-1.04a6.979 6.979 0 0 1 0-1.524L3.787 10.2a1 1 0 0 1-.31-1.092 8.977 8.977 0 0 1 1.756-3.042 1 1 0 0 1 1.1-.278l1.527.566a6.933 6.933 0 0 1 1.32-.762l.274-1.606a1 1 0 0 1 .791-.813 8.957 8.957 0 0 1 3.51 0 1 1 0 0 1 .791.813l.273 1.606a6.933 6.933 0 0 1 1.32.762l1.527-.566a1 1 0 0 1 1.1.278 8.977 8.977 0 0 1 1.756 3.041 1 1 0 0 1-.31 1.092l-1.254 1.04a6.979 6.979 0 0 1 0 1.524l1.254 1.04a1 1 0 0 1 .31 1.092 8.977 8.977 0 0 1-1.756 3.041 1 1 0 0 1-1.1.278l-1.527-.566a6.933 6.933 0 0 1-1.32.762l-.273 1.606a1 1 0 0 1-.791.813A8.985 8.985 0 0 1 12 21zm-.7-2.035a6.913 6.913 0 0 0 1.393 0l.247-1.451a1 1 0 0 1 .664-.779 4.974 4.974 0 0 0 1.696-.975 1 1 0 0 1 1.008-.186l1.381.512a7.012 7.012 0 0 0 .7-1.206l-1.133-.939a1 1 0 0 1-.343-.964 5.018 5.018 0 0 0 0-1.953 1 1 0 0 1 .343-.964l1.124-.94a7.012 7.012 0 0 0-.7-1.206l-1.38.512a1 1 0 0 1-1-.186 4.974 4.974 0 0 0-1.688-.976 1 1 0 0 1-.664-.779l-.248-1.45a6.913 6.913 0 0 0-1.393 0l-.25 1.45a1 1 0 0 1-.664.779A4.974 4.974 0 0 0 8.7 8.24a1 1 0 0 1-1 .186l-1.385-.512a7.012 7.012 0 0 0-.7 1.206l1.133.939a1 1 0 0 1 .343.964 5.018 5.018 0 0 0 0 1.953 1 1 0 0 1-.343.964l-1.128.94a7.012 7.012 0 0 0 .7 1.206l1.38-.512a1 1 0 0 1 1 .186 4.974 4.974 0 0 0 1.688.976 1 1 0 0 1 .664.779zm.7-3.725a3.24 3.24 0 0 1 0-6.48 3.24 3.24 0 0 1 0 6.48zm0-4.48A1.24 1.24 0 1 0 13.24 12 1.244 1.244 0 0 0 12 10.76z"/></svg>
          </button>
        </div>
      </div>
    {/if}

    {#if !activeChat || activeChat.messages.length === 0}
      <div class="empty">
        <div class="seg mode-toggle" role="tablist" aria-label="Session mode" data-testid="mode-toggle">
          <button class="seg-btn {pendingMode === 'chat' ? 'sel' : ''}" data-testid="mode-chat-btn" role="tab" onclick={() => { pendingMode = "chat"; writeLastMode(localStorage, "chat"); workspaceError = null; if (activeChat && activeChat.messages.length === 0) { activeChat.mode = "chat"; activeChat.workspace = null; } }}>Chat</button>
          <button class="seg-btn {pendingMode === 'work' ? 'sel' : ''}" data-testid="mode-work-btn" role="tab" onclick={() => { pendingMode = "work"; writeLastMode(localStorage, "work"); if (activeChat && activeChat.messages.length === 0) { activeChat.mode = "work"; activeChat.workspace = pendingWorkspace || null; } }}>Work</button>
        </div>
        <h1 class="empty-title">{pendingMode === "work" ? "What should I work on?" : "How can I help you today?"}</h1>
        {#if pendingMode === "work"}
          <p class="engine-line" data-testid="work-engine-line">
            {#if workEngineInfo?.installed}
              Engine: {workEngineLabel} {workEngineInfo.version ?? ""}
            {:else if workEngineInfo}
              Engine: {workEngineLabel} not found
            {:else}
              Engine: checking…
            {/if}
          </p>
        {/if}
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
          <span class="work-badge engine-badge" data-testid="work-engine-badge" data-engine={engineId(workEngineInfo)} title={workEngineInfo?.installed ? `${workEngineLabel} ${workEngineInfo.version ?? ""} — ${workEngineInfo.path ?? ""}` : (workEngineInfo?.missing ?? `${workEngineLabel} not found`)}>{#if workEngineInfo && !sessionEngineMissing}{@render engineIcon(engineId(workEngineInfo))}{/if}{sessionEngineMissing ? `no ${sessionEngineMissing}` : workEngineInfo ? (workEngineInfo.installed ? (workAccountLabel ? workEngineShort : engineId(workEngineInfo)) : `no ${engineId(workEngineInfo)}`) : "…"}</span>
          <span class="hdr-meters">
          {#if usageHoverEngine}
            {@const hov = usageStore[usageHoverEngine]}
            {@const hovLoading = !!usageFetching[usageHoverEngine]}
            {@const hovLabel = workEngines.find((e) => e.id === usageHoverEngine)?.label ?? usageHoverEngine}
            {@const hovRows = hov?.report ? usageHoverRows(hov.report) : []}
            {@const gauge = usageGaugeFrac(hovRows)}
            <span class="ctx-wrap usage-wrap">
              <!-- same 18px / r=7 / 2.5 stroke as the context ring, but a 270° gauge arc (gap at the bottom) filled by the most-used quota -->
              <button class="usage-meter-btn" data-testid="usage-meter-btn" aria-label={`${hovLabel} usage${hovRows.length ? ` ${Math.round(gauge * 100)}%` : ""}`} onmouseenter={() => primeUsageHover()} onfocus={() => primeUsageHover()} onclick={() => void openUsageModal()}>
                <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
                  <circle cx="9" cy="9" r="7" fill="none" stroke="var(--border-2)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="33 44" transform="rotate(135 9 9)" />
                  {#if gauge > 0}
                    <circle cx="9" cy="9" r="7" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray={`${(gauge * 33).toFixed(1)} 44`} transform="rotate(135 9 9)" />
                  {/if}
                </svg>
              </button>
              <span class="ctx-popup usage-hover" data-testid="usage-hover">
                <span class="usage-hover-head">
                  <span class="usage-hover-title">{hovLabel}{hov?.report?.plan ? ` · ${hov.report.plan}` : ""}</span>
                  <button
                    class="usage-hover-refresh"
                    data-testid="usage-hover-refresh"
                    disabled={hovLoading}
                    onclick={(e) => { e.stopPropagation(); void fetchUsageEngine(usageHoverEngine, { force: true }); }}
                    title="Refresh usage"
                  >{hovLoading ? "Refreshing…" : "Refresh"}</button>
                </span>
                {#if hov?.report}
                  {#if hov.error}
                    <span class="usage-hover-note usage-tone-low">{hov.error}</span>
                  {/if}
                  {#each hovRows as r}
                    <span class="usage-hover-row">
                      <span class="usage-hover-line"><span class="usage-hover-label" title={r.label}>{r.label}</span><span class="usage-hover-val {r.tone ? `usage-tone-${r.tone}` : ''}">{r.value}</span></span>
                      {#if r.usedPct !== null}
                        <span class="usage-hover-bar"><span class="usage-hover-fill {r.tone ?? ''}" style="width: {r.usedPct}%"></span></span>
                      {/if}
                      {#if r.reset}<span class="usage-hover-reset">{r.reset}</span>{/if}
                    </span>
                  {:else}
                    <span class="usage-hover-note">{hov.report.note ?? "No usage reported"}</span>
                  {/each}
                {:else if hovLoading}
                  <span class="usage-hover-note">Fetching usage…</span>
                {:else if hov?.error}
                  <span class="usage-hover-note">{hov.error}</span>
                {:else}
                  <span class="usage-hover-note">Hover to load usage</span>
                {/if}
                <span class="usage-hover-hint">
                  <span data-testid="usage-hover-updated">
                    {#if hov?.report}
                      Updated {formatUsageAge(hov.at, usageNow)}{hovLoading ? " · updating…" : ""}
                    {:else if hovLoading}
                      Updating…
                    {:else}
                      Not updated yet
                    {/if}
                  </span>
                  <span>Click for details</span>
                </span>
                {#if harnessUsageUrl(baseEngineId(usageHoverEngine))}
                  <button class="usage-hover-refresh usage-provider-link" data-testid="usage-hover-provider-link" onclick={(e) => { e.stopPropagation(); void openExternal(harnessUsageUrl(baseEngineId(usageHoverEngine))!); }}>Open usage page</button>
                {/if}
              </span>
            </span>
          {/if}
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
                <span class="ctx-row"><span>Spent</span><span data-testid="ctx-spent">${workSpent.toFixed(2)}</span></span>
              </span>
            </span>
          {/if}
          </span>
        </div>
      {/if}
      <div
        class="messages"
        bind:this={messagesEl}
        data-testid="messages"
        role="log"
        aria-label="Chat messages"
        onscroll={onMessagesScroll}
        onwheel={onMessagesWheel}
        ontouchstart={onMessagesTouchStart}
        ontouchmove={onMessagesTouchMove}
        onpointerdown={onMessagesPointerDown}
      >
        {#if forkInfo && forkNoteIndex == null}
          {@render forkNote(forkInfo.title, forkInfo.id, "Continue from chat")}
        {/if}
        {#if activeLoading}
          <div class="skel-thread" data-testid="session-loading" role="status" aria-label="Loading session">
            {#each [{ u: 46 }, { a: [96, 88, 72] }, { u: 34 }, { a: [92, 80, 94, 58] }] as b, k (k)}
              {#if b.u}
                <div class="skel skel-bubble" style="width: {b.u}%"></div>
              {:else}
                <div class="skel-answer">
                  {#each b.a ?? [] as w, j (j)}<div class="skel" style="width: {w}%"></div>{/each}
                </div>
              {/if}
            {/each}
          </div>
        {/if}
        {#each activeLoading ? [] : activeChat.messages as m, i}
          {@const copyKey = `${activeId}:${i}`}
          {@const bgDone = m.role === "user" && editingIndex !== i ? parseBgDoneMessage(m.content) : null}
          {@const mcpWake = !bgDone && m.role === "user" && editingIndex !== i ? parseMcpWakeMessage(m.content) : null}
          {@const workerDone = !bgDone && !mcpWake && m.role === "user" && editingIndex !== i ? parseWorkerDone(m.content) : null}
          {#if workerDone}
            <!-- a worker's turn ended: its report wakes the lead -->
            <div class="row asst-row bg-done-row" data-msg-index={i}><div class="col">
              <details class="bg-done" data-testid="worker-done">
                <summary>
                  <span class="bg-done-mark" class:bad={m.content.includes(") stopped.")} aria-hidden="true">{m.content.includes(") stopped.") ? "■" : "✓"}</span>
                  <span class="bg-done-title">Worker report</span>
                  {#if chats.some((c) => c.id === workerDone.id)}
                    <button class="fork-link" onclick={(e) => { e.preventDefault(); selectChat(workerDone.id); }}>{workerDone.title}</button>
                  {:else}
                    <span class="bg-done-meta">{workerDone.title}</span>
                  {/if}
                </summary>
                <pre class="bg-done-out">{m.content}</pre>
              </details>
            </div></div>
          {:else if bgDone}
            <!-- a finished background job wakes the agent with a user message;
                 show it as the tool result it is, on the agent's side -->
            <div class="row asst-row bg-done-row" data-msg-index={i}><div class="col">
              <details class="bg-done" data-testid="bg-done">
                <summary>
                  <span class="bg-done-mark" class:bad={!bgDone.ok} aria-hidden="true">{bgDone.ok ? "✓" : "✕"}</span>
                  <span class="bg-done-title">Background command finished</span>
                  <span class="bg-done-meta">{bgDone.status} · {bgDone.elapsed}</span>
                  <code class="bg-done-cmd" title={bgDone.command}>{bgDone.command}</code>
                </summary>
                {#if bgDone.output}<pre class="bg-done-out">{bgDone.output}</pre>{/if}
                {#if bgDone.stderr}<div class="bg-done-label">stderr</div><pre class="bg-done-out bg-done-err">{bgDone.stderr}</pre>{/if}
                {#if !bgDone.output && !bgDone.stderr}<div class="bg-done-label">(no output)</div>{/if}
                {#if bgDone.truncated}<div class="bg-done-label">output truncated at 32KB per stream</div>{/if}
              </details>
            </div></div>
          {:else if mcpWake}
            <div class="row asst-row bg-done-row" data-msg-index={i}><div class="col">
              <details class="bg-done" data-testid="mcp-wake">
                <summary>
                  <span class="bg-done-mark" aria-hidden="true">✓</span>
                  <span class="bg-done-title">MCP server active</span>
                  <span class="bg-done-meta">{mcpWake.summary}</span>
                </summary>
                <div class="bg-done-label">{m.content}</div>
              </details>
            </div></div>
          {:else if m.contextCleared}
            <div class="row asst-row" data-msg-index={i}><div class="col">
              <div class="compact-div" data-testid="context-cleared-divider"><Tooltip text="Messages above this line stay visible but are no longer sent to the model."><span>Context cleared</span></Tooltip></div>
            </div></div>
          {:else if m.role === "user"}
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
                    <span class="edit-hint">Esc to cancel · ⌘↵ to regenerate · paste images to attach</span>
                    <button class="edit-cancel" data-testid="cancel-edit-btn" onclick={cancelEdit}>Cancel</button>
                    <button class="edit-fork" data-testid="save-only-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => saveOnlyOrConfirm()}>Save</button>
                    <button class="edit-fork" data-testid="fork-save-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => void forkAndSave()}>Fork &amp; Regenerate</button>
                    <button class="edit-save" data-testid="save-edit-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => saveOrConfirm()}>Regenerate</button>
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
            {@const thinkingActive = activeStreaming && isLast && !m.content && !continuing}
            {@const isWorkMsg = (activeChat?.mode ?? "chat") === "work"}
            <!-- work turns keep thinking collapsed between rounds, so the label
                 stays shimmering for the whole turn, not just the first stream -->
            {@const thinkingLive = (activeStreaming || activeWorkRunning) && isLast && !continuing && (thinkingActive || isWorkMsg)}
            {@const handoff = isWorkMsg ? handoffAt(activeChat.messages, i) : null}
            <div class="row asst-row {editingIndex === i ? 'is-editing' : ''}" data-msg-index={i}><div class="col">
              {#if handoff}
                {@const wsLabels = handoff.workspace ? workspaceHandoffLabels(handoff.workspace.from, handoff.workspace.to) : null}
                {@const handoffTip = handoff.engine && handoff.workspace
                  ? `The new harness and workspace (${handoff.workspace.from} → ${handoff.workspace.to}) start a fresh session: it gets Vicuna's transcript of this conversation, not the previous session's own context.`
                  : handoff.workspace
                    ? `The new workspace (${handoff.workspace.from} → ${handoff.workspace.to}) starts its own session: it gets Vicuna's transcript of this conversation, not the previous workspace's own context.`
                    : "The new harness starts its own session: it gets Vicuna's transcript of this conversation, not the previous harness's own context."}
                <div class="compact-div handoff-div" data-testid="handoff-divider">
                  <Tooltip text={handoffTip}><span class="handoff-inner"><svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M17 3l4 4-4 4"/><path d="M3 7h18"/><path d="M7 21l-4-4 4-4"/><path d="M21 17H3"/></svg> Context handoff{#if handoff.engine} <span class="handoff-part"><span class="engine-chip">{@render engineIcon(labelHarness(handoff.engine.from) ?? "")}</span> {modelLabel(handoff.engine.from)} →</span> <span class="handoff-part"><span class="engine-chip">{@render engineIcon(labelHarness(handoff.engine.to) ?? "")}</span> {modelLabel(handoff.engine.to)}</span>{/if}{#if handoff.engine && wsLabels} <span class="handoff-sep">·</span>{/if}{#if wsLabels} <span class="handoff-part"><span class="ws-handoff-chip"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg></span> {wsLabels.from} →</span> <span class="handoff-part"><span class="ws-handoff-chip"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg></span> {wsLabels.to}</span>{/if}</span></Tooltip>
                </div>
              {/if}
              {#if i === activeSummaryIndex}
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
                    <button class="edit-save" data-testid="save-edit-btn" disabled={editDraft.trim() === "" && editImages.length === 0} onclick={() => saveOnlyOrConfirm()}>Save</button>
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
              <div class="answer">{#if m.content}{@html renderMarkdown(m.content)}{:else if activeStreaming && isLast && !m.thinking}<span class="typing" aria-label="Generating"><span></span><span></span><span></span></span>{/if}</div>
              {#if pendingAsk && pendingAsk.index === i && activeId != null}
                {@const askCard = pendingAsks[activeId]}
                {#if askCard}
                <div class="ask-card" data-testid="ask-card">
                  <div class="ask-question">{askCard.question}</div>
                  {#if askCard.agentQuestion}
                    {@render agentAskBody(askCard, "ask", () => sendAskBox(askCard), dismissAsk, answerLater)}
                  {:else}
                  <div class="ask-options">
                    {#each askCard.options as opt, oi}
                      <button class="ask-option" data-testid="ask-option-{oi}" onclick={() => answerAsk(opt)}>{opt}</button>
                    {/each}
                    {#if askCard.otherOpen}
                      <form
                        class="ask-other"
                        onsubmit={(e) => {
                          e.preventDefault();
                          const v = askCard.text.trim();
                          if (v) answerAsk(v);
                        }}
                      >
                        <input
                          class="ask-input"
                          data-testid="ask-input"
                          bind:value={askCard.text}
                          placeholder="Type your answer…"
                          aria-label="Custom answer"
                          onkeydown={(e) => {
                            if (e.key === "Escape") {
                              e.preventDefault();
                              e.stopPropagation();
                              askCard.otherOpen = false;
                            }
                          }}
                        />
                        <button class="ask-send" type="submit" data-testid="ask-send" disabled={!askCard.text.trim()}>Send</button>
                        <button class="ask-dismiss" type="button" data-testid="ask-dismiss" onclick={dismissAsk}>Dismiss</button>
                      </form>
                    {:else}
                      <div class="ask-other ask-other-closed">
                        <button
                          class="ask-option ask-other-toggle"
                          data-testid="ask-other"
                          onclick={() => {
                            askCard.otherOpen = true;
                            requestAnimationFrame(() => document.querySelector<HTMLInputElement>('[data-testid="ask-input"]')?.focus());
                          }}
                        >Other…</button>
                        <button class="ask-dismiss" type="button" data-testid="ask-dismiss" onclick={dismissAsk}>Dismiss</button>
                      </div>
                    {/if}
                  </div>
                  {/if}
                </div>
                {/if}
              {/if}
              {#each laterAsks.filter((a) => a.chatId === activeId && a.index === i) as la (la.id)}
                <div class="ask-card ask-card-later" data-testid="later-ask-card">
                  <div class="ask-later-tag">Answer later · the agent isn't waiting; your answer goes to it as a message</div>
                  <div class="ask-question">{la.question}</div>
                  {@render agentAskBody(la, "later-ask", () => void sendLaterAsk(la.id), () => { laterAsks = laterAsks.filter((x) => x.id !== la.id); }, null)}
                </div>
              {/each}
              <div class="actions">
                {#if !isWorkMsg}
                  <button class="act" data-testid="copy-btn" aria-label="Copy message" title="Copy" onclick={() => copyMessage(i)}>
                    {#if copiedKey === copyKey}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>{/if}
                  </button>
                {/if}
                <button class="act" data-testid="edit-btn" aria-label="Edit message" title="Edit" disabled={busy} onclick={() => startEdit(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
                </button>
                {#if isLast}
                  {@const noContinue = isCloudModel(selectedModel) || isOpenRouterSelection(selectedModel)}
                  {@const isWork = (activeChat?.mode ?? "chat") === "work"}
                  {#if !isWork}
                    <button class="act" data-testid="continue-btn" aria-label="Continue generating" title={noContinue ? "Continue isn't supported for this model — use Regenerate or a new message" : "Continue"} disabled={activeStreaming || noContinue} onclick={() => continueFrom(i)}>
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
                  {@const liveTurn = activeWorkRunning && isLast}
                  <span class="model-tag" data-testid="msg-model" title="Generated by {modelLabel(m.model)}">{modelLabel(m.model)}{#if !liveTurn && m.genMs != null && m.genMs > 0}{" "}{formatDuration(m.genMs)}{#if m.genTps != null}{" "}{m.genTps.toFixed(1)} tok/s{/if}{/if}</span>
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
      <div class="err" data-testid="error-box"><span>{error}</span><button type="button" class="err-x" data-testid="error-dismiss" aria-label="Dismiss error" title="Dismiss" onclick={() => (error = null)}>×</button></div>
    {/if}
    {#if activeId != null && toolStatuses[activeId]}
      <div class="tool-status" data-testid="tool-status">
        <span class="tool-status-label shimmer">{toolStatuses[activeId]}</span>
      </div>
    {/if}

    <div class="composer-wrap">
      {#if notice}
        <div class="notice" data-testid="notice-box" role="status"><span>{notice}</span><button type="button" class="notice-x" data-testid="notice-dismiss" aria-label="Dismiss notice" title="Dismiss" onclick={() => (notice = null)}>×</button></div>
      {/if}
      {#if queuedForActive.length}
        <div class="queue-strip" data-testid="queue-strip">
          {#each queuedForActive as q (q.id)}
              <div class="queue-chip" data-testid="queue-chip">
                <span class="queue-text">{parseBgDoneMessage(q.msg.content) ? `Background command finished: ${parseBgDoneMessage(q.msg.content)?.command}` : parseMcpWakeMessage(q.msg.content) ? `MCP server active: ${parseMcpWakeMessage(q.msg.content)?.summary}` : parseWorkerDone(q.msg.content) ? `Worker report: ${parseWorkerDone(q.msg.content)?.title}` : q.msg.content || "Image"}</span>
                <button class="queue-now" data-testid="queue-send-now" aria-label="Send this message now" title="Send now (interrupts the agent's current step)" onmousedown={(e) => e.preventDefault()} onclick={(e) => { e.stopPropagation(); sendQueuedNow(q.id); }}><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 19V5M5 12l7-7 7 7"/></svg></button>
                <button class="queue-x" data-testid="queue-remove" aria-label="Remove queued message" title="Remove from queue" onmousedown={(e) => e.preventDefault()} onclick={(e) => { e.stopPropagation(); unqueue(q.id); }}>×</button>
              </div>
          {/each}
          <span class="queue-hint">queued · joins when the agent finishes · ↑ sends now</span>
        </div>
      {/if}
      {#if busy}
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
            onblur={() => {
              workspaceEditing = false;
              const chat = activeChat;
              const path = pendingWorkspace;
              if (!path.trim()) return;
              void validateWorkspace(path).then((e) => {
                workspaceError = e;
                if (!e) void persistSessionWorkspace(chat, path);
              });
            }}
          />
          {#if workspaceError}
            <div class="goto-error" data-testid="workspace-inline-error">{workspaceError}</div>
          {/if}
        </div>
      {/if}
      {#if composerMode === "work" && !workGate.blocked && workEngineInfo?.warning}
        <div class="opencode-missing" data-testid="opencode-outdated" role="status">
          <span>{workEngineInfo.warning}</span>
          <button class="mini-btn" data-testid="opencode-recheck-outdated" onclick={() => void refreshWorkEngine()}>Check again</button>
        </div>
      {/if}
      {#if composerMode === "work" && workGate.blocked && workEngineChecked}
        <div class="opencode-missing" data-testid="opencode-missing" role="status">
          <span>{workGate.reason ?? `${workEngineLabel} not found`}</span>
          {#if workEngineInfo && !workEngineInfo.installed}
            <button class="mini-btn" data-testid="opencode-install-link" onclick={() => void openExternal(workEngineInfo?.installUrl ?? "https://opencode.ai/docs/#install")}>Install guide</button>
          {/if}
          {#each workEngines as eng (eng.id)}
            <button class="mini-btn" data-testid="engine-fallback" data-engine={eng.id} onclick={() => void pickWorkEngine(eng.id)}>Use {eng.label}</button>
          {/each}
          <button class="mini-btn" data-testid="opencode-recheck" onclick={() => void refreshWorkEngine()}>Check again</button>
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
            placeholder={composerMode === "work" ? (activeWorkRunning ? "Queue a follow-up — it joins at the next tool call…" : "Ask to explore, run, or change workspace files… ( / to focus)") : "Send a message ( / to focus)"}
            bind:this={composerEl}
            bind:value={input}
            onkeydown={onKey}
            onpaste={onPaste}
            onscroll={syncBackdropScroll}
            oninput={() => {
              caretHopped = false;
              // typing leaves history browsing (the recalled text is now a draft)
              historyIndex = null;
              historyDraft = "";
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
                class="mention-item {i === slash.index ? 'sel' : ''} {item.disabled ? 'disabled' : ''}"
                data-testid="slash-item"
                data-disabled={item.disabled ? "true" : "false"}
                role="option"
                aria-selected={i === slash.index}
                aria-disabled={item.disabled}
                disabled={item.disabled}
                title={item.disabled ? `${item.desc} — unavailable right now` : item.desc}
                onmousedown={(e) => {
                  // before blur: keep focus in the composer
                  e.preventDefault();
                  if (item.disabled) return;
                  if (slash) slash.index = i;
                  runSlash();
                }}
                onmousemove={() => {
                  if (slash && !item.disabled && slash.index !== i) slash.index = i;
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
              title={mcpEnabledCount ? `MCP servers (${mcpEnabledCount} enabled)` : "MCP servers"}
              aria-pressed={mcpOpen}
              onclick={openMcpPopup}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14.7 6.3a4.5 4.5 0 0 0-6 6L3 18l3 3 5.7-5.7a4.5 4.5 0 0 0 6-6L14 13l-3-3 3.7-3.7z"/></svg>
              {#if mcpEnabledCount}
                <span class="mcp-dot" data-testid="mcp-dot">{mcpEnabledCount}</span>
              {/if}
            </button>
            <button
              class="icon-btn mcp-btn"
              data-testid="skills-btn"
              aria-label="Skills"
              title={skillsEnabledCount ? `Skills (${skillsEnabledCount} enabled)` : "Skills"}
              aria-pressed={skillsOpen}
              onclick={openSkillsModal}
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.1L19 10l-5.1 1.9L12 17l-1.9-5.1L5 10l5.1-1.9L12 3z"/><path d="M5 19l1 2.5L8.5 22 6 23l-1 1-1-1-2.5-1L4 21.5 5 19z"/></svg>
              {#if skillsEnabledCount}
                <span class="mcp-dot" data-testid="skills-dot">{skillsEnabledCount}</span>
              {/if}
            </button>
            {@const rawWsPath = activeChat && activeChat.messages.length > 0 && activeChat.mode === "work" ? (activeChat.workspace ?? pendingWorkspace) : pendingWorkspace}
            {@const wsIsNone = isNoWorkspacePath(rawWsPath)}
            {@const wsPath = wsIsNone ? "" : displayWorkspace(rawWsPath)}
            {@const wsName = wsPath.trim() ? (workspaceBase(wsPath) || wsPath.trim()) : ""}
            <div class="ws-split">
              <button
                class="model-pill ws-pill {wsIsNone ? 'ws-pill-none' : ''}"
                data-testid="workspace-pill"
                title={wsIsNone ? undefined : (wsPath.trim() ? `Workspace: ${wsPath.trim()} — click to change` : "Pick a workspace folder")}
                aria-label="Pick workspace folder"
                aria-describedby={wsIsNone && !wsMenuOpen ? "workspace-pill-tip" : undefined}
                onclick={() => { wsMenuOpen = false; void pickWorkspace(); }}
              >
                {#if wsIsNone}
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M5.6 5.6l12.8 12.8"/></svg>
                  <span class="model-name">No workspace</span>
                {:else}
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                  <span class="model-name">{wsName || "Workspace"}</span>
                {/if}
              </button>
              {#if wsIsNone && !wsMenuOpen}
                <span class="ws-tooltip" id="workspace-pill-tip" role="tooltip">
                  Ready to chat without a project folder. Vicuna uses a temporary empty scratch folder if needed.
                  <span class="ws-tooltip-hint">Click to pick a folder.</span>
                </span>
              {/if}
              <button
                class="model-pill ws-caret"
                data-testid="workspace-menu-btn"
                aria-label="Workspace options"
                aria-haspopup="menu"
                aria-expanded={wsMenuOpen}
                title="Workspace options"
                onclick={() => (wsMenuOpen = !wsMenuOpen)}
              >
                <svg width="11" height="11" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m4 6 4 4 4-4"/></svg>
              </button>
              {#if wsMenuOpen}
                <button class="sched-backdrop" data-testid="workspace-menu-backdrop" aria-label="Close workspace menu" onclick={() => (wsMenuOpen = false)} tabindex="-1"></button>
                <div class="ws-menu" data-testid="workspace-menu" role="menu" aria-label="Workspace options">
                  <button
                    class="ctx-item ws-menu-opt"
                    role="menuitem"
                    data-testid="workspace-menu-pick"
                    onclick={() => { wsMenuOpen = false; void pickWorkspace(); }}
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                    <span>Pick folder…</span>
                  </button>
                  <button
                    class="ctx-item ws-menu-opt {wsIsNone ? 'sel' : ''}"
                    role="menuitemcheckbox"
                    aria-checked={wsIsNone}
                    aria-describedby="workspace-menu-none-tip"
                    data-testid="workspace-menu-none"
                    onclick={() => void selectNoWorkspace()}
                  >
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><path d="M5.6 5.6l12.8 12.8"/></svg>
                    <span>No workspace</span>
                    {#if wsIsNone}
                      <svg class="ws-menu-check" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 8.5l3 3 7-7"/></svg>
                    {/if}
                  </button>
                  <span class="ws-tooltip" id="workspace-menu-none-tip" role="tooltip">
                    Chat without a project folder. Vicuna uses a temporary empty scratch folder if needed.
                  </span>
                </div>
              {/if}
            </div>
            {#if workspacePickerOpen && (!isTauriRuntime() || onServer) && !ctxMenu}
              <span class="model-pill-wrap">
                <button class="model-backdrop" data-testid="workspace-picker-backdrop" aria-label="Close workspace picker" onclick={dismissWorkspacePicker} tabindex="-1"></button>
                <form
                  class="model-drop workspace-picker-drop"
                  data-testid="workspace-picker"
                  onsubmit={(e) => {
                    e.preventDefault();
                    void commitWorkspacePicker();
                  }}
                >
                  <div class="workspace-picker-label">Workspace absolute path</div>
                  <input
                    class="pull-input"
                    data-testid="workspace-picker-input"
                    placeholder={isWindows ? "C:\\path\\to\\workspace" : "/absolute/path/to/workspace"}
                    bind:value={workspaceDraft}
                    spellcheck={false}
                    bind:this={workspacePickerInput}
                    onkeydown={(e) => {
                      if (e.key === "Escape") {
                        e.preventDefault();
                        e.stopPropagation();
                        closeWorkspacePicker();
                      }
                    }}
                  />
                  {#if workspaceError}
                    <div class="goto-error" data-testid="workspace-picker-error">{workspaceError}</div>
                  {/if}
                </form>
              </span>
            {/if}
          {/if}
          {#if composerMode === "work"}
          <div class="model-pill-wrap">
            <button class="model-pill" data-testid="model-selector" data-engine={engineId(workEngineInfo)} title={workModel ? `${workEngineLabel}: ${workModel}` : `${workEngineLabel} default model`} disabled={!workEngineInfo?.installed && workEngines.length === 0} onclick={toggleModel}>
              {#if workEngineList.length > 1}<span class="engine-chip" data-testid="work-engine-chip" title={workEngineLabel}>{@render engineIcon(engineId(workEngineInfo))}</span>{/if}
              <span class="model-name" data-testid="work-model-name">{sessionEngineMissing ? "Pick a harness" : (workModel ?? `${workEngineShort} default`)}</span>
              <svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 6l4 4 4-4"/></svg>
            </button>
            {#if modelOpen}
              <button class="model-backdrop" data-testid="model-backdrop" aria-label="Close model menu" onclick={() => (modelOpen = false)} tabindex="-1"></button>
              <div class="model-drop" data-testid="model-dropdown" data-engine={engineId(workEngineInfo)}>
                {#if workEngineList.length > 1 || (workEngineList[0]?.id === "agy" && workEngineList[0].installed && accountEngines.includes("agy"))}
                  <div class="engine-select" data-testid="work-engine-picker">
                    <button class="engine-select-btn" data-testid="work-engine-toggle" aria-haspopup="listbox" aria-expanded={engineMenuOpen} aria-label="Work engine" onclick={() => (engineMenuOpen = !engineMenuOpen)}>
                      {@render engineIcon(engineId(workEngineInfo))}<span>{workEngineLabel}</span>
                      <svg class="engine-select-chev {engineMenuOpen ? 'open' : ''}" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M4 6l4 4 4-4"/></svg>
                    </button>
                    {#if engineMenuOpen}
                      <div class="engine-select-list" role="listbox" aria-label="Work engine">
                        {#each workEngineList as eng (eng.id)}
                          {@const acct = eng.account}
                          {@const base = baseEngineId(eng.id)}
                          {@const needsSignIn = !!acct && !acct.signedIn}
                          <div class="engine-opt-row {acct ? 'is-account' : ''}">
                            <button class="engine-select-opt {eng.id === engineId(workEngineInfo) ? 'sel' : ''}" role="option" aria-selected={eng.id === engineId(workEngineInfo)} data-testid="work-engine-opt" data-engine={eng.id} disabled={!eng.installed} title={needsSignIn ? "Not signed in yet: click for the sign-in command" : eng.installed ? `${eng.label} ${eng.version ?? ""}`.trim() : (eng.missing ?? "not installed")} onclick={() => { if (!eng.installed) return; if (needsSignIn) { accountSignInCopied = false; accountSignInFor = accountSignInFor?.engine === acct.engine ? null : acct; return; } engineMenuOpen = false; void pickWorkEngine(eng.id); }}>{@render engineIcon(eng.id)}<span>{acct ? acct.label : eng.label}{eng.installed ? "" : " (not installed)"}</span>{#if !acct && enginesWithAccounts.has(eng.id)}<span class="engine-acct-tag" data-testid={`${base}-default-tag`} title={`Your default ${eng.label} login`}>Default</span>{/if}{#if needsSignIn}<span class="engine-signin-hint" data-testid={`${base}-account-signin-hint`}>Sign in →</span>{/if}{#if eng.id === engineId(workEngineInfo)}<svg class="engine-select-check" width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 8.5l3 3 7-7"/></svg>{/if}</button>
                            {#if eng.installed && accountEngines.includes(eng.id)}
                              <button class="engine-acct-btn" data-testid={`${base}-account-add`} title={`Add another ${eng.label} account`} aria-label={`Add another ${eng.label} account`} onclick={() => { accountAddFor = accountAddFor === eng.id ? null : eng.id; accountRenameFor = null; accountAddName = ""; accountError = null; }}>+</button>
                            {:else if acct}
                              <button class="engine-acct-btn" data-testid={`${base}-account-rename`} data-account={acct.id} title={`Rename ${acct.label} (keeps its sign-in)`} aria-label={`Rename account ${acct.label}`} onclick={() => startWorkAccountRename(acct)}><svg width="12" height="12" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linejoin="round"><path d="M10.5 2.5l3 3L5 14H2v-3z"/></svg></button>
                              <button class="engine-acct-btn {accountRemoveArmed === acct.engine ? 'armed' : ''}" data-testid={`${base}-account-remove`} data-account={acct.id} title={accountRemoveArmed === acct.engine ? "Click again to remove (its login stays on disk)" : `Remove ${acct.label} from Vicuna`} aria-label={`Remove account ${acct.label}`} onclick={() => void removeWorkAccountRow(acct)}>{accountRemoveArmed === acct.engine ? "Remove?" : "−"}</button>
                            {/if}
                          </div>
                          {#if acct && accountRenameFor === acct.engine}
                            <form class="engine-acct-form" data-testid={`${base}-account-rename-form`} onsubmit={(e) => { e.preventDefault(); void renameWorkAccountRow(); }}>
                              <!-- svelte-ignore a11y_autofocus -->
                              <input class="engine-acct-input" data-testid={`${base}-account-rename-name`} maxlength="40" bind:value={accountRenameName} autofocus onfocus={(e) => (e.currentTarget as HTMLInputElement).select()} onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); accountRenameFor = null; } }} />
                              <button class="mini-btn" type="submit" data-testid={`${base}-account-rename-save`} disabled={!accountRenameName.trim() || accountRenameName.trim() === acct.label || accountBusy}>Rename</button>
                            </form>
                          {/if}
                          {#if acct && accountSignInFor?.engine === acct.engine}
                            <div class="engine-signin-card" data-testid={`${base}-signin-card`} use:revealCard>
                              <p class="engine-signin-title">Sign in to <b>{accountSignInFor.label}</b></p>
                              <p class="engine-signin-text">Run this in any terminal on {isRemoteServer() ? "the server" : "this computer"}, then sign in with the account you want to use.{#if base !== "agy" && /\$env:/.test(accountSignInFor.signInCommand)} Use a new PowerShell window for this command.{/if}</p>
                              <div class="engine-signin-cmd-wrap">
                                <code class="engine-signin-cmd" data-testid={`${base}-signin-command`}>{accountSignInFor.signInCommand}</code>
                                <button class="engine-signin-copy {accountSignInCopied ? 'done' : ''}" data-testid={`${base}-signin-copy`} title={accountSignInCopied ? "Copied" : "Copy command"} aria-label={accountSignInCopied ? "Copied" : "Copy command"} onclick={() => void copyWorkAccountSignIn()}>
                                  {#if accountSignInCopied}
                                    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.8"><path d="M3 8.5l3 3 7-7"/></svg>
                                  {:else}
                                    <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M11 5V3.5A1.5 1.5 0 0 0 9.5 2h-6A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5"/></svg>
                                  {/if}
                                </button>
                              </div>
                              <div class="engine-signin-actions">
                                <button class="mini-btn" data-testid={`${base}-signin-check`} onclick={() => void checkWorkAccountSignIn()}>I've signed in</button>
                                <button class="mini-btn" data-testid={`${base}-signin-close`} onclick={() => (accountSignInFor = null)}>Later</button>
                              </div>
                            </div>
                          {/if}
                          {#if accountAddFor === eng.id}
                            <form class="engine-acct-form" data-testid={`${base}-account-form`} onsubmit={(e) => { e.preventDefault(); void addWorkAccountRow(); }}>
                              <!-- svelte-ignore a11y_autofocus -->
                              <input class="engine-acct-input" data-testid={`${base}-account-name`} placeholder="Account name, e.g. Free or Work" maxlength="40" bind:value={accountAddName} autofocus onkeydown={(e) => { if (e.key === "Escape") { e.stopPropagation(); accountAddFor = null; } }} />
                              <button class="mini-btn" type="submit" data-testid={`${base}-account-create`} disabled={!accountAddName.trim() || accountBusy}>Add</button>
                            </form>
                          {/if}
                        {/each}
                        {#if accountError}<div class="engine-acct-error" data-testid="work-account-error">{accountError}</div>{/if}
                      </div>
                    {/if}
                  </div>
                {/if}
                <input
                  class="model-search"
                  data-testid="model-search"
                  placeholder={`Find ${workEngineLabel} model...`}
                  bind:value={modelQuery}
                  autofocus
                />
                <div class="model-list-drop">
                  {#if ocModelsLoading && !ocModels}
                    <div class="no-match" data-testid="opencode-models-loading">Loading models from {workEngineLabel}…</div>
                  {:else if ocModelsError}
                    <div class="no-match" data-testid="opencode-models-error">{ocModelsError}</div>
                  {/if}
                  <div class="model-opt-row {workModel === null ? 'sel' : ''}">
                    <button class="model-opt" data-testid="model-opt" data-model="" onclick={() => void pickWorkModel(null)}>
                      <span class="model-opt-top"><span class="model-opt-name">{workEngineShort} default</span></span>
                      <span class="model-opt-id">whatever {workEngineLabel}'s own config selects</span>
                    </button>
                  </div>
                  {#each workModelChoices as choice (choice.id)}
                    <div class="model-opt-row {choice.id === workModel ? 'sel' : ''}">
                      <button class="model-opt" data-testid="model-opt" data-provider={choice.provider} data-model={choice.id} title={choice.id} onclick={() => void pickWorkModel(choice.id)}>
                        <span class="model-opt-top"><span class="model-opt-name">{choice.name ?? choice.id}</span></span>
                        {#if choice.name && choice.name !== choice.id}<span class="model-opt-id">{choice.id}</span>{/if}
                      </button>
                    </div>
                  {:else}
                    {#if ocModels}<div class="no-match">No matches</div>{/if}
                  {/each}
                </div>
              </div>
            {/if}
          </div>
          {:else}
          <div class="model-pill-wrap">
            <button class="model-pill" data-testid="model-selector" onclick={toggleModel}>
              <span class="prov-ico" aria-hidden="true">
                {#if isOpenRouterSelection(selectedModel)}
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OPENROUTER_ICON_PATH}/></svg>
                {:else}
                  <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OLLAMA_ICON_PATH}/></svg>
                {/if}
              </span>
              <span class="model-name" title={selectedModel}>{modelLabel(selectedModel)}</span>
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
                  {#each filteredModelChoices as choice (choice.id)}
                    <div class="model-opt-row {choice.id === selectedModel ? 'sel' : ''}">
                      <button
                        class="model-opt"
                        data-testid="model-opt"
                        data-provider={choice.provider}
                        data-model={choice.id}
                        title={choice.sub || choice.name}
                        onclick={() => void pickChatModel(choice.id)}
                      >
                        <span class="model-opt-top">
                          <span class="prov-ico" aria-hidden="true">
                            {#if choice.provider === "openrouter"}
                              <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OPENROUTER_ICON_PATH}/></svg>
                            {:else}
                              <svg width="14" height="14" viewBox="0 0 24 24" fill="currentColor"><path d={OLLAMA_ICON_PATH}/></svg>
                            {/if}
                          </span>
                          <span class="model-opt-name">{choice.name}</span>
                        </span>
                        {#if choice.sub}<span class="model-opt-id">{choice.sub}</span>{/if}
                      </button>
                      {#if choice.provider === "ollama" && !isCloudModel(choice.id)}
                        {@const loaded = runningNames.has(choice.id)}
                        <button
                          class="model-mem-btn"
                          class:is-loaded={loaded}
                          data-testid={loaded ? "menu-unload-model-btn" : "menu-load-model-btn"}
                          data-name={choice.id}
                          title={loaded ? `Unload ${choice.id}` : `Load ${choice.id}`}
                          aria-label={loaded ? `Unload ${choice.id}` : `Load ${choice.id}`}
                          disabled={modelOp !== null || pulling !== null}
                          onclick={(e) => { e.stopPropagation(); void toggleModelLoaded(choice.id, loaded); }}
                        ><span class="mem-ico" class:mem-spin={modelOp === choice.id}>{#if loaded}<svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 2.5 13.5 9.5h-11z"/><rect x="3" y="11" width="10" height="2" rx="1"/></svg>{:else}<svg width="13" height="13" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true"><path d="M8 13.5 2.5 6.5h11z"/><rect x="3" y="3" width="10" height="2" rx="1"/></svg>{/if}</span></button>
                      {/if}
                    </div>
                  {:else}
                    <div class="no-match">No matches</div>
                  {/each}
                </div>
              </div>
            {/if}
          </div>
          {/if}
          {#if thinkLevels.length > 0 && composerMode !== "work"}
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
          {#if activeWorkRunning}
            <div class="send-split">
              <button class="send" data-testid="queue-btn" aria-label="Queue message" title="Queue for the next tool call" disabled={input.trim() === "" && attachImages.length === 0} onclick={send}>↑</button>
              {@render scheduleCaret(input.trim() === "" && attachImages.length === 0)}
            </div>
            <button class="send streaming" data-testid="stop-btn" aria-label="Stop" title="Stop (Esc twice)" onclick={stop}>■</button>
          {:else if activeStreaming}
            <button class="send streaming" data-testid="stop-btn" aria-label="Stop" onclick={stop}>■</button>
          {:else if composerMode === "work"}
            <div class="send-split">
              <button class="send" data-testid="send-btn" aria-label="Send" title={workGate.blocked ? (workGate.reason ?? "") : undefined} disabled={(input.trim() === "" && attachImages.length === 0) || workGate.blocked} onclick={send}>↑</button>
              {@render scheduleCaret(input.trim() === "" && attachImages.length === 0)}
            </div>
          {:else}
            <button class="send" data-testid="send-btn" aria-label="Send" disabled={input.trim() === "" && attachImages.length === 0} onclick={send}>↑</button>
          {/if}
        </div>
      </div>
    </div>
    {#if mcpOpen}
      <McpPopup
        bind:mcpOpen
        bind:mcpView
        bind:mcpQuery
        bind:mcpSel
        bind:mcpBuiltinSel
        bind:mcpBuiltinError
        bind:mcpSearchEl
        {mcpShowForm}
        bind:mcpForm
        {mcpFormError}
        {mcpConfirmDelete}
        {mcpHasLogin}
        {mcpSavingDefaults}
        {mcpDefaultFeedback}
        {mcpBusy}
        {mcpServers}
        {mcpFiltered}
        {mcpLive}
        {builtinShown}
        bind:mcpPatFor
        bind:mcpPatToken
        {mcpPatError}
        {findMcpServer}
        {builtinInstalled}
        {builtinStatus}
        {builtinPrimary}
        {mcpIsBusy}
        {mcpMoveSel}
        {mcpActivateSelected}
        {mcpStatusOf}
        {toggleMcpServer}
        {signInMcpServer}
        {openMcpForm}
        {closeMcpForm}
        {saveMcpForm}
        {deleteMcpServerEntry}
        {signOutMcp}
        {setMcpDefaults}
        {refreshMcpTools}
        {openTokenPage}
        {savePatAndConnect}
      />
    {/if}
  </main>
  {#if composerMode === "work"}
    <SidePanel
      bind:explorerOpen
      bind:explorerWidth
      {expResizing}
      explorerMinW={EXPLORER_MIN_W}
      explorerDefaultW={EXPLORER_DEFAULT_W}
      {sideTabs}
      {activeSideTab}
      bind:newTabMenu
      {explorerRoot}
      {explorerRootBase}
      bind:explorerQuery
      {explorerLoading}
      {explorerError}
      {explorerVisible}
      {expRenaming}
      bind:expRenameDraft
      {collapsedDirs}
      {pluginPanels}
      {gitRefreshTick}
      {onServer}
      {browserTabVisible}
      {browserStarting}
      {browserKillArmed}
      {browserDaemon}
      {browserProxyUrl}
      {browserFrameRev}
      {browserFrameSrc}
      {browserStartDisplay}
      {browserDownload}
      {browserStartSeconds}
      {browserError}
      {expNotice}
      {displayWorkspace}
      {formatSize}
      {panelFrame}
      onExpResizeStart={onExpResizeStart}
      onExpResizeKey={onExpResizeKey}
      onSwitchSideTab={switchSideTab}
      onCloseSideTab={closeSideTab}
      onRestartBrowserFromButton={restartBrowserFromButton}
      onRestartBrowserDaemon={restartBrowserDaemon}
      onEnsureBrowserDaemon={ensureBrowserDaemon}
      onOpenSideExplorer={openSideExplorer}
      onOpenSideTerminal={openSideTerminal}
      onOpenSideBrowser={openSideBrowser}
      onOpenSideGit={openSideGit}
      onOpenSidePanel={openSidePanel}
      onLoadExplorer={loadExplorer}
      onCommitExpRename={commitExpRename}
      onCancelExpRename={cancelExpRename}
      onToggleExplorerDir={toggleExplorerDir}
      onOpenExpCtx={openExpCtx}
      onOpenInEditor={openInEditor}
      onRevealWorkPath={revealWorkPath}
    />
  {/if}
</div>
{#if termCtx}
  <button
    class="ctx-backdrop"
    data-testid="terminal-menu-backdrop"
    aria-label="Close terminal menu"
    onclick={() => (termCtx = null)}
    oncontextmenu={(e) => {
      e.preventDefault();
      termCtx = null;
    }}
    tabindex="-1"
  ></button>
  <div class="ctx-menu" data-testid="terminal-menu" style="left: {termCtx.x}px; top: {termCtx.y}px;">
    <button class="ctx-item" data-testid="terminal-copy" disabled={!termCtx.sel} onclick={() => termMenu("copy")}>Copy</button>
    <button class="ctx-item" data-testid="terminal-paste" onclick={() => termMenu("paste")}>Paste</button>
    <button class="ctx-item" data-testid="terminal-select-all" onclick={() => termMenu("selectAll")}>Select All</button>
    <button class="ctx-item" data-testid="terminal-clear" onclick={() => termMenu("clear")}>Clear</button>
  </div>
{/if}
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
    {#if !expCtx.isDir}
      <button class="ctx-item" data-testid="explorer-open-external" onclick={() => void expOpenExternal()}>Open in default app</button>
    {/if}
    {#if onServer && !expCtx.isDir}
      <button class="ctx-item" data-testid="explorer-download" onclick={() => void expDownload()}>Download</button>
    {:else}
      <button class="ctx-item" data-testid="explorer-reveal" onclick={() => void expReveal()}>Reveal in Finder</button>
    {/if}
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
{#if showUpdatePrompt && availableAppUpdate}
  <UpdatePromptModal
    {availableAppUpdate}
    {appUpdateStatus}
    {appUpdateError}
    downloadProgress={appUpdateProgress}
    onInstall={appUpdateStatus === "installed" ? restartToApplyUpdate : installAppUpdate}
    onSkipVersion={skipAvailableAppUpdateVersion}
    onRemindLater={remindAboutAppUpdateLater}
    onClose={remindAboutAppUpdateLater}
  />
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
{#if folderPicker}
  <FolderPickerModal
    start={folderPicker.start}
    where={onServer ? srvLabel : ""}
    pickError={folderPickError}
    onPick={onFolderPicked}
    onClose={() => (folderPicker = null)}
  />
{/if}
{#if leadPrompt}
  {@const lp = leadPrompt}
  <TelegramModal
    sessionId={lp.id}
    sessionTitle={chats.find((c) => c.id === lp.id)?.title ?? ""}
    lead={{ prevTitle: lp.prevTitle, onConfirm: () => {
      // read the id BEFORE clearing: `lp` is a derived of leadPrompt and goes null with it
      const id = lp.id;
      leadPrompt = null;
      void toggleLead(id).then(refreshTelegramStatus);
    } }}
    onClose={() => { leadPrompt = null; void refreshTelegramStatus(); }}
  />
{/if}
{#if permissionsFor}
  {@const permChat = chats.find((c) => c.id === permissionsFor)}
  {#if permChat}
    <button class="confirm-backdrop" data-testid="permissions-backdrop" aria-label="Close permissions" onclick={() => (permissionsFor = null)} tabindex="-1"></button>
      <div class="confirm-card permissions-modal" role="dialog" aria-modal="true" aria-labelledby="permissions-title" data-testid="permissions-modal">
        <button class="modal-x" aria-label="Close permissions" onclick={() => (permissionsFor = null)}>×</button>
        <h2 id="permissions-title">Permissions · {permChat.title}</h2>
        <label class="permission-row"><input type="checkbox" data-testid="permission-read" checked={sessionPermissions(permChat).read} disabled={permissionBusy} onchange={(e) => void setSessionPermission("read", e.currentTarget.checked)} /><span><b>Read other sessions</b><small>Read another session’s history or state.</small></span></label>
        <label class="permission-row"><input type="checkbox" data-testid="permission-message" checked={sessionPermissions(permChat).message} disabled={permissionBusy} onchange={(e) => void setSessionPermission("message", e.currentTarget.checked)} /><span><b>Message other sessions</b><small>Send messages to other sessions and start turns.</small></span></label>
      </div>
  {/if}
{/if}
{#if telegramFor}
  <TelegramModal
    sessionId={telegramFor}
    sessionTitle={chats.find((c) => c.id === telegramFor)?.title ?? ""}
    allStatuses={telegramStatuses}
    sessions={chats.map((c) => ({ id: c.id, title: c.title, archived: !!c.archived }))}
    onSelectSession={(id) => { telegramFor = id; void refreshTelegramStatus(); }}
    onClose={() => { telegramFor = null; void refreshTelegramStatus(); }}
  />
{/if}
{#if bridgeModal}
  {@const activeBridgePlugin = bridgePlugins.find((p) => p.id === bridgeModal!.pluginId)}
  {#if activeBridgePlugin}
    <BridgeModal
      plugin={activeBridgePlugin}
      sessionId={bridgeModal.sessionId}
      sessionTitle={chats.find((c) => c.id === bridgeModal!.sessionId)?.title ?? ""}
      allStatuses={bridgeStatuses[activeBridgePlugin.id] ?? {}}
      sessions={chats.map((c) => ({ id: c.id, title: c.title, archived: !!c.archived }))}
      onSelectSession={(id) => { bridgeModal = { pluginId: activeBridgePlugin.id, sessionId: id }; void refreshBridgeStatuses(); }}
      onClose={() => { bridgeModal = null; void refreshBridgeStatuses(); }}
    />
  {/if}
{/if}
{#if srvDialogOpen}
  <ServerDialogModal
    bind:srvUrlDraft
    bind:srvTokenDraft
    bind:srvNameDraft
    bind:srvNameTouched
    {srvBusy}
    {srvErr}
    {srvList}
    {srvEditingId}
    bind:srvEditDraft
    {srvRenaming}
    {srvCopied}
    {srvCmdCopied}
    setupCmd={SERVER_SETUP_CMD}
    {defaultServerName}
    onClose={() => (srvDialogOpen = false)}
    onSubmit={() => void submitServer()}
    onFillRecent={fillRecentServer}
    onStartRename={startRenameServer}
    onSaveRename={() => void saveRenameServer()}
    onCancelRename={cancelRenameServer}
    onCopyUrl={(url) => void copyServerUrl(url)}
    onForget={(id) => void forgetServer(id)}
    onCopySetup={() => void copyServerSetup()}
  />
{/if}
<ConfirmModals
  {leadDeleteAsk}
  bind:pendingDeleteId
  bind:pendingMsgDelete
  {activeChat}
  {pendingSaveConfirm}
  {editingIndex}
  {pendingSaveAction}
  onConfirmDelete={confirmDelete}
  onConfirmMsgDelete={confirmMsgDelete}
  onCancelSaveConfirm={cancelSaveConfirm}
  onConfirmSaveHere={confirmSaveHere}
/>
{#if usageOpen}
  <UsageModal
    {workEngines}
    {workEngineList}
    {usageEngine}
    {usageReport}
    {usageUpdatedAt}
    {usageNow}
    {usageLoading}
    {usageError}
    onClose={closeUsageModal}
    onSelectTab={selectUsageTab}
    onRefresh={(eng) => void fetchUsageEngine(eng, { force: true })}
    onOpenExternal={(url) => void openExternal(url)}
  />
{/if}
<ScheduleModals
  bind:scheduleListOpen
  bind:quitPromptOpen
  bind:scheduleCustom
  {scheduled}
  {scheduledSorted}
  {scheduleNow}
  {scheduleNote}
  {scheduleTz}
  {serverPersist}
  {scheduleTarget}
  onDeliverScheduled={(id) => void deliverScheduled(id)}
  onOpenCustom={(id) => openScheduleCustom(id)}
  onRemoveScheduled={removeScheduled}
  onConfirmCustom={() => void confirmScheduleCustom()}
  onQuitAnyway={() => void quitVicuna(true)}
/>
{#if gotoOpen}
  <GotoPaletteModal
    bind:gotoInput
    bind:gotoSel
    {gotoError}
    {searchQuery}
    {searchItems}
    {searchShowHosts}
    {srvLabel}
    {remoteSearching}
    {localServerSearching}
    {remoteFailures}
    {remoteNotices}
    {searchSnippet}
    {highlightHtml}
    onClose={closeGoto}
    onKeyDown={onSearchKey}
    onOpenItem={openSearchItem}
    onPrefetchChat={(chat) => void ensureChatFull(chat, { silent: true })}
  />
{/if}
{#if settingsOpen}
  <SettingsModal
    bind:settingsOpen
    bind:settingsTab
    {buildInfo}
    bind:appUpdateStatus
    bind:appUpdateError
    {appUpdateLabel}
    {showUpdatePrompt}
    {autoCheckAppUpdatesEnabled}
    {updateOp}
    {updateRelease}
    {updateNote}
    bind:updateError
    onOpenAppUpdate={openAppUpdate}
    onSetAutoCheckAppUpdates={setAutoCheckAppUpdates}
    onCheckForUpdates={checkForUpdates}
    onApplyUpdate={applyUpdate}
    onOpenExternal={openExternal}
    {theme}
    {themeBase}
    {pluginTheme}
    {pluginThemeChoices}
    {pluginAccent}
    {accentSel}
    accents={ACCENTS}
    {fontSize}
    onSetTheme={setTheme}
    onSetPluginTheme={setPluginTheme}
    onSetAccent={setAccent}
    onSetFontSize={setFontSize}
    bind:notifyTurns
    bind:notifySound
    bind:notifySoundChoice
    cliInstallCmd={CLI_INSTALL_CMD}
    {cliCopied}
    {cliState}
    onCopyCliInstall={copyCliInstall}
    onQuitVicuna={quitVicuna}
    {workEngineInfo}
    {workEngineLabel}
    {browserDaemon}
    onRefreshWorkEngine={refreshWorkEngine}
    onPickWorkEngine={pickWorkEngine}
    onToggleWorkApproval={toggleWorkApproval}
    {continueMode}
    bind:stopWordsText
    onSetContinueMode={setContinueMode}
    onSetStopWords={setStopWords}
    {ollamaVersion}
    {serverChecking}
    {serverOp}
    {serverError}
    {serverChatTurns}
    bind:ollamaBase
    bind:browseView
    {runnersError}
    {sortedManageModels}
    {runners}
    {selectedModel}
    {modelOp}
    {pulling}
    bind:confirmDeleteModel
    bind:pullName
    {pullStatus}
    {pullDone}
    {pullTotal}
    {pullRate}
    bind:browseQuery
    bind:browseCat
    browseCats={BROWSE_CATS}
    {filteredCatalog}
    {catalog}
    {catalogUpdated}
    {fetching}
    {catalogError}
    {models}
    bind:variantPick
    onOpenModelsTab={openModelsTab}
    onStartServerFlow={startServerFlow}
    onStopServerFlow={stopServerFlow}
    onSetOllamaHost={setOllamaHost}
    onToggleModelLoaded={toggleModelLoaded}
    onRequestDeleteModel={requestDeleteModel}
    onSubmitPull={submitPull}
    onSubmitPullName={submitPullName}
    onCancelPull={cancelPull}
    onFetchCatalog={fetchCatalog}
    {formatPullRate}
    {isMlxVariant}
    {pickedVariant}
    {openrouterKey}
    {openrouterKeyBusy}
    {openrouterKeyInfo}
    bind:openrouterKeyEditing
    bind:openrouterKeyVisible
    bind:openrouterKeyDraft
    {openrouterKeyDirty}
    {openrouterKeyError}
    bind:openrouterQuery
    bind:openrouterCat
    {openrouterLoading}
    {openrouterModels}
    {openrouterBrowsing}
    {filteredOpenRouterModels}
    {openrouterUpdated}
    {openrouterError}
    {openrouterAddedSet}
    {openRouterUsageText}
    onOpenOpenRouterTab={openOpenRouterTab}
    onVerifyOpenRouterKey={verifyOpenRouterKey}
    onClearOpenRouterKey={clearOpenRouterKey}
    onSaveOpenRouterKey={saveOpenRouterKey}
    onFetchOpenRouterCatalog={fetchOpenRouterCatalog}
    onToggleOpenRouterAdded={toggleOpenRouterAdded}
  />
{/if}
{#if clearContextChatId}
  <button class="confirm-backdrop" data-testid="clear-context-backdrop" aria-label="Cancel clear context" onclick={() => (clearContextChatId = null)} tabindex="-1"></button>
  <div class="confirm-card" data-testid="clear-context-modal" role="dialog" aria-modal="true" aria-label="Clear context">
    <p class="confirm-text">Clear context?</p>
    <p class="confirm-copy">The agent starts fresh from here. Earlier messages stay visible above the line but are no longer sent to the model.</p>
    <div class="confirm-actions"><button class="confirm-cancel" onclick={() => (clearContextChatId = null)}>Cancel</button><button class="confirm-ok" data-testid="confirm-clear-context" onclick={() => void clearContextSession()}>Clear</button></div>
  </div>
{/if}
{#if pluginsOpen}
  <button
    class="confirm-backdrop"
    data-testid="plugins-backdrop"
    aria-label="Close plugins"
    onclick={() => (pluginsOpen = false)}
    tabindex="-1"
  ></button>
  <div
    class="confirm-card settings-card"
    data-testid="plugins-modal"
    role="dialog"
    aria-modal="true"
    aria-label="Plugins"
  >
    <button class="modal-x" data-testid="close-plugins-btn" aria-label="Close plugins" onclick={() => (pluginsOpen = false)}>×</button>
    <p class="confirm-text">Plugins</p>
    <PluginsPanel {plugins} activeThemeKey={pluginTheme?.key ?? null} onChanged={onPluginsChanged} onHarnessesChanged={() => void refreshWorkEngine()} onUseTheme={usePluginTheme} onOpenPanel={(k) => { pluginsOpen = false; openSidePanel(k); }} />
  </div>
{/if}
{/if}

{#if skillsOpen}
  <SkillsModal
    skills={skillsList}
    loaded={skillsLoaded}
    onToggleSkill={(slug, enabled) => setSkillForSession(activeChat?.id ?? null, slug, enabled)}
    onSetDefault={setSkillsAsDefault}
    onCreate={async (slug, content) => {
      const created = await saveSkill(slug, content);
      await refreshSkills();
      return created;
    }}
    onDeleteSkill={async (slug) => {
      await removeSkill(slug);
      forgetSkillSwitch(slug);
      await refreshSkills();
    }}
    onEdit={(skill) => void openSkillInEditor(skill)}
    onClose={() => (skillsOpen = false)}
  />
{/if}

{#if codeEditorPath && CodeEditorComp}
  {#key codeEditorPath}
    <CodeEditorComp
      root={codeEditorRoot}
      path={codeEditorPath}
      onclose={closeCodeEditor}
      onsaved={() => { gitRefreshTick++; if (explorerOpen && !explorerLoading) void loadExplorer(); void refreshSkills(); }}
      onopenexternal={isTauriRuntime() ? (p: string) => void openPathExternal(p) : undefined}
    />
  {/key}
{/if}

<input type="file" bind:this={fileInput} hidden multiple onchange={onFile} data-testid="file-input" />

<style>
  .confirm-card.permissions-modal { top: 50%; width: min(520px, calc(100vw - 48px)); padding: 24px; border: 1px solid var(--border); border-radius: 18px; background: var(--card); }
  .confirm-card.permissions-modal h2 { margin: 0 36px 22px 0; font-size: 18px; }
  .permission-row { display: flex; align-items: flex-start; gap: 12px; padding: 14px 0; border-top: 1px solid var(--border); cursor: pointer; }
  .permission-row input { margin-top: 3px; accent-color: var(--accent); }
  .permission-row span { display: grid; gap: 4px; }
  .permission-row small { color: var(--muted); }
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
    --bg: #3d3332; --text: #ece4e0; --text-dim: #b3a6a3;
    --muted: #b3a6a3; --muted-2: #a39592; --faint: #8e7f7c;
    --hover: #5d4f4d; --hover-2: #5d4f4d; --hover-3: #5d4f4d;
    --active-item: #5d4f4d; --item-bg: #4e4442; --composer: #4e4442;
    --card: #4e4442; --border: #665b58; --border-2: #756866; --border-3: #756866;
    --border-strong: #756866; --input-border: #756866;
    --bubble-user: #5d4f4d; --chip: #5d4f4d;
    --icon: #b3a6a3; --icon-dim: #a39592;
    --thinking: #b3a6a3; --link: #7ed491; --link-hover: #a2e1af;
    --danger: #ff8a80; --err-text: #ffb4a9; --err-bg: #4a1f1a;
    --code-bg: #4e4442; --code-inline: #5d4f4d;
    --code-keyword: #ff7b72; --code-string: #a5d6ff; --code-comment: #8b949e;
    --code-number: #79c0ff; --code-title: #d2a8ff; --code-builtin: #79c0ff;
    --code-literal: #79c0ff; --code-attr: #7ee787;
    --send-disabled: #665b58;
    --cancel-bg: #5d4f4d; --cancel-hover: #665b58;
    --ok: #7ed491;
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
  /* loading skeletons: grey blocks with a light sweep */
  .skel {
    height: 10px;
    border-radius: 5px;
    background: linear-gradient(90deg, color-mix(in srgb, var(--text) 7%, transparent) 25%, color-mix(in srgb, var(--text) 15%, transparent) 50%, color-mix(in srgb, var(--text) 7%, transparent) 75%);
    background-size: 200% 100%;
    animation: skel-sweep 1.4s ease-in-out infinite;
  }
  @keyframes skel-sweep { from { background-position: 100% 0; } to { background-position: -100% 0; } }
  .skel-list { padding: 4px 8px; display: flex; flex-direction: column; }
  .skel-label { width: 34%; height: 8px; margin: 10px 0 8px; opacity: .7; }
  .skel-row { height: 32px; display: flex; align-items: center; padding: 0 4px; }
  .skel-note { margin-top: 8px; font-size: 11px; color: var(--faint); padding: 0 4px; }
  .skel-thread { display: flex; flex-direction: column; gap: 22px; padding: 24px 0; max-width: 760px; width: 100%; margin: 0 auto; }
  .skel-bubble { align-self: flex-end; height: 38px; border-radius: 14px; }
  .skel-answer { display: flex; flex-direction: column; gap: 10px; }
  @media (prefers-reduced-motion: reduce) { .skel { animation: none; } }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  
  .scope-row { display: flex; align-items: center; gap: 6px; margin: 0 4px 10px 4px; }
  .scope-label { font-size: 12px; color: var(--muted); flex: none; }
  .scope-select { flex: 1; min-width: 0; background: transparent; border: 1px solid var(--border-2); border-radius: 999px; padding: 3px 9px; font-size: 12px; font-family: inherit; line-height: 1.5; color: var(--text-dim); cursor: pointer; text-overflow: ellipsis; }
  .scope-select:hover { background: var(--hover-2); }
  .scope-select.scoped { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, var(--border-2)); background: color-mix(in srgb, var(--accent) 6%, transparent); font-weight: 500; }
  .scope-select.scoped:hover { background: color-mix(in srgb, var(--accent) 11%, transparent); }
  .scope-select:focus { outline: none; border-color: var(--accent); }
  .history { overflow-y: auto; overflow-x: hidden; display: flex; flex-direction: column; gap: 2px; }
  .pinned-list { display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .group-label { font-size: 12px; color: var(--muted); margin: 12px 4px 6px 4px; }
  .chat-item { display: flex; align-items: center; gap: 6px; width: 100%; box-sizing: border-box; min-width: 0; border: none; background: transparent; padding: 7px 8px; border-radius: 8px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text); user-select: none; -webkit-user-select: none; }
  .chat-item:hover { background: var(--hover-3); }
  .chat-item.active { background: var(--active-item); }
  .chat-item.archived .title, .chat-item.archived .lead-badge { opacity: .55; }
  .archived-badge { opacity: .75; }
  .archive-toggle { flex: none; display: inline-flex; align-items: center; gap: 3px; background: transparent; border: 1px solid var(--border-2); border-radius: 999px; padding: 3px 7px; font-size: 11px; color: var(--muted); cursor: pointer; }
  .archive-toggle:hover { background: var(--hover-2); }
  .archive-toggle.on { color: var(--accent); border-color: var(--accent); }
  .chat-item .title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; min-width: 0; }
  .chat-item .work-badge { margin-left: 0; }
  .chat-item.streaming { background: var(--hover-3); }
  .chat-item.streaming.active { background: var(--active-item); }
  .stream-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex-shrink: 0; animation: stream-pulse 1.2s ease-in-out infinite; }
  @keyframes stream-pulse { 0%, 100% { opacity: .35; transform: scale(.85); } 50% { opacity: 1; transform: scale(1.15); } }
  .live-elapsed { flex-shrink: 0; font-size: 11px; font-variant-numeric: tabular-nums; white-space: nowrap; color: var(--muted-2); }
  .rename-input { flex: 1; min-width: 0; border: 1px solid var(--border-strong); border-radius: 6px; outline: none; font: inherit; font-size: 14px; padding: 2px 6px; background: var(--card); color: var(--text); }
  .cli-oneliner { flex: 1; min-width: 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; padding: 6px 8px; border: 1px solid var(--border-2); border-radius: 8px; overflow-wrap: anywhere; user-select: text; -webkit-user-select: text; }
  .srv-pick { position: relative; width: 100%; }
  .srv-btn { width: 100%; box-sizing: border-box; }
  .srv-btn .srv-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .srv-btn .srv-chev { flex: none; opacity: .6; }
  .srv-btn.remote { color: var(--accent); }
  .srv-menu { position: absolute; left: 0; right: 0; bottom: calc(100% + 6px); z-index: 41; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; display: flex; flex-direction: column; min-width: 220px; }
  .srv-menu-label { font-size: 12px; color: var(--muted); padding: 6px 10px 4px; }
  .srv-item { display: flex; align-items: center; gap: 6px; border: none; background: transparent; text-align: left; font: inherit; font-size: 14px; padding: 8px 10px; border-radius: 10px; cursor: pointer; color: var(--text); }
  .srv-item:hover { background: var(--hover-3); }
  .srv-check { width: 16px; flex: none; color: var(--muted); text-align: center; }
  .srv-item-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; min-width: 0; }
  .srv-sep { height: 1px; background: var(--border); margin: 4px 6px; }
  .srv-version { display: flex; align-items: center; gap: 6px; font-size: 11.5px; color: var(--muted); padding: 4px 10px; }
  .srv-skew { font-size: 10.5px; font-weight: 600; color: var(--accent); border: 1px solid currentColor; border-radius: 999px; padding: 0 6px; }
  .srv-upgrade { color: var(--accent); }
  .srv-spin { display: inline-block; width: 11px; height: 11px; border: 2px solid currentColor; border-right-color: transparent; border-radius: 50%; animation: srv-spin 0.8s linear infinite; vertical-align: -1px; }
  @keyframes srv-spin { to { transform: rotate(360deg); } }
  .srv-upgrade-prog { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .srv-upgrade-bar { display: block; height: 3px; border-radius: 2px; background: var(--hover-3, rgba(127,127,127,0.25)); overflow: hidden; }
  .srv-upgrade-bar > span { display: block; height: 100%; background: var(--accent); transition: width 0.4s ease; }
  .srv-upgrade-detail { font-size: 11px; color: var(--muted, inherit); opacity: 0.8; }
  @media (prefers-reduced-motion: reduce) { .srv-spin { animation-duration: 2.4s; } }
  .srv-dot { width: 7px; height: 7px; border-radius: 50%; background: var(--accent); flex-shrink: 0; }
  .srv-strip { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; padding: 8px 14px; margin: 10px 24px 0; border-radius: 12px; font-size: 13px; color: var(--danger); background: color-mix(in srgb, var(--danger) 10%, transparent); }
  .srv-strip span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .srv-menu-rename-row { display: flex; align-items: center; gap: 4px; padding: 4px 6px; }
  .srv-menu-rename-input { flex: 1; min-width: 0; border: 1px solid var(--accent); border-radius: 8px; padding: 5px 8px; font: inherit; font-size: 13px; background: transparent; color: var(--text); }
  .srv-menu-rename-input:focus { outline: none; }
  .srv-menu-rename-ok { flex: none; width: 28px; height: 28px; border: 1px solid var(--border-2); border-radius: 8px; background: transparent; color: var(--text); cursor: pointer; font-size: 13px; display: flex; align-items: center; justify-content: center; }
  .srv-menu-rename-ok:hover { background: var(--hover-3); }
  .ctx-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 40; }
  .work-badge.lead-badge { background: var(--bg); color: var(--accent); border: 1px solid var(--accent); padding: 0 7px; }
  .chat-item .work-badge.lead-badge { margin-right: auto; }
  .ctx-menu { position: fixed; z-index: 41; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; min-width: 150px; display: flex; flex-direction: column; }
  .ctx-item { border: none; background: transparent; text-align: left; font: inherit; font-size: 14.5px; padding: 8px 14px; border-radius: 10px; cursor: pointer; color: var(--text); }
  .ctx-item:hover { background: var(--hover-3); }
  .ctx-item.danger { color: var(--danger); }
  .ctx-item.ctx-telegram { display: flex; align-items: center; gap: 8px; width: 100%; box-sizing: border-box; }
  .ctx-sep { height: 1px; background: var(--border); margin: 4px 8px; }
  .ctx-tg-label { flex: 1; min-width: 0; }
  .ctx-tg-bot { font-size: 12px; color: var(--muted); max-width: 110px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ctx-tg-icon { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 18px; height: 18px; border-radius: 50%; background: #2aabee; color: #fff; line-height: 0; }
  .ctx-tg-dot { flex: none; margin-left: auto; width: 8px; height: 8px; border-radius: 50%; box-sizing: border-box; }
  .ctx-tg-dot.connected { background: #22c55e; }
  .ctx-tg-dot.disconnected { background: #ef4444; }
  .ctx-tg-dot.unconfigured { background: transparent; border: 1.5px solid var(--muted-2); }
  .tg-row-badge { flex: none; display: inline-flex; align-items: center; justify-content: center; width: 16px; height: 16px; border-radius: 50%; color: #fff; line-height: 0; }
  .tg-row-badge.connected { background: #2aabee; }
  .tg-row-badge.disconnected { background: #ef4444; opacity: 0.85; }
  .ctx-continue-row { display: flex; align-items: center; gap: 4px; }
  .ctx-continue { flex: 1; min-width: 0; white-space: nowrap; }
  .ctx-ws-pill { display: inline-flex; align-items: center; gap: 5px; flex-shrink: 0; max-width: 130px; border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 999px; padding: 5px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .ctx-ws-pill:hover { background: var(--hover-3); color: var(--text); border-color: var(--border-3); }
  .ctx-ws-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .ctx-ws-form { display: flex; flex-direction: column; gap: 6px; padding: 4px 8px 6px 8px; }
  .ctx-ws-form .pull-input { width: 100%; box-sizing: border-box; }
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-actions { display: flex; gap: 10px; }
  .confirm-copy { color: var(--muted); font-size: 13px; line-height: 1.5; margin: -10px 0 18px; }
  .confirm-cancel { flex: 1; border: none; background: var(--cancel-bg); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--text); }
  .confirm-cancel:hover { background: var(--cancel-hover); }
  .confirm-ok { flex: 1; border: none; background: var(--accent); border-radius: 14px; padding: 10px 0; font-size: 15px; font-family: inherit; cursor: pointer; color: var(--accent-fg); }
  .confirm-ok:hover { background: var(--accent-hover); }
  .empty-hint { color: var(--faint); font-size: 13px; padding: 0 8px; }
  .main { flex: 1; display: flex; flex-direction: column; position: relative; min-width: 0; container-type: inline-size; }
  .stream-bar { position: relative; height: 3px; border-radius: 999px; overflow: hidden; margin: 0 4px 8px 4px; background: var(--border); pointer-events: none; }
  .stream-bar span { position: absolute; top: 0; bottom: 0; width: 40%; background: linear-gradient(90deg, transparent, var(--accent), transparent); animation: stream-slide 1.2s ease-in-out infinite; }
  @keyframes stream-slide { from { left: -40%; } to { left: 100%; } }
  .queue-strip { display: flex; flex-wrap: wrap; align-items: center; gap: 6px; margin: 0 4px 8px 4px; }
  .queue-chip { display: inline-flex; align-items: center; gap: 6px; max-width: 100%; background: var(--hover-3); border: 1px solid var(--border-2); border-radius: 999px; padding: 3px 6px 3px 10px; font-size: 12px; color: var(--muted); }
  .queue-text { max-width: 320px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .queue-x { border: none; background: transparent; color: var(--muted); cursor: pointer; font-size: 14px; line-height: 1; padding: 0 2px; }
  .queue-x:hover { color: var(--text); }
  .queue-now { display: inline-flex; align-items: center; justify-content: center; border: none; background: transparent; color: var(--muted); cursor: pointer; padding: 1px 2px; border-radius: 4px; }
  .queue-now:hover { color: var(--accent); }
  .queue-hint { font-size: 11.5px; color: var(--faint); }
  .esc-hint { position: absolute; right: 24px; bottom: 100%; margin-bottom: 6px; background: var(--card); border: 1px solid var(--border-3); border-radius: 10px; padding: 5px 10px; font-size: 12px; color: var(--muted); box-shadow: 0 4px 16px rgba(0,0,0,.12); z-index: 6; }
  .collapsed-rail { position: absolute; left: 0; top: 0; bottom: 0; display: flex; flex-direction: column; justify-content: space-between; align-items: stretch; width: 60px; padding: 10px 8px; box-sizing: border-box; z-index: 5; }
  .collapsed-rail:has(.srv-menu) { z-index: 41; }
  .collapsed-rail .srv-menu { left: 0; right: auto; width: 244px; }
  .collapsed-rail-foot { display: flex; flex-direction: column; gap: 4px; }
  .collapsed-rail .icon-btn { width: 100%; padding: 4px 0; border-radius: 8px; }
  .srv-icon-btn { position: relative; }
  .srv-icon-btn.remote { color: var(--accent); }
  .srv-icon-btn .srv-dot { position: absolute; top: 3px; right: 9px; }
  .empty { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; padding: 24px; overflow-y: auto; }
  .empty-title { font-size: 24px; font-weight: 500; color: var(--text-dim); margin: 0 0 28px 0; text-align: center; }
  .suggest-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; width: 100%; max-width: 860px; }
  .suggest-card { display: flex; flex-direction: row; align-items: center; justify-content: space-between; gap: 12px; text-align: left; background: var(--card); border: 1px solid var(--border); border-radius: 16px; padding: 14px 14px 14px 18px; cursor: pointer; min-width: 0; }
  .suggest-card:hover { box-shadow: 0 4px 16px rgba(0,0,0,.08); border-color: var(--accent); }
  .suggest-text { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .suggest-title { font-size: 15px; font-weight: 650; color: var(--text); }
  .suggest-sub { font-size: 13.5px; color: var(--muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .suggest-icon { flex-shrink: 0; width: 36px; height: 36px; border-radius: 10px; background: var(--accent); color: var(--accent-fg); display: flex; align-items: center; justify-content: center; }
  .messages { flex: 1; overflow-y: auto; overflow-x: hidden; overflow-anchor: none; min-height: 0; min-width: 0; padding: 40px 24px 12px 24px; display: flex; flex-direction: column; gap: 14px; max-width: 860px; width: 100%; margin: 0 auto; box-sizing: border-box; }
  .row { display: flex; }
  .user-row { justify-content: flex-end; }
  .asst-row { justify-content: flex-start; }
  .bubble { max-width: 100%; padding: 10px 14px; border-radius: 16px; font-size: var(--msg-font-size, 14.5px); line-height: 1.5; white-space: pre-wrap; word-break: break-word; }
  .bubble.user { background: var(--accent); color: var(--accent-fg); }
  .bubble.asst { background: transparent; padding-left: 0; padding-right: 0; white-space: normal; word-break: normal; overflow-wrap: break-word; user-select: text; }
  .answer { user-select: text; white-space: normal; }
  .ask-card { margin: 12px 0 2px; max-width: 560px; border: 1px solid var(--border-2); border-radius: 14px; background: var(--card); padding: 12px; }
  .ask-question { font-size: 14px; font-weight: 600; color: var(--text); margin-bottom: 10px; line-height: 1.45; }
  .ask-options { display: flex; flex-direction: column; gap: 6px; }
  .ask-option { width: 100%; text-align: left; border: 1px solid var(--border-2); background: var(--bg); color: var(--text); border-radius: 10px; padding: 8px 11px; font: inherit; font-size: 13.5px; cursor: pointer; }
  .ask-option:hover { border-color: var(--accent); background: var(--hover-3); }
  .ask-option-multi { display: flex; align-items: center; gap: 9px; }
  .ask-option-multi.picked { border-color: var(--accent); }
  .ask-check { width: 16px; height: 16px; flex: none; display: inline-flex; align-items: center; justify-content: center; border: 1px solid var(--border-3); border-radius: 4px; font-size: 11px; line-height: 1; color: var(--accent-fg); }
  .ask-option-multi.picked .ask-check { background: var(--accent); border-color: var(--accent); }
  .ask-other-toggle { color: var(--muted); border-style: dashed; }
  .ask-other { display: flex; gap: 6px; }
  .ask-other-closed { align-items: stretch; }
  .ask-other-toggle { flex: 1; width: auto; }
  .ask-dismiss { border: 1px solid var(--border-2); background: transparent; color: var(--muted); border-radius: 10px; padding: 8px 12px; font: inherit; font-size: 13px; cursor: pointer; white-space: nowrap; }
  .ask-dismiss:hover { color: var(--text); border-color: var(--border-3); background: var(--hover-3); }
  .ask-input { flex: 1; min-width: 0; border: 1px solid var(--border-2); border-radius: 10px; background: var(--bg); color: var(--text); padding: 7px 10px; font: inherit; font-size: 13.5px; }
  .ask-input:focus { outline: none; border-color: var(--accent); }
  textarea.ask-input { resize: none; line-height: 1.45; max-height: 200px; overflow-y: auto; user-select: text; }
  .ask-option.picked { border-color: var(--accent); }
  .ask-compose { display: flex; flex-direction: column; margin-top: 4px; }
  .ask-compose .ask-other { align-items: stretch; }
  .ask-compose .ask-send, .ask-compose .ask-dismiss { align-self: flex-end; min-height: 34px; }
  .ask-image-strip { padding: 2px 0 8px 0; }
  .ask-later { align-self: flex-start; margin-top: 6px; border: none; background: transparent; color: var(--muted); font: inherit; font-size: 12.5px; padding: 2px 0; cursor: pointer; text-decoration: underline; text-underline-offset: 2px; }
  .ask-later:hover { color: var(--text); }
  .ask-card-later { border-style: dashed; }
  .ask-later-tag { font-size: 11.5px; color: var(--muted-2); margin-bottom: 6px; }
  /* a finished background job (a user-role message underneath), shown like
     a collapsed tool result on the agent's side */
  .bg-done-row .col { min-width: 0; }
  .bg-done { background: var(--code-bg); border-radius: 12px; margin: 4px 0 8px; overflow: hidden; max-width: 100%; }
  .bg-done summary { display: flex; align-items: baseline; gap: 8px; cursor: pointer; padding: 9px 12px; font-size: 12.5px; color: var(--muted-2); list-style: none; white-space: nowrap; overflow: hidden; }
  .bg-done summary::-webkit-details-marker { display: none; }
  .bg-done summary::before { content: "▸"; color: var(--muted); }
  .bg-done[open] summary::before { content: "▾"; }
  .bg-done summary:hover { color: var(--text); }
  .bg-done-mark { color: var(--ok, #2f9e44); font-weight: 600; }
  .bg-done-mark.bad { color: var(--danger, #e03131); }
  .bg-done-title { color: var(--text); }
  .bg-done-meta { font-variant-numeric: tabular-nums; }
  .bg-done-cmd { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; background: transparent; padding: 0; }
  .bg-done-out { margin: 0; border-radius: 0; border-top: 1px solid var(--border-2); background: transparent; padding: 10px 12px; font-size: 12.5px; max-height: 360px; overflow: auto; user-select: text; white-space: pre-wrap; }
  .bg-done-err { color: var(--danger, #e03131); }
  .bg-done-label { border-top: 1px solid var(--border-2); padding: 6px 12px; font-size: 11.5px; color: var(--muted-2); }
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
  @media (prefers-reduced-motion: reduce) { .typing span { animation: none; opacity: .7; } .shimmer { animation: none; } .stream-bar span { animation: none; width: 100%; } .stream-dot { animation: none; opacity: .8; } .mem-spin::after { animation: none; } .browser-restarting svg { animation: none; } }
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
  .answer :global(h1), .answer :global(h2), .answer :global(h3), .answer :global(h4), .answer :global(h5), .answer :global(h6) { margin: 14px 0 8px 0; font-weight: 650; line-height: 1.3; }
  .answer :global(h1) { font-size: 1.38em; } .answer :global(h2) { font-size: 1.24em; } .answer :global(h3) { font-size: 1.1em; } .answer :global(h4) { font-size: 1.03em; } .answer :global(h5), .answer :global(h6) { font-size: 1em; }
  .answer :global(a) { color: var(--link); }
  .answer :global(blockquote) { border-left: 3px solid var(--border-2); margin: 0 0 12px 0; padding-left: 12px; color: var(--text-dim); }
  .answer :global(.table-wrap) { width: fit-content; max-width: 100%; box-sizing: border-box; overflow-x: auto; margin: 0 0 12px 0; border: 1px solid var(--border-2); border-radius: 10px; background: var(--bg); }
  .answer :global(.table-wrap:last-child) { margin-bottom: 0; }
  .answer :global(table) { border-collapse: separate; border-spacing: 0; margin: 0; word-break: normal; overflow-wrap: normal; }
  .answer :global(th), .answer :global(td) { border: none; border-bottom: 1px solid var(--border); border-right: 1px solid var(--border); padding: 8px 12px; font-size: 13.5px; line-height: 1.55; vertical-align: top; }
  .answer :global(th) { background: var(--code-bg); font-weight: 600; color: var(--text); border-bottom-color: var(--border-2); vertical-align: bottom; }
  .answer :global(th:not([align])) { text-align: left; }
  .answer :global(th:last-child), .answer :global(td:last-child) { border-right: none; }
  .answer :global(tbody tr:last-child td) { border-bottom: none; }
  .answer :global(table > thead:last-child tr:last-child th) { border-bottom: none; }
  .answer :global(tbody tr:hover) { background: color-mix(in srgb, var(--hover-3) 55%, transparent); }
  .answer :global(th code), .answer :global(td code) { font-size: 12.5px; padding: 1px 4px; }
  .err { max-width: 720px; margin: 0 auto; color: var(--err-text); background: var(--err-bg); border-radius: 10px; padding: 8px 12px; font-size: 13px; display: flex; align-items: center; gap: 8px; }
  .err span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .err-x { flex: none; border: 0; background: none; color: inherit; opacity: .6; font-size: 15px; line-height: 1; padding: 2px 6px; border-radius: 6px; cursor: pointer; }
  .err-x:hover { opacity: 1; background: rgba(127,127,127,.25); }
  /* transient command feedback (undo/redo/compact): neutral, not an error.
     Floats above the composer (composer-wrap is position:relative) so it
     takes NO layout space — showing/hiding it must never shift the thread
     or the composer. */
  .notice { position: absolute; left: 24px; right: 24px; bottom: calc(100% + 8px); max-width: 720px; margin: 0 auto; z-index: 6; color: var(--text); background: var(--card); border: 1px solid var(--border-3); border-radius: 10px; padding: 8px 12px; font-size: 13px; display: flex; align-items: center; gap: 8px; box-shadow: 0 4px 16px rgba(0,0,0,.10); animation: notice-in .14s ease-out; }
  @keyframes notice-in { from { opacity: 0; transform: translateY(4px); } to { opacity: 1; transform: translateY(0); } }
  @media (prefers-reduced-motion: reduce) { .notice { animation: none; } }
  .notice span { flex: 1; min-width: 0; overflow-wrap: anywhere; }
  .notice-x { flex: none; border: 0; background: none; color: inherit; opacity: .5; font-size: max(14px, 1em); line-height: 1; padding: 2px 6px; border-radius: 6px; cursor: pointer; }
  .notice-x:hover { opacity: 1; background: rgba(127,127,127,.25); }
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
  /* The leading .spacer right-aligns the row; flex-start (not flex-end) keeps
   * any overflow on the right instead of spilling the + / tool buttons out of
   * the composer to the left, under the collapsed rail. The pills shrink first
   * (names ellipsize) so a side-panel-narrowed column still fits. */
  .composer-bar { display: flex; align-items: center; gap: 8px; margin-top: 10px; justify-content: flex-start; }
  /* The wrap must be a flex box too: a <button> in a block wrapper sizes to
   * its content and ignores the wrapper shrinking, so the model name ran under
   * the send button instead of ellipsizing. */
  .composer-bar > .ws-split, .composer-bar > .model-pill-wrap { min-width: 0; }
  .composer-bar > .model-pill-wrap { display: flex; }
  .composer-bar .ws-pill, .composer-bar > .model-pill-wrap > .model-pill { min-width: 0; }
  .mention-menu { position: absolute; bottom: calc(100% + 6px); left: 24px; width: min(420px, calc(100% - 48px)); z-index: 30; background: var(--card); border: 1px solid var(--border-2); border-radius: 14px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; max-height: 264px; overflow-y: auto; }
  .mention-item { display: flex; align-items: center; gap: 8px; width: 100%; box-sizing: border-box; border: none; background: transparent; font: inherit; font-size: 13px; color: var(--text); padding: 7px 10px; border-radius: 10px; cursor: pointer; text-align: left; min-width: 0; }
  .mention-item:hover { background: var(--hover-3); }
  .mention-item.sel { background: var(--hover-3); background: color-mix(in srgb, var(--accent) 18%, transparent); }
  .mention-item.disabled { opacity: .42; cursor: default; }
  .mention-item.disabled:hover { background: transparent; }
  .mention-item.disabled.sel { background: transparent; }
  .mention-item svg { flex-shrink: 0; color: var(--muted-2); }
  .mention-path { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .mention-empty { padding: 8px 12px; font-size: 13px; color: var(--muted); }
  .slash-desc { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 12px; color: var(--muted); text-align: right; }
  .spacer { flex: 1; }
  .model-pill { display: flex; align-items: center; gap: 6px; background: var(--card); color: var(--text); border: none; border-radius: 16px; padding: 7px 12px; font-size: 13.5px; cursor: pointer; max-width: 260px; }
  /* Work mode: workspace picker + dropdown caret joined into one split pill */
  .ws-split { position: relative; display: inline-flex; align-items: stretch; }
  .ws-split > .ws-pill { border-radius: 16px 0 0 16px; padding-right: 9px; }
  .ws-split > .ws-pill.ws-pill-none { color: var(--muted); }
  .ws-split > .ws-caret { border-radius: 0 16px 16px 0; padding: 7px 8px 7px 6px; border-left: 1px solid var(--border); color: var(--muted); }
  .ws-split > .ws-pill:hover, .ws-split > .ws-caret:hover { background: var(--hover-3); color: var(--text); }
  .ws-menu { position: absolute; right: 0; bottom: calc(100% + 8px); z-index: 41; min-width: 185px; background: var(--card); border: 1px solid var(--border); border-radius: 14px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 5px; display: flex; flex-direction: column; text-align: left; }
  .ws-menu-opt { display: flex; align-items: center; gap: 8px; }
  .ws-menu-opt.sel { background: var(--hover-3); }
  .ws-menu-check { margin-left: auto; color: var(--accent); }
  .ws-tooltip {
    position: absolute;
    right: 0;
    bottom: calc(100% + 8px);
    z-index: 41;
    box-sizing: border-box;
    width: min(240px, 50cqw);
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: 10px;
    background: var(--card);
    box-shadow: 0 4px 16px rgba(0,0,0,.14);
    color: var(--text);
    font-size: 12.5px;
    line-height: 1.5;
    text-align: left;
    pointer-events: none;
    visibility: hidden;
    opacity: 0;
    transition: opacity 100ms ease-out;
  }
  .ws-tooltip-hint { display: block; margin-top: 4px; color: var(--muted); }
  .ws-pill:is(:hover, :focus-visible) + .ws-tooltip,
  .ws-menu-opt:is(:hover, :focus-visible) + .ws-tooltip { visibility: visible; opacity: 1; }
  @container (max-width: 500px) {
    .composer-bar:has(.ws-split) { position: relative; flex-wrap: wrap; row-gap: 8px; }
    /* Wrapped: tools + workspace start the first line, model pill starts the
     * second with send pushed right (a right-aligned first line left the
     * model pill stranded under it). */
    .composer-bar:has(.ws-split) > .spacer { display: none; }
    .composer-bar:has(.ws-split) > .send-split { margin-left: auto; }
    .ws-split { position: static; }
    .ws-menu, .ws-tooltip { left: 0; right: auto; }
  }
  @media (prefers-reduced-motion: reduce) {
    .ws-tooltip { transition: none; }
  }
  .prov-ico { display: inline-flex; flex: none; align-items: center; justify-content: center; color: var(--muted-2); }
  .model-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: inherit; }
  .model-pill-wrap { position: relative; }
  .model-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 9; }
  .model-drop { position: absolute; bottom: 40px; right: 0; background: var(--card); border: 1px solid var(--border); border-radius: 20px; box-shadow: 0 12px 40px rgba(0,0,0,.14); width: 330px; overflow: hidden; z-index: 10; }
  .model-search { width: 100%; box-sizing: border-box; border: none; border-bottom: 1px solid var(--border); outline: none; padding: 13px 18px; font-size: 17px; font-family: inherit; background: var(--card); color: var(--text); }
  .model-search::placeholder { color: var(--faint); }
  .model-list-drop { max-height: 252px; overflow-y: auto; padding: 4px 0 8px 0; }
  .model-opt { display: flex; flex-direction: column; align-items: stretch; gap: 1px; width: 100%; text-align: left; border: none; background: transparent; padding: 9px 18px; font-size: 17px; cursor: pointer; min-width: 0; }
  .model-opt-top { display: flex; align-items: center; gap: 7px; min-width: 0; }
  .model-opt-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .model-opt-id { font-size: 11.5px; color: var(--muted-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; padding-left: 21px; }
  .model-opt:hover { background: var(--hover-3); }
  .model-opt.sel { background: var(--hover-3); }
  .model-opt-row { display: flex; align-items: center; }
  .model-opt-row:hover { background: var(--hover-3); }
  .model-opt-row.sel { background: var(--hover-3); }
  .model-opt-row .model-opt { flex: 1; min-width: 0; width: auto; padding-right: 6px; }
  .model-opt-row .model-opt:hover { background: transparent; }
  .model-mem-btn { flex-shrink: 0; display: flex; align-items: center; border: none; background: transparent; color: var(--muted); padding: 8px 14px 8px 8px; cursor: pointer; }
  .model-mem-btn:hover:not(:disabled) { color: var(--text); }
  .model-mem-btn.is-loaded { color: var(--accent); }
  .model-mem-btn.is-loaded:hover:not(:disabled) { color: var(--accent); }
  .model-mem-btn:disabled { opacity: .5; cursor: default; }
  .mem-ico { position: relative; display: flex; align-items: center; justify-content: center; width: 21px; height: 21px; }
  .mem-spin::after { content: ""; position: absolute; inset: 0; border-radius: 50%; border: 1.5px solid var(--border-strong); border-top-color: var(--accent); animation: mem-spin .8s linear infinite; }
  @keyframes mem-spin { to { transform: rotate(360deg); } }
  .no-match { padding: 12px 18px; font-size: 14px; color: var(--muted); }
  .send { width: 32px; height: 32px; border-radius: 50%; border: none; background: var(--send-disabled); color: #fff; font-size: 18px; cursor: pointer; }
  .send:disabled { opacity: .8; cursor: default; }
  .send:not(:disabled) { background: var(--accent); color: var(--accent-fg); }
  /* Work mode: send + schedule caret joined into one pill */
  .send-split { position: relative; display: inline-flex; align-items: stretch; }
  .send-split > .send:first-child { border-radius: 16px 0 0 16px; width: 34px; }
  .send-split > .send-caret { border-radius: 0 16px 16px 0; width: 24px; display: flex; align-items: center; justify-content: center; padding: 0 2px 0 0; border-left: 1px solid color-mix(in srgb, currentColor 35%, transparent); }
  .send-split > .send-caret:not(:disabled):hover, .send-split > .send:first-child:not(:disabled):hover { background: var(--accent-hover); }
  .sched-backdrop { position: fixed; inset: 0; z-index: 40; border: none; background: transparent; cursor: default; padding: 0; }
  .sched-menu { position: absolute; right: 0; bottom: calc(100% + 8px); z-index: 41; width: 250px; background: var(--card); border: 1px solid var(--border); border-radius: 16px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 6px; display: flex; flex-direction: column; text-align: left; }
  .sched-menu-head { font-size: 12.5px; color: var(--muted); padding: 8px 14px 4px 14px; }
  .sched-opt { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .sched-opt:hover { background: var(--hover); }
  .sched-opt-at { font-size: 12.5px; color: var(--muted); font-variant-numeric: tabular-nums; }
  .sched-menu-sep { height: 1px; background: var(--border); margin: 4px 8px; }
  .sched-menu-note { font-size: 11.5px; line-height: 1.4; color: var(--muted-2); padding: 6px 14px 8px 14px; border-top: 1px solid var(--border); margin-top: 4px; }
  .sched-count { margin-left: auto; min-width: 18px; height: 18px; box-sizing: border-box; padding: 0 6px; border-radius: 9px; background: var(--accent); color: var(--accent-fg); font-size: 11.5px; font-weight: 600; line-height: 18px; text-align: center; font-variant-numeric: tabular-nums; }
  .sched-count.missed { background: var(--danger); color: #fff; }
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
  .mini-btn { border: 1px solid var(--border-strong); background: transparent; color: var(--text); border-radius: 999px; padding: 4px 14px; font-size: 12.5px; font-family: inherit; cursor: pointer; flex-shrink: 0; }
  .mini-btn:hover:not(:disabled) { background: var(--hover); }
  .mini-btn:disabled { opacity: .5; cursor: default; }
  .mini-btn.primary { background: var(--accent); border-color: transparent; color: var(--accent-fg); }
  .mini-btn.primary:hover:not(:disabled) { background: var(--accent-hover); }
  .mini-btn.danger { border-color: var(--danger); color: var(--danger); }
  .mini-btn.danger:hover:not(:disabled) { background: var(--err-bg); }
  .pull-input { flex: 1; min-width: 0; border: 1px solid var(--input-border); border-radius: 999px; background: transparent; color: var(--text); font-size: 13px; font-family: inherit; padding: 6px 14px; }
  .pull-input:focus { outline: none; border-color: var(--accent); }
  .pull-input:disabled { opacity: .5; }
  .seg { display: flex; background: var(--hover); border-radius: 12px; padding: 3px; gap: 2px; }
  .seg-btn { border: none; background: transparent; cursor: pointer; font: inherit; font-size: 13.5px; color: var(--text-dim); padding: 6px 16px; border-radius: 9px; }
  .seg-btn.sel { background: var(--card); color: var(--text); box-shadow: 0 1px 4px rgba(0,0,0,.12); }
  .or-id { font-size: 11.5px; color: var(--muted-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .mode-toggle { margin-bottom: 18px; }
  .mode-toggle .seg-btn { min-width: 90px; }
  .mode-toggle .seg-btn.sel { background: var(--accent); color: var(--accent-fg); box-shadow: none; font-weight: 700; }
  .workspace-input { width: 100%; box-sizing: border-box; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .workspace-picker-drop { padding: 12px; display: flex; flex-direction: column; gap: 8px; }
  .workspace-picker-label { font-size: 12px; color: var(--muted); }
  .work-badge { flex-shrink: 0; font-size: 11px; font-weight: 700; color: var(--accent-fg); background: var(--accent); border-radius: 999px; padding: 1px 8px; margin-left: 8px; }
  .work-badge.engine-badge { display: inline-flex; align-items: center; gap: 4px; background: transparent; color: var(--muted-2); border: 1px solid var(--border-2); font-weight: 600; cursor: default; }
  .engine-line { margin: -6px 0 18px; color: var(--muted-2); font-size: 12px; }
  .engine-switch { display: flex; gap: 4px; padding: 8px 8px 0; }
  .engine-switch.settings-engines { padding: 2px 0; flex-wrap: wrap; }
  .engine-select { padding: 8px 8px 0; display: flex; flex-direction: column; gap: 2px; }
  .engine-select-btn, .engine-select-opt { display: flex; align-items: center; gap: 8px; width: 100%; border: 1px solid var(--border); background: transparent; color: var(--text); border-radius: 10px; padding: 6px 10px; font-size: 12.5px; cursor: pointer; text-align: left; }
  .engine-select-btn span, .engine-select-opt span { flex: 1; min-width: 0; }
  .engine-select-btn:hover, .engine-select-opt:hover:not(:disabled) { background: var(--hover); }
  .engine-select-chev { color: var(--muted); transition: transform .15s; }
  .engine-select-chev.open { transform: rotate(180deg); }
  .engine-select-list { display: flex; flex-direction: column; gap: 2px; padding: 2px 0; max-height: min(46vh, 380px); overflow-y: auto; }
  .engine-select-opt { border-color: transparent; color: var(--muted); }
  .engine-select-opt.sel { color: var(--text); background: var(--hover); }
  .engine-select-opt:disabled { opacity: .5; cursor: default; }
  .engine-opt-row { display: flex; align-items: center; gap: 2px; }
  .engine-opt-row .engine-select-opt { flex: 1; min-width: 0; }
  .engine-opt-row.is-account .engine-select-opt { padding-left: 28px; }
  .engine-acct-btn { flex: none; min-width: 26px; height: 26px; padding: 0 6px; border: 1px solid transparent; border-radius: 6px; background: transparent; color: var(--muted); font: inherit; font-size: 15px; line-height: 1; cursor: pointer; opacity: .65; }
  .engine-acct-tag { flex: none !important; font-size: 10.5px; padding: 1px 6px; border-radius: 999px; border: 1px solid var(--border); color: var(--muted); }
  .engine-acct-btn svg { display: block; margin: auto; }
  .engine-acct-btn:hover { opacity: 1; background: var(--hover); color: var(--text); }
  .engine-acct-btn.armed { opacity: 1; font-size: 11px; color: #e5484d; border-color: color-mix(in srgb, #e5484d 50%, transparent); }
  .engine-signin-hint { flex: none !important; font-size: 11px; color: var(--accent); }
  .engine-acct-form { display: flex; gap: 6px; padding: 4px 6px 6px 28px; }
  .engine-acct-input { flex: 1; min-width: 0; font: inherit; font-size: 12px; padding: 4px 8px; border: 1px solid var(--border); border-radius: 6px; background: var(--bg); color: var(--text); }
  .engine-acct-error { font-size: 11.5px; color: #e5484d; padding: 4px 8px; }
  .engine-signin-card { margin: 6px 4px 4px; padding: 10px; border: 1px solid var(--border); border-radius: 8px; background: var(--hover); user-select: text; }
  .engine-signin-title { margin: 0 0 4px; font-size: 13px; }
  .engine-signin-text { margin: 0 0 8px; font-size: 11.5px; color: var(--muted); line-height: 1.4; }
  .engine-signin-cmd { display: block; font-size: 11px; padding: 6px 8px; border-radius: 6px; background: var(--bg); word-break: break-all; }
  .engine-signin-actions { display: flex; gap: 6px; margin-top: 8px; flex-wrap: wrap; }
  .engine-signin-cmd-wrap { position: relative; }
  .engine-signin-cmd-wrap .engine-signin-cmd { padding-right: 30px; }
  .engine-signin-copy { position: absolute; top: 3px; right: 3px; width: 24px; height: 24px; display: grid; place-items: center; border: none; border-radius: 5px; background: transparent; color: var(--muted); cursor: pointer; }
  .engine-signin-copy:hover { background: var(--hover); color: var(--text); }
  .engine-signin-copy.done { color: var(--accent); }
  .engine-select-check { color: var(--accent); }
  .engine-opt { display: inline-flex; align-items: center; justify-content: center; gap: 6px; flex: 1; border: 1px solid var(--border); background: transparent; color: var(--muted); border-radius: 10px; padding: 5px 10px; font-size: 12.5px; cursor: pointer; }
  .engine-opt.sel { background: var(--hover); color: var(--text); border-color: var(--border-2); }
  .engine-opt:disabled { opacity: .5; cursor: default; }
  .engine-ico { flex: none; display: block; width: 14px; height: 14px; object-fit: contain; }
  .engine-chip { display: inline-flex; align-items: center; flex: none; line-height: 1; transform: translateY(1px); }
  .opencode-missing { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 0 0 8px; padding: 8px 12px; border: 1px solid var(--border-2); border-radius: 10px; color: var(--muted); font-size: 13px; }
  .opencode-missing span { flex: 1 1 220px; min-width: 0; overflow-wrap: anywhere; }
  .ws-badge { display: inline-flex; align-items: center; gap: 4px; max-width: 110px; min-width: 0; }
  .chat-item .work-badge.ws-badge { flex-shrink: 1; font-weight: 500; color: var(--accent); background: color-mix(in srgb, var(--accent) 13%, transparent); padding: 1px 7px; }
  .ws-badge svg { flex-shrink: 0; }
  .ws-badge span:last-child { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .work-banner { display: flex; align-items: center; gap: 8px; max-width: 860px; width: 100%; margin: 0 auto; padding: 14px 54px 0 24px; box-sizing: border-box; font-size: 12.5px; color: var(--muted-2); }
  .work-banner .work-badge { margin-left: 0; }
  .hdr-meters { margin-left: auto; display: inline-flex; align-items: center; gap: 8px; }
  .ctx-wrap { position: relative; display: inline-flex; cursor: default; }
  .usage-meter-btn { display: inline-flex; align-items: center; justify-content: center; padding: 0; border: 0; background: none; cursor: pointer; }
  .usage-meter-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 50%; }
  .usage-hover { width: 320px; padding: 12px 16px; }
  .usage-hover::before { content: ""; position: absolute; left: 0; right: 0; top: -10px; height: 10px; }
  .usage-hover-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-size: 13px; font-weight: 600; color: var(--text); padding-bottom: 8px; }
  .usage-hover-title { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .usage-hover-refresh { flex-shrink: 0; border: 1px solid var(--border-strong); background: transparent; color: var(--text-dim); border-radius: 6px; padding: 2px 7px; font: inherit; font-size: 11px; font-weight: 500; cursor: pointer; }
  .usage-hover-refresh:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .usage-hover-refresh:disabled { opacity: .55; cursor: default; }
  .usage-hover-row { display: block; padding: 6px 0; }
  .usage-hover-line { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; font-size: 13px; }
  .usage-hover-label { color: var(--muted-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .usage-hover-val { white-space: nowrap; font-variant-numeric: tabular-nums; flex-shrink: 0; color: var(--text); }
  .usage-hover-bar { display: block; height: 4px; margin-top: 5px; border-radius: 2px; background: var(--border-2); overflow: hidden; }
  .usage-hover-fill { display: block; height: 100%; border-radius: 2px; background: var(--accent); }
  .usage-hover-fill.ok { background: #16a34a; }
  .usage-hover-fill.warn { background: #b45309; }
  .usage-hover-fill.low { background: var(--danger); }
  .usage-hover-reset { display: block; font-size: 11px; color: var(--muted-2); margin-top: 4px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .usage-hover-note { display: block; font-size: 12px; color: var(--muted-2); padding: 2px 0; max-width: 280px; }
  .usage-hover-hint { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-size: 11px; color: var(--muted-2); border-top: 1px solid var(--border-2); margin-top: 6px; padding-top: 6px; }
  .usage-tone-ok { color: #15803d; }
  .usage-tone-warn { color: #b45309; }
  .usage-tone-low { color: var(--danger); }
  .ctx-popup { display: none; position: absolute; right: 0; top: calc(100% + 8px); background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 10px 14px; z-index: 30; min-width: 170px; }
  .ctx-wrap:hover .ctx-popup, .ctx-wrap:focus-within .ctx-popup { display: block; }
  .ctx-row { display: flex; justify-content: space-between; gap: 24px; font-size: 13px; color: var(--text); padding: 2px 0; font-variant-numeric: tabular-nums; }
  .ctx-row span:first-child { color: var(--muted-2); }
  .compact-div { display: flex; align-items: center; gap: 12px; margin: 16px 0 10px; color: var(--muted-2); font-size: 12.5px; cursor: help; }
  .compact-div::before, .compact-div::after { content: ""; flex: 1; height: 1px; background: var(--border-2); }
  .compact-div > span { white-space: nowrap; }
  /* Wrap whole chips ("Context handoff", "model →") onto new lines in a narrow
   * column instead of squeezing each text run into a word-per-line stack. */
  .handoff-inner { display: inline-flex; flex-wrap: wrap; justify-content: center; align-items: center; gap: 2px 6px; }
  .handoff-div > :global(.tooltip-wrap) { min-width: 0; }
  /* Keep each engine/folder icon on the same line as its label. */
  .handoff-part { display: inline-flex; align-items: center; gap: 6px; min-width: 0; }
  .ws-handoff-chip { display: inline-flex; align-items: center; opacity: 0.85; }
  .handoff-sep { opacity: 0.5; }
  .work-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .work-composer-bar { display: flex; align-items: center; gap: 8px; margin-bottom: 8px; }
  .work-composer-bar .work-badge { margin-left: 0; }
  .work-composer-bar .workspace-input { flex: 1; min-width: 0; }
  .explorer-toggle-btn { position: absolute; top: 10px; right: 12px; z-index: 7; background: var(--bg); }
  .explorer-toggle-btn[aria-pressed="true"] { color: var(--accent); }
  .mcp-btn { position: relative; }
  .mcp-dot { position: absolute; top: -4px; right: -4px; min-width: 15px; height: 15px; padding: 0 3px; box-sizing: border-box; border-radius: 999px; background: var(--accent); color: var(--accent-fg); font-size: 10px; font-weight: 700; line-height: 15px; text-align: center; }
  /* Narrow main column (small window OR side panels open): shift the Work
   * banner right so the collapsed-rail toggle can't overlap them.
   * Viewport media queries can't see panel-driven narrowing, hence @container. */
  @container (max-width: 920px) {
    .work-banner { padding-left: 60px; }
  }
  /* The collapsed rail (60px, absolute) sits over the column: once the
   * centered 860px thread can't clear it ((w-860)/2 < 60), reserve its width
   * so the thread and composer never slide under the rail icons. */
  @container (max-width: 980px) {
    .main:has(> .collapsed-rail) .messages { padding-left: 60px; }
    .main:has(> .collapsed-rail) .composer-wrap { padding-left: 60px; }
  }
  @media (max-width: 700px) {
    .messages { padding: 20px 12px 8px 12px; }
    .composer-wrap { padding: 8px 12px 14px 12px; }
    .suggest-grid { grid-template-columns: 1fr; }
    .empty-title { font-size: 20px; margin-bottom: 20px; }
    .model-pill { max-width: 150px; }
    .model-drop { width: 330px; max-width: calc(100vw - 48px); }
  .collapsed-rail-foot { display: none; }
    .work-banner { padding-left: 60px; }
    .sidebar { position: fixed; left: 0; top: 0; bottom: 0; z-index: 60; background: var(--bg); box-shadow: 8px 0 30px rgba(0,0,0,.15); }
    .explorer { position: fixed; right: 0; top: 0; bottom: 0; z-index: 60; background: var(--bg); box-shadow: -8px 0 30px rgba(0,0,0,.15); }
    .confirm-card { width: calc(100vw - 32px); }
    .settings-layout { flex-direction: column; }
    .settings-nav { width: auto; flex-direction: row; align-items: center; gap: 6px; padding: 12px 44px 10px 14px; border-right: none; border-bottom: 1px solid var(--border); overflow-x: auto; }
    .settings-nav-title, .settings-nav-label { display: none; }
    .settings-nav-group { flex-direction: row; gap: 4px; flex-shrink: 0; }
    .settings-nav-btn { width: auto; white-space: nowrap; padding: 6px 10px; font-size: 12.5px; }
    .settings-pane-head { padding: 12px 16px 10px 16px; }
    .settings-pane-body { padding: 14px 16px 18px 16px; }
  }
</style>
