import assert from "node:assert/strict";
import test from "node:test";
import { extractSessionId, sessionLinkAction } from "../src/lib/deepLink.ts";

const ID = "c19ce8f0-02c5-49e6-9744-10bf9d321fa5";

test("parses vicuna://s/<id> and its variants", () => {
  assert.equal(extractSessionId([`vicuna://s/${ID}`]), ID);
  assert.equal(extractSessionId([`vicuna://s/${ID}/`]), ID);
  assert.equal(extractSessionId([`vicuna:///s/${ID}`]), ID);
  assert.equal(extractSessionId([`vicuna://s/${ID}?from=safari#x`]), ID);
  assert.equal(extractSessionId([` vicuna://s/${ID} `]), ID);
  assert.equal(extractSessionId([`http://127.0.0.1:1420/s/${ID}`]), null); // host is not "s"
  assert.equal(extractSessionId([`/s/${ID}`]), ID);
});

test("ignores non-session links and picks the first session one", () => {
  assert.equal(extractSessionId(null), null);
  assert.equal(extractSessionId([]), null);
  assert.equal(extractSessionId(["vicuna://open?path=%2Ftmp"]), null);
  assert.equal(extractSessionId(["vicuna://s/"]), null);
  assert.equal(extractSessionId(["vicuna://open?path=%2Ftmp", `vicuna://s/${ID}`]), ID);
});

test("links that arrive before chats load are stashed, never dropped", () => {
  assert.deepEqual(sessionLinkAction(ID, { ready: false, localIds: [], serverCount: 0 }), { kind: "stash" });
  assert.deepEqual(sessionLinkAction(ID, { ready: false, localIds: [ID], serverCount: 2 }), { kind: "stash" });
});

test("loaded: local first, then connected servers", () => {
  assert.equal(sessionLinkAction(ID, { ready: true, localIds: [ID], serverCount: 1 }).kind, "open-local");
  assert.equal(sessionLinkAction(ID, { ready: true, localIds: ["x"], serverCount: 1 }).kind, "search-remote");
  assert.equal(sessionLinkAction(ID, { ready: true, localIds: [], serverCount: 0 }).kind, "not-found");
});
