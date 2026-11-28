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

test("assistant bubbles render m.images with the lightbox", () => {
  const i = app.indexOf('data-testid="assistant-images"');
  assert.ok(i > 0, "assistant image strip missing");
  const answer = app.indexOf('<div class="answer">', i);
  assert.ok(answer > i && answer - i < 800, "strip sits right before the answer");
  assert.match(app.slice(i, answer), /openLightbox\(img\)/);
});
