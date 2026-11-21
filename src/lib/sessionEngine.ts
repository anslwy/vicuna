/** Which harness + model a Work session runs on. A session that stored its
 *  own harness (`chats.extra.engine`, set by the picker, `manage_sessions` or
 *  Telegram `/model`) always runs on it; the header tag, the picker and the
 *  reply footer all read this same answer. Sessions without one (brand-new)
 *  follow the global pick, i.e. the last-used choice, and get it pinned on
 *  their first turn. */
export interface SessionPick {
  engine: string;
  model: string | null;
  /** true when the session stored its own harness */
  own: boolean;
}

export function sessionPick(
  chat: { engine?: string | null; workModel?: string | null } | null | undefined,
  global: { engine: string; model: string | null }
): SessionPick {
  const e = chat?.engine?.trim();
  if (e) return { engine: e, model: chat?.workModel || null, own: true };
  return { engine: global.engine, model: global.model, own: false };
}

/** Send gate for a session whose stored harness is not available: never
 *  fall back silently, ask the user to pick another one. */
export function missingHarnessReason(stored: string): string {
  return `This session runs on the harness "${stored}", which is not installed any more. Pick another harness for this session:`;
}

/** Harness part of a Work reply label (`claude/opus` → `claude`). */
export function labelHarness(label: string | null | undefined): string | null {
  const l = label?.trim();
  if (!l) return null;
  const i = l.indexOf("/");
  return i > 0 ? l.slice(0, i) : l;
}

/** A reply that runs on a different harness than the previous reply: the new
 *  harness starts its own engine session and only gets Vicuna's transcript,
 *  so the thread shows a "Context handoff" divider there. */
export function handoffAt(
  messages: readonly { role: string; model?: string | null }[],
  i: number
): { from: string; to: string } | null {
  const m = messages[i];
  if (!m || m.role !== "assistant") return null;
  const to = labelHarness(m.model);
  if (!to) return null;
  for (let k = i - 1; k >= 0; k--) {
    const p = messages[k];
    if (p.role !== "assistant") continue;
    const from = labelHarness(p.model);
    if (!from) continue;
    return from === to ? null : { from: p.model as string, to: m.model as string };
  }
  return null;
}
