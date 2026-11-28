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
