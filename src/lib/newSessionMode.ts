// Which mode the starter shows after `New`.
//
// `New` follows the session you're in (a Work session's New is Work in the
// same folder, a Chat session's is Chat); from home it uses the mode you last
// picked, remembered across launches so Work users don't re-click Work every
// time the app starts.

export type SessionMode = "chat" | "work";

export const LAST_MODE_KEY = "vicuna-last-mode";

export function normalizeMode(v: unknown): SessionMode | null {
  return v === "chat" || v === "work" ? v : null;
}

/** Mode + workspace for a new session started while `current` is open. */
export function newSessionStart(
  current: { mode?: SessionMode | null; workspace?: string | null } | null | undefined,
  saved: SessionMode
): { mode: SessionMode; workspace: string | null } {
  const mode = normalizeMode(current?.mode) ?? (current ? "chat" : saved);
  const workspace = mode === "work" && current?.mode === "work" && current.workspace ? current.workspace : null;
  return { mode, workspace };
}

export function readLastMode(storage: Pick<Storage, "getItem"> | null | undefined): SessionMode {
  try {
    return normalizeMode(storage?.getItem(LAST_MODE_KEY)) ?? "chat";
  } catch {
    return "chat";
  }
}

export function writeLastMode(storage: Pick<Storage, "setItem"> | null | undefined, mode: SessionMode): void {
  try {
    storage?.setItem(LAST_MODE_KEY, mode);
  } catch {
    // storage full/blocked: the in-memory toggle still works this run
  }
}
