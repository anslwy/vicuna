// Vicuna backend routing.
//
// Three transports, one call shape (same command names + camelCase args):
//   * desktop, This computer  → Tauri IPC (the local Rust backend)
//   * desktop, remote server  → `server_call` (Rust forwards over HTTP(S) to
//     a headless `vicuna serve`, e.g. a VPS behind tailscale serve;
//     see src-tauri/src/servers.rs). Each server has its own window (opened
//     from the sidebar's server menu with `?server=<id>` in its URL); the main
//     window is always This computer. Constant for the life of a window.
//   * plain browser (`npm run dev`) → the loopback dev bridge
//     (`cargo run` in bridge/), so live-browser drives the real backend.
// When none is present the call fails with the usual desktop error.

import { isTauriRuntime } from "./ollama";
import { searchLoadedChats, type LoadedChat } from "./serverSearch";

// ---- remote server selection (desktop only) ----

/** This window's remote server id (`?server=<id>`), or null for This
 *  computer. Per window, never shared: other windows keep their own server. */
const ACTIVE_SERVER: string | null = (() => {
  try {
    if (typeof window === "undefined" || !isTauriRuntime()) return null;
    // `?server=` from open_server_window; sessionStorage (per window) keeps it
    // across reloads, since in-app navigation rewrites the URL
    let id = new URLSearchParams(window.location.search).get("server");
    if (id) sessionStorage.setItem("vicuna-window-server", id);
    else id = sessionStorage.getItem("vicuna-window-server");
    // older builds kept the choice in localStorage for the one window
    if (!id) localStorage.removeItem("vicuna-server");
    return id && /^[A-Za-z0-9_-]+$/.test(id) ? id : null;
  } catch {
    return null;
  }
})();

export function activeServerId(): string | null {
  return ACTIVE_SERVER;
}

/** Desktop app pointed at a remote server (backend calls go there). */
export function isRemoteServer(): boolean {
  return ACTIVE_SERVER !== null;
}

/** Desktop app on its own backend: Tauri IPC applies. */
export function useLocalIpc(): boolean {
  return isTauriRuntime() && ACTIVE_SERVER === null;
}

/** Open (or focus) a server's own window, or focus the main window for
 *  This computer (null). Never replaces what the current window shows. */
export async function switchServer(id: string | null, title?: string): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  if (id) await invoke("open_server_window", { id, title: title ?? null });
  else await invoke("show_main_window");
}

/** Commands about THIS machine/app that stay local even when a server is
 *  active (app updates, CLI launcher, window, tray, the server list itself). */
const LOCAL_CMDS = new Set([
  "app_build_info",
  "github_latest_release",
  "github_download_asset",
  "cli_status",
  "cli_install",
  "cli_uninstall",
  "show_main_window",
  "quit_app",
  "tray_set_status",
  "server_list",
  "open_server_window",
  "open_chat_window",
  "server_connect",
  "server_touch",
  "server_remove",
  "server_lb_proxy_url"
]);

/** Local HTTP+WS proxy URL for a remote server's `lb` daemon. */
export async function serverLbProxyUrl(id: string): Promise<string> {
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<string>("server_lb_proxy_url", { id });
}

/** One call to the active server: `{ok, data | error}` unwrapped. */
async function serverInvoke(cmd: string, args: Record<string, any>): Promise<any> {
  const { invoke } = await import("@tauri-apps/api/core");
  const r = await invoke<any>("server_call", { id: ACTIVE_SERVER, path: "/invoke", body: { cmd, args } });
  if (!r?.ok) throw new Error(r?.error ?? `${cmd} failed on the server`);
  return r.data ?? null;
}

// ---- loopback dev bridge (plain browser) ----

function bridgePort(): number {
  try {
    // in-app navigation drops the query, so remember ?bridge= per tab
    const fromUrl = new URLSearchParams(window.location.search).get("bridge");
    if (fromUrl) sessionStorage.setItem("vicuna-bridge-port", fromUrl);
    const q = fromUrl ?? sessionStorage.getItem("vicuna-bridge-port");
    const n = q ? parseInt(q, 10) : NaN;
    if (Number.isFinite(n) && n > 0 && n < 65536) return n;
  } catch {
    /* ignore */
  }
  return 1421;
}

function bridgeBase(): string {
  return `http://127.0.0.1:${bridgePort()}`;
}

let healthCache: { ok: boolean; at: number } | null = null;

