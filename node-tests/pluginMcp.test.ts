import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { findPluginServer, isPluginMcpServer, maskedSecret, mcpConsentPayload, mcpLaunchLine, mcpStdioNote, pendingMcpConsent, safeHelpUrl, shortSha, secretsPayload, secretsState, type PluginMcpServer } from "../src/lib/pluginMcp.ts";
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

const sec = (key: string, configured: boolean, required = false, tail: string | null = configured ? "WXYZ" : null) =>
  ({ key, label: key, required, configured, tail });

test("secrets state drives the Keys indicator", () => {
  assert.equal(secretsState(stdio), "none");
  assert.equal(secretsState(null), "none");
  assert.equal(secretsState({ secretStatus: [sec("A_KEY", false, true)] }), "missing");
  assert.equal(secretsState({ secretStatus: [sec("A_KEY", true, true), sec("B", false)] }), "partial");
  assert.equal(secretsState({ secretStatus: [sec("A_KEY", true, true)] }), "ok");
  assert.equal(maskedSecret(sec("A", true)), "••••WXYZ");
  assert.equal(maskedSecret(sec("A", true, false, "")), "••••");
  assert.equal(maskedSecret(sec("A", false)), "");
});

test("secrets payload sends only typed keys and explicit clears", () => {
  assert.deepEqual(secretsPayload("demo", "gem", { A_KEY: " sk-1 ", B: "  " }, ["C"]), {
    id: "demo", server: "gem", secrets: { A_KEY: "sk-1", C: "" },
  });
  assert.equal(safeHelpUrl("https://aistudio.google.com/apikey"), "https://aistudio.google.com/apikey");
  assert.equal(safeHelpUrl("javascript:alert(1)"), null);
  assert.equal(safeHelpUrl("http://x.dev"), null);
  assert.equal(safeHelpUrl(null), null);
});

test("MCP rows map back to their plugin server", () => {
  const plugins = [{ id: "demo", mcpServers: [http, stdio] }];
  assert.equal(findPluginServer(plugins, { id: "demo-files", pluginId: "demo" })?.server.id, "files");
  assert.equal(findPluginServer(plugins, { id: "demo-nope", pluginId: "demo" }), null);
  assert.equal(findPluginServer(plugins, { id: "demo-files" }), null);
});

test("Keys buttons and secret actions are wired", () => {
  const panel = read("src/lib/PluginsPanel.svelte");
  const popup = read("src/lib/McpPopup.svelte");
  const modal = read("src/lib/PluginSecretsModal.svelte");
  assert.match(panel, /data-testid="plugin-mcp-keys-/);
  assert.match(popup, /data-testid="mcp-plugin-keys-/);
  assert.match(modal, /type="password"/);
  for (const c of ["plugin_mcp_set_secrets", "plugin_mcp_clear_secrets"]) {
    assert.ok(modal.includes(`"${c}"`), c);
    assert.ok(read("src-tauri/src/server/mod.rs").includes(`    "${c}",`), `${c} in API_ACTIONS`);
  }
});

test("bundled binaries: launch line, note and consent echo", () => {
  const sha = "a".repeat(64);
  const b = {
    id: "img", serverId: "demo-img", label: "Img", type: "stdio" as const, command: null,
    args: ["--stdio"], env: { IMG_MODE: "fast" }, enabled: false, installed: true,
    bundled: true, binaryAsset: "vicuna-mcp-image-edit-macos-aarch64", binarySha256: sha,
  };
  assert.equal(mcpLaunchLine(b), "IMG_MODE=fast bundled:vicuna-mcp-image-edit-macos-aarch64 --stdio");
  assert.equal(shortSha(sha), "aaaaaaaaaaaa");
  assert.match(mcpStdioNote(b), /^Bundled binary: vicuna-mcp-image-edit-macos-aarch64 .*SHA-256 aaaaaaaaaaaa/);
  assert.match(mcpStdioNote({ ...b, installed: false }), /missing; reinstall/);
  assert.deepEqual(mcpConsentPayload("demo", [b]).servers[0], {
    id: "img", type: "stdio", args: ["--stdio"], env: { IMG_MODE: "fast" },
    binaryAsset: "vicuna-mcp-image-edit-macos-aarch64", binarySha256: sha,
  });
  // command servers keep their old shape
  const c = { ...b, bundled: false, command: "files-mcp", binaryAsset: null, binarySha256: null };
  assert.equal(mcpLaunchLine(c), "IMG_MODE=fast files-mcp --stdio");
  assert.equal(mcpStdioNote(c), "Found `files-mcp` on this server's PATH.");
  assert.equal("binaryAsset" in mcpConsentPayload("demo", [c]).servers[0], false);
});
