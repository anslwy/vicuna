/** Follow-ups typed while a Work turn runs. The server-side setting
 *  (`WorkEngineInfo.sendNow`, default on) decides whether they interrupt
 *  the running step and go now, or queue until the agent finishes. */

/** Missing (older server) = on, the default. */
export function followUpSendsNow(info: { sendNow?: boolean } | null | undefined): boolean {
  return info?.sendNow ?? true;
}

/** `/send_now` (optionally followed by a message) typed into the composer
 *  while a turn runs. Returns the extra text, or null when it isn't the
 *  command. */
export function parseSendNow(text: string): { rest: string } | null {
  const m = /^\/send_now(?:@\S+)?(?:\s+([\s\S]*))?$/.exec(text.trim());
  return m ? { rest: (m[1] ?? "").trim() } : null;
}

export function runningPlaceholder(sendNow: boolean): string {
  return sendNow
    ? "Send a follow-up — it interrupts the agent and goes now…"
    : "Queue a follow-up — it joins when the agent finishes (/send_now sends it now)…";
}

export function queuedHint(sendNow: boolean): string {
  return sendNow ? "sending now · interrupting the agent…" : "queued · joins when the agent finishes · ↑ sends now";
}
