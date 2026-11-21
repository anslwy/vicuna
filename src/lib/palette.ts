// Cmd+P search palette: row ordering + cross-host lookup rules.
//
// Pure so the node tests can cover the parts that bit us: the keyboard cursor
// starts at index 0, so a query has to put its matches there (paste a session
// link → Enter), while an empty palette keeps "New" on top.

export type PaletteRow<C, R> =
  | { kind: "new" }
  | { kind: "chat"; chat: C; snippet?: string }
  | { kind: "remote"; hit: R };

/** Merge in-memory matches with full-text hits returned by the current server's
 *  `server_search_chats` (needed when sessions in `allChats` are `partial`, so
 *  their full message bodies are not in memory yet). Preserves local order,
 *  dedupes by chat id, and attaches the server's centered match snippet when a
 *  chat is partial or was only matched on the server. */
export function mergeLocalServerHits<C extends { id: string; partial?: boolean }>(
  localMatches: C[],
  serverHits: ReadonlyArray<{ id: string; snippet: string }>,
  allChats: ReadonlyArray<C>,
  limit = 20
): Array<{ chat: C; snippet?: string }> {
  const snippetById = new Map<string, string>();
  for (const h of serverHits) {
    if (h.snippet) snippetById.set(h.id, h.snippet);
  }
  const byId = new Map<string, C>();
  for (const c of allChats) byId.set(c.id, c);

  const out: Array<{ chat: C; snippet?: string }> = [];
  const seen = new Set<string>();
  for (const c of localMatches) {
    if (seen.has(c.id)) continue;
    seen.add(c.id);
    // Partial chats only hold 240-char previews in memory, so prefer the
    // server's match snippet when available.
    const snippet = c.partial ? snippetById.get(c.id) : undefined;
    out.push(snippet ? { chat: c, snippet } : { chat: c });
    if (out.length >= limit) return out;
  }
  for (const h of serverHits) {
    if (seen.has(h.id)) continue;
    const chat = byId.get(h.id);
    if (!chat) continue;
    seen.add(h.id);
    out.push(h.snippet ? { chat, snippet: h.snippet } : { chat });
    if (out.length >= limit) return out;
  }
  return out;
}

/** Matches first when there is a query (New drops to the bottom), New first
 *  when the palette is empty/unfiltered. The cursor never needs to move. */
export function orderPaletteRows<C, R>(matches: PaletteRow<C, R>[], query: string): PaletteRow<C, R>[] {
  const newRow: PaletteRow<C, R> = { kind: "new" };
  return query.trim() ? [...matches, newRow] : [newRow, ...matches];
}

/** The UUID inside a pasted `vicuna://s/<id>` link or `/s/<id>` URL. */
export const SESSION_ID_RE = /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/i;

export function sessionIdIn(text: string): string | null {
  const m = text.match(SESSION_ID_RE);
  return m ? m[0].toLowerCase() : null;
}

/** What to ask the connected hosts: a pasted session id is looked up by id
 *  alone; free text is only worth a network round trip from 2 characters. */
export function remoteQueryFor(query: string, sessionId: string | null): { ids: string[]; text: string } {
  const q = query.trim();
  if (sessionId) return { ids: [sessionId], text: "" };
  return { ids: [], text: q.length >= 2 ? q : "" };
}

/** Label every row with its host when more than this window's own host is in
 *  play. A server window always labels (its chats live on another machine). */
export function showHostLabels(onServer: boolean, remoteHits: number): boolean {
  return onServer || remoteHits > 0;
}

/** Turn a failed cross-host search into something the user can act on. An
 *  older `vicuna serve` has no `server_search_chats`, and that silence used to
 *  look exactly like "this session does not exist anywhere". */
export function describeServerSearchError(raw: string): string {
  const msg = (raw || "").trim();
  if (/unknown bridge command/i.test(msg)) {
    return "this server runs an older Vicuna — update it, then restart `vicuna serve` (or its service)";
  }
  if (!msg) return "search failed on that server";
  return msg;
}
