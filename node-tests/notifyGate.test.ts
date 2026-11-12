// Turn-finished notification gate (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { shouldNotify, notifyTitle, notifyBody, parseNotifyPref, askBody } from "../src/lib/notifyGate.ts";

const base = { enabled: true, focused: false, visible: true, userStopped: false };

test("focused + visible window: no notification", () => {
  assert.equal(shouldNotify({ ...base, focused: true }), false);
});
test("unfocused window (other app in front): notify", () => {
  assert.equal(shouldNotify(base), true);
});
test("hidden or minimised window: notify even if it reports focus", () => {
  assert.equal(shouldNotify({ ...base, focused: true, visible: false }), true);
  assert.equal(shouldNotify({ ...base, visible: false }), true);
});
test("user pressed Stop: never notify", () => {
  assert.equal(shouldNotify({ ...base, userStopped: true }), false);
  assert.equal(shouldNotify({ ...base, visible: false, userStopped: true }), false);
});
test("setting off: never notify", () => {
  assert.equal(shouldNotify({ ...base, enabled: false, visible: false }), false);
});
test("title falls back to Vicuna", () => {
  assert.equal(notifyTitle("  "), "Vicuna");
  assert.equal(notifyTitle(undefined), "Vicuna");
  assert.equal(notifyTitle("My chat"), "My chat");
});
test("body: status + clipped snippet", () => {
  assert.equal(notifyBody("chat", "done"), "Reply ready");
  assert.equal(notifyBody("work", "done", "ok\n done"), "Work finished: ok done");
  assert.equal(notifyBody("chat", "error", "boom"), "Error");
  const b = notifyBody("chat", "done", "x".repeat(200));
  assert.ok(b.length <= "Reply ready: ".length + 80);
  assert.ok(b.endsWith("…"));
});
test("pref defaults to on", () => {
  assert.equal(parseNotifyPref(null), true);
  assert.equal(parseNotifyPref("0"), false);
  assert.equal(parseNotifyPref("1"), true);
});
test("ask body: question clipped, empty falls back", () => {
  assert.equal(askBody("Which color?"), "Which color?");
  assert.equal(askBody("  \n multi\nline \t question  "), "multi line question");
  assert.equal(askBody(""), "The agent is waiting for your answer.");
  assert.equal(askBody("   "), "The agent is waiting for your answer.");
  const b = askBody("x".repeat(200));
  assert.ok(b.length <= 80);
  assert.ok(b.endsWith("…"));
});
