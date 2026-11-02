// Scroll events do not identify their cause. In WebKit, replacing streamed
// markdown or losing focus can reset scrollTop without any user gesture.
// Only input handlers may pause a pinned view; scroll events may resume it.
export type ScrollMetrics = {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
};

export function upwardScrollInput(deltaY: number, scrollTop: number): boolean {
  return deltaY < 0 && scrollTop > 0;
}

export function followAfterScroll(
  pinned: boolean,
  { scrollTop, scrollHeight, clientHeight }: ScrollMetrics,
  draggingScrollbar = false
): boolean {
  if (draggingScrollbar) return false;
  // Keep following through layout/focus resets, even when the height did not
  // shrink. A paused reader resumes only at the bottom, not merely near it.
  return pinned || scrollHeight - clientHeight - scrollTop <= 2;
}

export function scrollKeyDirection(event: {
  key: string;
  shiftKey: boolean;
  ctrlKey: boolean;
  metaKey: boolean;
  altKey: boolean;
}): number {
  if (event.altKey || event.ctrlKey) return 0;
  if (event.key === "ArrowUp" || event.key === "PageUp" || event.key === "Home") return -1;
  if (event.key === "ArrowDown" || event.key === "PageDown" || event.key === "End") return 1;
  if (event.key === " " && !event.metaKey) return event.shiftKey ? -1 : 1;
  return 0;
}

// ---------------------------------------------------------------------------
// Paused-reader anchoring
//
// A paused reader must not be moved by anything the stream does. Two engine
// behaviours break that on their own, both of them *after* an {@html}
// re-render that changes the answer's height above the viewport:
//
//   1. Chromium anchors an overflowing scroller even when the document sets
//      `overflow-anchor: none`, so a growing answer pushes the text down.
//   2. Collapsing `scrollHeight` (the thinking block is destroyed when the
//      answer starts) clamps `scrollTop`, so the view jumps toward the top.
//
// Fixing that from scroll events is impossible: an adjustment and a real user
// scroll look identical there. Instead the caller records a viewport anchor
// (an element top in client coordinates) immediately *before* the DOM write
// and re-applies it in the next frame. The anchor is a stable element that is
// on screen, so re-applying it puts the reader's pixels back exactly where
// they were.
// ---------------------------------------------------------------------------

/** What the reader was looking at, as pixels rather than as scrollTop. */
export type ViewAnchor = {
  el: HTMLElement;
  /** viewport top of `el`, relative to the scroller's box */
  top: number;
  /** how far `el` sat from the bottom of the content, for a reader at the end */
  fromBottom: number;
};

type ScrollerLike = {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  getBoundingClientRect(): { top: number };
};

/** The child of `scroller` that contains `el` (the scroller's own direct child). */
function topLevelChildOf(scroller: Element, el: Element): HTMLElement {
  let node: Element = el;
  while (node.parentElement && node.parentElement !== scroller) node = node.parentElement;
  return node as HTMLElement;
}

/**
 * Remember the visible element nearest the top edge of a paused scroller.
 * Returns null when there is nothing on screen to hold on to.
 */
export function captureViewAnchor(scroller: ScrollerLike, scrollerEl: Element): ViewAnchor | null {
  const box = scroller.getBoundingClientRect();
  let best: HTMLElement | null = null;
  let bestTop = 0;
  for (const row of Array.from(scrollerEl.querySelectorAll("[data-msg-index]")) as HTMLElement[]) {
    const top = row.getBoundingClientRect().top - box.top;
    if (top <= 4 && (!best || top > bestTop)) {
      best = topLevelChildOf(scrollerEl, row);
      bestTop = top;
    }
  }
  if (!best) {
    // nothing has reached the top edge yet (content is still short): hold the
    // first row, which is what a reader at the top of a thread is looking at
    const first = scrollerEl.firstElementChild as HTMLElement | null;
    if (!first) return null;
    best = first;
    bestTop = first.getBoundingClientRect().top - box.top;
  }
  return { el: best, top: bestTop, fromBottom: scroller.scrollHeight - scroller.scrollTop };
}

/**
 * Put the anchored element back where the reader saw it. Content that grew
 * above the viewport shifts `top` by exactly the amount the reader must be
 * compensated for, so the new scrollTop is `scrollTop + delta`. Returns
 * whether a correction was needed (the caller can skip a redundant write).
 *
 * A reader who was already at the end keeps their place at the end instead,
 * so a taller answer reveals more text rather than scrolling past it.
 */
export function restoreViewAnchor(scroller: ScrollerLike, anchor: ViewAnchor): boolean {
  const atEnd = anchor.fromBottom >= scroller.scrollHeight - 4;
  const target = atEnd
    ? scroller.scrollHeight - scroller.clientHeight
    : scroller.scrollTop + (anchor.el.getBoundingClientRect().top - scroller.getBoundingClientRect().top - anchor.top);
  const max = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
  const next = Math.min(max, Math.max(0, target));
  if (Math.abs(next - scroller.scrollTop) < 1) return false;
  scroller.scrollTop = next;
  return true;
}
