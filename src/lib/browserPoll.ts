// Pure poll-step decision for the visible Browser tab. Zero runtime imports
// (type-only) so plain node can check it: node scripts/check-browser-poll.mjs

import type { BrowserStatus } from "./browser";

export type BrowserPollAction = "ensure" | "update" | "keep";

/**
 * Next step for a visible Browser tab after a status poll.
 * - transport error (`fresh` null): keep the frame, retry next tick —
 *   a dead bridge must never yank a live-looking tab into an error.
 * - fresh running: adopt the new state (picks up engine-stale flips too).
 * - fresh down after running: re-ensure once (revive an owned daemon
 *   that died under the tab; failure lands in error + Retry).
 * - fresh down while already down: keep (the visible-state ensure owns
 *   that drive — the poll must not double-drive it).
 */
export function browserPollAction(
  prev: BrowserStatus | null,
  fresh: BrowserStatus | null
): BrowserPollAction {
  if (!fresh) return "keep";
  if (fresh.running) return "update";
  return prev?.running ? "ensure" : "keep";
}

/**
 * Whether a poll tick revived the daemon and the frame must re-navigate.
 * State updates alone leave the iframe on its failed navigation (still
 * blank after the daemon is back), so a successful poll-driven ensure
 * bumps the frame key. lb keeps tab state server-side, so the reload is
 * cheap even when the daemon never actually died (status false-negative).
 */
export function browserPollRevived(action: BrowserPollAction, ensured: boolean): boolean {
  return action === "ensure" && ensured;
}

export type BrowserButtonAction = "start" | "restart" | "takeover";

/**
 * Tab-bar button behavior from last-known state (`lb start` / `lb web
 * restart` / confirmed `lb kill` + start). Down (or unknown) always
 * starts — a restart there could only refuse, which reads as "does
 * nothing". Adopted daemons need the confirmed takeover path: plain
 * restart refuses those by design.
 */
export function browserButtonAction(st: BrowserStatus | null): BrowserButtonAction {
  if (!st?.running) return "start";
  return st.owned ? "restart" : "takeover";
}
