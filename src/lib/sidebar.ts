// Sidebar scoping for the session list.
//
// The list is scoped by *where a session lives*, so the scope dropdown can
// offer one entry per workspace a user actually works in:
//   * plain chat sessions live in the "Chat" channel — they appear under All
//     and under Chat, but never under a workspace entry;
//   * work sessions live in the folder they were run against, so the workspace
//     entry shows only that project's work sessions.
//
// A work session with no recorded workspace has no folder to live in, so it
// is treated as a chat: it stays visible under Chat and All rather than
// disappearing from every entry.
//
// Option keys are encoded so a workspace root is never mistaken for one of the
// two reserved channels: "all", "chat", or "W:<root>". Raw roots are still
// accepted anywhere a filter is read (older builds stored them directly).
//
// Zero runtime imports (type-only) so plain node can check it:
//   node scripts/check-sidebar.mjs

import type { Chat } from "./store";

/** Chat = the workspace-less channel; any other value is a workspace root. */
export const CHAT_FILTER = "chat";
/** All = no scoping at all (the default). */
export const ALL_FILTER = "all";
/** Prefix that marks an option key as a workspace root. */
export const WORKSPACE_PREFIX = "W:";

/** Final path segment on any platform, with Rust's Windows verbatim prefix
 *  stripped. Mirrors displayWorkspace/workspaceBase in ChatApp.svelte, which
 *  cannot be imported here (it is a Svelte component). */
export function workspaceBase(p: string): string {
  let out = p;
  if (out.startsWith("\\\\?\\UNC\\")) out = "\\" + out.slice("\\\\?\\UNC\\".length);
  else if (out.startsWith("\\\\?\\")) out = out.slice("\\\\?\\".length);
  return out.trim().split(/[/\\]/).filter(Boolean).pop() ?? "";
}

/** The folder a session lives in: its workspace root, or CHAT_FILTER when it
 *  has none (plain chats and workspace-less work sessions). */
export function folderOf(c: Chat): string {
  const ws = (c.workspace ?? "").trim();
  if (!ws) return CHAT_FILTER;
  return displayRoot(ws);
}

/** Normalized workspace root used as a filter key (verbatim prefix stripped,
 *  separators unified) so one folder never splits into two entries. */
function displayRoot(p: string): string {
  let out = p;
  if (out.startsWith("\\\\?\\UNC\\")) out = "\\" + out.slice("\\\\?\\UNC\\".length);
  else if (out.startsWith("\\\\?\\")) out = out.slice("\\\\?\\".length);
  return out.replace(/\\/g, "/").replace(/\/+$/, "") || CHAT_FILTER;
}

/** Encode a folder (a root or CHAT_FILTER) as a dropdown option key. */
export function folderOptionKey(folder: string): string {
  if (folder === ALL_FILTER || folder === CHAT_FILTER) return folder;
  return WORKSPACE_PREFIX + folder;
}

/**
 * The workspace root an option key points at, or null for All/Chat.
 *
 * Accepts a raw root too, so a filter stored by an older build (which kept
 * the bare root in prefs) keeps working instead of silently falling back.
 */
export function workspaceOfFolderKey(key: string): string | null {
  if (key === ALL_FILTER || key === CHAT_FILTER) return null;
  if (key.startsWith(WORKSPACE_PREFIX)) {
    const root = key.slice(WORKSPACE_PREFIX.length);
    return root ? root : null;
  }
  return displayRoot(key) === CHAT_FILTER ? null : displayRoot(key);
}

export interface FolderOption {
  /** Filter value: "all", "chat", or WORKSPACE_PREFIX + workspace root. */
  key: string;
  /** Dropdown label: "All", "Chat", or the workspace's final segment. */
  label: string;
  /** Full path for the tooltip (empty for All/Chat). */
  title: string;
  /** Sessions that live in this folder (before date grouping). */
  count: number;
}

/**
 * Entries offered by the scope dropdown, derived from the sessions themselves
 * rather than from prefs — so an entry never appears for a workspace no
 * session lives in. All is always first, then Chat, then each workspace by the
 * recency of the newest session in it.
 *
 * Chat is always offered, even with zero sessions: it is where plain chats
 * will go, so hiding it would make the dropdown jump around.
 *
 * `chats` is expected in display order (most recent first); the order is
 * preserved when deriving each workspace's recency.
 */
