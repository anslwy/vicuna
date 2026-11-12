// Work-mode send gate + opencode model picker rows (run: `npm test`).
// Lives outside test/ and tests/ on purpose: those are type-checked by svelte-check,
// which has no Node typings here; Node runs this file directly (type stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import { workSendGate, opencodeModelChoices, workReplyModel, type WorkEngineInfo, type OpencodeModels } from "../src/lib/workGate.ts";

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
  assert.equal(gate.reason, "OpenCode CLI not found, install it: https://opencode.ai/docs/#install");
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

// Regression: Work replies were labelled with the Chat model
// (e.g. muse-glimmer:30b-mlx) while the turn ran on opencode/beam-free.
test("Work reply label is the opencode model, never the Chat model", () => {
  assert.equal(workReplyModel("opencode/beam-free", "ollama/x"), "opencode/beam-free");
  assert.equal(workReplyModel(null, "opencode/beam-free"), "opencode/beam-free");
  assert.equal(workReplyModel(undefined, null), "opencode");
  assert.equal(workReplyModel("  ", ""), "opencode");
});

// ---------- Claude Code as a second engine ----------
import { engineId, engineLabel, workEngineChoices } from "../src/lib/workGate.ts";

const claudeMissing: WorkEngineInfo = {
  id: "claude", label: "Claude Code", installed: false, path: null, version: null,
  installUrl: "https://docs.claude.com/en/docs/claude-code/setup", model: null, configPath: null,
  missing: "Claude Code CLI not found, install it: https://docs.claude.com/en/docs/claude-code/setup",
  engines: [
    { id: "opencode", label: "opencode", installed: true, version: "2.2.6", missing: null },
    { id: "claude", label: "Claude Code", installed: false, version: null, missing: "Claude Code CLI not found" }
  ]
};

test("Claude missing: Send blocked with the engine's own install hint", () => {
  const g = workSendGate(claudeMissing);
  assert.equal(g.blocked, true);
  assert.match(g.reason ?? "", /Claude Code CLI not found, install it: https:\/\/docs\.claude\.com/);
  assert.equal(engineLabel(claudeMissing), "Claude Code");
});

test("engine picker lists only installed engines; older backends default to opencode", () => {
  assert.deepEqual(workEngineChoices(claudeMissing).map((e) => e.id), ["opencode"]);
  const both = { ...claudeMissing, engines: claudeMissing.engines!.map((e) => ({ ...e, installed: true })) };
  assert.deepEqual(workEngineChoices(both).map((e) => e.id), ["opencode", "claude"]);
  const legacy: WorkEngineInfo = { installed: true, path: "/x", version: "1", installUrl: "u", model: null, configPath: null };
  assert.equal(engineId(legacy), "opencode");
  assert.equal(engineLabel(legacy), "OpenCode");
  assert.deepEqual(workEngineChoices(legacy), []);
});

test("Claude reply label is claude/<model>, never the Chat model", () => {
  assert.equal(workReplyModel("claude/haiku", "sonnet", "claude"), "claude/haiku");
  assert.equal(workReplyModel(null, "haiku", "claude"), "claude/haiku");
  assert.equal(workReplyModel(null, null, "claude"), "claude");
  assert.equal(workReplyModel(null, "opencode/beam-free", "opencode"), "opencode/beam-free");
});

// ---------- Codex as a third engine ----------
test("Codex engine: label, reply label, gate and picker", () => {
  const codex: WorkEngineInfo = {
    id: "codex", label: "Codex", installed: true, path: "/usr/local/bin/codex", version: "0.166.0",
    installUrl: "https://developers.openai.com/codex/cli", model: null, configPath: null,
    engines: [
      { id: "opencode", label: "opencode", installed: true, version: "2.2.6", missing: null },
      { id: "claude", label: "Claude Code", installed: false, version: null, missing: "Claude Code CLI not found" },
      { id: "codex", label: "Codex", installed: true, version: "0.166.0", missing: null },
    ],
  } as WorkEngineInfo;
  assert.equal(engineId(codex), "codex");
  assert.equal(engineLabel(codex), "Codex");
  assert.equal(engineLabel({ ...codex, label: "" } as WorkEngineInfo), "Codex");
  assert.deepEqual(workEngineChoices(codex).map((e) => e.id), ["opencode", "codex"]);
  assert.equal(workSendGate(codex).blocked, false);
  assert.equal(workReplyModel(null, "gpt-6-luna", "codex"), "codex/gpt-6-luna");
  assert.equal(workReplyModel("codex/gpt-6.1-sol", null, "codex"), "codex/gpt-6.1-sol");
  assert.equal(workReplyModel(null, null, "codex"), "codex");
  const missing = { ...codex, installed: false, missing: "Codex CLI not found, install it: https://developers.openai.com/codex/cli" } as WorkEngineInfo;
  assert.equal(workSendGate(missing).reason, missing.missing);
});

// ---------- fx as a fourth engine ----------
test("fx engine: label, provider/model reply label, gate and picker", () => {
  const fx: WorkEngineInfo = {
    id: "fx", label: "fx", installed: true, path: "/home/u/.local/bin/fx", version: "0.0.16",
    installUrl: "https://fx.sh/docs", model: null, configPath: null,
    engines: [
      { id: "opencode", label: "OpenCode", installed: true, version: "2.2.6", missing: null },
      { id: "claude", label: "Claude Code", installed: true, version: "2.1.0", missing: null },
      { id: "codex", label: "Codex", installed: true, version: "0.166.0", missing: null },
      { id: "fx", label: "fx", installed: true, version: "0.0.16", missing: null },
    ],
  } as WorkEngineInfo;
  assert.equal(engineId(fx), "fx");
  assert.equal(engineLabel(fx), "fx");
  assert.equal(engineLabel({ ...fx, label: "" } as WorkEngineInfo), "fx");
  assert.deepEqual(workEngineChoices(fx).map((e) => e.id), ["opencode", "claude", "codex", "fx"]);
  assert.equal(workSendGate(fx).blocked, false);
  // both routes: fx's codex provider and a configured ollama provider
  assert.equal(workReplyModel(null, "codex/gpt-6-luna", "fx"), "fx/codex/gpt-6-luna");
  assert.equal(workReplyModel(null, "ollama/deepseek-v4.1-flash:cloud", "fx"), "fx/ollama/deepseek-v4.1-flash:cloud");
  assert.equal(workReplyModel("fx/ollama/deepseek-v4.1-flash:cloud", "codex/gpt-6-luna", "fx"), "fx/ollama/deepseek-v4.1-flash:cloud");
  assert.equal(workReplyModel(null, null, "fx"), "fx");
  const missing = { ...fx, installed: false, missing: "fx CLI not found, install it: https://fx.sh/docs" } as WorkEngineInfo;
  assert.equal(workSendGate(missing).blocked, true);
  assert.equal(workSendGate(missing).reason, missing.missing);
});
