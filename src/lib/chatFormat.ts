import type { ChatMsg } from "$lib/ollama";
import type { Chat } from "$lib/store";
import { latestContextStartIndex } from "$lib/sessionEngine";

export type AccentDef = {
  label: string;
  light: { bg: string; hover: string; fg: string };
  dark: { bg: string; hover: string; fg: string };
};

export const ACCENTS: Record<string, AccentDef> = {
  orange: { label: "Orange", light: { bg: "#ef6c00", hover: "#d96000", fg: "#ffffff" }, dark: { bg: "#ef6c00", hover: "#d96000", fg: "#ffffff" } },
  black: { label: "Black", light: { bg: "#111111", hover: "#000000", fg: "#ffffff" }, dark: { bg: "#e8e8e8", hover: "#ffffff", fg: "#111111" } },
  blue: { label: "Blue", light: { bg: "#2563eb", hover: "#1d4ed8", fg: "#ffffff" }, dark: { bg: "#3b82f6", hover: "#2563eb", fg: "#ffffff" } },
  green: { label: "Green", light: { bg: "#16a34a", hover: "#15803d", fg: "#ffffff" }, dark: { bg: "#22c55e", hover: "#16a34a", fg: "#111111" } }
};

export function formatDuration(ms: number): string {
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(s < 10 ? 1 : 0)}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${Math.round(s % 60)}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

export function formatPullRate(bps: number): string {
  if (!isFinite(bps) || bps <= 0) return "";
  if (bps >= 1e9) return `${(bps / 1e9).toFixed(1)} GB/s`;
  if (bps >= 1e6) return `${(bps / 1e6).toFixed(1)} MB/s`;
  if (bps >= 1e3) return `${(bps / 1e3).toFixed(0)} KB/s`;
  return `${Math.round(bps)} B/s`;
}

export function formatLiveElapsed(ms: number): string {
  const totalS = Math.max(0, Math.floor(ms / 1000));
  const s = totalS % 60;
  const totalM = Math.floor(totalS / 60);
  const m = totalM % 60;
  const totalH = Math.floor(totalM / 60);
  const h = totalH % 24;
  const d = Math.floor(totalH / 24);
  if (d > 0) return `${d}d ${h}h`;
  if (totalH > 0) return `${totalH}h ${m}m`;
  if (totalM > 0) return `${totalM}m ${s}s`;
  return `${s}s`;
}

export { cmpVersion } from "./version";

export function defaultServerName(url: string): string {
  return url.split("://")[1] ?? url;
}

export function stopWordsToText(list: string[]): string {
  return list.map((s) => s.replace(/\n/g, "\\n")).join("\n");
}

export function textToStopWords(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim().replace(/\\n/g, "\n"))
    .filter((s) => s.length > 0);
}

export function capLevel(l: string): string {
  return l.length > 0 ? l[0].toUpperCase() + l.slice(1) : l;
}

export function isMlxVariant(tag: string): boolean {
  return /-mlx$/i.test(tag);
}

export function formatSize(n: number): string {
  if (n < 1024) return `${n}B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(n < 10240 ? 1 : 0)}KB`;
  return `${(n / (1024 * 1024)).toFixed(1)}MB`;
}

export function decodeEntities(s: string): string {
  return s
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&amp;/g, "&");
}

/** Harness markup (collapsed tool blocks, ask records) is display-only.
 *  Models that see it in their own history imitate the HTML instead of
 *  calling tools, so history sends a compact plain-text form instead:
 *  tool blocks drop out, ask records become prose. */
export function stripHarnessMarkup(content: string): string {
  return content
    .replace(/<details class="tool-result">[\s\S]*?<\/details>/g, "")
    .replace(
      /<div class="ask-log">\s*<div class="ask-log-q">([\s\S]*?)<\/div>\s*<div class="ask-log-a">([\s\S]*?)<\/div>\s*<\/div>/g,
      (_m, q: string, a: string) => {
        const answer = /ask-log-dismissed/.test(a)
          ? "dismissed the question"
          : `answered: ${decodeEntities(a.replace(/<[^>]+>/g, "").replace(/^Answer:\s*/, ""))}`;
        return `Asked the user: ${decodeEntities(q)} — they ${answer}.`;
      }
    )
    .replace(/<details class="tool-result">[\s\S]*$/g, "")
    .replace(/<div class="ask-log">[\s\S]*$/g, "")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}

export function toHistory(m: ChatMsg): ChatMsg {
  const content = m.role === "assistant" ? stripHarnessMarkup(m.content) : m.content;
  const base: ChatMsg = m.images?.length
    ? { role: m.role, content, images: [...m.images] }
    : { role: m.role, content };
  if (m.tool_calls?.length) base.tool_calls = m.tool_calls;
  if (m.tool_name) base.tool_name = m.tool_name;
  return base;
}

export function compactedHistory(messages: ChatMsg[]): ChatMsg[] {
  const from = latestContextStartIndex(messages);
  return messages.slice(from).map(toHistory);
}

/** Strip Rust's Windows verbatim prefix (`\\?\C:\...`, `\\?\UNC\...`) so
 *  older stored sessions display cleanly. */
export function displayWorkspace(p: string): string {
  if (p.startsWith("\\\\?\\UNC\\")) return "\\" + p.slice("\\\\?\\UNC\\".length);
  if (p.startsWith("\\\\?\\")) return p.slice("\\\\?\\".length);
  return p;
}

/** Final path segment on any platform (`/` and `\` separators). */
export function workspaceBase(p: string): string {
  return displayWorkspace(p).trim().split(/[/\\]/).filter(Boolean).pop() ?? "";
}

/** Excerpt centered on the first content match (or the opening message). */
export function searchSnippet(c: Chat, q: string): string {
  const firstUser = c.messages.find((m) => m.role === "user")?.content
    ?? c.messages.find((m) => !m.contextCleared)?.content ?? "";
  if (!q) return firstUser.slice(0, 140);
  const low = q.toLowerCase();
  const hit = c.messages.find((m) => !m.contextCleared && m.content.toLowerCase().includes(low));
  const src = hit?.content ?? firstUser;
  const idx = src.toLowerCase().indexOf(low);
  if (idx < 0) return src.slice(0, 140);
  const start = Math.max(0, idx - 48);
  const end = Math.min(src.length, idx + q.length + 96);
  return (start > 0 ? "…" : "") + src.slice(start, end) + (end < src.length ? "…" : "");
}

export function highlightHtml(text: string, q: string): string {
  const esc = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
  const base = esc(text);
  const needle = q.trim();
  if (!needle) return base;
  const pattern = esc(needle).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  try {
    return base.replace(new RegExp(`(${pattern})`, "gi"), '<mark class="search-mark">$1</mark>');
  } catch {
    return base;
  }
}