/** Is a bridge reachable? A remote server counts (desktop routes through
 *  it). Loopback probes are cached ~10s; a refused localhost port fails fast
 *  so the common no-bridge case costs one quick probe per burst. */
export async function bridgeAvailable(): Promise<boolean> {
  if (typeof window === "undefined") return false;
  if (isRemoteServer()) return true;
  if (healthCache && Date.now() - healthCache.at < 10000) return healthCache.ok;
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 1500);
    const res = await fetch(`${bridgeBase()}/health`, { signal: ctrl.signal });
    clearTimeout(t);
    const ok = res.ok;
    healthCache = { ok, at: Date.now() };
    return ok;
  } catch {
    healthCache = { ok: false, at: Date.now() };
    return false;
  }
}

/** Remote polls long-poll: the server holds the request until events arrive
 *  (or REMOTE_WAIT_MS passes), so output shows up at once instead of on the
 *  next tick — terminal echo needs this. */
const REMOTE_WAIT_MS = 15000;

async function pollOnce(id: string): Promise<any> {
  if (isRemoteServer()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<any>("server_call", { id: ACTIVE_SERVER, path: `/poll/${id}?wait=${REMOTE_WAIT_MS}`, body: null, timeoutMs: REMOTE_WAIT_MS + 10000 });
  }
  const res = await fetch(`${bridgeBase()}/poll/${id}`);
  if (!res.ok) throw new Error(`poll ${res.status}`);
  return await res.json();
}

/** Poll-based stand-in for a Tauri Channel. The bridge queues events per
 *  channel id; this drains them into onmessage — every 200ms on loopback,
 *  back-to-back long-polls to a remote server. Polls never overlap. */
export class BridgeChannel {
  readonly id: string;
  onmessage: ((ev: any) => void) | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private stopped = false;
  private failures = 0;
  private inFlight = false;

  /** `id` reattaches to an existing server-side channel (a Work turn that
   *  kept running while this window was closed); omitted = a new channel. */
  constructor(id?: string) {
    this.id = id ?? crypto.randomUUID();
    if (isRemoteServer()) void this.longPollLoop();
    else this.timer = setInterval(() => void this.poll(), 200);
  }

  private async longPollLoop(): Promise<void> {
    while (!this.stopped) {
      const ok = await this.poll();
      // back off after a failure; on success poll again immediately
      if (!ok && !this.stopped) await new Promise((r) => setTimeout(r, 300));
    }
  }

  /** Marker object to splice into invoke args where Tauri takes a Channel. */
  marker(): { __bridge_channel: string } {
    return { __bridge_channel: this.id };
  }

  stop(): void {
    this.stopped = true;
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  private async poll(): Promise<boolean> {
    if (this.inFlight) return true;
    this.inFlight = true;
    try {
      const j = await pollOnce(this.id);
      if (this.stopped) return true;
      this.failures = 0;
      for (const ev of j?.events ?? []) {
        try {
          this.onmessage?.(ev);
        } catch (e) {
          console.error("bridge event handler failed:", e);
        }
      }
      return true;
    } catch {
      // repeated poll failures mean the bridge/server went away mid-turn —
      // surface it as an error event instead of hanging. A remote server
      // gets more slack (network blips), ~15s instead of ~2s.
      this.failures += 1;
      if (this.failures >= (isRemoteServer() ? 40 : 10)) {
        this.stop();
        this.onmessage?.({
          type: "error",
          message: isRemoteServer()
            ? "Lost the connection to the server mid-turn."
            : "Bridge unreachable — is `cargo run --bin bridge` still running?"
        });
      }
      return false;
    } finally {
      this.inFlight = false;
    }
  }
}

/** POST one command to the bridge (or the active remote server). Throws the
 *  backend's error string. */
export async function bridgeInvoke(cmd: string, args: Record<string, any>): Promise<any> {
  if (isRemoteServer()) return await serverInvoke(cmd, args);
  const res = await fetch(`${bridgeBase()}/invoke`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ cmd, args })
  });
  if (!res.ok) throw new Error(`bridge ${cmd} failed: HTTP ${res.status}`);
  const j = await res.json();
  if (!j.ok) throw new Error(j.error ?? `${cmd} failed`);
  return j.data ?? null;
}

/** Tauri invoke when shelled on This computer, the active server when one is
 *  picked, bridge POST when plain-browser-with-bridge, otherwise the classic
 *  desktop-needed error. Drop-in for non-streaming commands (same arg keys
 *  everywhere — the bridge speaks camelCase too). */
