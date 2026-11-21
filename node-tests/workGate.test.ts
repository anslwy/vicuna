// Work-mode send gate + opencode model picker rows (run: `npm test`).
// Lives outside test/ and tests/ on purpose: those are type-checked by svelte-check,
// which has no Node typings here; Node runs this file directly (type stripping).
import { test } from "node:test";
import assert from "node:assert/strict";
import { workSendGate, opencodeModelChoices, workReplyModel, usageMeterTone, usageTabs, usageHoverRows, usageGaugeFrac, USAGE_POLL_INTERVAL_MS, formatUsageAge, isUsageStale, usagePollOrder, parseUsageCache, type WorkEngineInfo, type OpencodeModels } from "../src/lib/workGate.ts";

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
import { engineId, engineLabel, workEngineChoices, workEngineMenuItems } from "../src/lib/workGate.ts";

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

test("engine menu items include uninstalled harnesses in order so they render disabled", () => {
  assert.deepEqual(
    workEngineMenuItems(claudeMissing).map((e) => ({ id: e.id, installed: e.installed })),
    [
      { id: "opencode", installed: true },
      { id: "claude", installed: false },
    ],
  );
  assert.deepEqual(workEngineMenuItems(null), []);
  const legacy: WorkEngineInfo = { installed: true, path: "/x", version: "1", installUrl: "u", model: null, configPath: null };
  assert.deepEqual(workEngineMenuItems(legacy), []);
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

// ---------- Antigravity (agy) as an engine ----------
test("Antigravity engine: label, reply label, gate and picker", () => {
  const agy: WorkEngineInfo = {
    id: "agy", label: "Antigravity", installed: true, path: "/home/u/.local/bin/agy", version: "1.5.6",
    installUrl: "https://antigravity.google/", model: null, configPath: null,
    engines: [
      { id: "opencode", label: "OpenCode", installed: true, version: "2.2.6", missing: null },
      { id: "claude", label: "Claude Code", installed: false, version: null, missing: "Claude Code CLI not found" },
      { id: "codex", label: "Codex", installed: false, version: null, missing: "Codex CLI not found" },
      { id: "agy", label: "Antigravity", installed: true, version: "1.5.6", missing: null },
    ],
  } as WorkEngineInfo;
  assert.equal(engineId(agy), "agy");
  assert.equal(engineLabel(agy), "Antigravity");
  assert.equal(engineLabel({ ...agy, label: "" } as WorkEngineInfo), "Antigravity");
  assert.deepEqual(workEngineChoices(agy).map((e) => e.id), ["opencode", "agy"]);
  assert.equal(workSendGate(agy).blocked, false);
  assert.equal(workReplyModel(null, "gemini-3.8-flash-medium", "agy"), "agy/gemini-3.8-flash-medium");
  assert.equal(workReplyModel("agy/gemini-4-argon-high", null, "agy"), "agy/gemini-4-argon-high");
  assert.equal(workReplyModel(null, null, "agy"), "agy");
  const missing = { ...agy, installed: false, missing: "Antigravity CLI (agy) not found, install it: https://antigravity.google/" } as WorkEngineInfo;
  assert.deepEqual(workSendGate(missing), { blocked: true, reason: missing.missing });
});

// ---------- pi as a sixth engine ----------
test("pi engine: label, provider/model reply label, gate and picker", () => {
  const pi: WorkEngineInfo = {
    id: "pi", label: "pi", installed: true, path: "/home/u/.pi/agent/bin/pi", version: "1.5.0",
    installUrl: "https://pi.dev", model: null, configPath: null,
    engines: [
      { id: "opencode", label: "OpenCode", installed: false, version: null, missing: "opencode CLI not found" },
      { id: "pi", label: "pi", installed: true, version: "1.5.0", missing: null },
    ],
  } as WorkEngineInfo;
  assert.equal(engineId(pi), "pi");
  assert.equal(engineLabel(pi), "pi");
  assert.equal(engineLabel({ ...pi, label: "" } as WorkEngineInfo), "pi");
  assert.deepEqual(workEngineChoices(pi).map((e) => e.id), ["pi"]);
  assert.equal(workSendGate(pi).blocked, false);
  assert.equal(workReplyModel(null, "ollama/deepseek-v4.1-flash:cloud", "pi"), "pi/ollama/deepseek-v4.1-flash:cloud");
  assert.equal(workReplyModel("pi/ollama/deepseek-v4.1-flash:cloud", "openai/gpt-6-luna", "pi"), "pi/ollama/deepseek-v4.1-flash:cloud");
  assert.equal(workReplyModel(null, null, "pi"), "pi");
  const missing = { ...pi, installed: false, missing: "pi CLI not found, install it: https://pi.dev" } as WorkEngineInfo;
  assert.equal(workSendGate(missing).blocked, true);
  assert.equal(workSendGate(missing).reason, missing.missing);
});

test("usageTabs keeps only installed quota harnesses and picks the opening tab", () => {
  const engines = [
    { id: "opencode", installed: true },
    { id: "claude", installed: true },
    { id: "codex", installed: false },
    { id: "agy", installed: true },
    { id: "pi", installed: true },
  ];
  assert.deepEqual(usageTabs(engines, "agy").tabs.map((e) => e.id), ["claude", "agy"]);
  assert.equal(usageTabs(engines, "agy").initial, "agy");
  assert.equal(usageTabs(engines, "opencode").initial, "claude");
  assert.equal(usageTabs(engines, "codex").initial, "claude");
  assert.equal(usageTabs([{ id: "pi", installed: true }], "pi").initial, null);
});

test("usageMeterTone colors remaining vs used quotas consistently", () => {
  assert.equal(usageMeterTone(85, "remaining"), "ok");
  assert.equal(usageMeterTone(49.3, "remaining"), "warn");
  assert.equal(usageMeterTone(15, "remaining"), "low");
  assert.equal(usageMeterTone(12, "used"), "ok");
  assert.equal(usageMeterTone(60, "used"), "warn");
  assert.equal(usageMeterTone(100, "used"), "low");
});

test("usageHoverRows lists meters, prefixes multi-group titles, falls back to stats", () => {
  const meter = (label: string, percent: number, kind = "used") => ({ label, percent, kind, valueText: `${percent}% ${kind}`, resetText: "Resets 3pm" });
  const single = usageHoverRows({ groups: [{ title: "Claude", meters: [meter("Session", 12), meter("Week", 90)] }], stats: [] });
  assert.deepEqual(single.map((r) => [r.label, r.value, r.tone]), [["Session", "12% used", "ok"], ["Week", "90% used", "low"]]);
  assert.equal(single[0].reset, "Resets 3pm");
  const multi = usageHoverRows({ groups: [{ title: "Gemini", meters: [meter("Quota", 80, "remaining")] }, { title: "Claude", meters: [meter("Quota", 10, "remaining")] }], stats: [] });
  assert.deepEqual(multi.map((r) => r.label), ["Gemini · Quota", "Claude · Quota"]);
  // remaining quotas flip to used; the gauge shows the most-used one
  assert.deepEqual(multi.map((r) => r.usedPct), [20, 90]);
  assert.equal(usageGaugeFrac(single), 0.9);
  assert.equal(usageGaugeFrac(multi), 0.9);
  assert.equal(usageGaugeFrac([]), 0);
  const stats = usageHoverRows({ groups: [], stats: [{ label: "Tokens", value: "1.2M" }] });
  assert.deepEqual(stats, [{ label: "Tokens", value: "1.2M", reset: null, tone: null, usedPct: null }]);
});

test("usageTabs includes plugin harnesses that report usage", () => {
  const engines = [
    { id: "claude", installed: true },
    { id: "plugin:muse/muse", installed: true, usage: true },
    { id: "plugin:x/y", installed: true, usage: false },
    { id: "plugin:z/z", installed: false, usage: true },
  ];
  assert.deepEqual(usageTabs(engines, "plugin:muse/muse").tabs.map((e) => e.id), ["claude", "plugin:muse/muse"]);
  assert.equal(usageTabs(engines, "plugin:muse/muse").initial, "plugin:muse/muse");
});

test("formatUsageAge formats relative timestamps cleanly", () => {
  const now = 1_700_000_000_000;
  assert.equal(formatUsageAge(now, now), "just now");
  assert.equal(formatUsageAge(now - 59_000, now), "just now");
  assert.equal(formatUsageAge(now + 5_000, now), "just now");
  assert.equal(formatUsageAge(now - 60_000, now), "1m ago");
  assert.equal(formatUsageAge(now - 12 * 60_000, now), "12m ago");
  assert.equal(formatUsageAge(now - 2 * 60 * 60_000, now), "2h ago");
  assert.equal(formatUsageAge(now - 3 * 24 * 60 * 60_000, now), "3d ago");
});

test("isUsageStale checks missing and expired timestamps", () => {
  const now = 1_700_000_000_000;
  assert.equal(isUsageStale(undefined, now), true);
  assert.equal(isUsageStale(null, now), true);
  assert.equal(isUsageStale(now - USAGE_POLL_INTERVAL_MS + 1, now), false);
  assert.equal(isUsageStale(now - USAGE_POLL_INTERVAL_MS, now), true);
});

test("usagePollOrder puts the active quota harness first including plugins", () => {
  const engines = [
    { id: "claude", installed: true },
    { id: "codex", installed: true },
    { id: "agy", installed: true },
    { id: "plugin:muse/muse", installed: true, usage: true },
    { id: "pi", installed: true },
  ];
  assert.deepEqual(usagePollOrder(engines, "agy"), ["agy", "claude", "codex", "plugin:muse/muse"]);
  assert.deepEqual(usagePollOrder(engines, "plugin:muse/muse"), ["plugin:muse/muse", "claude", "codex", "agy"]);
  assert.deepEqual(usagePollOrder(engines, "opencode"), ["claude", "codex", "agy", "plugin:muse/muse"]);
  assert.deepEqual(usagePollOrder([{ id: "pi", installed: true }], "pi"), []);
});

test("parseUsageCache restores fresh reports and drops expired or malformed entries", () => {
  const now = 1_700_000_000_000;
  const raw = JSON.stringify({
    agy: { report: { engine: "agy", groups: [] }, at: now - 120_000 },
    claude: { report: { engine: "claude", groups: [] }, at: now - 48 * 60 * 60_000 },
    codex: { report: null, error: "timeout", at: now - 10_000 },
    bad: "nope",
  });
  const parsed = parseUsageCache<{ engine: string; groups: unknown[] }>(raw, now);
  assert.deepEqual(Object.keys(parsed), ["agy"]);
  assert.equal(parsed.agy.report?.engine, "agy");
  assert.equal(parsed.agy.error, null);
  assert.equal(parsed.agy.at, now - 120_000);
  assert.deepEqual(parseUsageCache("not json", now), {});
  assert.deepEqual(parseUsageCache(null, now), {});
});
