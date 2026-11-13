// In-app editor state rules (src/lib/editorModel.ts). Run: `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  EDITOR_MAX_BYTES,
  VIM_PREF_KEY,
  escapeAction,
  fileBase,
  isDirty,
  readOnlyReason,
  readVimPref,
  saveAction,
  writeVimPref
} from "../src/lib/editorModel.ts";

function memStore() {
  const m = new Map<string, string>();
  return { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), m };
}

test("vim pref defaults off and round-trips", () => {
  const s = memStore();
  assert.equal(readVimPref(s), false);
  writeVimPref(s, true);
  assert.equal(s.m.get(VIM_PREF_KEY), "1");
  assert.equal(readVimPref(s), true);
  writeVimPref(s, false);
  assert.equal(readVimPref(s), false);
  // missing / throwing storage never breaks the editor
  assert.equal(readVimPref(null), false);
  const broken = { getItem: () => { throw new Error("x"); }, setItem: () => { throw new Error("x"); } };
  assert.equal(readVimPref(broken), false);
  writeVimPref(broken, true);
});

test("Esc closes the editor only outside Vim mode", () => {
  assert.equal(escapeAction(false, false, false), "close");
  assert.equal(escapeAction(true, false, false), "none");
  assert.equal(escapeAction(true, true, false), "dismiss-conflict");
  assert.equal(escapeAction(true, false, true), "dismiss-confirm");
});

test("dirty tracks the saved baseline, not edit history", () => {
  assert.equal(isDirty("a", "a"), false);
  assert.equal(isDirty("ab", "a"), true);
  // typed then undone back to the saved text → clean again
  assert.equal(isDirty("a", "a"), false);
});

test("truncated or oversized files open read-only", () => {
  assert.equal(readOnlyReason(false, 10), null);
  assert.equal(readOnlyReason(false, EDITOR_MAX_BYTES), null);
  assert.match(readOnlyReason(true, 10) ?? "", /Read-only/);
  assert.match(readOnlyReason(false, EDITOR_MAX_BYTES + 1) ?? "", /512KB/);
});

test("save detects on-disk changes before writing", () => {
  assert.equal(saveAction("x", "x", "x"), "clean");
  assert.equal(saveAction("y", "x", "x"), "save");
  assert.equal(saveAction("y", "x", null), "save");
  assert.equal(saveAction("y", "x", "z"), "conflict");
  // disk moved even though the buffer is clean: still a conflict (reload offer)
  assert.equal(saveAction("x", "x", "z"), "conflict");
});

test("fileBase handles both separators", () => {
  assert.equal(fileBase("src/lib/a.ts"), "a.ts");
  assert.equal(fileBase("src\\lib\\b.rs"), "b.rs");
  assert.equal(fileBase("c.md"), "c.md");
});
