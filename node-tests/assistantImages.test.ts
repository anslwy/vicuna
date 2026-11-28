import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const app = readFileSync(root + "src/lib/ChatApp.svelte", "utf8");
const work = readFileSync(root + "src/lib/work.ts", "utf8");

test("work events declare assistantimage", () => {
  assert.match(work, /type: "assistantimage"; data_url: string/);
});

test("live turns append show_image pictures to the reply", () => {
  const i = app.indexOf('case "assistantimage"');
  assert.ok(i > 0, "handler missing");
  const body = app.slice(i, i + 700);
  assert.match(body, /images: \[\.\.\.\(m\.images \?\? \[\]\), ev\.data_url\]/);
  assert.match(body, /chats = \[\.\.\.chats\]/);
});

test("assistant bubbles render m.images below the answer with the lightbox", () => {
  const answer = app.indexOf('<div class="answer">');
  assert.ok(answer > 0, "answer div missing");
  const i = app.indexOf('data-testid="assistant-images"', answer);
  assert.ok(i > answer && i - answer < 800, "strip sits right after the answer");
  assert.match(app.slice(i, i + 500), /openLightbox\(img\)/);
});
