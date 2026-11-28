import assert from "node:assert/strict";
import test from "node:test";
import { promptHistoryStepDir, historyCaretPos } from "../src/lib/promptHistory.ts";

test("ArrowUp starts history walk only when caret is collapsed before the first character", () => {
  // Empty input: caret at 0 recalls history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "", selectionStart: 0, selectionEnd: 0, browsing: false }),
    -1,
  );
  // Non-empty single-line input: caret at 0 recalls history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "hello world", selectionStart: 0, selectionEnd: 0, browsing: false }),
    -1,
  );
  // Non-empty single-line input: caret in middle or end does NOT recall history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "hello world", selectionStart: 5, selectionEnd: 5, browsing: false }),
    0,
  );
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "hello world", selectionStart: 11, selectionEnd: 11, browsing: false }),
    0,
  );
  // Whitespace-only input with caret after spaces does NOT recall history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "   ", selectionStart: 3, selectionEnd: 3, browsing: false }),
    0,
  );
  // Multi-line input: only caret at 0 recalls history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "line 1\nline 2", selectionStart: 0, selectionEnd: 0, browsing: false }),
    -1,
  );
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "line 1\nline 2", selectionStart: 3, selectionEnd: 3, browsing: false }),
    0,
  );
  // Non-collapsed selection starting at 0 does NOT recall history
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "hello", selectionStart: 0, selectionEnd: 5, browsing: false }),
    0,
  );
  // ArrowDown when not browsing never steps
  assert.equal(
    promptHistoryStepDir({ key: "ArrowDown", input: "", selectionStart: 0, selectionEnd: 0, browsing: false }),
    0,
  );
});

test("while browsing history, ArrowUp still requires caret at 0 and ArrowDown on last line steps newer", () => {
  // Single-line recalled message: caret at end lets Up move to leftmost first
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "previous prompt", selectionStart: 15, selectionEnd: 15, browsing: true }),
    0,
  );
  // Once caret is at 0, Up steps older
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "previous prompt", selectionStart: 0, selectionEnd: 0, browsing: true }),
    -1,
  );
  // Multi-line recalled message: ArrowUp on line 2 or middle of line 1 lets caret move; at 0 steps older
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "line 1\nline 2", selectionStart: 13, selectionEnd: 13, browsing: true }),
    0,
  );
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "line 1\nline 2", selectionStart: 4, selectionEnd: 4, browsing: true }),
    0,
  );
  assert.equal(
    promptHistoryStepDir({ key: "ArrowUp", input: "line 1\nline 2", selectionStart: 0, selectionEnd: 0, browsing: true }),
    -1,
  );
  // Multi-line recalled message: ArrowDown on line 1 lets caret move down; on line 2 steps newer
  assert.equal(
    promptHistoryStepDir({ key: "ArrowDown", input: "line 1\nline 2", selectionStart: 4, selectionEnd: 4, browsing: true }),
    0,
  );
  assert.equal(
    promptHistoryStepDir({ key: "ArrowDown", input: "line 1\nline 2", selectionStart: 10, selectionEnd: 10, browsing: true }),
    1,
  );
});

test("historyCaretPos places caret at top-left on ArrowUp and at end on ArrowDown", () => {
  assert.equal(historyCaretPos(-1, "previous message"), 0);
  assert.equal(historyCaretPos(-1, "line 1\nline 2"), 0);
  assert.equal(historyCaretPos(1, "newer message"), 13);
  assert.equal(historyCaretPos(1, ""), 0);
});
