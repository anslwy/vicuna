import assert from "node:assert/strict";
import test from "node:test";
import { newSessionStart, readLastMode, writeLastMode, LAST_MODE_KEY } from "../src/lib/newSessionMode.ts";

test("New from a Work session stays Work in the same folder", () => {
  assert.deepEqual(newSessionStart({ mode: "work", workspace: "/repo" }, "chat"), { mode: "work", workspace: "/repo" });
});

test("New from a Chat session is Chat even when Work was saved", () => {
  assert.deepEqual(newSessionStart({ mode: "chat", workspace: null }, "work"), { mode: "chat", workspace: null });
  // sessions without a mode are old chats
  assert.deepEqual(newSessionStart({}, "work"), { mode: "chat", workspace: null });
});

test("New from home uses the saved mode", () => {
  assert.deepEqual(newSessionStart(null, "work"), { mode: "work", workspace: null });
  assert.deepEqual(newSessionStart(undefined, "chat"), { mode: "chat", workspace: null });
});

test("a Work session with no folder leaves the workspace to the defaults", () => {
  assert.deepEqual(newSessionStart({ mode: "work", workspace: "" }, "chat"), { mode: "work", workspace: null });
});

test("the saved mode round-trips and falls back to Chat", () => {
  const m = new Map<string, string>();
  const s = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v) };
  assert.equal(readLastMode(s), "chat");
  writeLastMode(s, "work");
  assert.equal(m.get(LAST_MODE_KEY), "work");
  assert.equal(readLastMode(s), "work");
  m.set(LAST_MODE_KEY, "garbage");
  assert.equal(readLastMode(s), "chat");
  assert.equal(readLastMode(null), "chat");
  const throwing = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); } };
  assert.equal(readLastMode(throwing), "chat");
  writeLastMode(throwing, "work");
});
