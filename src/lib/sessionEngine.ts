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
  const i = l.indexOf("/", l.startsWith("plugin:") ? l.indexOf("/") + 1 : 0);
  return i > 0 ? l.slice(0, i) : l;
}

export const NO_WORKSPACE_LABEL = "No workspace";

/** Same rule as `isNoWorkspacePath` in sidebar.ts (kept local: node tests load
 *  this file without a bundler, so extensionless sibling imports fail). */
function isNoWorkspacePath(p: string | null | undefined): boolean {
  const s = (p ?? "").trim();
  if (!s) return false;
  if (s === "__vicuna_no_workspace__" || s === "none") return true;
  return /\/vicuna\/scratch\/[A-Za-z0-9_-]{1,64}$/.test(s.replace(/\\/g, "/").replace(/\/+$/, ""));
}

/** Normalize a workspace path for handoff comparisons (strip Windows verbatim
 *  prefix and trailing slashes). */
function normWorkspace(p: string | null | undefined): string | null {
  // No workspace mode (sentinel or its per-session scratch folder) is one value.
  if (isNoWorkspacePath(p)) return NO_WORKSPACE_LABEL;
  const s = (p ?? "").replace(/^\\\\\?\\/, "").trim();
  if (!s) return null;
  const trimmed = s.replace(/[/\\]+$/, "");
  return trimmed || s;
}

/** Short display labels for a workspace handoff: folder basenames when they
 *  differ, or the shortest differing trailing path segments when two paths
 *  share the same folder name. */
export function workspaceHandoffLabels(from: string, to: string): { from: string; to: string } {
  const a = (normWorkspace(from) ?? from).split(/[/\\]/).filter(Boolean);
  const b = (normWorkspace(to) ?? to).split(/[/\\]/).filter(Boolean);
  if (!a.length || !b.length) return { from, to };
  const max = Math.max(a.length, b.length);
  for (let n = 1; n <= max; n++) {
    const la = a.slice(-n).join("/");
    const lb = b.slice(-n).join("/");
    if (la !== lb) return { from: la, to: lb };
  }
  return { from: normWorkspace(from) ?? from, to: normWorkspace(to) ?? to };
}

export interface HandoffInfo {
  engine?: { from: string; to: string };
  workspace?: { from: string; to: string };
}

export function latestSummaryIndex(messages: readonly { summary?: boolean }[]): number {
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].summary) return i;
  }
  return -1;
}

/** A reply that runs on a different harness or workspace than the previous
 *  reply: the new harness or workspace starts its own engine session and only
 *  gets Vicuna's transcript, so the thread shows a "Context handoff" divider. */
export function handoffAt(
  messages: readonly { role: string; model?: string | null; workspace?: string | null; summary?: boolean }[],
  i: number
): HandoffInfo | null {
  const m = messages[i];
  if (!m || m.role !== "assistant" || m.summary) return null;

  let engine: { from: string; to: string } | undefined;
  const toEngine = labelHarness(m.model);
  if (toEngine) {
    for (let k = i - 1; k >= 0; k--) {
      const p = messages[k];
      if (p.role !== "assistant" || p.summary) continue;
      const fromEngine = labelHarness(p.model);
      if (!fromEngine) continue;
      if (fromEngine !== toEngine) {
        engine = { from: p.model as string, to: m.model as string };
      }
      break;
    }
  }

  let workspace: { from: string; to: string } | undefined;
  const toWs = normWorkspace(m.workspace);
  if (toWs) {
    for (let k = i - 1; k >= 0; k--) {
      const p = messages[k];
      if (p.role !== "assistant" || p.summary) continue;
      const fromWs = normWorkspace(p.workspace);
      if (!fromWs) continue;
      if (fromWs !== toWs) {
        workspace = { from: fromWs, to: toWs };
      }
      break;
    }
  }

  if (!engine && !workspace) return null;
  return { ...(engine ? { engine } : {}), ...(workspace ? { workspace } : {}) };
}
