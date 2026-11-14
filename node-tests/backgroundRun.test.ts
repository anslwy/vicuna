// Backgrounded vicuna-mcp `run` jobs + late ask answers (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { bgDoneMessage, formatRunElapsed, isBgDoneMessage, parseBgDoneMessage } from "../src/lib/backgroundRun.ts";

test("finished-job messages parse back for the tool-result display", () => {
  const msg = bgDoneMessage(
    { chatId: "c", jobId: "bg-1", command: 'for i in 1 2; do echo "t $i"; done', startedAt: 0 },
    { running: false, exit_code: 0, stdout: "t 1\nt 2\n", stderr: "warn ```x```\n", truncated: true },
    101_000
  );
  assert.ok(isBgDoneMessage(msg));
  assert.deepEqual(parseBgDoneMessage(msg), {
    status: "exit 0",
    ok: true,
    elapsed: "1m 41s",
    jobId: "bg-1",
    command: 'for i in 1 2; do echo "t $i"; done',
    output: "t 1\nt 2",
    stderr: "warn ```x```",
    truncated: true
  });
  const killed = parseBgDoneMessage(bgDoneMessage({ chatId: "c", jobId: "bg-2", command: "sleep 99", startedAt: 0 }, { running: false, exit_code: null, stdout: "", stderr: "", truncated: false }, 5000))!;
  assert.equal(killed.status, "killed");
  assert.equal(killed.ok, false);
  assert.equal(killed.output, "");
  // a user typing something similar is not mistaken for one
  assert.equal(isBgDoneMessage("Background command finished? not really"), false);
  assert.equal(parseBgDoneMessage("hello"), null);
});
import { laterAnswerMessage } from "../src/lib/askCard.ts";

const run = { chatId: "c", jobId: "bg-3", command: "npm test", startedAt: 0 };

test("elapsed labels", () => {
  assert.equal(formatRunElapsed(45_000), "45s");
  assert.equal(formatRunElapsed(133_000), "2m 13s");
  assert.equal(formatRunElapsed(3_840_000), "1h 4m");
  assert.equal(formatRunElapsed(-5), "0s");
});

test("finished job message carries command, exit, output and stderr", () => {
  const msg = bgDoneMessage(run, { running: false, exit_code: 1, stdout: "ok 1\nnot ok 2\n", stderr: "boom\n", truncated: false }, 133_000);
  assert.ok(msg.startsWith("Background command finished (exit 1, after 2m 13s, job bg-3):"), msg);
  assert.ok(msg.includes("```\nnpm test\n```"));
  assert.ok(msg.includes("Output:\n```\nok 1\nnot ok 2\n```"));
  assert.ok(msg.includes("Stderr:\n```\nboom\n```"));
  assert.ok(msg.trimEnd().endsWith("Continue from here."));
});

test("killed / silent / truncated jobs say so, and fences can't be broken", () => {
  const msg = bgDoneMessage(run, { running: false, exit_code: null, stdout: "a ``` b", stderr: "", truncated: true }, 1000);
  assert.ok(msg.includes("(killed, after 1s"));
  assert.ok(msg.includes("````\na ``` b\n````"));
  assert.ok(msg.includes("truncated"));
  const silent = bgDoneMessage(run, { running: false, exit_code: 0, stdout: "", stderr: "  ", truncated: false }, 0);
  assert.ok(silent.includes("(no output)"));
});

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
