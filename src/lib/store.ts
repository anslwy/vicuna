import type { ChatMsg } from "./ollama";
import { validateOverride, type CatalogOverride } from "./library";
import { normalizeContinueMode, type ContinueMode, type PromptStyle } from "./continue";
import { activeServerId, invokeOrBridge, isServerBacked } from "./bridge";
import {
  deleteCachedChats,
  pruneCachedChats,
  readCachedChats,
  resolveCachedChatRow,
  writeCachedChat
} from "./chatCache";

export interface Chat {
  id: string;
  title: string;
  createdAt: number;
  /** last user-message time (ms). New sends/steers bump it; assistant-only
   *  activity (regenerate/continue) does not. Absent on pre-migration
   *  sessions — readers fall back to createdAt via chatActivity(). */
  updatedAt?: number;
  messages: ChatMsg[];
  /** Server list copy (`chats_list`): message text is a short preview, with
   *  no thinking or images. Never saved; the window swaps in the full copy
   *  (`chats_get`) when the session is opened or written to. */
  partial?: boolean;
  forkedFrom?: { id: string; title: string } | null;
  /** index of the last forked message; the note renders right after it */
  forkFromIndex?: number | null;
  /** chat = plain Ollama chat, work = workspace agent (tool loop) */
  mode?: "chat" | "work";
  /** canonical workspace root for work sessions (null/undefined = unset) */
  workspace?: string | null;
  /** Legacy Work objective, kept for compatibility with existing records. */
  goal?: string | null;
  /** Whether the server can restore a Work turn that was undone. */
  canRedo?: boolean;
  /** Work harness this session runs on ("claude" | "codex" | …); null = the
   *  global pick in Settings. */
  engine?: string | null;
  /** that harness's model id; null = the engine's saved/default model */
  workModel?: string | null;
  /** "lead" = may create/start/stop/remove other sessions (vicuna-mcp
   *  `manage_sessions`); a worker remembers its lead in `leadId`. */
  role?: "lead" | null;
  leadId?: string | null;
  /** shown in the sidebar's Pinned section regardless of scope */
  pinned?: boolean;
  /** hidden from the sidebar unless "Show archived" is on; nothing else
   *  changes (messages, engine sessions, jobs and turns are untouched) */
  archived?: boolean;
}

export { chatExtraJson, parseChatExtra } from "./leadSessions";
import { chatExtraJson, parseChatExtra } from "./leadSessions";

/** effective sidebar time: last user message, falling back to creation
 *  for sessions that predate updatedAt. */
export function chatActivity(c: Chat): number {
  return typeof c.updatedAt === "number" ? c.updatedAt : c.createdAt;
}

const LS_CHATS = "vicuna-chats";
const LS_MODEL = "vicuna-model";
/** Fetched Discover catalog override (see src/lib/library.ts). Bundled
 *  catalog.ts stays the fallback; a successful Fetch persists here. */
const LS_CATALOG = "vicuna-catalog";
const DB_PATH = "sqlite:vicuna.db";

/** Marker for a stream in flight. Written synchronously when a stream starts
 *  and cleared when it settles, so a refresh mid-stream can resume. The
 *  partial tokens themselves persist via the regular chat save. */
const LS_PENDING = "vicuna-pending-stream";

/** Unsent composer drafts, keyed by chat id (`__home__` when no session is
 *  active). Plain localStorage even on desktop: sqlite owns sent history,
 *  but the requirement is drafts survive a restart via localStorage. */
const LS_DRAFTS = "vicuna-drafts";
const HOME_DRAFT_KEY = "__home__";

/** Last-open session, restored on launch so an unsent draft is right back
 *  in the composer. localStorage everywhere (+ sqlite mirror on desktop). */
const LS_ACTIVE_CHAT = "vicuna-active-chat";

function draftKey(id: string | null): string {
  // server windows keep their own composer draft (several windows share
  // this localStorage)
  const base = id ?? HOME_DRAFT_KEY;
  const server = activeServerId();
  return server ? `${server}::${base}` : base;
}

export function readDrafts(): Record<string, string> {
  try {
    const raw = localStorage.getItem(LS_DRAFTS);
    if (!raw) return {};
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof v === "string" && v.length > 0) out[k] = v;
    }
    return out;
  } catch {
    return {};
  }
}

export function readDraft(id: string | null): string {
  try {
    return readDrafts()[draftKey(id)] ?? "";
  } catch {
    return "";
  }
}

export function writeDraft(id: string | null, text: string): void {
  try {
    const key = draftKey(id);
    if (!text) {
      clearDraft(id);
      return;
    }
    const all = readDrafts();
    all[key] = text;
    localStorage.setItem(LS_DRAFTS, JSON.stringify(all));
  } catch {
    /* storage full or unavailable: sent history is unaffected */
  }
}

export function clearDraft(id: string | null): void {
  try {
    const key = draftKey(id);
    const all = readDrafts();
    if (!(key in all)) return;
    delete all[key];
    localStorage.setItem(LS_DRAFTS, JSON.stringify(all));
  } catch {
    /* ignore */
  }
}

export interface PendingStream {
  chatId: string;
  index: number;
  /** fresh: placeholder generated from scratch (history excludes it);
   *  continue: appending to an existing partial (history includes it) */
  mode: "fresh" | "continue";
  model: string;
  think?: boolean | string;
  temperature?: number;
  stop?: string[];
  /** set when the resume must go through raw /api/generate (Continue on a
   *  model whose chat template closes the assistant turn): the prompt is
   *  rebuilt from this style + the persisted partial. */
  rawStyle?: PromptStyle;
  /** written by a Work turn: never resumed through the Chat path (the
   *  server-side turn is reattached instead, see liveWorkTurns) */
  work?: boolean;
}

