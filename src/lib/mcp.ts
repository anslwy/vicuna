// Vicuna MCP client (display-layer adapter, like ollama.ts).
//
// Work mode only. Server configs live in ~/vicuna/config/vicuna.json
// (opencode.json shape: `mcp` maps name → {command, type local/remote, url,
// env, enabled}) — the file is the source of truth and the UI edits it via
// the backend. In Tauri the Rust backend owns connections (stdio spawning +
// HTTP); in a plain browser only remote (http) servers can connect, via
// direct fetch.

import { isTauriRuntime } from "./ollama";

export interface McpOAuthCreds {
  client_id: string;
  client_secret?: string;
}

export interface McpServerConfig {
  id: string;
  name: string;
  /** "stdio" spawns `command`; "http" POSTs JSON-RPC to `url` */
  transport: "stdio" | "http";
  command?: string[];
  env?: Record<string, string>;
  url?: string;
  headers?: Record<string, string>;
  /** static OAuth client for servers without open registration
   *  (e.g. your own GitHub OAuth App) — skips dynamic registration */
  oauth?: McpOAuthCreds;
  enabled: boolean;
}

export interface McpTool {
  name: string;
  description?: string;
  inputSchema?: unknown;
}

export interface McpServerInfo {
  id: string;
  name: string;
  connected: boolean;
  tools: McpTool[];
}

export interface McpCallResult {
  content: string;
  is_error: boolean;
}

type TauriCore = typeof import("@tauri-apps/api/core");

async function tauri(): Promise<TauriCore> {
  return await import("@tauri-apps/api/core");
}

/** Human summary for the server row ("lb mcp" / "https://…/mcp"). */
export function describeServer(c: McpServerConfig): string {
  if (c.transport === "http") return c.url ?? "";
  return (c.command ?? []).join(" ");
}

/** Returns an error string, or null when the config is usable. */
export function validateServer(d: {
  name: string;
  transport: "stdio" | "http";
  command: string;
  args: string;
  envText: string;
  url: string;
  headersText: string;
}): string | null {
  if (!d.name.trim()) return "Give the server a name.";
  if (d.transport === "stdio") {
    if (!d.command.trim()) return "Local servers need a command (e.g. `lb`).";
  } else {
    const u = d.url.trim();
    if (!u.startsWith("http://") && !u.startsWith("https://")) {
      return "Remote servers need an http(s) URL.";
    }
  }
  for (const [label, raw] of [["Env", d.envText], ["Headers", d.headersText]] as const) {
    if (raw.trim()) {
      try {
        const v = JSON.parse(raw);
        if (typeof v !== "object" || v === null || Array.isArray(v)) {
          return `${label} must be a JSON object.`;
        }
      } catch {
        return `${label} is not valid JSON.`;
      }
    }
  }
  return null;
}

export function parseStringMap(raw: string): Record<string, string> | undefined {
  const t = raw.trim();
  if (!t) return undefined;
  const v = JSON.parse(t) as Record<string, unknown>;
  const out: Record<string, string> = {};
  for (const [k, v2] of Object.entries(v)) out[k] = String(v2);
  return out;
}

/** All servers from ~/vicuna/config/vicuna.json (desktop only). */
export async function listMcpServers(): Promise<McpServerConfig[]> {
  if (!isTauriRuntime()) return [];
  const { invoke } = await tauri();
  const list = await invoke<McpServerConfig[]>("mcp_list_servers");
  return Array.isArray(list) ? list : [];
}

export async function saveMcpServer(c: McpServerConfig): Promise<McpServerConfig[]> {
  if (!isTauriRuntime()) throw new Error("Managing MCP servers needs the Vicuna desktop app.");
  const { invoke } = await tauri();
  return await invoke<McpServerConfig[]>("mcp_save_server", {
    config: {
      id: c.name.trim(),
      name: c.name.trim(),
      transport: c.transport,
      command: c.command ?? null,
      env: c.env ?? null,
      url: c.url ?? null,
      headers: c.headers ?? null,
      oauth: c.oauth ?? null,
      enabled: c.enabled
    }
  });
}

export async function deleteMcpServer(id: string): Promise<McpServerConfig[]> {
  if (!isTauriRuntime()) throw new Error("Managing MCP servers needs the Vicuna desktop app.");
  const { invoke } = await tauri();
  return await invoke<McpServerConfig[]>("mcp_delete_server", { id });
}

/** True when a backend error means "this server needs browser sign-in"
 *  (401/403 from the MCP endpoint). Shared by the popup row label and the
 *  connect-vs-sign-in routing so both always agree. */
export function isAuthError(msg: string | null | undefined): boolean {
  return !!msg && /401|403|unauthorized|sign.?in|forbidden/i.test(msg);
}

/** Browser sign-in for an OAuth-protected remote server. Opens the system
 *  browser; resolves with the connected server info once login completes
 *  (rejects on timeout/denial, or when the server needs no sign-in). */
