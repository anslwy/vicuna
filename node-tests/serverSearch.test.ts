import assert from "node:assert/strict";
import test from "node:test";
import { excerpt, searchLoadedChats, type LoadedChat } from "../src/lib/serverSearch.ts";

const chat = (
  id: string,
  title: string,
  msgs: { role: string; content: string }[],
  updatedAt = 1
): LoadedChat => ({ id, title, messages: msgs, createdAt: 1, updatedAt, mode: "work", workspace: "/deck/p" });

const load: LoadedChat[] = [
  chat("5b352ab9-6c95-4dbf-911c-3cdfbdc78518", "host mode", [
    { role: "user", content: "can you check why host mode does not have browse" },
    { role: "assistant", content: "Let me look." }
  ], 300),
  chat("31f1236a-afe1-4f92-9be9-4446ac2060c2", "goal loop", [
    { role: "user", content: "keep working until the tests pass" }
  ], 200),
  chat("b7e4fafc-934d-4c62-b57b-95875d6a8349", "100% literal", [
    { role: "user", content: "percent sign 100% here" }
  ], 100)
];

test("a pasted session id is found by id alone, case-insensitively", () => {
  const hits = searchLoadedChats(load, "", ["5B352AB9-6C95-4DBF-911C-3CDFBDC78518"]);
  assert.equal(hits.length, 1);
  assert.equal(hits[0].title, "host mode");
  assert.match(hits[0].snippet, /host mode does not have browse/);
});

test("text matches title, id and message bodies, newest first", () => {
  const byText = searchLoadedChats(load, "browse", []);
  assert.deepEqual(byText.map((h) => h.title), ["host mode"]);
  // the snippet centers on the match (clipped with a leading ellipsis)
  assert.match(byText[0].snippet, /^….*host mode does not have browse$/);
  assert.deepEqual(searchLoadedChats(load, "percent", []).map((h) => h.title), ["100% literal"]);
  assert.deepEqual(searchLoadedChats(load, "31f1236a", []).map((h) => h.title), ["goal loop"]);
});

test("no match is an empty list, and ids beat text matches without duplicating", () => {
  assert.deepEqual(searchLoadedChats(load, "zzz-nothing", []), []);
  assert.deepEqual(searchLoadedChats(load, "zzz", ["31f1236a-afe1-4f92-9be9-4446ac2060c2"]).map((h) => h.title), ["goal loop"]);
  const both = searchLoadedChats(load, "mode", ["5b352ab9-6c95-4dbf-911c-3cdfbdc78518"]);
  assert.equal(new Set(both.map((h) => h.id)).size, both.length);
});

test("rows carry the display fields the palette renders", () => {
  const [hit] = searchLoadedChats(load, "", ["5b352ab9-6c95-4dbf-911c-3cdfbdc78518"]);
  assert.equal(hit.updatedAt, 300);
  assert.equal(hit.mode, "work");
  assert.equal(hit.workspace, "/deck/p");
  // numeric strings (sqlite rows arrive stringified) still sort by recency
  const stringy: LoadedChat[] = [
    { id: "a", title: "a", updatedAt: "100", messages: [{ role: "user", content: "match" }] },
    { id: "b", title: "b", updatedAt: "900", messages: [{ role: "user", content: "match" }] }
  ];
  assert.deepEqual(searchLoadedChats(stringy, "match", []).map((h) => h.id), ["b", "a"]);
});

test("excerpt flattens newlines and clips with ellipses", () => {
  assert.equal(excerpt("a\nb  c", ""), "a b  c");
  const long = "x".repeat(300) + "needle" + "y".repeat(300);
  const out = excerpt(long, "needle");
  assert.match(out, /^…x+needley+…$/);
  assert.ok(out.length < 160);
  assert.equal(excerpt("short text", "absent"), "short text");
});