export function writePendingStream(p: PendingStream): void {
  try {
    localStorage.setItem(LS_PENDING, JSON.stringify(p));
  } catch {
    /* ignore */
  }
}

export function readPendingStream(): PendingStream | null {
  try {
    const raw = localStorage.getItem(LS_PENDING);
    if (!raw) return null;
    const p = JSON.parse(raw) as PendingStream;
    if (typeof p.chatId !== "string" || typeof p.index !== "number") return null;
    if (p.mode !== "fresh" && p.mode !== "continue") return null;
    return p;
  } catch {
    return null;
  }
}

export function clearPendingStream(): void {
  try {
    localStorage.removeItem(LS_PENDING);
  } catch {
    /* ignore */
  }
}

/** clear the resume marker only when it belongs to this session: several
 *  sessions can stream at once, and one settling must never drop another's
 *  marker (a refresh would then lose that turn instead of resuming it) */
export function clearPendingStreamFor(chatId: string): void {
  try {
    const raw = localStorage.getItem(LS_PENDING);
    if (!raw) return;
    const p = JSON.parse(raw) as PendingStream;
    if (p.chatId === chatId) localStorage.removeItem(LS_PENDING);
  } catch {
    /* ignore */
  }
}

// legacy keys from the pre-rebrand "ollama-desktop-min" builds; read once as
// fallback so existing users keep their chats/prefs, writes go to vicuna-*.
const LEGACY_LS_CHATS = "ollama-min-chats";
const LEGACY_LS_MODEL = "ollama-min-model";
const LEGACY_LS_PREFS = "ollama-min-prefs";

function lsGet(key: string, legacyKey?: string): string | null {
  try {
    const v = localStorage.getItem(key);
    if (v !== null) return v;
    if (legacyKey) return localStorage.getItem(legacyKey);
    return null;
  } catch {
    return null;
  }
}

interface Backend {
  loadChats(): Promise<Chat[]>;
  saveChats(chats: Chat[]): Promise<void>;
  loadModel(): Promise<string | null>;
  saveModel(m: string): Promise<void>;
  loadCatalog(): Promise<string | null>;
  saveCatalog(json: string): Promise<void>;
  loadActiveChat(): Promise<string | null>;
  saveActiveChat(id: string | null): Promise<void>;
}

