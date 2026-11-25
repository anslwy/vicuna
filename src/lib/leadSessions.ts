import type { SkillsEnabledMap } from "./skills";
import type { McpSelection } from "./mcp";

// Lead session (vicuna-mcp `manage_sessions`): pure text helpers. The lead
// creates/messages/stops/removes worker sessions; the window performs the
// action (ChatApp `leadControl`) and these build what the lead model reads.

export interface SessionRow {
  id: string;
  title: string;
  status: "running" | "idle" | "compacting";
  engine: string;
  model: string | null;
  workspace: string | null;
  role: "lead" | "worker" | "other";
  updatedAt: number;
  archived?: boolean;
}

/** first line of a worker's finished-turn message; keep in sync with parseWorkerDone */
export const WORKER_DONE_HEAD = "Worker session finished";

/** tail of a long text, cut on a line boundary when one is near */
export function tailText(text: string, max: number): string {
  const t = text.trim();
  if (t.length <= max) return t;
  let cut = t.slice(t.length - max);
  const nl = cut.indexOf("\n");
  if (nl >= 0 && nl < 200) cut = cut.slice(nl + 1);
  return `…${cut}`;
}

/** the message a lead receives when one of its workers ends a turn */
export function workerDoneMessage(w: { id: string; title: string; engine: string; model: string | null }, reply: string, stopped: boolean): string {
  const how = stopped ? "stopped" : "finished its turn";
  const who = w.model ? `${w.engine}/${w.model}` : w.engine;
  const body = reply.trim() ? tailText(reply, 6000) : "(no reply text)";
  return `${WORKER_DONE_HEAD}: "${w.title}" (${w.id}, ${who}) ${how}.\nIts last reply:\n\n${body}\n\nUse manage_sessions (send/stop/delete) or read_session for more.`;
}

/** recognizes workerDoneMessage text: { title, id } or null */
export function parseWorkerDone(text: string): { title: string; id: string } | null {
  const m = /^Worker session finished: "(.*)" \(([^,()]+), /.exec(text);
  return m ? { title: m[1], id: m[2] } : null;
}

/** `list` result text */
export function sessionListText(rows: SessionRow[], engines: string[], defaultEngine: string): string {
  const lines: string[] = [];
  lines.push(`Installed engines: ${engines.length ? engines.join(", ") : "(none detected)"} (default: ${defaultEngine}).`);
  if (!rows.length) {
    lines.push("No other Work sessions.");
    return lines.join("\n");
  }
  lines.push(`Work sessions (newest first, ${rows.length}):`);
  for (const r of rows) {
    const tag = r.role === "lead" ? " [lead, you]" : r.role === "worker" ? " [your worker]" : "";
    const model = r.model ? `${r.engine}/${r.model}` : r.engine;
    const arch = r.archived ? " [archived]" : "";
    lines.push(`- ${r.id} · "${r.title}"${tag}${arch} · ${r.status} · ${model}${r.workspace ? ` · ${r.workspace}` : ""}`);
  }
  return lines.join("\n");
}

export interface ChatExtra extends McpSelection {
  skillsEnabled?: SkillsEnabledMap;
  engine?: string | null;
  workModel?: string | null;
  role?: "lead" | null;
  leadId?: string | null;
  pinned?: boolean;
  archived?: boolean;
  permissions?: { read?: boolean; message?: boolean; remote?: boolean };
}

/** `remote` = a lead may manage sessions on the user's saved servers (off unless granted). */
export function sessionPermissions(extra: Partial<Pick<ChatExtra, "role" | "permissions">>): { read: boolean; message: boolean; remote: boolean } {
  return { read: extra.permissions?.read ?? true, message: extra.permissions?.message ?? extra.role === "lead", remote: extra.permissions?.remote ?? false };
}

/** persisted `extra` JSON column: per-session engine/model/lead/pin */
export function chatExtraJson(c: ChatExtra): string | null {
  const o: Record<string, unknown> = {};
  if (c.engine) o.engine = c.engine;
  if (c.workModel) o.workModel = c.workModel;
  if (c.role === "lead") o.role = "lead";
  if (c.leadId) o.leadId = c.leadId;
  if (c.pinned) o.pinned = true;
  if (c.archived) o.archived = true;
  if (c.permissions) o.permissions = c.permissions;
  if (c.skillsEnabled) o.skillsEnabled = c.skillsEnabled;
  if (c.mcpEnabled) o.mcpEnabled = c.mcpEnabled;
  if (c.mcpOnly) o.mcp = c.mcpOnly;
  return Object.keys(o).length ? JSON.stringify(o) : null;
}

export function parseChatExtra(raw: unknown): ChatExtra {
  let o: any = raw;
  if (typeof raw === "string") {
    try {
      o = JSON.parse(raw);
    } catch {
      o = null;
    }
  }
  if (!o || typeof o !== "object") return {};
  const out: ChatExtra = {};
  if (typeof o.engine === "string" && o.engine) out.engine = o.engine;
  if (typeof o.workModel === "string" && o.workModel) out.workModel = o.workModel;
  if (o.role === "lead") out.role = "lead";
  if (typeof o.leadId === "string" && o.leadId) out.leadId = o.leadId;
  if (o.pinned === true) out.pinned = true;
  if (o.archived === true) out.archived = true;
  if (o.permissions && typeof o.permissions === "object") {
    const permissions: { read?: boolean; message?: boolean; remote?: boolean } = {};
    if (typeof o.permissions.read === "boolean") permissions.read = o.permissions.read;
    if (typeof o.permissions.message === "boolean") permissions.message = o.permissions.message;
    if (typeof o.permissions.remote === "boolean") permissions.remote = o.permissions.remote;
    if (Object.keys(permissions).length) out.permissions = permissions;
  }
  if (o.mcpEnabled && typeof o.mcpEnabled === "object" && !Array.isArray(o.mcpEnabled)) {
    out.mcpEnabled = Object.fromEntries(Object.entries(o.mcpEnabled).filter(
      (entry): entry is [string, boolean] => typeof entry[1] === "boolean"
    ));
  }
  if (o.skillsEnabled && typeof o.skillsEnabled === "object" && !Array.isArray(o.skillsEnabled)) {
    out.skillsEnabled = Object.fromEntries(Object.entries(o.skillsEnabled).filter(
      (entry): entry is [string, boolean] => typeof entry[1] === "boolean"
    ));
  }
  if (Array.isArray(o.mcp)) out.mcpOnly = o.mcp.filter((value: unknown) => typeof value === "string");
  return out;
}

/** Enforce a single lead session in memory: clears `role` on every other chat
 *  currently marked `"lead"` and returns their ids. */
export function demoteOtherLeads<T extends { id: string; role?: "lead" | null }>(chats: T[], keepId: string): string[] {
  const demoted: string[] = [];
  for (const c of chats) {
    if (c.id !== keepId && c.role === "lead") {
      c.role = null;
      demoted.push(c.id);
    }
  }
  return demoted;
}
