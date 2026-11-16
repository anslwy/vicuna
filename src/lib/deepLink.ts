// Pure rules for `vicuna://s/<id>` session deep links (node-tested in
// node-tests/deepLink.test.ts). ChatApp owns the side effects.

/** Session id named by the first `…://s/<id>` (or `/s/<id>`) URL, if any.
 *  Tolerates a trailing slash, query or fragment (`vicuna://s/<id>?x`). */
export function extractSessionId(urls: readonly string[] | null | undefined): string | null {
  for (const raw of urls ?? []) {
    const u = String(raw ?? "").trim();
    const path = u.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "").replace(/[?#].*$/, "");
    const m = path.match(/^\/?s\/([A-Za-z0-9-]+)\/?$/);
    if (m) return m[1];
  }
  return null;
}

export type SessionLinkAction =
  /** chats aren't loaded yet: keep it until they are */
  | { kind: "stash" }
  /** the session is in this window's list */
  | { kind: "open-local" }
  /** not here: look it up on the connected servers */
  | { kind: "search-remote" }
  /** not here and no server to ask */
  | { kind: "not-found" };

/** What to do with a session link given this window's state. Decided only
 *  once chats have loaded, so a link that arrives early (cold start, or a
 *  remote host whose list loads late) is never judged against an empty list. */
export function sessionLinkAction(
  id: string,
  state: { ready: boolean; localIds: readonly string[]; serverCount: number }
): SessionLinkAction {
  if (!state.ready) return { kind: "stash" };
  if (state.localIds.includes(id)) return { kind: "open-local" };
  return state.serverCount > 0 ? { kind: "search-remote" } : { kind: "not-found" };
}
