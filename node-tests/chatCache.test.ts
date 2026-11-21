import assert from "node:assert/strict";
import test from "node:test";
import { resolveCachedChatRow, selectCacheEvictions } from "../src/lib/chatCache.ts";

test("resolveCachedChatRow upgrades a partial row when rev matches a full cached copy", () => {
  const full = { id: "c1", rev: 7, title: "Hello", messages: [{ role: "user", content: "full body" }] };
  const partial = { id: "c1", rev: 7, partial: true, title: "Hello", messages: [{ role: "user", content: "short" }] };
  const cached = new Map([["c1", { rev: 7, raw: full }]]);

  const res = resolveCachedChatRow(partial, cached);
  assert.equal(res.hit, true);
  assert.equal(res.row, full);
  assert.equal(res.row.partial, undefined);
});

test("resolveCachedChatRow misses when server rev is newer than cache", () => {
  const stale = { id: "c1", rev: 6, title: "Old", messages: [{ role: "user", content: "old" }] };
  const partial = { id: "c1", rev: 7, partial: true, title: "New", messages: [{ role: "user", content: "short" }] };
  const cached = new Map([["c1", { rev: 6, raw: stale }]]);

  const res = resolveCachedChatRow(partial, cached);
  assert.equal(res.hit, false);
  assert.equal(res.row, partial);
});

test("resolveCachedChatRow leaves non-partial rows untouched and ignores partial cache entries", () => {
  const fullFromServer = { id: "c1", rev: 7, title: "Full", messages: [] };
  const cached = new Map([["c1", { rev: 7, raw: { id: "c1", rev: 7, partial: true } }]]);

  assert.equal(resolveCachedChatRow(fullFromServer, cached).hit, false);
  assert.equal(resolveCachedChatRow({ id: "c1", rev: 7, partial: true }, cached).hit, false);
});

test("selectCacheEvictions drops deleted chats and evicts oldest entries past the cap", () => {
  const entries = [
    { k: "s:a", id: "a", at: 10 },
    { k: "s:b", id: "b", at: 20 },
    { k: "s:c", id: "c", at: 30 },
    { k: "s:gone", id: "gone", at: 99 }
  ];
  const drop = selectCacheEvictions(entries, new Set(["a", "b", "c"]), 2);
  assert.deepEqual(drop.sort(), ["s:a", "s:gone"]);
});
