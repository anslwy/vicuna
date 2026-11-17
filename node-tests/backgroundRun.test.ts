// Legacy `run` wake-up parsing, MCP wake-ups + late ask answers (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { isBgDoneMessage, mcpWakeMessage, parseBgDoneMessage, parseMcpWakeMessage } from "../src/lib/backgroundRun.ts";

// Literal message as the removed `run` tool stored it in old chats.
const LEGACY = 'Background command finished (exit 0, after 1m 41s, job bg-1):\n\n```\nfor i in 1 2; do echo "t $i"; done\n```\n\nOutput:\n```\nt 1\nt 2\n```\n\nStderr:\n````\nwarn ```x```\n````\n\n(output was truncated at 32KB per stream)\n\nContinue from here.';

test("legacy finished-job messages still parse for the tool-result display", () => {
  assert.ok(isBgDoneMessage(LEGACY));
  assert.deepEqual(parseBgDoneMessage(LEGACY), {
    status: "exit 0",
    ok: true,
    elapsed: "1m 41s",
    jobId: "bg-1",
    command: 'for i in 1 2; do echo "t $i"; done',
    output: "t 1\nt 2",
    stderr: "warn ```x```",
    truncated: true
  });
  const killed = parseBgDoneMessage("Background command finished (killed, after 5s, job bg-2):\n\n```\nsleep 99\n```\n\n(no output)\n\nContinue from here.")!;
  assert.equal(killed.status, "killed");
  assert.equal(killed.ok, false);
  assert.equal(killed.output, "");
  // a user typing something similar is not mistaken for one
  assert.equal(isBgDoneMessage("Background command finished? not really"), false);
  assert.equal(parseBgDoneMessage("hello"), null);
});

test("mcp wake-up message formats and parses back for card display", () => {
  const msg = mcpWakeMessage([
    { name: "dummy-echo", tools: 1 },
    { name: "kurisu", tools: 4 },
  ]);
  assert.equal(
    msg,
    "MCP server(s) now active: `dummy-echo` (1 tool), `kurisu` (4 tools). You can call their tools now."
  );
  assert.deepEqual(parseMcpWakeMessage(msg), {
    summary: "dummy-echo (1 tool), kurisu (4 tools)",
  });
  assert.equal(parseMcpWakeMessage("hello"), null);
});
import { laterAnswerMessage } from "../src/lib/askCard.ts";

test("late answers quote the question and keep images", () => {
  assert.deepEqual(laterAnswerMessage("Which  db?", { text: " Postgres ", images: [], multi: false, picked: [] }), {
    text: 'Answer to your earlier question "Which db?":\n\nPostgres',
    images: []
  });
  const multi = laterAnswerMessage("Toppings?", { text: "", images: ["data:image/png;base64,eA=="], multi: true, picked: ["Ham"] })!;
  assert.equal(multi.text, 'Answer to your earlier question "Toppings?":\n\nSelected: Ham');
  assert.equal(multi.images.length, 1);
  const imgOnly = laterAnswerMessage("Look?", { text: "", images: ["x", "y"], multi: false, picked: [] })!;
  assert.ok(imgOnly.text.endsWith("(see the attached images)"));
  assert.equal(laterAnswerMessage("Q?", { text: "  ", images: [], multi: true, picked: [] }), null);
});