function isTauri(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

// ---- localStorage backend (web dev / fallback) ----

const lsBackend: Backend = {
  async loadChats(): Promise<Chat[]> {
    try {
      const raw = lsGet(LS_CHATS, LEGACY_LS_CHATS);
      const list = raw ? (JSON.parse(raw) as Chat[]) : [];
      // normalize pre-work-mode sessions
      return list.map((c) => ({
        ...c,
        updatedAt: typeof c.updatedAt === "number" ? c.updatedAt : c.createdAt,
        mode: c.mode === "work" ? "work" : "chat",
        workspace: c.workspace ?? null
      }));
    } catch {
      return [];
    }
  },
  async saveChats(chats: Chat[]): Promise<void> {
    try {
      localStorage.setItem(LS_CHATS, JSON.stringify(chats));
    } catch {
      /* storage full or unavailable */
    }
  },
  async loadModel(): Promise<string | null> {
    try {
      return lsGet(LS_MODEL, LEGACY_LS_MODEL);
    } catch {
      return null;
    }
  },
  async saveModel(m: string): Promise<void> {
    try {
      localStorage.setItem(LS_MODEL, m);
    } catch {
      /* ignore */
    }
  },
  async loadActiveChat(): Promise<string | null> {
    try {
      return localStorage.getItem(LS_ACTIVE_CHAT);
    } catch {
      return null;
    }
  },
  async saveActiveChat(id: string | null): Promise<void> {
    try {
      if (id) localStorage.setItem(LS_ACTIVE_CHAT, id);
      else localStorage.removeItem(LS_ACTIVE_CHAT);
    } catch {
      /* ignore */
    }
  },
  async loadCatalog(): Promise<string | null> {
    try {
      return localStorage.getItem(LS_CATALOG);
    } catch {
      return null;
    }
  },
  async saveCatalog(json: string): Promise<void> {
    try {
      localStorage.setItem(LS_CATALOG, json);
    } catch {
      /* storage full: fetched catalog just won't survive a restart */
    }
  }
};

// images are stored as a JSON array of data URLs (or NULL when absent)
function parseImages(raw: string | null): string[] | undefined {
  if (!raw) return undefined;
  try {
    const arr: unknown = JSON.parse(raw);
    if (Array.isArray(arr) && arr.every((s): s is string => typeof s === "string")) return arr;
  } catch {
    /* corrupt entry: drop it rather than breaking the chat */
  }
  return undefined;
}

function serializeImages(images: string[] | undefined): string | null {
  return images?.length ? JSON.stringify(images) : null;
}

/** exact payload shape for the Rust chats_save / server chats_merge commands (camelCase keys) */
function toSavePayload(chats: Chat[]) {
  return chats.map((c) => ({
    id: c.id,
    title: c.title,
    createdAt: c.createdAt,
    updatedAt: chatActivity(c),
    forkedFrom: c.forkedFrom ? JSON.stringify(c.forkedFrom) : null,
    forkFromIndex: c.forkFromIndex ?? null,
    mode: c.mode ?? "chat",
    workspace: c.workspace ?? null,
    goal: c.goal ?? null,
    extra: chatExtraJson(c),
    messages: c.messages.map((m) => ({
      role: m.role,
      content: m.content,
      thinking: m.thinking ?? "",
      model: m.model ?? null,
      images: m.images?.length ? m.images : null,
      genMs: m.genMs ?? null,
      genTps: m.genTps ?? null,
      genCost: m.genCost ?? null,
      summary: m.summary ? true : null
    }))
  }));
}

// ---- sqlite backend (Tauri desktop) ----

async function createSqliteBackend(): Promise<Backend> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
  const { invoke } = await import("@tauri-apps/api/core");
  const db = await Database.load(DB_PATH);
  await db.execute(`
    CREATE TABLE IF NOT EXISTS chats (
      id TEXT PRIMARY KEY,
      title TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      forked_from TEXT,
      fork_from_index INTEGER
    )`);
  // migrations for databases created before fork attribution existed
  for (const col of ["forked_from TEXT", "fork_from_index INTEGER", "model TEXT", "mode TEXT", "workspace TEXT", "goal TEXT", "updated_at INTEGER", "extra TEXT"]) {
    try {
      await db.execute(`ALTER TABLE chats ADD COLUMN ${col}`);
    } catch {
      /* column already exists */
    }
  }
  await db.execute(`
    CREATE TABLE IF NOT EXISTS messages (
      id INTEGER PRIMARY KEY AUTOINCREMENT,
      chat_id TEXT NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
      role TEXT NOT NULL,
      content TEXT NOT NULL DEFAULT '',
      thinking TEXT NOT NULL DEFAULT '',
      model TEXT,
      idx INTEGER NOT NULL
    )`);
  await db.execute(
    `CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, idx)`
  );
  // databases created before these columns existed lack them
  for (const col of ["images TEXT", "gen_ms REAL", "gen_tps REAL", "summary INTEGER", "gen_cost REAL"]) {
    try {
      await db.execute(`ALTER TABLE messages ADD COLUMN ${col}`);
    } catch {
      /* column already exists */
    }
  }
  await db.execute(`
    CREATE TABLE IF NOT EXISTS meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    )`);

  // one-time migration from localStorage
  const count = await db.select<{ n: number }[]>(
    "SELECT COUNT(*) as n FROM chats"
  );
  if ((count[0]?.n ?? 0) === 0) {
    const legacy = await lsBackend.loadChats();
    if (legacy.length > 0) {
      for (const c of legacy) {
        await db.execute(
          "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, updated_at) VALUES ($1, $2, $3, $4, $5, $6)",
          [c.id, c.title, c.createdAt, c.forkedFrom ? JSON.stringify(c.forkedFrom) : null, c.forkFromIndex ?? null, chatActivity(c)]
        );
        let i = 0;
        for (const m of c.messages) {
          await db.execute(
            "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
            [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++, serializeImages(m.images), m.genMs ?? null, m.genTps ?? null, m.summary ? 1 : null, m.genCost ?? null]
          );
        }
      }
      const legacyModel = await lsBackend.loadModel();
      if (legacyModel) {
        await db.execute(
          "INSERT OR REPLACE INTO meta (key, value) VALUES ('model', $1)",
          [legacyModel]
        );
      }
      try {
        localStorage.removeItem(LS_CHATS);
        localStorage.removeItem(LS_MODEL);
      } catch {
        /* ignore */
      }
    }
  }

  // serialized + coalesced chat writer (see saveChats below)
  let saveActive = false;
  let queuedChats: Chat[] | null = null;
  type SqlDb = {
    execute(query: string, bindValues?: unknown[]): Promise<unknown>;
  };
  async function rewriteChats(db: SqlDb, chats: Chat[]): Promise<void> {
    // Atomic rewrite through Rust: one sqlx transaction on the plugin's own
    // pool. The old per-statement JS loop was the data-loss bug: a page
    // unload (right-click → Refresh on Windows) killed the awaiting loop
    // right after the DELETEs landed, wiping all history (live loss,
    // 2026-11-11). Each plugin execute() can grab a different pooled
    // connection, so BEGIN/COMMIT from JS is unsafe — the transaction
    // therefore lives in the `chats_save` command.
    try {
      await invoke("chats_save", { chats: toSavePayload(chats) });
      return;
    } catch (e) {
      console.error("chats_save failed, falling back to statement loop:", e);
    }
    // fallback: old non-atomic statement loop (stale shell without
    // chats_save). Never remove: keeps an old shell + new frontend working.
    await db.execute("DELETE FROM messages");
    await db.execute("DELETE FROM chats");
    for (const c of chats) {
      await db.execute(
        "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        [c.id, c.title, c.createdAt, c.forkedFrom ? JSON.stringify(c.forkedFrom) : null, c.forkFromIndex ?? null, c.mode ?? "chat", c.workspace ?? null, c.goal ?? null, chatActivity(c), chatExtraJson(c)]
      );
      let i = 0;
      for (const m of c.messages) {
        await db.execute(
          "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps, summary, gen_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
          [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++, serializeImages(m.images), m.genMs ?? null, m.genTps ?? null, m.summary ? 1 : null, m.genCost ?? null]
        );
      }
    }
  }

  return {
    async loadChats(): Promise<Chat[]> {      const rows = await db.select<
        { id: string; title: string; created_at: number; forked_from: string | null; fork_from_index: number | null; mode: string | null; workspace: string | null; goal: string | null; updated_at: number | null; extra: string | null }[]
      >("SELECT id, title, created_at, forked_from, fork_from_index, mode, workspace, goal, updated_at, extra FROM chats ORDER BY COALESCE(updated_at, created_at) DESC");
      const chats: Chat[] = [];
      for (const r of rows) {
        const msgs = await db.select<
          { role: string; content: string; thinking: string; model: string | null; images: string | null; gen_ms: number | null; gen_tps: number | null; summary: number | null; gen_cost: number | null }[]
        >(
          "SELECT role, content, thinking, model, images, gen_ms, gen_tps, summary, gen_cost FROM messages WHERE chat_id = $1 ORDER BY idx ASC",
          [r.id]
        );
        chats.push({
          id: r.id,
          title: r.title,
          createdAt: r.created_at,
          updatedAt: typeof r.updated_at === "number" ? r.updated_at : r.created_at,
          forkedFrom: r.forked_from ? (JSON.parse(r.forked_from) as Chat["forkedFrom"]) : null,
          forkFromIndex: r.fork_from_index ?? null,
          mode: r.mode === "work" ? "work" : "chat",
          workspace: r.workspace ?? null,
          goal: r.goal ?? null,
          ...parseChatExtra(r.extra),
          messages: msgs.map((m) => ({
            role: m.role as ChatMsg["role"],
            content: m.content,
            thinking: m.thinking || undefined,
          model: m.model || undefined,
          images: parseImages(m.images),
          genMs: typeof m.gen_ms === "number" ? m.gen_ms : undefined,
          genTps: typeof m.gen_tps === "number" ? m.gen_tps : undefined,
          genCost: typeof m.gen_cost === "number" ? m.gen_cost : undefined,
          summary: m.summary === 1 ? true : undefined
        }))
        });
      }
      return chats;
    },
    async saveChats(chats: Chat[]): Promise<void> {
      // single-writer + coalescing: save() is fire-and-forget and streaming
      // triggers it every ~2s, so overlapping full rewrites are the norm.
      // Two interleaved DELETE/INSERT sequences corrupt the DB (UNIQUE + FK
      // failures, and a kill mid-storm wipes history). At most one rewrite
      // runs; late arrivals collapse into a single trailing write of the
      // newest snapshot.
      queuedChats = chats;
      if (saveActive) return;
      saveActive = true;
      try {
        while (queuedChats) {
          const next = queuedChats;
          queuedChats = null;
          try {
            await rewriteChats(db, next);
          } catch (e) {
            console.error("saveChats failed:", e);
          }
        }
      } finally {
        saveActive = false;
      }
    },
    async loadModel(): Promise<string | null> {
      const rows = await db.select<{ value: string }[]>(
        "SELECT value FROM meta WHERE key = 'model'"
      );
      return rows[0]?.value ?? null;
    },
    async saveModel(m: string): Promise<void> {
      await db.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('model', $1)",
        [m]
      );
    },
    async loadCatalog(): Promise<string | null> {
      const rows = await db.select<{ value: string }[]>(
        "SELECT value FROM meta WHERE key = 'catalog'"
      );
      return rows[0]?.value ?? null;
    },
    async saveCatalog(json: string): Promise<void> {
      await db.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('catalog', $1)",
        [json]
      );
    },
    async loadActiveChat(): Promise<string | null> {
      const rows = await db.select<{ value: string }[]>(
        "SELECT value FROM meta WHERE key = 'active_chat'"
      );
      return rows[0]?.value ?? null;
    },
    async saveActiveChat(id: string | null): Promise<void> {
      if (!id) {
        await db.execute("DELETE FROM meta WHERE key = 'active_chat'");
        return;
      }
      await db.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('active_chat', $1)",
        [id]
      );
    }
  };
}

