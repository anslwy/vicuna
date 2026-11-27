import assert from "node:assert/strict";
import test from "node:test";
import { followUpSendsNow, parseSendNow, runningPlaceholder, queuedHint } from "../src/lib/followUp.ts";

test("follow-ups send now by default, including older servers that omit the field", () => {
  assert.equal(followUpSendsNow(null), true);
  assert.equal(followUpSendsNow({}), true);
  assert.equal(followUpSendsNow({ sendNow: true }), true);
  assert.equal(followUpSendsNow({ sendNow: false }), false);
});

test("/send_now is recognised with or without a message", () => {
  assert.deepEqual(parseSendNow("/send_now"), { rest: "" });
  assert.deepEqual(parseSendNow("  /send_now  "), { rest: "" });
  assert.deepEqual(parseSendNow("/send_now@VicunaBot"), { rest: "" });
  assert.deepEqual(parseSendNow("/send_now also check the tests"), { rest: "also check the tests" });
  assert.equal(parseSendNow("/send_nowx"), null);
  assert.equal(parseSendNow("please /send_now"), null);
  assert.equal(parseSendNow("/compact"), null);
});

test("running hints match the active mode", () => {
  assert.match(runningPlaceholder(true), /interrupts/);
  assert.match(runningPlaceholder(false), /joins when the agent finishes/);
  assert.match(runningPlaceholder(false), /\/send_now/);
  assert.match(queuedHint(false), /↑ sends now/);
  assert.match(queuedHint(true), /sending now/);
});