export async function invokeOrBridge<T>(cmd: string, args: Record<string, any>): Promise<T> {
  if (isTauriRuntime()) {
    if (isRemoteServer() && !LOCAL_CMDS.has(cmd)) return (await serverInvoke(cmd, args)) as T;
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<T>(cmd, args);
  }
  if (await bridgeAvailable()) return (await bridgeInvoke(cmd, args)) as T;
  throw new Error("This needs the Vicuna desktop app (or start `cargo run --bin bridge` for browser testing).");
}

// ---- server list (desktop) ----

export interface ServerInfo {
  id: string;
  url: string;
  name: string;
  lastUsed: number;
}

export async function listServers(): Promise<ServerInfo[]> {
  if (!isTauriRuntime()) return [];
  const { invoke } = await import("@tauri-apps/api/core");
  return (await invoke<ServerInfo[]>("server_list")) ?? [];
}

/** Verify + save (`url` may carry `#token`). */
export async function connectServer(url: string, token: string): Promise<ServerInfo> {
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<ServerInfo>("server_connect", { url, token: token || null });
}

export async function touchServer(id: string): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("server_touch", { id });
}

export async function removeServer(id: string): Promise<void> {
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("server_remove", { id });
}

/** One cross-host search hit, shaped for the Cmd+P palette (no message bodies). */
export interface ServerChatHit {
  id: string;
  title: string;
  snippet: string;
  updatedAt: number;
  mode: string;
  workspace: string | null;
}

/** One cross-host search result: the rows, plus `degraded` when the server
 *  runs an older Vicuna without `server_search_chats` and the list was
 *  searched from its `chats_load` payload instead. */
export interface ServerSearchResult {
  hits: ServerChatHit[];
  degraded: boolean;
}

/** `chats_load` payloads cached per server for a short while: the fallback
 *  below downloads every session, and the palette queries on each keystroke. */
const loadedChatsCache = new Map<string, { at: number; chats: LoadedChat[] }>();
const loadedChatsInFlight = new Map<string, Promise<LoadedChat[]>>();
const LOADED_CHATS_TTL = 30_000;

async function loadServerChats(invoke: any, id: string): Promise<LoadedChat[]> {
  const hit = loadedChatsCache.get(id);
  if (hit && Date.now() - hit.at < LOADED_CHATS_TTL) return hit.chats;
  const running = loadedChatsInFlight.get(id);
  if (running) return running;
  const p = (async () => {
    try {
      const r = await invoke("server_call", {
        id,
        path: "/invoke",
        body: { cmd: "chats_load", args: {} },
        timeoutMs: 20_000
      });
      if (!r?.ok) throw new Error(r?.error ?? "could not load that server's sessions");
      const chats = (r.data ?? []) as LoadedChat[];
      loadedChatsCache.set(id, { at: Date.now(), chats });
      return chats;
    } finally {
      loadedChatsInFlight.delete(id);
    }
  })();
  loadedChatsInFlight.set(id, p);
  return p;
}

/** Search a connected server's sessions (title/id/message text, plus the
 *  chats named by `ids` for a pasted link). Desktop only: other windows are
 *  the only place a server's chats are not already loaded.
 *
 *  A server whose `vicuna serve` predates `server_search_chats` still answers
 *  `chats_load`, so the same search runs over that payload locally and the
 *  result is flagged `degraded` (works, just slower + a stale list). */
export async function searchServerChats(
  id: string,
  query: string,
  ids: string[],
  limit = 6
): Promise<ServerSearchResult> {
  if (!isTauriRuntime()) return { hits: [], degraded: false };
  const { invoke } = await import("@tauri-apps/api/core");
  const r = await invoke<any>("server_call", {
    id,
    path: "/invoke",
    body: { cmd: "server_search_chats", args: { query, ids, limit } },
    timeoutMs: 5000
  });
  if (r?.ok) return { hits: (r.data ?? []) as ServerChatHit[], degraded: false };
  const err = String(r?.error ?? "search failed on the server");
  if (!/unknown (bridge )?command/i.test(err)) throw new Error(err);
  // the old server has no such command — search its session list instead
  const chats = await loadServerChats(invoke, id);
  return { hits: searchLoadedChats(chats, query, ids, limit), degraded: true };
}

/** Raise the window that owns a host (a server's own window, or the main
 *  window for This computer) and open `chatId` there. */
export async function openChatInWindow(serverId: string | null, chatId: string): Promise<void> {
  if (!isTauriRuntime()) throw new Error("Opening a session on another host needs the desktop app.");
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("open_chat_window", { serverId, chatId, title: null });
}