// ---- remote server backend (desktop connected to `vicuna serve`) ----
// Chats live on the server, shared by every desktop that connects. Saves are
// a merge, never a rewrite (a rewrite from one client would delete what
// another created): only chats whose payload changed since the last save go
// up, plus the ids THIS client deleted. Same coalescing single-writer as the
// sqlite backend. Everything that isn't chats (model, catalog, active chat)
// stays in localStorage, keyed per server.

function serverKey(base: string, id: string): string {
  return `${base}:server:${id}`;
}

/** Hooks ChatApp installs for the server backend (see createServerBackend). */
export const serverSync: {
  /** chats the server is recording right now (a live Work turn): never sent */
  skip: (id: string) => boolean;
  /** the server refused these (it has a newer copy): adopt them */
  onConflict: (chats: Chat[]) => void;
} = { skip: () => false, onConflict: () => {} };

/** Raw server chat JSON (chats_load / chats_get / a conflict) → Chat. */
export function normalizeServerChat(c: any): Chat {
  return {
    id: String(c.id),
    title: typeof c.title === "string" ? c.title : "Untitled",
    createdAt: typeof c.createdAt === "number" ? c.createdAt : 0,
    updatedAt: typeof c.updatedAt === "number" ? c.updatedAt : c.createdAt ?? 0,
    forkedFrom: c.forkedFrom && typeof c.forkedFrom.id === "string" ? c.forkedFrom : null,
    forkFromIndex: typeof c.forkFromIndex === "number" ? c.forkFromIndex : null,
    mode: c.mode === "work" ? "work" : "chat",
    workspace: typeof c.workspace === "string" ? c.workspace : null,
    goal: typeof c.goal === "string" ? c.goal : null,
    canRedo: c.canRedo === true,
    ...(c.partial === true ? { partial: true } : {}),
    ...parseChatExtra(c.extra),
    messages: (Array.isArray(c.messages) ? c.messages : []).map((m: any) => ({
      role: m.role as ChatMsg["role"],
      content: typeof m.content === "string" ? m.content : "",
      thinking: m.thinking || undefined,
      model: m.model || undefined,
      images: Array.isArray(m.images) && m.images.length ? m.images : undefined,
      genMs: typeof m.genMs === "number" ? m.genMs : undefined,
      genTps: typeof m.genTps === "number" ? m.genTps : undefined,
      genCost: typeof m.genCost === "number" ? m.genCost : undefined,
      summary: m.summary === true ? true : undefined
    }))
  };
}

