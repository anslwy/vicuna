import assert from "node:assert/strict";
import test from "node:test";
import { commandLine, consentPayload, isPluginEngine, pendingConsent, pluginHarnessName, type PluginHarness } from "../src/lib/pluginHarness.ts";

const h: PluginHarness = { id: "muse", label: "Muse", engine: "plugin:muse-acp/muse", command: "muse", args: ["acp", "--model", "a b"], enabled: false, installed: true };

test("engine ids", () => {
  assert.ok(isPluginEngine("plugin:x/y"));
  assert.ok(!isPluginEngine("opencode"));
  assert.equal(pluginHarnessName("plugin:muse-acp/muse"), "muse");
});

test("command line shows the exact argv", () => {
  assert.equal(commandLine(h), 'muse acp --model "a b"');
});

test("consent echoes exactly what was shown", () => {
  assert.deepEqual(consentPayload("muse-acp", [h]), { id: "muse-acp", harnesses: [{ id: "muse", command: "muse", args: ["acp", "--model", "a b"] }] });
  assert.deepEqual(pendingConsent({ pendingConsent: [h] }), [h]);
  assert.deepEqual(pendingConsent(null), []);
});

test("consent echoes the usage command too", () => {
  const u = { ...h, usage: { command: "muse", args: ["usage", "--json"] } };
  assert.deepEqual(consentPayload("muse-acp", [u]).harnesses[0].usage, { command: "muse", args: ["usage", "--json"] });
  assert.equal(commandLine(u.usage), "muse usage --json");
  assert.ok(!("usage" in consentPayload("muse-acp", [h]).harnesses[0]));
});

test("plugin icons accept only SVG data URIs and otherwise use puzzle fallback", async () => {
  const { pluginEngineIcon } = await import("../src/lib/pluginHarness.ts");
  const id = "plugin:sample/agent";
  const icon = "data:image/svg+xml;base64,PHN2Zy8+";
  assert.equal(pluginEngineIcon(id, [{ id, icon }]), icon);
  for (const bad of [undefined, null, "", "https://example.org/icon.svg", "javascript:alert(1)", "data:image/svg+xml,<svg/>", "data:image/png;base64,PHN2Zy8+"]) {
    assert.equal(pluginEngineIcon(id, [{ id, icon: bad }]), null);
  }
  assert.equal(pluginEngineIcon(id, []), null);
  assert.equal(pluginEngineIcon("claude", [{ id: "claude", icon }]), null);
});
