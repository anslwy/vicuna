// The headless server (`vicuna serve`) records Work turns itself
// (bridge/src/persist.rs) and must write exactly what the window writes.
// The fixture below is checked against the real frontend formatters here and
// against the Rust port in `formatters_match_the_shared_frontend_fixture`
// (run: `npm test`; regenerate with `REGEN=1 node --test node-tests/serverRecorder.test.ts`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, writeFileSync } from "node:fs";
import { formatAskRecord, formatToolCallHead, formatToolCallTail, formatDisplayNote } from "../src/lib/toolFormat.ts";
import { workReplyModel } from "../src/lib/workGate.ts";
import { workerDoneMessage } from "../src/lib/leadSessions.ts";
import { mcpWakeMessage } from "../src/lib/backgroundRun.ts";
import { wakeMessage } from "../src/lib/schedule.ts";

const FIXTURE = new URL("./fixtures/server-format.json", import.meta.url);

const fns: Record<string, (...a: any[]) => string> = {
  formatToolCallHead,
  formatToolCallTail,
  formatAskRecord,
  formatDisplayNote,
  workReplyModel,
  workerDoneMessage,
  mcpWakeMessage,
  wakeMessage
};

const cases: Array<{ fn: string; args: unknown[] }> = [
  { fn: "formatToolCallHead", args: ["bash", { command: "ls -la", description: "List <files> & \"more\"" }] },
  { fn: "formatToolCallHead", args: ["exec", { command: "x".repeat(150) }] },
  { fn: "formatToolCallHead", args: ["read", {}] },
  { fn: "formatToolCallHead", args: ["mcp__vicuna-mcp__ask_user", { question: "Fruit?", choices: ["Kiwi", "Mango"] }] },
  { fn: "formatToolCallHead", args: ["exec", { command: "echo 😀 ünïcode" }] },
  { fn: "formatToolCallTail", args: ["a.txt & <b> \"q\" 'single'"] },
  { fn: "formatToolCallTail", args: ["é😀".repeat(2500)] },
  { fn: "formatAskRecord", args: ["Which   fruit?\nPick one", "Kiwi <3", false] },
  { fn: "formatAskRecord", args: ["Which fruit?", "", true] },
  { fn: "workReplyModel", args: [null, "opus", "claude"] },
  { fn: "workReplyModel", args: ["opencode/deepseek", null, "opencode"] },
  { fn: "workReplyModel", args: [null, null, "codex"] },
  { fn: "workerDoneMessage", args: [{ id: "w1", title: "Build", engine: "claude", model: "opus" }, "  done  ", false] },
  { fn: "workerDoneMessage", args: [{ id: "w2", title: "T", engine: "opencode", model: null }, "", true] },
  { fn: "workerDoneMessage", args: [{ id: "w3", title: "Long", engine: "codex", model: null }, "line\n".repeat(2000), false] },
  { fn: "mcpWakeMessage", args: [[{ name: "gh", tools: 1 }, { name: "x", tools: null }, { name: "y", tools: 5 }]] },
  { fn: "wakeMessage", args: ["  check CI  "] },
  { fn: "formatDisplayNote", args: ["\n> Auto-approved: Bash: cat <<'EOF'\n<!-- body --> & \"q\"\n", true] },
  { fn: "formatDisplayNote", args: ["\n> Auto-approved: Bash: ls <dir>\n", false] }
];

test("server-side formatter fixture matches the frontend", () => {
  const fresh = cases.map((c) => ({ ...c, expected: fns[c.fn](...c.args) }));
  if (process.env.REGEN) writeFileSync(FIXTURE, JSON.stringify(fresh, null, 2) + "\n");
  const stored = JSON.parse(readFileSync(FIXTURE, "utf8"));
  assert.deepEqual(stored, fresh);
});

import { scheduleDiff, type ScheduledItem } from "../src/lib/schedule.ts";

test("scheduleDiff sends changes and explicit removals only", () => {
  const a: ScheduledItem = { id: "a", chatId: null, title: "", workspace: "/w", text: "x", at: 1, createdAt: 0 };
  const b: ScheduledItem = { ...a, id: "b", text: "y" };
  const sent = new Map([["a", JSON.stringify(a)], ["b", JSON.stringify(b)]]);
  // unchanged list: nothing to send
  assert.deepEqual(scheduleDiff(sent, [a, b]), { put: [], remove: [] });
  // edited a, deleted b, added c
  const a2 = { ...a, at: 5 };
  const c = { ...a, id: "c" };
  assert.deepEqual(scheduleDiff(sent, [a2, c]), { put: [a2, c], remove: ["b"] });
  // an id the window never saw is not "removed" by its absence
  assert.deepEqual(scheduleDiff(new Map(), []), { put: [], remove: [] });
});
