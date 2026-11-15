// Cmd+P search palette: row ordering + cross-host lookup rules.
//
// Pure so the node tests can cover the parts that bit us: the keyboard cursor
// starts at index 0, so a query has to put its matches there (paste a session
// link → Enter), while an empty palette keeps "New" on top.

export type PaletteRow<C, R> = { kind: "new" } | { kind: "chat"; chat: C } | { kind: "remote"; hit: R };

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
