import assert from "node:assert/strict";
import test from "node:test";
import { chatExtraJson, parseChatExtra } from "../src/lib/leadSessions.ts";
import { isMcpEnabled, resolveMcpEnabled, toggleMcpEnabled } from "../src/lib/mcp.ts";

const servers = [
  { id: "browser", enabled: false },
  { id: "custom", enabled: true },
  { id: "vicuna-mcp", enabled: true }
];

test("a Work session's switches persist independently of other sessions and defaults", () => {
  const legacy = parseChatExtra(null);
  const toggled = { mcpEnabled: toggleMcpEnabled(servers, legacy, "custom", false) };
  toggled.mcpEnabled = toggleMcpEnabled(servers, toggled, "browser", true);
  assert.deepEqual(parseChatExtra(chatExtraJson(toggled)), toggled);
  assert.deepEqual(resolveMcpEnabled(servers, toggled), { browser: true, custom: false });
  assert.deepEqual(resolveMcpEnabled(servers, legacy), { browser: false, custom: true });
  assert.equal(isMcpEnabled(servers[2], { mcpEnabled: { "vicuna-mcp": false } }), true);
});

test("new sessions snapshot the saved defaults and Set default uses the current session", () => {
  const existing = { mcpEnabled: resolveMcpEnabled(servers) };
  const current = { mcpEnabled: { browser: true, custom: false, removed: true } };
  const saved = resolveMcpEnabled(servers, current);
  assert.deepEqual(saved, { browser: true, custom: false });

  const newDefaults = servers.map((server) => ({ ...server, enabled: saved[server.id] ?? server.enabled }));
  assert.deepEqual(resolveMcpEnabled(newDefaults), { browser: true, custom: false });
  assert.deepEqual(resolveMcpEnabled(newDefaults, existing), { browser: false, custom: true });
  assert.deepEqual(resolveMcpEnabled(newDefaults, parseChatExtra(null)), saved);
});

test("legacy CLI allowlists round-trip and can be changed with session switches", () => {
  const session = parseChatExtra('{"mcp":["browser"]}');
  assert.deepEqual(resolveMcpEnabled(servers, session), { browser: true, custom: false });
  session.mcpEnabled = toggleMcpEnabled(servers, session, "custom", true);
  assert.deepEqual(parseChatExtra(chatExtraJson(session)), session);
  assert.deepEqual(resolveMcpEnabled(servers, session), { browser: true, custom: true });
});
