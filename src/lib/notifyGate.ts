// Pure "should we notify?" helpers for turn-finished OS notifications (no
// imports, so `npm test` can load them with Node's built-in test runner).
// The Tauri side lives in ./notify.

export type TurnEnd = "done" | "error" | "stopped";
export type TurnMode = "chat" | "work";

export interface NotifyContext {
  /** Settings toggle (on by default). */
  enabled: boolean;
  /** The app window has OS focus. */
  focused: boolean;
  /** The window is shown and not minimised. */
  visible: boolean;
  /** The user pressed Stop for this turn — never notify for that. */
  userStopped: boolean;
}

/** Notify only when the turn ended on its own (done, error, agent stop) and
 *  the user can't see the window (unfocused, hidden or minimised). */
export function shouldNotify(ctx: NotifyContext): boolean {
  if (!ctx.enabled || ctx.userStopped) return false;
  return !ctx.focused || !ctx.visible;
}

/** Notification title: the session title, or "Vicuna" when it has none. */
export function notifyTitle(title: string | null | undefined): string {
  const t = (title ?? "").trim();
  return t || "Vicuna";
}

/** Short status plus up to ~80 chars of the reply. */
export function notifyBody(mode: TurnMode, end: TurnEnd, reply?: string): string {
  const status = end === "error" ? "Error" : mode === "work" ? "Work finished" : "Reply ready";
  const snippet = (reply ?? "").replace(/\s+/g, " ").trim();
  if (!snippet || end === "error") return status;
  const cut = snippet.length > 80 ? snippet.slice(0, 79).trimEnd() + "…" : snippet;
  return `${status}: ${cut}`;
}

export const NOTIFY_PREF_KEY = "vicuna-notify-turns";
/** Chime alongside the notification (on by default, same "0" = off rule). */
export const NOTIFY_SOUND_PREF_KEY = "vicuna-notify-sound";
/** Which chime preset (see CHIME_OPTIONS in ./chime); missing = default. */
export const NOTIFY_SOUND_CHOICE_KEY = "vicuna-notify-sound-choice";

/** Notification body for an agent question (ask card): the question, clipped. */
export function askBody(question: string): string {
  const q = (question ?? "").replace(/\s+/g, " ").trim();
  if (!q) return "The agent is waiting for your answer.";
  return q.length > 80 ? q.slice(0, 79).trimEnd() + "…" : q;
}

/** Stored value → enabled (anything but "0" counts as on). */
export function parseNotifyPref(raw: string | null): boolean {
  return raw !== "0";
}
