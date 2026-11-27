// Cmd+P palette: local session search, pasted session-link lookup, and
// connected-server search (with the old-server fallback). Created once by ChatApp.
import { sessionIdIn, mergeLocalServerHits, orderPaletteRows, showHostLabels, remoteQueryFor, describeServerSearchError } from "./palette";
import { type Chat } from "./store";
import { isServerBacked, type ServerChatHit, invokeOrBridge, searchServerChats, openChatInWindow } from "./bridge";
import { type RemoteHit, type SearchItem } from "./chatAppData";
import type { ServerSwitcherState } from "./serverSwitcherState.svelte";

export interface GotoPaletteDeps {
  readonly srv: ServerSwitcherState;
  readonly chats: Chat[];
  readonly activeServer: string | null;
  readonly onServer: boolean;
  readonly newChat: () => void;
  readonly selectChat: (id: string) => void;
  error: string | null;
}

export function createGotoPaletteState(deps: GotoPaletteDeps) {
  let gotoOpen = $state(false);
  let gotoInput = $state("");
  let gotoError = $state<string | null>(null);
  let gotoSel = $state(0);
  function openGoto() {
    gotoInput = "";
    gotoError = null;
    gotoSel = 0;
    gotoOpen = true;
    // fresh host list: a pasted link is looked up on every connected server
    void deps.srv.refreshServers();
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
    return { id, chat: deps.chats.find((c) => c.id.toLowerCase() === id) ?? null };
  });
  const searchResults = $derived.by((): Chat[] => {
    const q = searchQueryLow;
    if (!q) return deps.chats.filter((c) => c.messages.length > 0).slice(0, 8);
    return deps.chats.filter((c) => {
      if (c.messages.length === 0) return false;
      if (c.title.toLowerCase().includes(q)) return true;
      if (c.id.toLowerCase().includes(q)) return true;
      return c.messages.some((m) => !m.contextCleared && m.content.toLowerCase().includes(q));
    }).slice(0, 20);
  });
  /** connected hosts whose chats are NOT loaded in this window */
  const remoteTargets = $derived(deps.srv.srvList.filter((s) => s.id !== deps.activeServer));
  const hasPartialChats = $derived(isServerBacked() && deps.chats.some((c) => c.partial));
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
      ? mergeLocalServerHits(searchResults, localServerHits, deps.chats, 20)
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
  const searchShowHosts = $derived(showHostLabels(deps.onServer, remoteHits.length));
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
    if (it.kind === "new") deps.newChat();
    else deps.selectChat(it.chat.id);
  }
  /** A hit on another host: raise that host's window on the session. The
   *  palette's own window keeps whatever it was doing. */
  async function openRemoteHit(hit: RemoteHit) {
    closeGoto();
    try {
      await openChatInWindow(hit.serverId, hit.id);
    } catch (e: any) {
      deps.error = typeof e === "string" ? e : (e?.message ?? String(e));
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

  return {
    get gotoOpen() { return gotoOpen; },
    set gotoOpen(v) { gotoOpen = v; },
    get gotoInput() { return gotoInput; },
    set gotoInput(v) { gotoInput = v; },
    get gotoError() { return gotoError; },
    set gotoError(v) { gotoError = v; },
    get gotoSel() { return gotoSel; },
    set gotoSel(v) { gotoSel = v; },
    openGoto,
    closeGoto,
    get searchQuery() { return searchQuery; },
    get searchQueryLow() { return searchQueryLow; },
    get gotoSessionId() { return gotoSessionId; },
    get gotoIdHit() { return gotoIdHit; },
    get searchResults() { return searchResults; },
    get remoteTargets() { return remoteTargets; },
    get hasPartialChats() { return hasPartialChats; },
    get localServerHits() { return localServerHits; },
    set localServerHits(v) { localServerHits = v; },
    get localServerSearching() { return localServerSearching; },
    set localServerSearching(v) { localServerSearching = v; },
    get remoteHits() { return remoteHits; },
    set remoteHits(v) { remoteHits = v; },
    get remoteSearching() { return remoteSearching; },
    set remoteSearching(v) { remoteSearching = v; },
    get remoteFailures() { return remoteFailures; },
    set remoteFailures(v) { remoteFailures = v; },
    get remoteNotices() { return remoteNotices; },
    set remoteNotices(v) { remoteNotices = v; },
    get remoteKey() { return remoteKey; },
    set remoteKey(v) { remoteKey = v; },
    get remoteTimer() { return remoteTimer; },
    set remoteTimer(v) { remoteTimer = v; },
    get remotePending() { return remotePending; },
    set remotePending(v) { remotePending = v; },
    get remoteResolve() { return remoteResolve; },
    set remoteResolve(v) { remoteResolve = v; },
    get searchItems() { return searchItems; },
    get searchShowHosts() { return searchShowHosts; },
    openSearchItem,
    openRemoteHit,
    submitSearch,
    onSearchKey
  };
}

export type GotoPaletteState = ReturnType<typeof createGotoPaletteState>;
