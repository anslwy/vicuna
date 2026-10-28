import type { ChatMsg } from "./ollama";

export interface Chat {
  id: string;
  title: string;
  createdAt: number;
  messages: ChatMsg[];
  forkedFrom?: { id: string; title: string } | null;
  /** index of the last forked message; the note renders right after it */
  forkFromIndex?: number | null;
}

const LS_CHATS = "vicuna-chats";
const LS_MODEL = "vicuna-model";
const DB_PATH = "sqlite:vicuna.db";

/** Marker for a stream in flight. Written synchronously when a stream starts
 *  and cleared when it settles, so a refresh mid-stream can resume. The
 *  partial tokens themselves persist via the regular chat save. */
const LS_PENDING = "vicuna-pending-stream";

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
      return raw ? (JSON.parse(raw) as Chat[]) : [];
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
  }
};

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
  for (const col of ["forked_from TEXT", "fork_from_index INTEGER", "model TEXT"]) {
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
            "INSERT INTO messages (chat_id, role, content, thinking, model, idx) VALUES ($1, $2, $3, $4, $5, $6)",
            [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++]
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

  return {
    async loadChats(): Promise<Chat[]> {
      const rows = await db.select<
        { id: string; title: string; created_at: number; forked_from: string | null; fork_from_index: number | null }[]
      >("SELECT id, title, created_at, forked_from, fork_from_index FROM chats ORDER BY created_at DESC");
      const chats: Chat[] = [];
      for (const r of rows) {
        const msgs = await db.select<
          { role: string; content: string; thinking: string; model: string | null }[]
        >(
          "SELECT role, content, thinking, model FROM messages WHERE chat_id = $1 ORDER BY idx ASC",
          [r.id]
        );
        chats.push({
          id: r.id,
          title: r.title,
          createdAt: r.created_at,
          forkedFrom: r.forked_from ? (JSON.parse(r.forked_from) as Chat["forkedFrom"]) : null,
          forkFromIndex: r.fork_from_index ?? null,
          messages: msgs.map((m) => ({
            role: m.role as ChatMsg["role"],
            content: m.content,
            thinking: m.thinking || undefined,
          model: m.model || undefined
        }))
        });
      }
      return chats;
    },
    async saveChats(chats: Chat[]): Promise<void> {
      // full rewrite is simple and fast enough at this scale
      await db.execute("DELETE FROM messages");
      await db.execute("DELETE FROM chats");
      for (const c of chats) {
        await db.execute(
          "INSERT INTO chats (id, title, created_at, forked_from, fork_from_index) VALUES ($1, $2, $3, $4, $5)",
          [c.id, c.title, c.createdAt, c.forkedFrom ? JSON.stringify(c.forkedFrom) : null, c.forkFromIndex ?? null]
        );
        let i = 0;
        for (const m of c.messages) {
          await db.execute(
            "INSERT INTO messages (chat_id, role, content, thinking, model, idx) VALUES ($1, $2, $3, $4, $5, $6)",
            [c.id, m.role, m.content, m.thinking ?? "", m.model ?? null, i++]
          );
        }
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
}

export const DEFAULT_STOP_WORDS = ["\nUser:", "\nAssistant:", "<end_of_turn>", "<|im_end|>"];

export const DEFAULT_PREFS: Prefs = { theme: "light", accent: "orange", fontSize: 14.5, sidebarOpen: true, stopWords: [...DEFAULT_STOP_WORDS], ollamaBase: "" };
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
      ollamaBase: typeof p.ollamaBase === "string" ? p.ollamaBase : DEFAULT_PREFS.ollamaBase
    };
  } catch {
    return { ...DEFAULT_PREFS };
  }
}

async function readSqlitePrefs(): Promise<Partial<Prefs>> {
  const { default: Database } = await import("@tauri-apps/plugin-sql");
  const db = await Database.load(DB_PATH);
  const rows = await db.select<{ key: string; value: string }[]>(
    "SELECT key, value FROM meta WHERE key IN ('pref_theme', 'pref_accent', 'pref_font_size', 'pref_sidebar', 'pref_stop_words', 'pref_ollama_base')"
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
