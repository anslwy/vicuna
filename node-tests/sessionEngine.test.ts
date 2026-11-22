import assert from "node:assert/strict";
import test from "node:test";
import { sessionPick, missingHarnessReason } from "../src/lib/sessionEngine.ts";

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

import { handoffAt, workspaceHandoffLabels } from "../src/lib/sessionEngine.ts";
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
