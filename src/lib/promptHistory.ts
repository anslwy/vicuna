/** Decide whether ArrowUp/ArrowDown in the composer should step through
 *  terminal-style prompt history (-1 = older, 1 = newer, 0 = normal caret).
 *
 *  ArrowUp only steps to an older message when the caret is collapsed before
 *  the first character (position 0), both when editing a draft and while
 *  browsing history — so pressing Up on the first line moves the caret to the
 *  leftmost position first, and pressing Up again walks older.
 *  While browsing, ArrowDown on the last line walks newer / restores the draft. */
export function promptHistoryStepDir(opts: {
  key: string;
  input: string;
  selectionStart: number | null | undefined;
  selectionEnd: number | null | undefined;
  browsing: boolean;
}): -1 | 0 | 1 {
  if (opts.key !== "ArrowUp" && opts.key !== "ArrowDown") return 0;
  const start = opts.selectionStart ?? opts.input.length;
  const end = opts.selectionEnd ?? start;
  if (start !== end) return 0;
  if (opts.key === "ArrowUp") {
    return start === 0 ? -1 : 0;
  }
  if (!opts.browsing) return 0;
  return !opts.input.slice(start).includes("\n") ? 1 : 0;
}

/** Where to place the caret after filling the composer from prompt history:
 *  top-left (0) on ArrowUp so repeated Up presses walk older without moving
 *  the caret first, and end-of-text on ArrowDown / draft restore. */
export function historyCaretPos(dir: -1 | 1, text: string): number {
  return dir < 0 ? 0 : text.length;
}