export function folderOptions(chats: Chat[]): FolderOption[] {
  const byKey = new Map<string, FolderOption & { recency: number }>();
  byKey.set(ALL_FILTER, { key: ALL_FILTER, label: "All", title: "", count: 0, recency: -1 });
  byKey.set(CHAT_FILTER, { key: CHAT_FILTER, label: "Chat", title: "", count: 0, recency: -1 });
  let total = 0;
  for (let i = 0; i < chats.length; i++) {
    const c = chats[i];
    // empty drafts never reach the list, so they must not mint an entry either
    if (c.messages.length === 0) continue;
    total++;
    const folder = folderOf(c);
    const key = folderOptionKey(folder);
    let opt = byKey.get(key);
    if (!opt) {
      opt = { key, label: workspaceBase(folder), title: folder, count: 0, recency: i };
      byKey.set(key, opt);
    }
    opt.count++;
    // first sighting in display order = this folder's newest session
    if (opt.recency > i) opt.recency = i;
  }
  byKey.get(ALL_FILTER)!.count = total;
  const order = [...byKey.values()].sort((a, b) => {
    if (a.key === ALL_FILTER) return -1;
    if (b.key === ALL_FILTER) return 1;
    if (a.key === CHAT_FILTER) return -1;
    if (b.key === CHAT_FILTER) return 1;
    return a.recency - b.recency;
  });
  return disambiguate(order.map(({ recency: _recency, ...rest }) => rest));
}

/**
 * Two workspaces can share a final segment (`~/work/api` and `~/side/api`).
 * A dropdown shows only the selected option's label, so a duplicate would be
 * indistinguishable — qualify each with its parent folder instead.
 */
function disambiguate(options: FolderOption[]): FolderOption[] {
  const seen = new Map<string, number>();
  for (const o of options) seen.set(o.label, (seen.get(o.label) ?? 0) + 1);
  return options.map((o) => {
    if ((seen.get(o.label) ?? 0) < 2 || !o.title) return o;
    const parts = o.title.split("/").filter(Boolean);
    const parent = parts.length > 1 ? parts[parts.length - 2] : "";
    return parent ? { ...o, label: `${parent} › ${o.label}` } : o;
  });
}

/** Normalize a stored filter: anything that is not a string or is empty
 *  falls back to All, so a corrupt localStorage value cannot blank the list. */
export function normalizeSessionFilter(v: unknown): string {
  if (typeof v !== "string") return ALL_FILTER;
  const t = v.trim();
  return t ? t : ALL_FILTER;
}

/**
 * Resolve a stored filter against the entries that exist right now, returning
 * a canonical option key.
 *
 * A filter pointing at a folder whose last session was deleted would otherwise
 * leave an empty sidebar with no visible reason, so it falls back to All
 * instead. All and Chat always resolve.
 */
export function resolveSessionFilter(filter: unknown, options: FolderOption[]): string {
  const f = normalizeSessionFilter(filter);
  if (f === ALL_FILTER) return ALL_FILTER;
  const ws = workspaceOfFolderKey(f);
  const wanted = ws === null ? f : folderOptionKey(ws);
  if (options.some((o) => o.key === wanted)) return wanted;
  return ALL_FILTER;
}

/** True when a session belongs in the given (already resolved) filter. */
export function matchesFilter(c: Chat, filter: string): boolean {
  const f = normalizeSessionFilter(filter);
  if (f === ALL_FILTER) return true;
  const ws = workspaceOfFolderKey(f);
  if (ws === null) return folderOf(c) === CHAT_FILTER;
  return folderOf(c) === ws;
}

/** Filter a list, preserving order. */
export function filterChats(chats: Chat[], filter: string): Chat[] {
  const f = normalizeSessionFilter(filter);
  if (f === ALL_FILTER) return chats;
  return chats.filter((c) => matchesFilter(c, f));
}

export interface SessionGroups {
  today: Chat[];
  yesterday: Chat[];
  week: Chat[];
  older: Chat[];
}

/**
 * Date buckets over an already-ordered list. Pure form of the sidebar's
 * `grouped`: order by last user-message time, skip empty drafts, then split
 * on today / yesterday / this week / older.
 */
export function groupByDate(chats: Chat[], now: number): SessionGroups {
  const d = new Date(now);
  const startOfToday = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const startOfYesterday = startOfToday - 24 * 3600 * 1000;
  const weekAgo = now - 7 * 24 * 3600 * 1000;
  const groups: SessionGroups = { today: [], yesterday: [], week: [], older: [] };
  for (const c of [...chats].sort((a, b) => activityOf(b) - activityOf(a))) {
    if (c.messages.length === 0) continue;
    const t = activityOf(c);
    if (t >= startOfToday) groups.today.push(c);
    else if (t >= startOfYesterday) groups.yesterday.push(c);
    else if (t >= weekAgo) groups.week.push(c);
    else groups.older.push(c);
  }
  return groups;
}

/** Last user-message time, falling back to creation (same rule as
 *  chatActivity in store.ts, kept local so this module stays import-free). */
function activityOf(c: Chat): number {
  return typeof c.updatedAt === "number" ? c.updatedAt : c.createdAt;
}
