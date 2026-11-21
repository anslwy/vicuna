// Pure helpers for plugin-provided messaging bridges (Discord, etc.).
// Node-tested in node-tests/bridgePlugin.test.ts.

export interface PluginSetupStep {
  text: string;
  linkText?: string | null;
  linkUrl?: string | null;
}

export interface PluginBridgeDef {
  name?: string;
  color?: string | null;
  iconPath?: string | null;
  tokenPlaceholder?: string;
  setupSteps?: PluginSetupStep[];
  pairCommand?: string;
  supportsVoice?: boolean;
}

export interface BridgeStatusSnapshot {
  sessionId?: string;
  configured?: boolean;
  enabled?: boolean;
  connected?: boolean;
  /** Alias for `connected` (mirrors Telegram's `polling` field). */
  polling?: boolean;
  paired?: { id: string | number; name: string } | null;
  botUsername?: string;
  botUrl?: string;
  inviteUrl?: string;
  tokenTail?: string;
  pairCode?: string;
  lastError?: string;
}

export type BridgeConnState = "connected" | "disconnected" | "unconfigured";

export const DEFAULT_BRIDGE_ICON =
  "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z";

/** Defense-in-depth SVG `<path d="...">` check (matches server validation). */
export function safeIconPath(p: unknown): string {
  if (typeof p !== "string") return DEFAULT_BRIDGE_ICON;
  const s = p.trim();
  if (!s || s.length > 2048 || !/^[0-9a-zA-Z .,-]+$/.test(s)) return DEFAULT_BRIDGE_ICON;
  return s;
}

export function bridgeDisplayName(plugin: { name: string; bridge?: PluginBridgeDef | null }): string {
  const n = plugin.bridge?.name?.trim();
  return n || plugin.name;
}

export function bridgeConnState(s: BridgeStatusSnapshot | null | undefined): BridgeConnState {
  if (!s?.configured) return "unconfigured";
  const online = Boolean(s.connected ?? s.polling);
  return s.enabled && online && !!s.paired ? "connected" : "disconnected";
}

export function bridgeConnLabel(name: string, state: BridgeConnState): string {
  switch (state) {
    case "connected":
      return `${name} connected`;
    case "disconnected":
      return `${name} disconnected`;
    case "unconfigured":
      return `${name} not set up`;
  }
}

export function bridgeConnTitle(name: string, s: BridgeStatusSnapshot | null | undefined): string {
  const state = bridgeConnState(s);
  if (state === "unconfigured") return `Set up ${name} bridge for this session`;
  const prefix = s?.botUsername ? `@${s.botUsername} · ` : "";
  if (state === "connected") return `${prefix}${name} connected`;
  if (!s?.enabled) return `${prefix}${name} paused`;
  if (!s?.paired) return `${prefix}${name} waiting to pair`;
  if (s?.lastError) return `${prefix}${name} error: ${s.lastError}`;
  return `${prefix}${name} disconnected`;
}