let serverBackendApi: {
  adopt(raw: any): Chat;
  saveRow(chat: Chat): Record<string, unknown>;
  idle(): Promise<void>;
} | null = null;

/** Server mode: take the server's copy of a chat as the new baseline (its
 *  revision, and "already saved" for the change detection). */
export function adoptServerChat(raw: any): Chat {
  return serverBackendApi ? serverBackendApi.adopt(raw) : normalizeServerChat(raw);
}

/** Server mode: a chat's save row carrying the revision this window has. */
export function serverSaveRow(chat: Chat): Record<string, unknown> | null {
  return serverBackendApi ? serverBackendApi.saveRow(chat) : null;
}

/** Server mode: resolves once queued chat saves have landed. */
export function serverSavesIdle(): Promise<void> {
  return serverBackendApi ? serverBackendApi.idle() : Promise.resolve();
}

/** The server's session list. Servers with `chats_list` send light copies
 *  (message previews, `partial: true`); older ones only have the full
 *  `chats_load`. */
export async function loadServerChatList(): Promise<any[]> {
  let raw: unknown;
  try {
    raw = await invokeOrBridge<any[]>("chats_list", {});
  } catch (e) {
    if (!/unknown bridge command/i.test(String((e as any)?.message ?? e))) throw e;
    raw = await invokeOrBridge<any[]>("chats_load", {});
  }
  return Array.isArray(raw) ? raw : [];
}

/** `keyScope`: the remote server id that scopes this window's local keys
 *  (model, active chat); null = This computer, which keeps its old keys. */
function createServerBackend(keyScope: string | null): Backend {
  const cacheScope = keyScope ?? "local";
  const sent = new Map<string, string>(); // chat id -> last payload JSON the server has
  // chat id -> server revision this window last saw. Sent as baseRev: the
  // server refuses a save whose base moved on (it recorded a Work turn while
  // this window slept, or another window saved) and returns its copy instead,
  // so an outdated window can never overwrite newer rows.
  const revs = new Map<string, number>();
  let saveActive = false;
  let queued: Chat[] | null = null;
  let idleWaiters: Array<() => void> = [];

  const withRev = (it: ReturnType<typeof toSavePayload>[number]) =>
    revs.has(it.id) ? { ...it, baseRev: revs.get(it.id) } : it;

  async function push(chats: Chat[]): Promise<void> {
    const items = toSavePayload(chats);
    const byId = new Map(chats.map((c) => [c.id, c]));
    const partialIds = new Set(chats.filter((c) => c.partial).map((c) => c.id));
    const current = new Set(items.map((i) => i.id));
    const changed: typeof items = [];
    const json = new Map<string, string>();
    for (const it of items) {
      const j = JSON.stringify(it);
      json.set(it.id, j);
      // a live server-recorded turn: the server is its only writer
      // a list copy (partial) holds message previews only: never send it
      if (sent.get(it.id) !== j && !serverSync.skip(it.id) && !partialIds.has(it.id)) changed.push(it);
    }
    const deleted = [...sent.keys()].filter((id) => !current.has(id));
    if (changed.length === 0 && deleted.length === 0) return;
    const out = await invokeOrBridge<any>("chats_merge", { chats: changed.map(withRev), deleted });
    const written: Record<string, number> = out && typeof out === "object" && out.revs ? out.revs : {};
    const hasRevs = !!(out && typeof out === "object" && out.revs);
    for (const it of changed) {
      // older servers answer null (everything written, no revisions)
      if (!hasRevs || it.id in written) {
        sent.set(it.id, json.get(it.id)!);
        if (hasRevs) {
          revs.set(it.id, written[it.id]);
          const orig = byId.get(it.id);
          writeCachedChat(cacheScope, {
            ...it,
            forkedFrom: orig?.forkedFrom ?? null,
            canRedo: orig?.canRedo === true,
            rev: written[it.id]
          });
        }
      }
    }
    for (const id of deleted) {
      sent.delete(id);
      revs.delete(id);
    }
    if (deleted.length) deleteCachedChats(cacheScope, deleted);
    const conflicts: Chat[] = (Array.isArray(out?.conflicts) ? out.conflicts : []).map((raw: any) => adopt(raw));
    if (conflicts.length) {
      // A save requested during this request may still hold the rejected
      // objects. Never rebase that stale transcript onto the adopted revision.
      const fresh = new Map(conflicts.map((c) => [c.id, c]));
      if (queued) queued = queued.map((c) => fresh.get(c.id) ?? c);
      serverSync.onConflict(conflicts);
    }
  }

  function adopt(raw: any): Chat {
    const chat = normalizeServerChat(raw);
    if (typeof raw?.rev === "number") revs.set(chat.id, raw.rev);
    sent.set(chat.id, JSON.stringify(toSavePayload([chat])[0]));
    writeCachedChat(cacheScope, raw);
    return chat;
  }

  serverBackendApi = {
    adopt,
    saveRow: (chat) => withRev(toSavePayload([chat])[0]),
    idle: () => (saveActive ? new Promise((r) => idleWaiters.push(r)) : Promise.resolve())
  };

  const ls = (base: string) => (keyScope ? serverKey(base, keyScope) : base);
  return {
    async loadChats(): Promise<Chat[]> {
      const [raw, cached] = await Promise.all([loadServerChatList(), readCachedChats(cacheScope)]);
      // what the server already has: the first save sends nothing unchanged
      sent.clear();
      revs.clear();
      const chats: Chat[] = (Array.isArray(raw) ? raw : []).map((c: any) => {
        const { row } = resolveCachedChatRow(c, cached);
        return adopt(row);
      });
      pruneCachedChats(cacheScope, new Set(chats.map((c) => c.id)));
      return chats;
    },
    async saveChats(chats: Chat[]): Promise<void> {
      queued = chats;
      if (saveActive) return;
      saveActive = true;
      try {
        while (queued) {
          const next = queued;
          queued = null;
          try {
            await push(next);
          } catch (e) {
            console.error("server saveChats failed:", e);
          }
        }
      } finally {
        saveActive = false;
        const w = idleWaiters;
        idleWaiters = [];
        for (const f of w) f();
      }
    },
    async loadModel() {
      return localStorage.getItem(ls(LS_MODEL));
    },
    async saveModel(m: string) {
      localStorage.setItem(ls(LS_MODEL), m);
    },
    async loadCatalog() {
      return localStorage.getItem(LS_CATALOG);
    },
    async saveCatalog(json: string) {
      localStorage.setItem(LS_CATALOG, json);
    },
    async loadActiveChat() {
      return localStorage.getItem(ls(LS_ACTIVE_CHAT));
    },
    async saveActiveChat(id: string | null) {
      if (id) localStorage.setItem(ls(LS_ACTIVE_CHAT), id);
      else localStorage.removeItem(ls(LS_ACTIVE_CHAT));
    }
  };
}

