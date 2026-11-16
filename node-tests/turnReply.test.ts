import assert from "node:assert/strict";
import test from "node:test";
import { createTurnReplyRoute } from "../src/lib/turnReply.ts";

test("early session-control replies wait for the originating startup id", async () => {
  const route = createTurnReplyRoute();
  const sent: string[][] = [];
  const reply = route.send("sess-1", "created", async (...args) => { sent.push(args); });
  await Promise.resolve();
  assert.deepEqual(sent, []);
  route.bind("turn-1");
  await reply;
  assert.deepEqual(sent, [["turn-1", "sess-1", "created"]]);
});

test("reattached ask and wake replies retain their origin after replacement", async () => {
  const old = createTurnReplyRoute();
  const next = createTurnReplyRoute();
  old.bind("turn-old");
  next.bind("turn-new");
  old.bind("turn-new"); // immutable once bound
  const sent: string[][] = [];
  const deliver = async (...args: string[]) => { sent.push(args); };
  await old.send("ask-1", "yes", deliver);
  await next.send("ask-1", "no", deliver);
  await old.send("wake-2", "scheduled", deliver);
  assert.deepEqual(sent.map(x => x[0]), ["turn-old", "turn-new", "turn-old"]);
});

test("startup and transport failures propagate instead of dropping replies", async () => {
  const failed = createTurnReplyRoute();
  failed.bind(null);
  await assert.rejects(failed.send("sess-1", "ok", async () => {}), /failed to start/);
  const route = createTurnReplyRoute();
  route.bind("turn-1");
  await assert.rejects(route.send("sess-1", "ok", async () => { throw new Error("offline"); }), /offline/);
});
