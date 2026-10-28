<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { goto } from "$app/navigation";
  import { listModels, listRunning, loadModelIntoMemory, unloadModelFromMemory, pullModel, deleteModel, chatStream, generateStream, setOllamaBase, ollamaBase as activeOllamaBase, isCloudModel, serverVersion, startServer as startOllamaServer, stopServer as stopOllamaServer, isTauriRuntime, type ChatMsg, type RunningModel } from "$lib/ollama";
  import { renderMarkdown } from "$lib/markdown";
  import { MODEL_CATALOG, CATALOG_UPDATED, type CatalogCat } from "$lib/catalog";
  import {
    initStorage,
    loadChats,
    saveChats,
    loadModel,
    saveModel,
    loadPrefs,
    savePrefs,
    readLocalPrefs,
    writePendingStream,
    readPendingStream,
    clearPendingStream,
    DEFAULT_STOP_WORDS,
    type Chat
  } from "$lib/store";

  let { sessionId = null }: { sessionId: string | null } = $props();

  let chats = $state<Chat[]>([]);
  let activeId = $state<string | null>(null);
  let ready = $state(false);
  const initialPrefs = typeof window !== "undefined" ? readLocalPrefs() : { theme: "light" as const, accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "" };
  let sidebarOpen = $state(initialPrefs.sidebarOpen);
  let input = $state("");
  let models = $state<string[]>(["muse-glimmer:30b-mlx"]);
  let selectedModel = $state("muse-glimmer:30b-mlx");
  let modelOpen = $state(false);
  let modelQuery = $state("");
  let streaming = $state(false);
  let continuing = $state(false);
  /** picked or pasted image attachments as data URLs (shown as thumbnails, sent as base64) */
  let attachImages = $state<string[]>([]);
  let error = $state<string | null>(null);
  let aborter = $state<AbortController | null>(null);
  let messagesEl = $state<HTMLDivElement | null>(null);
  let composerEl = $state<HTMLTextAreaElement | null>(null);
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

  const activeChat = $derived(chats.find((c) => c.id === activeId) ?? null);

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
    flushSave();
  }

  function newChat() {
    aborter?.abort();
    aborter = null;
    streaming = false;
    // reuse the existing empty draft instead of stacking blank sessions
    const draft = chats.find((c) => c.messages.length === 0);
    if (draft) {
      pruneEmptyDrafts(draft.id);
      activeId = draft.id;
      isPinned = true;
      editingIndex = null;
      editDraft = "";
      input = "";
      error = null;
      void goto(`/s/${draft.id}`);
      return;
    }
    const c: Chat = {
      id: crypto.randomUUID(),
      title: "New chat",
      createdAt: Date.now(),
      messages: []
    };
    chats = [c, ...chats];
    activeId = c.id;
    isPinned = true;
    editingIndex = null;
    editDraft = "";
    input = "";
    error = null;
    save();
    void goto(`/s/${c.id}`);
  }

  function selectChat(id: string) {
    if (id === activeId) return;
    aborter?.abort();
    aborter = null;
    streaming = false;
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
    aborter?.abort();
    aborter = null;
    streaming = false;
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
  let settingsOpen = $state(false);
  let settingsTab = $state<"general" | "models">("general");
  let runners = $state<RunningModel[]>([]);
  let runnersError = $state<string | null>(null);
  let modelOp = $state<string | null>(null);
  let pullName = $state("");
  let pulling = $state<string | null>(null);
  let pullStatus = $state("");
  let pullDone = $state(0);
  let pullTotal = $state(0);
  let pullAborter: AbortController | null = null;
  let confirmDeleteModel = $state<string | null>(null);
  let browseView = $state<"installed" | "discover">("installed");
  let browseQuery = $state("");
  let browseCat = $state<"all" | CatalogCat | "small">("all");
  let variantPick = $state<Record<string, string>>({});

  async function openSettings() {
    settingsTab = "general";
    settingsOpen = true;
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
    runnersError = null;
    pullAborter = new AbortController();
    try {
      await pullModel(
        name,
        (p) => {
          pullStatus = p.status;
          if (typeof p.completed === "number") pullDone = p.completed;
          if (typeof p.total === "number") pullTotal = p.total;
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
    savePrefs({ theme, accent, fontSize, sidebarOpen, stopWords, ollamaBase });
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
    gotoOpen = true;
  }

  function closeGoto() {
    gotoOpen = false;
    gotoInput = "";
    gotoError = null;
  }

  function submitGoto() {
    const m = gotoInput.match(/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i);
    if (!m) {
      gotoError = "Couldn't find a session id in that link.";
      return;
    }
    const id = m[0].toLowerCase();
    if (!chats.some((c) => c.id.toLowerCase() === id)) {
      gotoError = "No chat found for that session.";
      return;
    }
    const target = chats.find((c) => c.id.toLowerCase() === id)!;
    closeGoto();
    selectChat(target.id);
  }

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

  // deep links (`vicuna://s/<id>`, e.g. pasted into a browser) land here via
  // the deep-link plugin; navigate once chats are loaded, stash until then
  let pendingDeepLinkId: string | null = null;
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
    if (!id) return;
    if (ready && chats.some((c) => c.id === id)) {
      activeId = id;
      isPinned = true;
      void goto(`/s/${id}`);
    } else {
      // initApp flushes this after load; unknown ids are dropped there
      pendingDeepLinkId = id;
    }
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

  // an empty draft you navigate away from without sending anything is
  // discarded instead of lingering in the list doing nothing
  function dropActiveDraft(exceptId: string | null) {
    if (!activeId || activeId === exceptId) return;
    const c = chats.find((x) => x.id === activeId);
    if (c && c.messages.length === 0) {
      chats = chats.filter((x) => x.id !== activeId);
      save();
    }
  }

  // keep at most one empty draft: pressing New Chat repeatedly must not
  // pile up blank sessions (also cleans up previously accumulated ones)
  function pruneEmptyDrafts(keepId: string | null) {
    let kept = false;
    const remaining = chats.filter((c) => {
      if (c.messages.length > 0) return true;
      if (keepId !== null) return c.id === keepId;
      if (!kept) {
        kept = true;
        return true;
      }
      return false;
    });
    if (remaining.length !== chats.length) {
      chats = remaining;
      save();
    }
  }

  function requestMsgDelete(i: number) {
    if (streaming) return;
    ctxMenu = null;
    pendingMsgDelete = i;
  }

  function confirmMsgDelete() {
    const chat = chats.find((c) => c.id === activeId);
    const i = pendingMsgDelete;
    pendingMsgDelete = null;
    if (!chat || i == null || streaming) return;
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
  const BROWSE_CATS = ["all", "chat", "code", "reasoning", "vision", "embedding", "small"] as const;
  const filteredCatalog = $derived.by(() => {
    const q = browseQuery.trim().toLowerCase();
    return MODEL_CATALOG.filter((e) => {
      if (browseCat === "small") {
        if (Math.max(...e.variants.map((v) => v.sizeMB)) > 2560) return false;
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

  // delegated handler for code-block copy buttons rendered via {@html}
  const CODE_CHECK_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>';
  $effect(() => {
    const el = messagesEl;
    if (!el) return;
    const onClick = (e: MouseEvent) => {
      const t = e.target as HTMLElement | null;
      const btn = t?.closest?.("[data-copy-code]") as HTMLElement | null;
      if (!btn) return;
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
    if (!m || streaming) return;
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
    if (!chat || editingIndex == null || streaming) return;
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
    if (text && (chat.title === "New chat" || chat.title === "New Chat")) {
      chat.title = text.slice(0, 32) + (text.length > 32 ? ".." : "");
    }
    cancelEdit();
    const history: ChatMsg[] = [
      ...chat.messages.map(toHistory)
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
    if (!c || streaming) return;
    aborter?.abort();
    aborter = null;
    const fork: Chat = {
      id: crypto.randomUUID(),
      title: `${c.title} (fork)`,
      createdAt: Date.now(),
      messages: JSON.parse(JSON.stringify(c.messages.slice(0, i + 1))),
      forkedFrom: { id: c.id, title: c.title },
      forkFromIndex: i
    };
    chats = [fork, ...chats];
    activeId = fork.id;
    isPinned = true;
    editingIndex = null;
    editDraft = "";
    editImages = [];
    pendingSaveConfirm = false;
    save();
    void goto(`/s/${fork.id}`);
  }

  async function streamInto(chat: Chat, index: number, history: ChatMsg[], removeIfEmpty: boolean, opts: { think?: boolean; temperature?: number; generatePrompt?: string; stop?: string[]; mode?: "fresh" | "continue"; onImagesStripped?: () => void } = {}) {
    streaming = true;
    const ctrl = new AbortController();
    aborter = ctrl;
    let produced = false;
    isPinned = true;
    // mark the in-flight stream synchronously: a refresh before settle
    // resumes from the persisted partial instead of losing the turn
    writePendingStream({
      chatId: chat.id,
      index,
      mode: opts.mode ?? "fresh",
      model: selectedModel,
      think: opts.think,
      temperature: opts.temperature,
      stop: opts.stop
    });
    save();
    scrollBottom();
    try {
      const onTok = (tok: string) => {
        produced = true;
        const c = chats.find((x) => x.id === chat.id);
        if (c && c.messages[index]) {
          const m = c.messages[index];
          c.messages[index] = { role: "assistant", content: m.content + tok, thinking: m.thinking, thinkingOpen: m.thinkingOpen, model: m.model ?? selectedModel };
          chats = [...chats];
        }
        scrollBottom();
        saveSoon();
      };
      const onTh = (tok: string) => {
        produced = true;
        const c = chats.find((x) => x.id === chat.id);
        if (c && c.messages[index]) {
          const m = c.messages[index];
          c.messages[index] = { role: "assistant", content: m.content, thinking: (m.thinking ?? "") + tok, thinkingOpen: m.thinkingOpen, model: m.model ?? selectedModel };
          chats = [...chats];
        }
        scrollBottom();
        saveSoon();
      };
      if (opts.generatePrompt !== undefined) {
        await generateStream(selectedModel, opts.generatePrompt, onTok, ctrl.signal, undefined, 0, { temperature: opts.temperature, stop: opts.stop });
      } else {
        await chatStream(selectedModel, history, onTok, ctrl.signal, onTh, 0, { think: opts.think, temperature: opts.temperature, stop: opts.stop, onImagesStripped: opts.onImagesStripped ?? noteImagesStripped });
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
      aborter = null;
      // user stop / natural settle clears the marker; a refresh/close
      // abort keeps it so the next load resumes the turn
      if (!unloading) clearPendingStream();
      save();
      scrollBottom();
    }
  }

  async function continueFrom(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || streaming) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    error = null;
    // Continue via /api/chat: resend the history ending with the partial
    // reply as-is so the model carries on exactly where it stopped instead
    // of starting a fresh turn. No re-thinking and low temperature: no
    // fresh plans, no wandering. Custom stop words end the generation if
    // it tries to start a new turn ("User:" etc.).
    const history: ChatMsg[] = chat.messages.slice(0, i + 1).map(toHistory);
    continuing = true;
    try {
      await streamInto(chat, i, history, false, { think: false, temperature: 0.2, stop: stopWords, mode: "continue" });
    } finally {
      continuing = false;
    }
  }

  async function regenerate(i: number) {
    const chat = chats.find((c) => c.id === activeId);
    if (!chat || streaming) return;
    if (i !== chat.messages.length - 1) return;
    const target = chat.messages[i];
    if (!target || target.role !== "assistant") return;
    error = null;
    cancelEdit();
    // drop the old reply and generate fresh with the currently selected model
    chat.messages = [...chat.messages.slice(0, i)];
    const freshHistory: ChatMsg[] = chat.messages.map(toHistory);
    chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    chats = [...chats];
    await streamInto(chat, chat.messages.length - 1, freshHistory, true);
  }

  async function send() {
    const text = input.trim();
    if ((!text && attachImages.length === 0) || streaming) return;
    let chat = chats.find((c) => c.id === activeId);
    let freshChat = false;
    const images = attachImages.length ? [...attachImages] : undefined;
    if (!chat) {
      // sending from home (or any stateless view): create the session inline.
      // navigation is deferred until the stream finishes, otherwise the
      // page unmount would abort the just-started request.
      // NOTE: the session must enter reactive state COMPLETE (both messages).
      // Svelte 5 caches proxied reads, so mutating the raw object after
      // prepending stays invisible until the first streamed token writes
      // through the proxy — the composer would look stuck on empty.
      const titleSrc = text || (images ? "Image" : "New chat");
      chat = {
        id: crypto.randomUUID(),
        title: titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : ""),
        createdAt: Date.now(),
        messages: [
          { role: "user", content: text, ...(images ? { images } : {}) },
          { role: "assistant", content: "", thinking: "", model: selectedModel }
        ]
      };
      chats = [chat, ...chats];
      activeId = chat.id;
      isPinned = true;
      freshChat = true;
      save();
    } else {
      chat.messages = [...chat.messages, { role: "user", content: text, ...(images ? { images } : {}) }];
      if (chat.title === "New chat" || chat.title === "New Chat") {
        const titleSrc = text || (images ? "Image" : "New chat");
        chat.title = titleSrc.slice(0, 32) + (titleSrc.length > 32 ? ".." : "");
      }
      chat.messages = [...chat.messages, { role: "assistant", content: "", thinking: "", model: selectedModel }];
    }
    input = "";
    attachImages = [];
    error = null;

    // history excludes the trailing placeholder being generated
    const history: ChatMsg[] = chat.messages.slice(0, -1).map(toHistory);
    await streamInto(chat, chat.messages.length - 1, history, true);
    if (freshChat && activeId === chat.id) {
      // user stayed put: now reflect the session in the URL
      void goto(`/s/${chat.id}`);
    }
  }

  function stop() {
    aborter?.abort();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  }

  function pickFile() {
    fileInput?.click();
  }

  // history entries keep attached images so regenerate/continue/resume
  // resend the same picture instead of silently dropping it
  function toHistory(m: ChatMsg): ChatMsg {
    return m.images?.length
      ? { role: m.role, content: m.content, images: [...m.images] }
      : { role: m.role, content: m.content };
  }

  function noteImagesStripped() {
    error = `Note: ${selectedModel} doesn't support images, so the attached picture was not sent.`;
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

  async function onFile(e: Event) {
    const el = e.target as HTMLInputElement;
    const files = el.files ? [...el.files] : [];
    el.value = "";
    if (!files.length) return;
    let skipped = 0;
    for (const f of files) {
      if (!f.type.startsWith("image/")) {
        skipped++;
        continue;
      }
      try {
        attachImages = [...attachImages, await imageFileToDataUrl(f)];
      } catch {
        error = `Couldn't read image (${f.name || f.type || "unknown type"})`;
        skipped++;
      }
    }
    if (skipped > 0) {
      error = `Skipped ${skipped} non-image file${skipped === 1 ? "" : "s"} — only images can be attached`;
    }
    autosizeComposer();
    requestAnimationFrame(() => composerEl?.focus());
  }

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
      // narrow screens start collapsed regardless of the saved preference
      sidebarOpen = window.innerWidth < 700 ? false : prefs.sidebarOpen;
      chats = await loadChats();
      // collapse previously accumulated empty drafts into one (prefer the
      // session in the URL so a deep-linked blank draft isn't yanked away)
      const empties = chats.filter((c) => c.messages.length === 0);
      if (empties.length > 1) {
        const keepId = empties.some((c) => c.id === sessionId) ? sessionId : empties[0].id;
        chats = chats.filter((c) => c.messages.length > 0 || c.id === keepId);
        save();
      }
      if (sessionId) {
        // deep link: only open the chat named in the URL
        if (chats.some((c) => c.id === sessionId)) {
          activeId = sessionId;
        } else {
          void goto("/");
        }
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
      const m = await loadModel();
      if (m) selectedModel = m;
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
        const resumeHistory: ChatMsg[] = (
          pending.mode === "continue" ? pc.messages.slice(0, pending.index + 1) : pc.messages.slice(0, pending.index)
        ).map(toHistory);
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
        <button class="icon-btn" data-testid="sidebar-toggle" aria-label="Toggle sidebar" onclick={() => setSidebar(false)}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="2" y="3" width="16" height="14" rx="2"/><line x1="8" y1="3" x2="8" y2="17"/></svg>
        </button>
      </div>

      <div class="side-actions">
        <button class="newchat" data-testid="new-chat-btn" onclick={newChat}>
          <svg width="16" height="16" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.6"><path d="M12 3H5a2 2 0 0 0-2 2v10a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2v-7"/><path d="M14 3l3 3L8 15l-4 1 1-4L14 3z"/></svg>
          <span>New Chat</span>
        </button>
        <button class="goto" data-testid="goto-btn" onclick={openGoto}>
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg>
          <span>Go to</span>
        </button>
      </div>

      <div class="history">
        {#snippet chatList(items: Chat[])}
          {#each items as c (c.id)}
            <button
              class="chat-item {c.id === activeId ? 'active' : ''}"
              data-testid="chat-item"
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
                <span class="title">{c.title}</span>
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
        {#if chats.length === 0}
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
          <button class="ctx-item danger" data-testid="delete-item" onclick={() => requestDelete(ctxMenu!.chatId)}>Delete</button>
        </div>
      {/if}
    </aside>
  {/if}

  <main class="main">
    {#snippet forkNote(title: string, sourceId: string)}
      {@const source = chats.find((c) => c.id === sourceId)}
      <div class="fork-note" data-testid="fork-note">
        <span>Forked from </span>
        {#if source}
          <button class="fork-link" data-testid="fork-source-link" onclick={() => selectChat(source.id)}>{title}</button>
        {:else}
          <span>{title}</span>
        {/if}
      </div>
    {/snippet}
    {#if !sidebarOpen}
      <div class="collapsed-rail">
        <button class="icon-btn" data-testid="sidebar-toggle" aria-label="Open sidebar" onclick={() => setSidebar(true)}>
          <svg width="18" height="18" viewBox="0 0 20 20" fill="none" stroke="currentColor" stroke-width="1.5"><rect x="2" y="3" width="16" height="14" rx="2"/><line x1="8" y1="3" x2="8" y2="17"/></svg>
        </button>
        <button class="icon-btn" data-testid="settings-btn-collapsed" aria-label="Open settings" title="Settings" onclick={openSettings}>
          <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"><path fill="currentColor" stroke="none" d="M12 21a8.985 8.985 0 0 1-1.755-.173 1 1 0 0 1-.791-.813l-.273-1.606a6.933 6.933 0 0 1-1.32-.762l-1.527.566a1 1 0 0 1-1.1-.278 8.977 8.977 0 0 1-1.756-3.041 1 1 0 0 1 .31-1.092l1.254-1.04a6.979 6.979 0 0 1 0-1.524L3.787 10.2a1 1 0 0 1-.31-1.092 8.977 8.977 0 0 1 1.756-3.042 1 1 0 0 1 1.1-.278l1.527.566a6.933 6.933 0 0 1 1.32-.762l.274-1.606a1 1 0 0 1 .791-.813 8.957 8.957 0 0 1 3.51 0 1 1 0 0 1 .791.813l.273 1.606a6.933 6.933 0 0 1 1.32.762l1.527-.566a1 1 0 0 1 1.1.278 8.977 8.977 0 0 1 1.756 3.041 1 1 0 0 1-.31 1.092l-1.254 1.04a6.979 6.979 0 0 1 0 1.524l1.254 1.04a1 1 0 0 1 .31 1.092 8.977 8.977 0 0 1-1.756 3.041 1 1 0 0 1-1.1.278l-1.527-.566a6.933 6.933 0 0 1-1.32.762l-.273 1.606a1 1 0 0 1-.791.813A8.985 8.985 0 0 1 12 21zm-.7-2.035a6.913 6.913 0 0 0 1.393 0l.247-1.451a1 1 0 0 1 .664-.779 4.974 4.974 0 0 0 1.696-.975 1 1 0 0 1 1.008-.186l1.381.512a7.012 7.012 0 0 0 .7-1.206l-1.133-.939a1 1 0 0 1-.343-.964 5.018 5.018 0 0 0 0-1.953 1 1 0 0 1 .343-.964l1.124-.94a7.012 7.012 0 0 0-.7-1.206l-1.38.512a1 1 0 0 1-1-.186 4.974 4.974 0 0 0-1.688-.976 1 1 0 0 1-.664-.779l-.248-1.45a6.913 6.913 0 0 0-1.393 0l-.25 1.45a1 1 0 0 1-.664.779A4.974 4.974 0 0 0 8.7 8.24a1 1 0 0 1-1 .186l-1.385-.512a7.012 7.012 0 0 0-.7 1.206l1.133.939a1 1 0 0 1 .343.964 5.018 5.018 0 0 0 0 1.953 1 1 0 0 1-.343.964l-1.128.94a7.012 7.012 0 0 0 .7 1.206l1.38-.512a1 1 0 0 1 1 .186 4.974 4.974 0 0 0 1.688.976 1 1 0 0 1 .664.779zm.7-3.725a3.24 3.24 0 0 1 0-6.48 3.24 3.24 0 0 1 0 6.48zm0-4.48A1.24 1.24 0 1 0 13.24 12 1.244 1.244 0 0 0 12 10.76z"/></svg>
        </button>
      </div>
    {/if}

    {#if !activeChat || activeChat.messages.length === 0}
      <div class="empty">
        <h1 class="empty-title">How can I help you today?</h1>
        <div class="suggest-grid" data-testid="empty-suggestions">
          {#each SUGGESTIONS as s, idx}
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
      <div
        class="messages"
        bind:this={messagesEl}
        data-testid="messages"
        role="log"
        aria-label="Chat messages"
        onscroll={onMessagesScroll}
      >
        {#if forkInfo && forkNoteIndex == null}
          {@render forkNote(forkInfo.title, forkInfo.id)}
        {/if}
        {#each activeChat.messages as m, i}
          {@const copyKey = `${activeId}:${i}`}
          {#if m.role === "user"}
            <div class="row user-row {editingIndex === i ? 'is-editing' : ''}"><div class="col">
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
              <div class="bubble user">{m.content}</div>
              <div class="actions">
                <button class="act" data-testid="copy-btn" aria-label="Copy message" title="Copy" onclick={() => copyMessage(i)}>
                  {#if copiedKey === copyKey}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>{/if}
                </button>
                <button class="act" data-testid="edit-btn" aria-label="Edit message" title="Edit" disabled={streaming} onclick={() => startEdit(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
                </button>
                <button class="act" data-testid="msg-delete-btn" aria-label="Delete message" title="Delete" disabled={streaming} onclick={() => requestMsgDelete(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
                </button>
              </div>
              {/if}
            </div></div>
          {:else}
            {@const isLast = i === activeChat.messages.length - 1}
            {@const thinkingActive = streaming && isLast && !m.content && !continuing}
            <div class="row asst-row {editingIndex === i ? 'is-editing' : ''}"><div class="col">
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
                {@const expanded = thinkingActive || (m.thinkingOpen ?? false)}
                <div class="thinking" data-testid="thinking-block">
                  <button
                    class="thinking-head"
                    data-testid="thinking-toggle"
                    aria-expanded={expanded}
                    onclick={() => { m.thinkingOpen = !expanded; chats = [...chats]; }}
                  >
                    <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18h6"/><path d="M10 21h4"/><path d="M12 3a6 6 0 0 0-4 10.5c.8.7 1 1.5 1 2.5h6c0-1 .2-1.8 1-2.5A6 6 0 0 0 12 3z"/></svg>
                    <span>{thinkingActive ? "Thinking..." : "Thinking"}</span>
                    <span class="chev">{expanded ? "▾" : "▸"}</span>
                  </button>
                  {#if expanded}
                    <div class="thinking-body">{m.thinking}</div>
                  {/if}
                </div>
              {/if}
              <div class="answer">{@html m.content ? renderMarkdown(m.content) : (streaming && isLast ? "…" : "")}</div>
              <div class="actions">
                <button class="act" data-testid="copy-btn" aria-label="Copy message" title="Copy" onclick={() => copyMessage(i)}>
                  {#if copiedKey === copyKey}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>{:else}<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="9" y="9" width="13" height="13" rx="2"/><path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"/></svg>{/if}
                </button>
                <button class="act" data-testid="edit-btn" aria-label="Edit message" title="Edit" disabled={streaming} onclick={() => startEdit(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>
                </button>
                {#if isLast}
                  {@const noContinue = isCloudModel(selectedModel)}
                  <button class="act" data-testid="continue-btn" aria-label="Continue generating" title={noContinue ? "Continue isn't supported for cloud models" : "Continue"} disabled={streaming || noContinue} onclick={() => continueFrom(i)}>
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polygon points="6 3 20 12 6 21 6 3"/></svg>
                  </button>
                  <button class="act" data-testid="regen-btn" aria-label="Regenerate response" title="Regenerate" disabled={streaming} onclick={() => void regenerate(i)}>
                    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="23 4 23 10 17 10"/><path d="M20.49 15a9 9 0 1 1-2.12-9.36L23 10"/></svg>
                  </button>
                {/if}
                <button class="act" data-testid="fork-btn" aria-label="Fork chat from here" title="Fork" disabled={streaming} onclick={() => forkAt(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.2"/><circle cx="6" cy="18" r="2.2"/><circle cx="18" cy="8" r="2.2"/><path d="M6 8.2v7.6"/><path d="M18 10.2c0 3.5-5 2.8-8.5 5.3"/></svg>
                </button>
                <button class="act" data-testid="msg-delete-btn" aria-label="Delete message" title="Delete" disabled={streaming} onclick={() => requestMsgDelete(i)}>
                  <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="3 6 5 6 21 6"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/></svg>
                </button>
                {#if m.model}
                  <span class="model-tag" data-testid="msg-model" title="Generated by {m.model}">{m.model}</span>
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

    <div class="composer-wrap">
      {#if !isPinned}
        <button
          class="scroll-jump"
          data-testid="jump-bottom"
          aria-label="Scroll to bottom and resume auto-scroll"
          onclick={jumpToBottom}
        >↓</button>
      {/if}
      <div class="composer">
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
        <textarea
          data-testid="message-input"
          placeholder="Send a message ( / to focus )"
          bind:this={composerEl}
          bind:value={input}
          onkeydown={onKey}
          onpaste={onPaste}
          rows={1}
        ></textarea>
        <div class="composer-bar">
          <div class="spacer"></div>
          <button class="pill-btn" data-testid="attach-btn" aria-label="Attach image" onclick={pickFile}>+</button>
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
          {#if streaming}
            <button class="send streaming" data-testid="stop-btn" aria-label="Stop" onclick={stop}>■</button>
          {:else}
            <button class="send" data-testid="send-btn" aria-label="Send" disabled={input.trim() === "" && attachImages.length === 0} onclick={send}>↑</button>
          {/if}
        </div>
      </div>
    </div>
  </main>
</div>
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
    class="confirm-card"
    data-testid="goto-dialog"
    role="dialog"
    aria-modal="true"
    aria-label="Go to session"
  >
    <p class="confirm-text">Go to session</p>
    <input
      class="goto-input"
      data-testid="goto-input"
      placeholder="Paste a session link or ID…"
      bind:value={gotoInput}
      autofocus
      onkeydown={(e) => {
        if (e.key === "Enter") submitGoto();
        else if (e.key === "Escape") closeGoto();
      }}
    />
    {#if gotoError}
      <p class="goto-error" data-testid="goto-error">{gotoError}</p>
    {/if}
    <div class="confirm-actions">
      <button class="confirm-cancel" data-testid="cancel-goto-btn" onclick={closeGoto}>Cancel</button>
      <button class="confirm-ok goto-go" data-testid="goto-go-btn" onclick={submitGoto}>Go</button>
    </div>
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
            <span class="pull-pct">{((pullDone / pullTotal) * 100).toFixed(0)}% · {(pullDone / 1e9).toFixed(1)} / {(pullTotal / 1e9).toFixed(1)} GB</span>
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
          <button class="chip {browseCat === c ? 'sel' : ''}" data-testid="discover-chip" data-cat={c} onclick={() => (browseCat = c)}>{c === "all" ? "All" : c[0].toUpperCase() + c.slice(1)}</button>
        {/each}
      </div>
      <p class="browse-count" data-testid="discover-count">{filteredCatalog.length} of {MODEL_CATALOG.length} models · catalog {CATALOG_UPDATED}</p>
      {#each filteredCatalog as entry}
        {@const picked = pickedVariant(entry.name, entry.variants[0].tag)}
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

<input type="file" bind:this={fileInput} hidden accept="image/*" multiple onchange={onFile} data-testid="file-input" />

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
  }
  :global(html, body) { margin: 0; height: 100%; background: var(--bg); color: var(--text); font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", Inter, Helvetica, Arial, sans-serif; }
  .app { display: flex; height: 100vh; background: var(--bg); }
  .sidebar { width: 264px; min-width: 264px; padding: 10px 10px 20px 10px; display: flex; flex-direction: column; }
  .topbar { display: flex; align-items: center; justify-content: flex-end; padding: 2px 4px 12px 4px; }
  .icon-btn { background: transparent; border: none; cursor: pointer; color: var(--icon); padding: 4px; border-radius: 6px; }
  .icon-btn:hover { background: var(--hover); }
  .side-actions { display: flex; flex-direction: column; gap: 2px; margin-bottom: 18px; }
  .newchat { display: flex; gap: 8px; align-items: center; background: var(--item-bg); border: none; border-radius: 8px; padding: 8px 10px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text); }
  .newchat:hover { background: var(--hover-2); }
  .goto { display: flex; gap: 8px; align-items: center; background: transparent; border: none; border-radius: 8px; padding: 8px 10px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text-dim); }
  .goto:hover { background: var(--hover-2); }
  .side-footer { margin-top: auto; padding-top: 10px; display: flex; flex-direction: column; align-items: stretch; }
  .side-footer .goto { width: 100%; box-sizing: border-box; }
  .goto-input { width: 100%; box-sizing: border-box; border: 1px solid var(--input-border); border-radius: 12px; outline: none; padding: 10px 14px; font-size: 14px; font-family: inherit; margin-bottom: 12px; background: transparent; color: var(--text); }
  .goto-input:focus { border-color: var(--muted); }
  .goto-error { color: var(--danger); font-size: 13px; margin: 0 0 12px 0; }
  
  .history { overflow-y: auto; display: flex; flex-direction: column; gap: 2px; }
  .group-label { font-size: 12px; color: var(--muted); margin: 12px 4px 6px 4px; }
  .chat-item { display: flex; justify-content: space-between; align-items: center; border: none; background: transparent; padding: 7px 8px; border-radius: 8px; font-size: 14px; cursor: pointer; text-align: left; color: var(--text); user-select: none; -webkit-user-select: none; }
  .chat-item:hover { background: var(--hover-3); }
  .chat-item.active { background: var(--active-item); }
  .chat-item .title { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; flex: 1; }
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
  .collapsed-rail { position: absolute; left: 0; top: 0; bottom: 0; display: flex; flex-direction: column; justify-content: space-between; align-items: stretch; width: 60px; padding: 10px 8px; box-sizing: border-box; z-index: 5; }
  .collapsed-rail .icon-btn { width: 100%; padding: 8px 0; border-radius: 8px; }
  .empty { flex: 1; display: flex; flex-direction: column; align-items: center; justify-content: center; padding: 24px; overflow-y: auto; }
  .empty-title { font-size: 24px; font-weight: 500; color: var(--text-dim); margin: 0 0 28px 0; text-align: center; }
  .suggest-grid { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; width: 100%; max-width: 860px; }
  .suggest-card { display: flex; flex-direction: row; align-items: center; justify-content: space-between; gap: 12px; text-align: left; background: var(--card); border: 1px solid var(--border); border-radius: 16px; padding: 14px 14px 14px 18px; cursor: pointer; }
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
  .bubble.asst { background: transparent; padding-left: 0; padding-right: 0; }
  .col { display: flex; flex-direction: column; max-width: 75%; }
  .user-row .col { align-items: flex-end; }
  .asst-row .col { align-items: flex-start; }
  .actions { display: flex; gap: 2px; margin-top: 6px; align-items: center; }
  .model-tag { opacity: 0; transition: opacity 0.15s; font-size: 12px; color: var(--muted-2); margin-left: 6px; white-space: nowrap; max-width: 200px; overflow: hidden; text-overflow: ellipsis; }
  .row:hover .model-tag, .row:focus-within .model-tag { opacity: 1; }
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
  .thinking-body { color: var(--thinking); font-size: 15px; line-height: 1.65; white-space: pre-wrap; word-break: break-word; }
  .answer:empty { display: none; }
  .answer :global(p) { margin: 0 0 12px 0; }
  .answer :global(p:last-child) { margin-bottom: 0; }
  .answer :global(strong) { font-weight: 700; }
  .answer :global(em) { font-style: italic; }
  .answer :global(code) { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 13px; background: var(--code-inline); border-radius: 6px; padding: 1px 5px; }
  .answer :global(pre) { background: var(--code-bg); border-radius: 12px; padding: 12px 14px; overflow-x: auto; font-size: 13.5px; line-height: 1.6; }
  .answer :global(pre code) { background: transparent; padding: 0; }
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
  .composer-wrap { position: relative; padding: 12px 24px 22px 24px; max-width: 860px; width: 100%; margin: 0 auto; box-sizing: border-box; }
  .scroll-jump { position: absolute; top: -44px; left: 50%; transform: translateX(-50%); width: 44px; height: 36px; background: var(--card); border: 1px solid var(--border-3); border-radius: 12px; box-shadow: 0 4px 16px rgba(0,0,0,.10); cursor: pointer; font-size: 17px; line-height: 1; color: var(--text); z-index: 5; }
  .scroll-jump:hover { background: var(--composer); }
  .composer { background: var(--composer); border-radius: 22px; padding: 12px 12px 10px 16px; }
  .composer textarea { width: 100%; box-sizing: border-box; border: none; background: transparent; resize: none; outline: none; font-size: 15px; font-family: inherit; color: var(--text); overflow-x: hidden; overflow-y: auto; overflow-wrap: break-word; max-height: 200px; }
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
  .spacer { flex: 1; }
  .pill-btn { width: 32px; height: 32px; border-radius: 50%; border: none; background: var(--card); cursor: pointer; font-size: 18px; color: var(--icon-dim); display: flex; align-items: center; justify-content: center; }
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
  .browse-count { font-size: 12px; color: var(--muted); margin: 0; padding: 0 4px 8px 4px; }
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

  @media (max-width: 700px) {
    .messages { padding: 20px 12px 8px 12px; }
    .composer-wrap { padding: 8px 12px 14px 12px; }
    .col { max-width: 92%; }
    .suggest-grid { grid-template-columns: 1fr; }
    .empty-title { font-size: 20px; margin-bottom: 20px; }
    .model-pill { max-width: 150px; }
    .model-drop { width: 300px; max-width: calc(100vw - 48px); }
  .collapsed-rail [data-testid='settings-btn-collapsed'] { display: none; }
    .sidebar { position: fixed; left: 0; top: 0; bottom: 0; z-index: 60; background: var(--bg); box-shadow: 8px 0 30px rgba(0,0,0,.15); }
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
