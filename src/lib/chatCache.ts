// Client-side IndexedDB cache for full server chats, keyed by (scope, chatId)
// and validated against the monotonic server `rev` returned by `chats_list`.
//
// Cold start stays fast (`chats_list` returns lightweight previews), while any
// session whose `rev` matches the local cache is hydrated in 0 ms without a
// `chats_get` round-trip.

const DB_NAME = "vicuna-chat-cache";
const DB_VERSION = 1;
const STORE_NAME = "chats";
const SCOPE_INDEX = "by_scope";
export const DEFAULT_MAX_CACHED_CHATS = 200;

export interface CachedChatEntry {
  k: string;
  scope: string;
  id: string;
  rev: number;
  raw: any;
  at: number;
}

/** Decide whether a `chats_list` row can be upgraded from the local cache.
 *  Pure so node tests can verify rev validation without IndexedDB. */
export function resolveCachedChatRow(
  raw: any,
  cached: ReadonlyMap<string, { rev: number; raw: any }>
): { row: any; hit: boolean } {
  if (!raw || typeof raw.id !== "string") return { row: raw, hit: false };
  if (raw.partial !== true || typeof raw.rev !== "number") return { row: raw, hit: false };
  const entry = cached.get(raw.id);
  if (entry && entry.rev === raw.rev && entry.raw && entry.raw.partial !== true) {
    return { row: entry.raw, hit: true };
  }
  return { row: raw, hit: false };
}

/** Return the composite keys (`k`) to delete from a scope: entries no longer in
 *  `keepIds`, plus the oldest entries above `maxEntries`. */
export function selectCacheEvictions(
  entries: ReadonlyArray<Pick<CachedChatEntry, "k" | "id" | "at">>,
  keepIds: ReadonlySet<string>,
  maxEntries = DEFAULT_MAX_CACHED_CHATS
): string[] {
  const drop: string[] = [];
  const kept: Array<Pick<CachedChatEntry, "k" | "id" | "at">> = [];
  for (const e of entries) {
    if (!keepIds.has(e.id)) drop.push(e.k);
    else kept.push(e);
  }
  if (kept.length > maxEntries) {
    kept.sort((a, b) => a.at - b.at);
    for (let i = 0; i < kept.length - maxEntries; i++) {
      drop.push(kept[i].k);
    }
  }
  return drop;
}

let dbPromise: Promise<IDBDatabase | null> | null = null;

function openCacheDb(): Promise<IDBDatabase | null> {
  if (typeof indexedDB === "undefined") return Promise.resolve(null);
  if (dbPromise) return dbPromise;
  dbPromise = new Promise<IDBDatabase | null>((resolve) => {
    try {
      const req = indexedDB.open(DB_NAME, DB_VERSION);
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains(STORE_NAME)) {
          const store = db.createObjectStore(STORE_NAME, { keyPath: "k" });
          store.createIndex(SCOPE_INDEX, "scope", { unique: false });
        }
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => resolve(null);
      req.onblocked = () => resolve(null);
    } catch {
      resolve(null);
    }
  });
  return dbPromise;
}

function cacheKey(scope: string, id: string): string {
  return `${scope}:${id}`;
}

/** Read every cached full chat for `scope`. Never throws; returns empty on error. */
export async function readCachedChats(scope: string): Promise<Map<string, { rev: number; raw: any }>> {
  const out = new Map<string, { rev: number; raw: any }>();
  const db = await openCacheDb();
  if (!db) return out;
  return new Promise((resolve) => {
    try {
      const tx = db.transaction(STORE_NAME, "readonly");
      const req = tx.objectStore(STORE_NAME).index(SCOPE_INDEX).getAll(scope);
      req.onsuccess = () => {
        const rows = Array.isArray(req.result) ? (req.result as CachedChatEntry[]) : [];
        for (const r of rows) {
          if (r && typeof r.id === "string" && typeof r.rev === "number" && r.raw && r.raw.partial !== true) {
            out.set(r.id, { rev: r.rev, raw: r.raw });
          }
        }
        resolve(out);
      };
      req.onerror = () => resolve(out);
    } catch {
      resolve(out);
    }
  });
}

/** Persist a full server chat row (must carry a numeric `rev` and not be partial). */
export function writeCachedChat(scope: string, raw: any): void {
  if (!raw || typeof raw.id !== "string" || typeof raw.rev !== "number" || raw.partial === true) return;
  const clean = { ...raw };
  delete clean.partial;
  const entry: CachedChatEntry = {
    k: cacheKey(scope, raw.id),
    scope,
    id: raw.id,
    rev: raw.rev,
    raw: clean,
    at: Date.now()
  };
  void openCacheDb().then((db) => {
    if (!db) return;
    try {
      const tx = db.transaction(STORE_NAME, "readwrite");
      tx.objectStore(STORE_NAME).put(entry);
    } catch {
      // ignore quota / storage errors
    }
  });
}

/** Remove deleted chats from the cache for `scope`. */
export function deleteCachedChats(scope: string, ids: string[]): void {
  if (!ids.length) return;
  void openCacheDb().then((db) => {
    if (!db) return;
    try {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      for (const id of ids) store.delete(cacheKey(scope, id));
    } catch {
      // ignore
    }
  });
}

/** Drop entries no longer on the server and cap the cache size per scope. */
export function pruneCachedChats(
  scope: string,
  keepIds: ReadonlySet<string>,
  maxEntries = DEFAULT_MAX_CACHED_CHATS
): void {
  void openCacheDb().then((db) => {
    if (!db) return;
    try {
      const tx = db.transaction(STORE_NAME, "readwrite");
      const store = tx.objectStore(STORE_NAME);
      const req = store.index(SCOPE_INDEX).getAll(scope);
      req.onsuccess = () => {
        const rows = Array.isArray(req.result) ? (req.result as CachedChatEntry[]) : [];
        const drop = selectCacheEvictions(rows, keepIds, maxEntries);
        for (const k of drop) store.delete(k);
      };
    } catch {
      // ignore
    }
  });
}