// ---- active backend ----

let backend: Backend = lsBackend;
let backendName = "localStorage";

export function storageKind(): string {
  return backendName;
}

export async function initStorage(): Promise<void> {
  if (isServerBacked()) {
    // no silent fallback: writing a server's chats into local storage (or
    // showing local chats as the server's) would be worse than an error.
    // This computer is server-backed too (the embedded/local `vicuna serve`).
    backend = createServerBackend(activeServerId());
    backendName = "server";
    return;
  }
  if (!isTauri()) {
    backend = lsBackend;
    backendName = "localStorage";
    return;
  }
  try {
    backend = await createSqliteBackend();
    backendName = "sqlite";
  } catch (e) {
    console.error("sqlite init failed, falling back to localStorage:", e);
    backend = lsBackend;
    backendName = "localStorage";
  }
}

export function loadChats(): Promise<Chat[]> {
  return backend.loadChats();
}

export function saveChats(chats: Chat[]): Promise<void> {
  return backend.saveChats(chats).catch((e) => {
    console.error("saveChats failed:", e);
  });
}

export function loadModel(): Promise<string | null> {
  return backend.loadModel();
}

export function saveModel(m: string): Promise<void> {
  return backend.saveModel(m).catch((e) => {
    console.error("saveModel failed:", e);
  });
}

function validChatId(v: unknown): v is string {
  return typeof v === "string" && v.length > 0 && v.length < 200;
}

export async function loadActiveChat(): Promise<string | null> {
  try {
    const raw = await backend.loadActiveChat();
    return validChatId(raw) ? raw : null;
  } catch {
    return null;
  }
}

export function saveActiveChat(id: string | null): void {
  if (activeServerId()) {
    backend.saveActiveChat(id).catch(() => {});
    return;
  }
  try {
    if (id) localStorage.setItem(LS_ACTIVE_CHAT, id);
    else localStorage.removeItem(LS_ACTIVE_CHAT);
  } catch {
    /* ignore */
  }
  if (isTauri()) {
    backend.saveActiveChat(id).catch((e) => console.error("saveActiveChat failed:", e));
  }
}

export async function loadCatalog(): Promise<CatalogOverride | null> {
  try {
    const raw = await backend.loadCatalog();
    if (!raw) return null;
    return validateOverride(JSON.parse(raw));
  } catch {
    return null;
  }
}

export function saveCatalog(c: CatalogOverride): void {
  const json = JSON.stringify(c);
  try {
    localStorage.setItem(LS_CATALOG, json);
  } catch {
    /* ignore */
  }
  if (isTauri()) {
    backend.saveCatalog(json).catch((e) => console.error("saveCatalog failed:", e));
  }
}

// ---- appearance prefs (theme + accent) ----

