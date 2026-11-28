// Plugin MCP servers (plugin_mcp.rs): pure helpers for the Plugins panel,
// its consent dialog and the MCP popup's plugin badge.

export type PluginMcpServer = {
  id: string;
  /** MCP server id everywhere else: `<plugin-id>-<server-id>` */
  serverId: string;
  label: string;
  description?: string;
  type: "http" | "stdio";
  url?: string | null;
  command?: string | null;
  args: string[];
  env: Record<string, string>;
  installHint?: string | null;
  enabled: boolean;
  installed: boolean;
  /** Declared secrets with masked status (values never reach the window). */
  secretStatus?: PluginMcpSecretStatus[];
};

export type PluginMcpSecretStatus = {
  key: string;
  label: string;
  description?: string | null;
  helpUrl?: string | null;
  placeholder?: string | null;
  required: boolean;
  configured: boolean;
  /** last 4 chars when configured (empty for short values), else null */
  tail: string | null;
};

/** The exact launch the dialog shows: the URL, or argv (+ env) quoted like a shell line. */
export function mcpLaunchLine(s: Pick<PluginMcpServer, "type" | "url" | "command" | "args" | "env">): string {
  if (s.type === "http") return s.url ?? "";
  const q = (a: string) => (/\s/.test(a) || a === "" ? JSON.stringify(a) : a);
  const env = Object.entries(s.env ?? {}).map(([k, v]) => `${k}=${q(v)}`);
  return [...env, s.command ?? "", ...(s.args ?? [])].map((a, i) => (i < env.length ? a : q(a))).join(" ");
}

/** `plugin_mcp_consent` args: echo exactly what was shown (the server refuses any drift). */
export function mcpConsentPayload(pluginId: string, servers: PluginMcpServer[]) {
  return {
    id: pluginId,
    servers: servers.map((s) =>
      s.type === "http"
        ? { id: s.id, type: s.type, url: s.url ?? "" }
        : { id: s.id, type: s.type, command: s.command ?? "", args: [...(s.args ?? [])], env: { ...(s.env ?? {}) } }
    ),
  };
}

export function pendingMcpConsent(p: { pendingMcpConsent?: PluginMcpServer[] } | null | undefined): PluginMcpServer[] {
  return p?.pendingMcpConsent ?? [];
}

/** Plugin-provided rows in the MCP popup are read-only (no Edit/Delete). */
export function isPluginMcpServer(c: { pluginId?: string | null } | null | undefined): boolean {
  return !!c?.pluginId;
}

/** Configure-button state: none (no secrets), missing (a required key unset), partial, ok. */
export function secretsState(s: Pick<PluginMcpServer, "secretStatus"> | null | undefined): "none" | "missing" | "partial" | "ok" {
  const list = s?.secretStatus ?? [];
  if (!list.length) return "none";
  if (list.some((x) => x.required && !x.configured)) return "missing";
  return list.every((x) => x.configured) ? "ok" : "partial";
}

/** `••••WXYZ` for a configured key (dots only when the value is short). */
export function maskedSecret(x: Pick<PluginMcpSecretStatus, "configured" | "tail">): string {
  return x.configured ? `••••${x.tail ?? ""}` : "";
}

/** `plugin_mcp_set_secrets` args: only typed keys; `""` removes (`clear`). */
export function secretsPayload(pluginId: string, serverId: string, typed: Record<string, string>, clear: string[] = []) {
  const secrets: Record<string, string> = {};
  for (const [k, v] of Object.entries(typed)) if (v.trim()) secrets[k] = v.trim();
  for (const k of clear) secrets[k] = "";
  return { id: pluginId, server: serverId, secrets };
}

/** Only https help links are opened. */
export function safeHelpUrl(u: string | null | undefined): string | null {
  if (!u) return null;
  try {
    return new URL(u).protocol === "https:" ? u : null;
  } catch {
    return null;
  }
}

/** The plugin server behind an MCP-dialog row (`<plugin>-<server>` id). */
export function findPluginServer(
  plugins: { id: string; mcpServers?: PluginMcpServer[] }[],
  row: { id: string; pluginId?: string | null }
): { pluginId: string; server: PluginMcpServer } | null {
  const p = plugins.find((x) => x.id === row.pluginId);
  const server = p?.mcpServers?.find((m) => m.serverId === row.id);
  return p && server ? { pluginId: p.id, server } : null;
}
