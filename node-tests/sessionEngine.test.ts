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

import { handoffAt } from "../src/lib/sessionEngine.ts";
test("handoff divider only where the harness changes", () => {
  const ms = [
    { role: "user" }, { role: "assistant", model: "claude/opus" },
    { role: "user" }, { role: "assistant", model: "claude/haiku" },
    { role: "user" }, { role: "assistant", model: "agy/gemini-4-neon-medium" },
    { role: "user" }, { role: "assistant", model: "agy/gemini-4-neon-medium" }
  ];
  assert.equal(handoffAt(ms, 1), null);
  assert.equal(handoffAt(ms, 3), null, "same harness, other model: same engine session");
  assert.deepEqual(handoffAt(ms, 5), { from: "claude/haiku", to: "agy/gemini-4-neon-medium" });
  assert.equal(handoffAt(ms, 7), null);
  assert.equal(handoffAt(ms, 4), null);
});