export interface Prefs {
  theme: "light" | "dark";
  accent: string;
  fontSize: number;
  sidebarOpen: boolean;
  stopWords: string[];
  /** Custom Ollama endpoint for plain-browser mode ("" = default http://localhost:11434).
   *  The Tauri shell always uses its local backend and ignores this. */
  ollamaBase: string;
  /** Desktop webview zoom factor (Cmd/Ctrl +/−/0). Plain browsers zoom
   *  natively and ignore this. */
  zoom: number;
  /** Last-used Work mode workspace (absolute path). Pre-fills the workspace
   *  picker and "Continue in Work". */
  defaultWorkspace: string;
  /** Reasoning effort for models with string think levels (gpt-oss family):
   *  "auto" sends think:true (server default), otherwise the level verbatim.
   *  Ignored for boolean-only / non-thinking models. */
  thinkLevel: string;
  /** How Continue extends a reply: "raw" (default — prefill the family's own
   *  prompt through /api/generate where the format is known), "auto" (raw
   *  only for gemma4, native otherwise), or "native" (always /api/chat). */
  continueMode: ContinueMode;
  /** OpenRouter API key for the Settings → OpenRouter catalog. Stored like
   *  every other pref (localStorage + sqlite meta); only ever sent to
   *  openrouter.ai. */
  openrouterApiKey?: string;
  /** Curated OpenRouter model ids: only these appear in the model picker
   *  and the default Settings list (the live catalog has 400+ entries).
   *  Added/removed per row; search browses the full catalog. */
  openrouterAdded: string[];
  /** Sidebar scope: "all", "chat", or a workspace root (see
   *  src/lib/sidebar.ts). Persisted so the sidebar reopens how it was left. */
  sessionFilter: string;
}

export const DEFAULT_STOP_WORDS = ["\nUser:", "\nAssistant:", "<end_of_turn>", "<|im_end|>"];

export const DEFAULT_PREFS: Prefs = { theme: "light", accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "", zoom: 1, defaultWorkspace: "", thinkLevel: "auto", continueMode: "raw", openrouterApiKey: "", openrouterAdded: [], sessionFilter: "all" };
const LS_PREFS = "vicuna-prefs";

/** Normalize a stored workspace for the current platform: strip Rust's
 *  Windows verbatim prefix (`\\?\...`) left by older builds, and map the
 *  legacy fresh-install default ("/") to unset — the filesystem root is no
 *  longer a valid workspace (it crawls TCC-protected spots on macOS). */
export function normalizeStoredWorkspace(ws: string): string {
  let out = ws;
  if (out.startsWith("\\\\?\\UNC\\")) out = "\\" + out.slice("\\\\?\\UNC\\".length);
  else if (out.startsWith("\\\\?\\")) out = out.slice("\\\\?\\".length);
  if (out === "/") out = "";
  return out;
}

export function readLocalPrefs(): Prefs {
  try {
    const raw = lsGet(LS_PREFS, LEGACY_LS_PREFS);
    if (!raw) return { ...DEFAULT_PREFS, defaultWorkspace: normalizeStoredWorkspace(DEFAULT_PREFS.defaultWorkspace) };
    const p = JSON.parse(raw) as Partial<Prefs>;
    return {
      theme: p.theme === "dark" ? "dark" : "light",
      accent: typeof p.accent === "string" ? p.accent : DEFAULT_PREFS.accent,
      fontSize: typeof p.fontSize === "number" && p.fontSize >= 12 && p.fontSize <= 35 ? p.fontSize : DEFAULT_PREFS.fontSize,
      sidebarOpen: typeof p.sidebarOpen === "boolean" ? p.sidebarOpen : DEFAULT_PREFS.sidebarOpen,
      stopWords: Array.isArray(p.stopWords) ? p.stopWords.filter((s): s is string => typeof s === "string") : [...DEFAULT_STOP_WORDS],
      ollamaBase: typeof p.ollamaBase === "string" ? p.ollamaBase : DEFAULT_PREFS.ollamaBase,
      zoom: typeof p.zoom === "number" && p.zoom >= 0.5 && p.zoom <= 3 ? p.zoom : DEFAULT_PREFS.zoom,
      defaultWorkspace: typeof p.defaultWorkspace === "string" ? normalizeStoredWorkspace(p.defaultWorkspace) : normalizeStoredWorkspace(DEFAULT_PREFS.defaultWorkspace),
      thinkLevel: typeof p.thinkLevel === "string" && p.thinkLevel.length > 0 ? p.thinkLevel : DEFAULT_PREFS.thinkLevel,
      continueMode: normalizeContinueMode(p.continueMode),
      openrouterApiKey: typeof p.openrouterApiKey === "string" ? p.openrouterApiKey : DEFAULT_PREFS.openrouterApiKey,
      openrouterAdded: Array.isArray(p.openrouterAdded) ? p.openrouterAdded.filter((s): s is string => typeof s === "string") : [...DEFAULT_PREFS.openrouterAdded],
      sessionFilter: typeof p.sessionFilter === "string" ? p.sessionFilter : DEFAULT_PREFS.sessionFilter
    };
  } catch {
    return { ...DEFAULT_PREFS, defaultWorkspace: normalizeStoredWorkspace(DEFAULT_PREFS.defaultWorkspace) };
  }
}

