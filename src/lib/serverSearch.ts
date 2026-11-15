/** Fallback cross-host search for a `vicuna serve` that predates the
 *  `server_search_chats` command (every released build before the palette
 *  gained cross-host lookup does). Such a server still answers `chats_load`
 *  with its full session list, messages included, so the palette can search
 *  that locally instead of reporting the host as unsearchable.
 *
 *  Same rules as the Rust `server_store::search_chats`: exact id lookups first
 *  (a pasted session link), then case-insensitive matches over id, title and
 *  message text, newest first. Display rows only. */

export interface LoadedChatMessage {
  role?: string;
  content?: string;
}

export interface LoadedChat {
  id: string;
  title?: string | null;
  messages?: LoadedChatMessage[] | null;
  createdAt?: number | string | null;
  updatedAt?: number | string | null;
  mode?: string | null;
  workspace?: string | null;
}

export interface FallbackHit {
  id: string;
  title: string;
  snippet: string;
  updatedAt: number;
  mode: string;
  workspace: string | null;
}

function num(v: unknown): number {
  const n = typeof v === "number" ? v : parseInt(String(v ?? ""), 10);
  return Number.isFinite(n) ? n : 0;
}

/** A display excerpt around the first match (or the opening user message).
 *  Mirrors the Rust side so both search routes read the same. */
export function excerpt(text: string, q: string): string {
  const flat = (text || "").replace(/[\r\n]+/g, " ").trim();
  const chars = Array.from(flat);
  const head = chars.slice(0, 160).join("");
  if (!q) return head;
  const idx = flat.toLowerCase().indexOf(q.toLowerCase());
  if (idx < 0) return head;
  const startChar = Array.from(flat.slice(0, idx)).length;
  const len = Array.from(q).length;
  const from = Math.max(0, startChar - 40);
  const to = Math.min(chars.length, startChar + len + 80);
  return (from > 0 ? "…" : "") + chars.slice(from, to).join("") + (to < chars.length ? "…" : "");
}

function contents(c: LoadedChat): string[] {
  return (c.messages ?? []).map((m) => m?.content ?? "").filter((s) => typeof s === "string" && s.length > 0);
}

function snippetFor(c: LoadedChat, q: string): string {
  if (q) {
    const low = q.toLowerCase();
    const hit = contents(c).find((s) => s.toLowerCase().includes(low));
    if (hit) return excerpt(hit, q);
  }
  const user = (c.messages ?? []).find((m) => m?.role === "user" && (m.content ?? "").trim())?.content;
  return excerpt(user || contents(c)[0] || "", "");
}

function matches(c: LoadedChat, low: string): boolean {
  if ((c.id ?? "").toLowerCase().includes(low)) return true;
  if ((c.title ?? "").toLowerCase().includes(low)) return true;
  return contents(c).some((s) => s.toLowerCase().includes(low));
}

function hitFor(c: LoadedChat, q: string): FallbackHit {
  const id = String(c.id ?? "");
  return {
    id,
    title: String(c.title ?? ""),
    snippet: snippetFor(c, q),
    updatedAt: num(c.updatedAt) || num(c.createdAt),
    mode: c.mode === "work" ? "work" : "chat",
    workspace: c.workspace ?? null
  };
}

/** Search a `chats_load` payload: chats named by `ids` first (case-insensitive
 *  exact id), then text matches newest first, at most `limit` rows. */
export function searchLoadedChats(
  chats: LoadedChat[],
  query: string,
  ids: string[],
  limit = 6
): FallbackHit[] {
  const size = Math.min(50, Math.max(1, Math.floor(limit) || 6));
  const list = (chats || []).filter((c) => c && typeof c.id === "string" && c.id);
  const byId = new Map(list.map((c) => [c.id.toLowerCase(), c]));
  const out: FallbackHit[] = [];
  const seen = new Set<string>();

  for (const want of (ids || []).slice(0, 10)) {
    const key = String(want ?? "").trim().toLowerCase();
    if (!key || seen.has(key)) continue;
    const c = byId.get(key);
    if (!c) continue;
    out.push(hitFor(c, ""));
    seen.add(key);
  }

  const q = (query || "").trim();
  if (q) {
    const low = q.toLowerCase();
    const rest = list
      .filter((c) => !seen.has(c.id.toLowerCase()) && matches(c, low))
      .sort((a, b) => (num(b.updatedAt) || num(b.createdAt)) - (num(a.updatedAt) || num(a.createdAt)))
      .slice(0, size);
    for (const c of rest) {
      out.push(hitFor(c, q));
      seen.add(c.id.toLowerCase());
    }
  }
  return out;
}
