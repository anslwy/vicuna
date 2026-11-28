import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { isPluginMcpServer, mcpConsentPayload, mcpLaunchLine, pendingMcpConsent, type PluginMcpServer } from "../src/lib/pluginMcp.ts";
import { KIND_LABEL, filterCatalog, type CatalogEntry } from "../src/lib/pluginCatalog.ts";

const root = new URL("..", import.meta.url).pathname;
const read = (p: string) => readFileSync(root + p, "utf8");

const http: PluginMcpServer = { id: "web", serverId: "demo-web", label: "Web", type: "http", url: "https://mcp.example.com/mcp", args: [], env: {}, enabled: false, installed: true };
const stdio: PluginMcpServer = { id: "files", serverId: "demo-files", label: "Files", type: "stdio", command: "files-mcp", args: ["--root", "my dir"], env: { FILES_MODE: "ro" }, enabled: false, installed: false };

test("launch line shows exactly what runs", () => {
  assert.equal(mcpLaunchLine(http), "https://mcp.example.com/mcp");
  assert.equal(mcpLaunchLine(stdio), 'FILES_MODE=ro files-mcp --root "my dir"');
});

test("consent payload echoes the launch per transport", () => {
  assert.deepEqual(mcpConsentPayload("demo", [http, stdio]), {
    id: "demo",
    servers: [
      { id: "web", type: "http", url: "https://mcp.example.com/mcp" },
      { id: "files", type: "stdio", command: "files-mcp", args: ["--root", "my dir"], env: { FILES_MODE: "ro" } },
    ],
  });
  assert.deepEqual(pendingMcpConsent({ pendingMcpConsent: [http] }), [http]);
  assert.deepEqual(pendingMcpConsent(null), []);
});

test("plugin rows are recognised and read-only in the MCP popup", () => {
  assert.ok(isPluginMcpServer({ pluginId: "demo" }));
  assert.ok(!isPluginMcpServer({}) && !isPluginMcpServer(null));
  const popup = read("src/lib/McpPopup.svelte");
  assert.match(popup, /\{#if !isBrowserMcpId\(s\.id\) && !isPluginMcpServer\(s\)\}\s*<span class="mcp-row-actions">/);
  assert.match(popup, /data-testid="mcp-plugin-badge"/);
});

test("catalog knows the mcp kind", () => {
  assert.equal(KIND_LABEL.mcp, "MCP");
  const e: CatalogEntry = { id: "x", name: "X", description: "d", author: "a", source: "a/x", kinds: ["mcp"], tags: [], installed: null, conflict: false, compatible: true };
  assert.equal(filterCatalog([e], "mcp").length, 1);
});

test("Plugins panel asks consent and revokes through API actions", () => {
  const panel = read("src/lib/PluginsPanel.svelte");
  for (const c of ["plugin_mcp_consent", "plugin_mcp_revoke"]) {
    assert.ok(panel.includes(`"${c}"`), c);
    assert.ok(read("src-tauri/src/server/mod.rs").includes(`    "${c}",`), `${c} in API_ACTIONS`);
  }
  assert.match(panel, /data-testid="mcp-consent"/);
});
