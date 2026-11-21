// Pure rules for the per-session Telegram bridge connection status indicators
// (node-tested in node-tests/telegram.test.ts).

export interface TelegramStatusSnapshot {
  sessionId?: string;
  configured?: boolean;
  enabled?: boolean;
  polling?: boolean;
  paired?: unknown;
  botUsername?: string;
  lastError?: string;
}

export type TelegramConnState = "connected" | "disconnected" | "unconfigured";

/** Classify a session's Telegram status into the three dot states:
 *  - `"unconfigured"` (no color): no bot configured or status unavailable
 *  - `"connected"` (green): configured, enabled, polling, and paired
 *  - `"disconnected"` (red): configured, but disabled, not polling, or unpaired */
export function telegramConnState(s: TelegramStatusSnapshot | null | undefined): TelegramConnState {
  if (!s?.configured) return "unconfigured";
  return s.enabled && s.polling && !!s.paired ? "connected" : "disconnected";
}

export function telegramConnLabel(state: TelegramConnState): string {
  switch (state) {
    case "connected":
      return "Telegram connected";
    case "disconnected":
      return "Telegram disconnected";
    case "unconfigured":
      return "Telegram not set up";
  }
}

/** Rich hover tooltip for a session's Telegram badge/menu item. */
export function telegramConnTitle(s: TelegramStatusSnapshot | null | undefined): string {
  const state = telegramConnState(s);
  if (state === "unconfigured") return "Set up Telegram bridge for this session";
  const prefix = s?.botUsername ? `@${s.botUsername} · ` : "";
  if (state === "connected") return `${prefix}Telegram connected`;
  if (!s?.enabled) return `${prefix}Telegram paused`;
  if (!s?.paired) return `${prefix}Telegram waiting to pair`;
  if (s?.lastError) return `${prefix}Telegram error: ${s.lastError}`;
  return `${prefix}Telegram disconnected`;
}
