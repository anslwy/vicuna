import assert from "node:assert/strict";
import test from "node:test";
import { orderPaletteRows, remoteQueryFor, sessionIdIn, showHostLabels, describeServerSearchError } from "../src/lib/palette.ts";

const chat = (id: string) => ({ kind: "chat" as const, chat: { id } });

test("a query puts its first match on the cursor, New last", () => {
  const rows = orderPaletteRows([chat("a"), chat("b")], "dep");
  assert.deepEqual(rows.map((r) => r.kind), ["chat", "chat", "new"]);
  assert.equal(rows[0].kind === "chat" && rows[0].chat.id, "a");
});

test("an empty query keeps New on top of the recent chats", () => {
  assert.deepEqual(orderPaletteRows([chat("a")], "").map((r) => r.kind), ["new", "chat"]);
  assert.deepEqual(orderPaletteRows([], "   ").map((r) => r.kind), ["new"]);
  // whitespace-only still counts as unfiltered
  assert.equal(orderPaletteRows([], "  ")[0].kind, "new");
});

test("no matches leaves New as the only, selectable row", () => {
  assert.deepEqual(orderPaletteRows([], "nothing here").map((r) => r.kind), ["new"]);
});

test("session ids come out of every link shape", () => {
  const id = "d76220cf-92cb-4039-83b1-8f0ad95556f4";
  assert.equal(sessionIdIn(`vicuna://s/${id}`), id);
  assert.equal(sessionIdIn(`http://127.0.0.1:1420/s/${id}`), id);
  assert.equal(sessionIdIn(id.toUpperCase()), id);
  assert.equal(sessionIdIn("just some words"), null);
});

test("a pasted link is looked up by id, text only from two characters", () => {
  const id = "d76220cf-92cb-4039-83b1-8f0ad95556f4";
  assert.deepEqual(remoteQueryFor(`vicuna://s/${id}`, id), { ids: [id], text: "" });
  assert.deepEqual(remoteQueryFor("deploy", null), { ids: [], text: "deploy" });
  assert.deepEqual(remoteQueryFor("d", null), { ids: [], text: "" });
});

test("host labels appear once another host or a server window is in play", () => {
  assert.equal(showHostLabels(false, 0), false);
  assert.equal(showHostLabels(false, 2), true);
  assert.equal(showHostLabels(true, 0), true);
});

test("an old server's unknown-command error becomes an update hint", () => {
  const hint = describeServerSearchError("unknown bridge command server_search_chats");
  assert.match(hint, /older Vicuna/);
  assert.match(hint, /vicuna serve/);
  // real transport problems keep their own text
  assert.equal(describeServerSearchError("https://h:8443 did not answer in time."), "https://h:8443 did not answer in time.");
  assert.equal(describeServerSearchError("  "), "search failed on that server");
});
