// Pure helpers for plugin-provided messaging bridges (Discord, etc.).
// Node-tested in node-tests/bridgePlugin.test.ts.

export interface PluginSetupStep {
  text: string;
  linkText?: string | null;
  linkUrl?: string | null;
}

export type BridgeAuthMode = "token" | "qr";

export interface PluginBridgeDef {
  name?: string;
  color?: string | null;
  iconPath?: string | null;
  tokenPlaceholder?: string;
  authMode?: BridgeAuthMode;
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
  /** Optional base64 PNG/SVG data URL for linked-device QR pairing (`authMode: "qr"`). */
  qrDataUrl?: string;
  /** Optional custom instruction shown above the pair code box. */
  pairingHint?: string;
  /**
   * Optional plugin-driven input step (e.g. phone number, then SMS code). While present the
   * modal shows this field instead of its normal view and submits it via `bridge_save {token}`.
   */
  inputPrompt?: BridgeInputPrompt | null;
  lastError?: string;
}

export interface BridgeInputPrompt {
  label: string;
  placeholder: string;
  secret: boolean;
  submitLabel: string;
}

function cleanText(v: unknown, max: number): string {
  if (typeof v !== "string") return "";
  const s = v.replace(/[\u0000-\u001f\u007f]+/g, " ").trim();
  return s.length > max ? s.slice(0, max) : s;
}

/** Normalize a plugin-supplied input prompt; null when absent or empty. */
export function safeInputPrompt(p: unknown): BridgeInputPrompt | null {
  if (!p || typeof p !== "object") return null;
  const o = p as Record<string, unknown>;
  const label = cleanText(o.label, 300);
  const placeholder = cleanText(o.placeholder, 120);
  if (!label && !placeholder) return null;
  return { label, placeholder, secret: o.secret === true, submitLabel: cleanText(o.submitLabel, 30) || "Submit" };
}

export type BridgeConnState = "connected" | "disconnected" | "unconfigured";

export const DEFAULT_BRIDGE_ICON =
  "M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z";

export function bridgeAuthMode(bridge: PluginBridgeDef | null | undefined): BridgeAuthMode {
  return bridge?.authMode === "qr" ? "qr" : "token";
}

const MAX_QR_DATA_URL_LEN = 256 * 1024;
const SAFE_QR_PREFIXES = ["data:image/png;base64,", "data:image/svg+xml;base64,"] as const;

/** Validate a plugin-supplied QR data URL before rendering it in an `<img>`. */
export function safeQrDataUrl(u: unknown): string | null {
  if (typeof u !== "string") return null;
  const s = u.trim();
  if (!s || s.length > MAX_QR_DATA_URL_LEN) return null;
  const prefix = SAFE_QR_PREFIXES.find((p) => s.startsWith(p));
  if (!prefix) return null;
  const body = s.slice(prefix.length);
  if (!body || !/^[A-Za-z0-9+/=\s]+$/.test(body)) return null;
  return s;
}

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
