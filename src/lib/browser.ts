// Vicuna Browser tab: `lb` (live-browser) sidecar daemon adapter.
//
// The daemon (Chrome via CDP + JSON API + Svelte UI) is the shared engine:
// the side-panel tab iframes its web UI while the Work agent drives the
// same tabs through `lb mcp`. Desktop resolves the bundled sidecar first,
// PATH fallback; plain browsers go through the loopback bridge (PATH only)
// or iframe an already-running daemon directly.

import { invokeOrBridge } from "./bridge";
import type { McpServerConfig } from "./mcp";

export const LB_PORT = 18765;
export const LB_URL = `http://127.0.0.1:${LB_PORT}`;

/** Synthetic MCP server id for the shared browser (never in vicuna.json). */
export const BROWSER_MCP_ID = "browser";

export interface LbHealth {
  browser_alive: boolean;
  probe: string;
  tabs: number;
  /** false when the daemon answers but its UI files 404 (reinstall under
   *  a live daemon). Optional: older backends omit it — treat missing as ok. */
  ui_ok?: boolean;
}

export interface BrowserStatus {
  running: boolean;
  owned: boolean;
  engine_stale: boolean;
  url: string;
  bin?: string | null;
  source?: string | null;
  lb_version?: string | null;
  health?: LbHealth | null;
  warning?: string | null;
}

export async function browserStatus(): Promise<BrowserStatus> {
  return await invokeOrBridge<BrowserStatus>("browser_status", {});
}

export async function browserStart(): Promise<BrowserStatus> {
  return await invokeOrBridge<BrowserStatus>("browser_start", {});
}

export async function browserStop(): Promise<BrowserStatus> {
  return await invokeOrBridge<BrowserStatus>("browser_stop", {});
}

export async function browserRestart(): Promise<BrowserStatus> {
  return await invokeOrBridge<BrowserStatus>("browser_restart", {});
}

export interface EngineInfo {
  path: string;
  downloaded: boolean;
}

export async function browserEnsureEngine(): Promise<EngineInfo> {
  return await invokeOrBridge<EngineInfo>("browser_ensure_engine", {});
}

export async function browserTakeover(): Promise<BrowserStatus> {
  return await invokeOrBridge<BrowserStatus>("browser_takeover", {});
}

/** Absolute `lb mcp` argv for the default `browser` MCP entry. */
export async function browserMcpCommand(): Promise<string[]> {
  return await invokeOrBridge<string[]>("browser_mcp_command", {});
}

/** True when the id is the synthetic built-in browser row. */
export function isBrowserMcpId(id: string): boolean {
  return id === BROWSER_MCP_ID;
}

/** Build the synthetic `browser` MCP config (in-memory only: connect takes
 *  any config, so no vicuna.json entry is needed and the row can never be
 *  deleted — it is available whenever the `lb` binary resolves). */
export function browserMcpConfig(mcpArgv: string[], serverUrl: string): McpServerConfig {
  return {
    id: BROWSER_MCP_ID,
    name: "browser",
    transport: "stdio",
    command: mcpArgv,
    env: { LB_SERVER: serverUrl },
    url: undefined,
    headers: undefined,
    enabled: true
  };
}