export async function signInServer(c: McpServerConfig): Promise<McpServerInfo> {
  if (!isTauriRuntime()) throw new Error("Browser sign-in needs the Vicuna desktop app.");
  const { invoke } = await tauri();
  return await invoke<McpServerInfo>("mcp_signin", {
    config: {
      id: c.id,
      name: c.name,
      transport: c.transport,
      command: c.command ?? null,
      env: c.env ?? null,
      url: c.url ?? null,
      headers: c.headers ?? null,
      oauth: c.oauth ?? null,
      enabled: c.enabled
    }
  });
}

/** Forget the stored OAuth login and drop the connection, if any. */
export async function signOutServer(id: string): Promise<void> {
  if (!isTauriRuntime()) return;
  const { invoke } = await tauri();
  await invoke("mcp_signout", { id });
}

/** Whether an OAuth login is stored for the server. */
export async function loginPresent(id: string): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  const { invoke } = await tauri();
  return await invoke<boolean>("mcp_login_present", { id });
}

export async function connectServer(c: McpServerConfig): Promise<McpServerInfo> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    return await invoke<McpServerInfo>("mcp_connect", {
      config: {
        id: c.id,
        name: c.name,
        transport: c.transport,
        command: c.command ?? null,
        env: c.env ?? null,
        url: c.url ?? null,
        headers: c.headers ?? null,
      oauth: c.oauth ?? null,
        enabled: c.enabled
      }
    });
  }
  // plain browser: no process spawning — only remote servers can connect
  if (c.transport !== "http" || !c.url) {
    throw new Error(`"${c.name}" is a local command server: connect it from the Vicuna desktop app.`);
  }
  const tools = await remoteRequest<McpTool[]>(c.url, c.headers ?? {}, "tools/list", {});
  return { id: c.id, name: c.name, connected: true, tools };
}

export async function disconnectServer(id: string, _transport: "stdio" | "http"): Promise<void> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    await invoke("mcp_disconnect", { id });
    return;
  }
  // nothing held browser-side
}

export async function listServerTools(c: McpServerConfig): Promise<McpTool[]> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    return await invoke<McpTool[]>("mcp_list_tools", { id: c.id });
  }
  if (c.transport !== "http" || !c.url) throw new Error("not connected");
  return await remoteRequest<McpTool[]>(c.url, c.headers ?? {}, "tools/list", {});
}

export async function callServerTool(c: McpServerConfig, tool: string, args: unknown): Promise<string> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    const res = await invoke<McpCallResult>("mcp_call_tool", {
      serverId: c.id,
      tool,
      args: (args ?? {}) as Record<string, unknown>
    });
    return res.is_error ? `Tool error: ${res.content}` : res.content;
  }
  if (c.transport !== "http" || !c.url) {
    throw new Error(`"${c.name}" is a local command server: use the Vicuna desktop app.`);
  }
  const res = await remoteRequest<{ content: string; is_error: boolean }>(c.url, c.headers ?? {}, "tools/call", {
    name: tool,
    arguments: args ?? {}
  });
  return res.is_error ? `Tool error: ${res.content}` : res.content;
}

let remoteId = 1;

/** Minimal streamable-HTTP JSON-RPC for the plain-browser fallback. */
async function remoteRequest<T>(url: string, headers: Record<string, string>, method: string, params: unknown): Promise<T> {
  // handshake first so a bare URL fails fast with a clear error
  if (method !== "initialize") {
    await remoteRequest(url, headers, "initialize", {
      protocolVersion: "2024-11-05",
      capabilities: {},
      clientInfo: { name: "vicuna", version: "0.1.0" }
    });
  }
  const res = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json", Accept: "application/json, text/event-stream", ...headers },
    body: JSON.stringify({ jsonrpc: "2.0", id: remoteId++, method, params })
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`MCP ${method} failed: ${res.status} ${txt.slice(0, 200)}`);
  }
  const ct = res.headers.get("content-type") ?? "";
  const body = await res.text();
  let payload = body.trim();
  if (ct.includes("text/event-stream")) {
    payload = "";
    for (const line of body.split("\n")) {
      const t = line.trim();
      if (t.startsWith("data:")) {
        const data = t.slice(5).trim();
        if (data && data !== "[DONE]") payload = data;
      }
    }
    if (!payload) throw new Error("MCP server sent an empty event stream");
  }
  const msg = JSON.parse(payload);
  if (msg.error) throw new Error(`MCP ${method} failed: ${JSON.stringify(msg.error).slice(0, 200)}`);
  const result = msg.result;
  if (method === "tools/list") {
    if (!Array.isArray(result?.tools)) throw new Error("MCP server returned no tools array");
    return result.tools as T;
  }
  if (method === "tools/call") {
    const blocks = Array.isArray(result?.content) ? result.content : [];
    const parts: string[] = [];
    for (const b of blocks) {
      if (b?.type === "text") parts.push(String(b.text ?? ""));
      else if (b?.type === "image" || b?.type === "audio") parts.push(`[${b.type} content omitted]`);
      else parts.push(JSON.stringify(b));
    }
    return { content: parts.join("\n"), is_error: result?.isError === true } as T;
  }
  return result as T;
}

/** Tool names must survive the model round-trip: keep them readable, make
 *  them unique across servers by prefixing only on collision. */
export function sanitizeToolName(name: string): string {
  const clean = name.replace(/[^a-zA-Z0-9_-]/g, "_").slice(0, 64);
  return clean || "tool";
}
