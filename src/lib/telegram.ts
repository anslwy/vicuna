// Pure rules for the Telegram bridge connection status indicator in the lead
// session menu (node-tested in node-tests/telegram.test.ts).

export interface TelegramStatusSnapshot {
  configured?: boolean;
  enabled?: boolean;
  polling?: boolean;
  paired?: unknown;
}

export type TelegramConnState = "connected" | "disconnected" | "unconfigured";

/** Classify `telegram_status` into the three menu dot states:
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
