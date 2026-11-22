import assert from "node:assert/strict";
import test from "node:test";
import { sessionPick, missingHarnessReason, handoffAt, workspaceHandoffLabels, latestSummaryIndex } from "../src/lib/sessionEngine.ts";

const global = { engine: "agy", model: "gemini-4-neon-medium" };

test("a session's own harness wins over the global pick", () => {
  assert.deepEqual(sessionPick({ engine: "claude", workModel: "opus" }, global), { engine: "claude", model: "opus", own: true });
  assert.deepEqual(sessionPick({ engine: "claude", workModel: null }, global), { engine: "claude", model: null, own: true });
});

test("sessions without one follow the global (last-used) pick", () => {
  assert.deepEqual(sessionPick({}, global), { ...global, own: false });
  assert.deepEqual(sessionPick({ engine: "  ", workModel: "opus" }, global), { ...global, own: false });
  assert.deepEqual(sessionPick(null, global), { ...global, own: false });
});

test("missing harness reason names it", () => {
  assert.match(missingHarnessReason("plugin:muse"), /plugin:muse.*not installed/);
});

test("handoff divider only where the harness changes", () => {
  const ms = [
    { role: "user" }, { role: "assistant", model: "claude/opus" },
    { role: "user" }, { role: "assistant", model: "claude/haiku" },
    { role: "user" }, { role: "assistant", model: "agy/gemini-4-neon-medium" },
    { role: "user" }, { role: "assistant", model: "agy/gemini-4-neon-medium" }
  ];
  assert.equal(handoffAt(ms, 1), null);
  assert.equal(handoffAt(ms, 3), null, "same harness, other model: same engine session");
  assert.deepEqual(handoffAt(ms, 5), { engine: { from: "claude/haiku", to: "agy/gemini-4-neon-medium" } });
  assert.equal(handoffAt(ms, 7), null);
  assert.equal(handoffAt(ms, 4), null);
});

test("compaction does not create a handoff within the same harness", () => {
  const messages = [
    { role: "assistant", model: "agy/gemini-4-neon-medium" },
    { role: "assistant", model: "gemma4:31b-cloud", summary: true },
    { role: "user" },
    { role: "assistant", model: "agy/gemini-4-neon-medium" }
  ];
  assert.equal(handoffAt(messages, 1), null);
  assert.equal(handoffAt(messages, 3), null);
});

test("a harness handoff after compaction starts from the previous Work reply", () => {
  const messages = [
    { role: "assistant", model: "agy/gemini-4-neon-medium" },
    { role: "assistant", model: "gemma4:31b-cloud", summary: true },
    { role: "user" },
    { role: "assistant", model: "claude/opus" }
  ];
  assert.deepEqual(handoffAt(messages, 3), {
    engine: { from: "agy/gemini-4-neon-medium", to: "claude/opus" }
  });
});

test("workspace handoffs skip compaction summaries even when they record a workspace", () => {
  const messages = [
    { role: "assistant", model: "claude/opus", workspace: "/repo/alpha" },
    { role: "assistant", model: "gemma4:31b-cloud", workspace: "/repo/summary", summary: true },
    { role: "user" },
    { role: "assistant", model: "claude/opus", workspace: "/repo/alpha" },
    { role: "assistant", model: "gemma4:31b-cloud", workspace: "/repo/summary", summary: true },
    { role: "user" },
    { role: "assistant", model: "claude/opus", workspace: "/repo/beta" }
  ];
  assert.equal(handoffAt(messages, 1), null);
  assert.equal(handoffAt(messages, 3), null);
  assert.deepEqual(handoffAt(messages, 6), { workspace: { from: "/repo/alpha", to: "/repo/beta" } });
});

test("a second compaction keeps the first summary identifiable and moves the boundary", () => {
  const reply = { role: "assistant", model: "agy/gemini-4-neon-medium" };
  const messages = [
    reply,
    { role: "assistant", model: "gemma4:31b-cloud", summary: true },
    reply,
    { role: "assistant", model: "qwen3:0.6b", summary: true },
    reply
  ];
  assert.equal(latestSummaryIndex(messages), 3);
  for (let i = 1; i < messages.length; i++) {
    assert.equal(handoffAt(messages, i), null);
  }
});

test("handoff divider where the workspace changes", () => {
  const ms = [
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/repo/alpha" },
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/repo/alpha/" },
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/repo/beta" },
    { role: "user" }, { role: "assistant", model: "agy/gemini-4-neon-medium", workspace: "/repo/gamma" }
  ];
  assert.equal(handoffAt(ms, 1), null);
  assert.equal(handoffAt(ms, 3), null, "trailing slash normalizes to the same workspace");
  assert.deepEqual(handoffAt(ms, 5), { workspace: { from: "/repo/alpha", to: "/repo/beta" } });
  assert.deepEqual(handoffAt(ms, 7), {
    engine: { from: "claude/opus", to: "agy/gemini-4-neon-medium" },
    workspace: { from: "/repo/beta", to: "/repo/gamma" }
  });
});

test("handoff ignores legacy messages without a recorded workspace", () => {
  const ms = [
    { role: "user" }, { role: "assistant", model: "claude/opus" },
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/repo/alpha" }
  ];
  assert.equal(handoffAt(ms, 3), null);
});

test("workspaceHandoffLabels uses basenames and disambiguates collisions", () => {
  assert.deepEqual(workspaceHandoffLabels("/work/vicuna", "/tmp/vic-wt"), { from: "vicuna", to: "vic-wt" });
  assert.deepEqual(workspaceHandoffLabels("/work/a/vicuna", "/work/b/vicuna"), { from: "a/vicuna", to: "b/vicuna" });
});

test("No workspace mode: sentinel and its scratch folder are one value, labelled cleanly", () => {
  const msgs = [
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "__vicuna_no_workspace__" },
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/home/me/vicuna/scratch/abc-123" },
    { role: "user" }, { role: "assistant", model: "claude/opus", workspace: "/repo/alpha" },
  ];
  assert.equal(handoffAt(msgs, 3), null);
  assert.deepEqual(handoffAt(msgs, 5), { workspace: { from: "No workspace", to: "/repo/alpha" } });
  assert.deepEqual(workspaceHandoffLabels("No workspace", "/repo/alpha"), { from: "No workspace", to: "alpha" });
});
