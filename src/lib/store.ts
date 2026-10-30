import type { ChatMsg } from "./ollama";
import { validateOverride, type CatalogOverride } from "./library";

export interface Chat {
  id: string;
  title: string;
  createdAt: number;
  messages: ChatMsg[];
  forkedFrom?: { id: string; title: string } | null;
  /** index of the last forked message; the note renders right after it */
  forkFromIndex?: number | null;
  /** chat = plain Ollama chat, work = workspace agent (tool loop) */
  mode?: "chat" | "work";
  /** canonical workspace root for work sessions (null/undefined = unset) */
  workspace?: string | null;
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
  return id ?? HOME_DRAFT_KEY;
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
  think?: boolean;
  temperature?: number;
  stop?: string[];
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

// ---- sqlite backend (Tauri desktop) ----

async function createSqliteBackend(): Promise<Backend> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
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
  for (const col of ["forked_from TEXT", "fork_from_index INTEGER", "model TEXT", "mode TEXT", "workspace TEXT"]) {
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
  for (const col of ["images TEXT", "gen_ms REAL", "gen_tps REAL"]) {
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
          "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index) VALUES ($1, $2, $3, $4, $5)",
          [c.id, c.title, c.createdAt, c.forkedFrom ? JSON.stringify(c.forkedFrom) : null, c.forkFromIndex ?? null]
        );
        let i = 0;
        for (const m of c.messages) {
          await db.execute(
            "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++, serializeImages(m.images), m.genMs ?? null, m.genTps ?? null]
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
    // full rewrite is simple and fast enough at this scale
    await db.execute("DELETE FROM messages");
    await db.execute("DELETE FROM chats");
    for (const c of chats) {
      await db.execute(
        "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index, mode, workspace) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        [c.id, c.title, c.createdAt, c.forkedFrom ? JSON.stringify(c.forkedFrom) : null, c.forkFromIndex ?? null, c.mode ?? "chat", c.workspace ?? null]
      );
      let i = 0;
      for (const m of c.messages) {
        await db.execute(
          "INSERT INTO messages (chat_id, role, content, thinking, model, idx, images, gen_ms, gen_tps) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
          [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++, serializeImages(m.images), m.genMs ?? null, m.genTps ?? null]
        );
      }
    }
  }

  return {
    async loadChats(): Promise<Chat[]> {      const rows = await db.select<
        { id: string; title: string; created_at: number; forked_from: string | null; fork_from_index: number | null; mode: string | null; workspace: string | null }[]
      >("SELECT id, title, created_at, forked_from, fork_from_index, mode, workspace FROM chats ORDER BY created_at DESC");
      const chats: Chat[] = [];
      for (const r of rows) {
        const msgs = await db.select<
          { role: string; content: string; thinking: string; model: string | null; images: string | null; gen_ms: number | null; gen_tps: number | null }[]
        >(
          "SELECT role, content, thinking, model, images, gen_ms, gen_tps FROM messages WHERE chat_id = $1 ORDER BY idx ASC",
          [r.id]
        );
        chats.push({
          id: r.id,
          title: r.title,
          createdAt: r.created_at,
          forkedFrom: r.forked_from ? (JSON.parse(r.forked_from) as Chat["forkedFrom"]) : null,
          forkFromIndex: r.fork_from_index ?? null,
          mode: r.mode === "work" ? "work" : "chat",
          workspace: r.workspace ?? null,
          messages: msgs.map((m) => ({
            role: m.role as ChatMsg["role"],
            content: m.content,
            thinking: m.thinking || undefined,
          model: m.model || undefined,
          images: parseImages(m.images),
          genMs: typeof m.gen_ms === "number" ? m.gen_ms : undefined,
          genTps: typeof m.gen_tps === "number" ? m.gen_tps : undefined
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

// ---- active backend ----

let backend: Backend = lsBackend;
let backendName = "localStorage";

export function storageKind(): string {
  return backendName;
}

export async function initStorage(): Promise<void> {
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
}

export const DEFAULT_STOP_WORDS = ["\nUser:", "\nAssistant:", "<end_of_turn>", "<|im_end|>"];

export const DEFAULT_PREFS: Prefs = { theme: "light", accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "", zoom: 1, defaultWorkspace: "/" };
const LS_PREFS = "vicuna-prefs";

export function readLocalPrefs(): Prefs {
  try {
    const raw = lsGet(LS_PREFS, LEGACY_LS_PREFS);
    if (!raw) return { ...DEFAULT_PREFS };
    const p = JSON.parse(raw) as Partial<Prefs>;
    return {
      theme: p.theme === "dark" ? "dark" : "light",
      accent: typeof p.accent === "string" ? p.accent : DEFAULT_PREFS.accent,
      fontSize: typeof p.fontSize === "number" && p.fontSize >= 12 && p.fontSize <= 24 ? p.fontSize : DEFAULT_PREFS.fontSize,
      sidebarOpen: typeof p.sidebarOpen === "boolean" ? p.sidebarOpen : DEFAULT_PREFS.sidebarOpen,
      stopWords: Array.isArray(p.stopWords) ? p.stopWords.filter((s): s is string => typeof s === "string") : [...DEFAULT_STOP_WORDS],
      ollamaBase: typeof p.ollamaBase === "string" ? p.ollamaBase : DEFAULT_PREFS.ollamaBase,
      zoom: typeof p.zoom === "number" && p.zoom >= 0.5 && p.zoom <= 3 ? p.zoom : DEFAULT_PREFS.zoom,
      defaultWorkspace: typeof p.defaultWorkspace === "string" ? p.defaultWorkspace : DEFAULT_PREFS.defaultWorkspace
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

async function readSqlitePrefs(): Promise<Partial<Prefs>> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
  const db = await Database.load(DB_PATH);
  const rows = await db.select<{ key: string; value: string }[]>(
    "SELECT key, value FROM meta WHERE key IN ('pref_theme', 'pref_accent', 'pref_font_size', 'pref_sidebar', 'pref_stop_words', 'pref_ollama_base', 'pref_zoom', 'pref_default_workspace')"
  );
  const out: Partial<Prefs> = {};
  for (const r of rows) {
    if (r.key === "pref_theme" && (r.value === "light" || r.value === "dark")) out.theme = r.value;
    if (r.key === "pref_accent" && r.value) out.accent = r.value;
    if (r.key === "pref_font_size" && !isNaN(Number(r.value))) {
      const n = Number(r.value);
      if (n >= 12 && n <= 24) out.fontSize = n;
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
    if (r.key === "pref_default_workspace" && typeof r.value === "string") out.defaultWorkspace = r.value;
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
}

export async function loadPrefs(): Promise<Prefs> {
  const local = readLocalPrefs();
  if (!isTauri()) return local;
  try {
    const remote = await readSqlitePrefs();
    return { ...local, ...remote };
  } catch (e) {
    console.error("loadPrefs failed, using local:", e);
    return local;
  }
}

export function savePrefs(p: Prefs): void {
  try {
    localStorage.setItem(LS_PREFS, JSON.stringify(p));
  } catch {
    /* ignore */
  }
  if (isTauri()) {
    writeSqlitePrefs(p).catch((e) => console.error("savePrefs failed:", e));
  }
}