async function readSqlitePrefs(): Promise<Partial<Prefs>> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
  const db = await Database.load(DB_PATH);
  const rows = await db.select<{ key: string; value: string }[]>(
    "SELECT key, value FROM meta WHERE key IN ('pref_theme', 'pref_accent', 'pref_font_size', 'pref_sidebar', 'pref_stop_words', 'pref_ollama_base', 'pref_zoom', 'pref_default_workspace', 'pref_think_level', 'pref_continue_mode', 'pref_openrouter_key', 'pref_openrouter_added', 'pref_session_filter')"
  );
  const out: Partial<Prefs> = {};
  for (const r of rows) {
    if (r.key === "pref_theme" && (r.value === "light" || r.value === "dark")) out.theme = r.value;
    if (r.key === "pref_accent" && r.value) out.accent = r.value;
    if (r.key === "pref_font_size" && !isNaN(Number(r.value))) {
      const n = Number(r.value);
      if (n >= 12 && n <= 35) out.fontSize = n;
    }
    if (r.key === "pref_sidebar") out.sidebarOpen = r.value === "1";
    if (r.key === "pref_stop_words") {
      try {
        const arr = JSON.parse(r.value);
        if (Array.isArray(arr)) out.stopWords = arr.filter((s): s is string => typeof s === "string");
      } catch {
        /* ignore */
      }
    }
    if (r.key === "pref_ollama_base" && typeof r.value === "string") out.ollamaBase = r.value;
    if (r.key === "pref_zoom" && !isNaN(Number(r.value))) {
      const n = Number(r.value);
      if (n >= 0.5 && n <= 3) out.zoom = n;
    }
    if (r.key === "pref_default_workspace" && typeof r.value === "string") out.defaultWorkspace = normalizeStoredWorkspace(r.value);
    if (r.key === "pref_think_level" && typeof r.value === "string" && r.value.length > 0) out.thinkLevel = r.value;
    if (r.key === "pref_continue_mode") out.continueMode = normalizeContinueMode(r.value);
    if (r.key === "pref_openrouter_key" && typeof r.value === "string") out.openrouterApiKey = r.value;
    if (r.key === "pref_session_filter" && typeof r.value === "string") out.sessionFilter = r.value;
    if (r.key === "pref_openrouter_added") {
      try {
        const arr = JSON.parse(r.value);
        if (Array.isArray(arr)) out.openrouterAdded = arr.filter((s): s is string => typeof s === "string");
      } catch {
        /* ignore */
      }
    }
  }
  return out;
}

async function writeSqlitePrefs(p: Prefs): Promise<void> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
  const db = await Database.load(DB_PATH);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_theme', $1)", [p.theme]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_accent', $1)", [p.accent]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_font_size', $1)", [String(p.fontSize)]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_sidebar', $1)", [p.sidebarOpen ? "1" : "0"]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_stop_words', $1)", [JSON.stringify(p.stopWords)]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_ollama_base', $1)", [p.ollamaBase]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_zoom', $1)", [String(p.zoom)]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_default_workspace', $1)", [p.defaultWorkspace]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_think_level', $1)", [p.thinkLevel]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_continue_mode', $1)", [p.continueMode]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_openrouter_key', $1)", [p.openrouterApiKey ?? ""]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_openrouter_added', $1)", [JSON.stringify(p.openrouterAdded ?? [])]);
  await db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('pref_session_filter', $1)", [p.sessionFilter ?? "all"]);
}

/** Prefs that belong to one host: on a remote server they are kept per
 *  server so a server's workspace, sidebar scope, theme or accent never
 *  overwrites This computer (or another server). */
interface ServerScopedPrefs {
  defaultWorkspace: string;
  sessionFilter: string;
  theme?: "light" | "dark";
  accent?: string;
}

function readServerPrefs(id: string): ServerScopedPrefs {
  try {
    const p = JSON.parse(localStorage.getItem(serverKey(LS_PREFS, id)) ?? "{}");
    const out: ServerScopedPrefs = {
      defaultWorkspace: typeof p.defaultWorkspace === "string" ? p.defaultWorkspace : "",
      sessionFilter: typeof p.sessionFilter === "string" ? p.sessionFilter : DEFAULT_PREFS.sessionFilter
    };
    if (p.theme === "light" || p.theme === "dark") out.theme = p.theme;
    if (typeof p.accent === "string" && p.accent.length > 0) out.accent = p.accent;
    return out;
  } catch {
    return { defaultWorkspace: "", sessionFilter: DEFAULT_PREFS.sessionFilter };
  }
}

function withServerPrefs(p: Prefs): Prefs {
  const id = activeServerId();
  if (!id) return p;
  const sp = readServerPrefs(id);
  return {
    ...p,
    defaultWorkspace: sp.defaultWorkspace,
    sessionFilter: sp.sessionFilter,
    ...(sp.theme ? { theme: sp.theme } : {}),
    ...(sp.accent ? { accent: sp.accent } : {})
  };
}

/** Synchronous startup read (before initApp): local prefs with the active
 *  server's scoped values overlaid. */
export function readStartupPrefs(): Prefs {
  return withServerPrefs(readLocalPrefs());
}

export async function loadPrefs(): Promise<Prefs> {
  const local = readLocalPrefs();
  if (!isTauri()) return local;
  try {
    const remote = await readSqlitePrefs();
    return withServerPrefs({ ...local, ...remote });
  } catch (e) {
    console.error("loadPrefs failed, using local:", e);
    return withServerPrefs(local);
  }
}

export function savePrefs(p: Prefs): void {
  const server = activeServerId();
  if (server) {
    try {
      localStorage.setItem(
        serverKey(LS_PREFS, server),
        JSON.stringify({ defaultWorkspace: p.defaultWorkspace, sessionFilter: p.sessionFilter, theme: p.theme, accent: p.accent })
      );
    } catch {
      /* ignore */
    }
    const local = readLocalPrefs();
    p = { ...p, defaultWorkspace: local.defaultWorkspace, sessionFilter: local.sessionFilter, theme: local.theme, accent: local.accent };
  }
  try {
    localStorage.setItem(LS_PREFS, JSON.stringify(p));
  } catch {
    /* ignore */
  }
  if (isTauri()) {
    writeSqlitePrefs(p).catch((e) => console.error("savePrefs failed:", e));
  }
}
