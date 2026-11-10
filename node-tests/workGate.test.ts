// Work-mode send gate + opencode model picker rows (run: `npm test`).
// Lives outside test/ and tests/ on purpose: those are type-checked by svelte-check,
// which has no Node typings here; Node runs this file directly (type stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import { workSendGate, opencodeModelChoices, type WorkEngineInfo, type OpencodeModels } from "../src/lib/workGate.ts";

const base: WorkEngineInfo = {
  installed: true,
  path: "/usr/local/bin/opencode",
  version: "2.2.6",
  installUrl: "https://opencode.ai/docs/#install",
  model: null,
  configPath: null
};

test("opencode missing => Work send disabled with the install link", () => {
  const gate = workSendGate({ ...base, installed: false, path: null, version: null });
  assert.equal(gate.blocked, true);
  assert.equal(gate.reason, "opencode CLI not found, install it: https://opencode.ai/docs/#install");
});

test("detection not answered yet => still disabled (no fallback engine)", () => {
  assert.equal(workSendGate(null).blocked, true);
});

test("opencode installed => send allowed", () => {
  assert.deepEqual(workSendGate(base), { blocked: false, reason: null });
});

const list: OpencodeModels = {
  models: [
    { id: "ollama/deepseek-v4.1-flash:cloud", provider: "ollama" },
    { id: "opencode/big-pickle", provider: "opencode" }
  ],
  current: null
};

test("model rows keep opencode's order and filter case-insensitively", () => {
  assert.deepEqual(opencodeModelChoices(list, null, "").map((m) => m.id), [
    "ollama/deepseek-v4.1-flash:cloud",
    "opencode/big-pickle"
  ]);
  assert.deepEqual(opencodeModelChoices(list, null, "PICKLE").map((m) => m.id), ["opencode/big-pickle"]);
});

test("a saved pick missing from opencode's list stays visible", () => {
  const rows = opencodeModelChoices(list, "anthropic/gone", "");
  assert.equal(rows[0].id, "anthropic/gone");
  assert.equal(rows[0].provider, "anthropic");
  assert.equal(rows.length, 3);
});

test("no list yet => only the saved pick", () => {
  assert.deepEqual(opencodeModelChoices(null, null, ""), []);
  assert.equal(opencodeModelChoices(null, "ollama/x", "").length, 1);
});
