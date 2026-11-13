/**
 * Scheduled sends (Work mode): pure rules, no DOM and no app state.
 *
 * A scheduled message waits in the frontend and is sent by a timer in the
 * running app, so it only goes out while Vicuna is open. One whose time
 * passed while the app was closed is marked `missed` at launch and waits for
 * the user (Send now / Reschedule / Delete) instead of firing late.
 */

export type ScheduledItem = {
  id: string;
  /** target session; null or a since-removed id means "start a new session" */
  chatId: string | null;
  /** session title at schedule time (shown in lists, also after a delete) */
  title: string;
  /** canonical workspace root the turn runs in */
  workspace: string;
  text: string;
  images?: string[];
  /** when to send (epoch ms) */
  at: number;
  createdAt: number;
  /** the time passed while Vicuna was closed, or the send failed */
  missed?: boolean;
  /** why the last send attempt failed */
  error?: string;
};

export const SCHEDULE_KEY = "vicuna-scheduled";
/** shown wherever scheduling is offered or listed */
export const SCHEDULE_OPEN_NOTE = "Scheduled messages are only sent while Vicuna is open.";

export type SchedulePreset = { id: string; label: string; at: number };

const MIN = 60_000;

/** The quick picks of the schedule menu. */
export function schedulePresets(now: number): SchedulePreset[] {
  return [
    { id: "30m", label: "In 30 minutes", at: now + 30 * MIN },
    { id: "1h", label: "In 1 hour", at: now + 60 * MIN },
    { id: "3h", label: "In 3 hours", at: now + 180 * MIN }
  ];
}

function pad(n: number): string {
  return String(n).padStart(2, "0");
}

/** `2026-11-12` in local time (value of a date input) */
export function dateInputValue(ms: number): string {
  const d = new Date(ms);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

/** `21:30` in local time (value of a time input) */
export function timeInputValue(ms: number): string {
  const d = new Date(ms);
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** Default for the custom dialog: one hour ahead, rounded up to 5 minutes. */
export function defaultCustomTime(now: number): number {
  const step = 5 * MIN;
  return Math.ceil((now + 60 * MIN) / step) * step;
}

/** Local date + time input values → epoch ms, or null when unparseable. */
export function parseCustomTime(date: string, time: string): number | null {
  const d = /^(\d{4})-(\d{2})-(\d{2})$/.exec(date.trim());
  const t = /^(\d{1,2}):(\d{2})/.exec(time.trim());
  if (!d || !t) return null;
  const [y, mo, da] = [Number(d[1]), Number(d[2]), Number(d[3])];
  const [h, mi] = [Number(t[1]), Number(t[2])];
  if (mo < 1 || mo > 12 || da < 1 || da > 31 || h > 23 || mi > 59) return null;
  const out = new Date(y, mo - 1, da, h, mi, 0, 0);
  // reject rollovers like Feb 31 → Mar 3
  if (out.getMonth() !== mo - 1 || out.getDate() !== da) return null;
  const ms = out.getTime();
  return Number.isFinite(ms) ? ms : null;
}

/** Why a custom time can't be used, or null when it is fine. */
export function customTimeError(at: number | null, now: number): string | null {
  if (at == null) return "Pick a date and a time.";
  if (at <= now) return "Pick a time in the future.";
  return null;
}

function startOfDay(ms: number): number {
  const d = new Date(ms);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

/** `9:30 PM` */
export function formatClock(ms: number): string {
  const d = new Date(ms);
  const h = d.getHours();
  return `${h % 12 === 0 ? 12 : h % 12}:${pad(d.getMinutes())} ${h < 12 ? "AM" : "PM"}`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** `Today 9:30 PM` / `Tomorrow 9:00 AM` / `Yesterday 6:00 PM` / `Nov 14, 9:00 AM` */
export function formatWhen(at: number, now: number): string {
  // calendar-day difference (rounded: a DST day is 23 or 25 hours long)
  const days = Math.round((startOfDay(at) - startOfDay(now)) / 86_400_000);
  const clock = formatClock(at);
  if (days === 0) return `Today ${clock}`;
  if (days === 1) return `Tomorrow ${clock}`;
  if (days === -1) return `Yesterday ${clock}`;
  const d = new Date(at);
  const year = d.getFullYear() === new Date(now).getFullYear() ? "" : ` ${d.getFullYear()}`;
  return `${MONTHS[d.getMonth()]} ${d.getDate()}${year}, ${clock}`;
}

/** Soonest first; missed ones lead (they need a decision). */
export function sortScheduled(items: ScheduledItem[]): ScheduledItem[] {
  return [...items].sort((a, b) => Number(!!b.missed) - Number(!!a.missed) || a.at - b.at || a.createdAt - b.createdAt);
}

/** Items the timer should send now (missed ones wait for the user). */
export function dueItems(items: ScheduledItem[], now: number): ScheduledItem[] {
  return sortScheduled(items.filter((it) => !it.missed && it.at <= now));
}

/** Launch pass: anything already due was missed while the app was closed. */
export function markMissed(items: ScheduledItem[], now: number): ScheduledItem[] {
  return items.map((it) => (!it.missed && it.at <= now ? { ...it, missed: true } : it));
}

/** Read a stored list, dropping anything malformed. */
export function parseScheduled(raw: string | null | undefined): ScheduledItem[] {
  if (!raw) return [];
  let data: unknown;
  try {
    data = JSON.parse(raw);
  } catch {
    return [];
  }
  if (!Array.isArray(data)) return [];
  const out: ScheduledItem[] = [];
  for (const v of data) {
    if (!v || typeof v !== "object") continue;
    const o = v as Record<string, unknown>;
    if (typeof o.id !== "string" || !o.id) continue;
    if (typeof o.at !== "number" || !Number.isFinite(o.at)) continue;
    if (typeof o.workspace !== "string" || !o.workspace) continue;
    const text = typeof o.text === "string" ? o.text : "";
    const images = Array.isArray(o.images) ? o.images.filter((x): x is string => typeof x === "string") : [];
    if (!text && images.length === 0) continue;
    out.push({
      id: o.id,
      chatId: typeof o.chatId === "string" && o.chatId ? o.chatId : null,
      title: typeof o.title === "string" ? o.title : "",
      workspace: o.workspace,
      text,
      ...(images.length ? { images } : {}),
      at: o.at,
      createdAt: typeof o.createdAt === "number" ? o.createdAt : o.at,
      ...(o.missed === true ? { missed: true } : {}),
      ...(typeof o.error === "string" && o.error ? { error: o.error } : {})
    });
  }
  return out;
}

/** One-line preview of the message text. */
export function previewText(item: Pick<ScheduledItem, "text" | "images">, max = 60): string {
  const flat = item.text.replace(/\s+/g, " ").trim();
  if (!flat) return item.images?.length ? "Image" : "";
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat;
}

/** Tray menu rows: `Today 9:30 PM · fix the tests`, capped, with a "+N more" row. */
export function trayScheduleLines(items: ScheduledItem[], now: number, max = 5): string[] {
  const sorted = sortScheduled(items);
  const lines = sorted.slice(0, max).map((it) => {
    const when = it.missed ? `Missed ${formatWhen(it.at, now)}` : formatWhen(it.at, now);
    return `${when} · ${previewText(it, 36)}`;
  });
  if (sorted.length > max) lines.push(`+${sorted.length - max} more`);
  return lines;
}

/** Tray section header: `1 scheduled message` / `3 scheduled messages`. */
export function scheduledCountLabel(n: number): string {
  return `${n} scheduled message${n === 1 ? "" : "s"}`;
}
