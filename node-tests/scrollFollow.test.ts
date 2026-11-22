import assert from "node:assert/strict";
import test from "node:test";
import {
  captureOpenToolDetails,
  captureViewAnchor,
  followAfterScroll,
  restoreOpenToolDetails,
  restoreViewAnchor,
  scrollKeyDirection,
  upwardScrollInput
} from "../src/lib/scrollFollow.ts";

type FakeRow = {
  msgIndex: number;
  /** content-space Y offset from the top of the scroller's content */
  contentTop: number;
  isConnected: boolean;
  parentElement: unknown;
  details: Array<{ open: boolean }>;
  getBoundingClientRect(): { top: number };
  getAttribute(name: string): string | null;
  querySelectorAll(sel: string): unknown[];
};

function makeFakeScroller(opts: {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
  boxTop?: number;
  rows: Array<{ msgIndex: number; contentTop: number; details?: Array<{ open: boolean }> }>;
}) {
  const boxTop = opts.boxTop ?? 100;
  const scroller = {
    scrollTop: opts.scrollTop,
    scrollHeight: opts.scrollHeight,
    clientHeight: opts.clientHeight,
    getBoundingClientRect: () => ({ top: boxTop }),
    firstElementChild: null as unknown as HTMLElement | null,
    rows: [] as FakeRow[],
    querySelectorAll(sel: string) {
      if (sel === "[data-msg-index]") return this.rows;
      return [];
    },
    querySelector(sel: string) {
      const m = /^\[data-msg-index="(\d+)"\]$/.exec(sel);
      if (!m) return null;
      const idx = Number(m[1]);
      return this.rows.find((r) => r.msgIndex === idx) ?? null;
    }
  };
  scroller.rows = opts.rows.map((r) => {
    const row: FakeRow = {
      msgIndex: r.msgIndex,
      contentTop: r.contentTop,
      isConnected: true,
      parentElement: scroller,
      details: r.details ?? [],
      getBoundingClientRect() {
        return { top: boxTop + this.contentTop - scroller.scrollTop };
      },
      getAttribute(name: string) {
        return name === "data-msg-index" ? String(this.msgIndex) : null;
      },
      querySelectorAll(sel: string) {
        if (sel === "details.tool-result") return this.details;
        return [];
      }
    };
    return row;
  });
  scroller.firstElementChild = (scroller.rows[0] as unknown as HTMLElement) ?? null;
  return scroller;
}

test("followAfterScroll only resumes at the bottom, never during scrollbar drag", () => {
  assert.equal(followAfterScroll(false, { scrollTop: 3400, scrollHeight: 5100, clientHeight: 800 }), false);
  assert.equal(followAfterScroll(false, { scrollTop: 4299, scrollHeight: 5100, clientHeight: 800 }), true);
  assert.equal(followAfterScroll(true, { scrollTop: 1300, scrollHeight: 5100, clientHeight: 800 }), true);
  assert.equal(followAfterScroll(true, { scrollTop: 4300, scrollHeight: 5100, clientHeight: 800 }, true), false);
  assert.equal(upwardScrollInput(-10, 100), true);
  assert.equal(upwardScrollInput(-10, 0), false);
  assert.equal(upwardScrollInput(10, 100), false);
  assert.equal(scrollKeyDirection({ key: "PageUp", shiftKey: false, ctrlKey: false, metaKey: false, altKey: false }), -1);
  assert.equal(scrollKeyDirection({ key: "PageDown", shiftKey: false, ctrlKey: false, metaKey: false, altKey: false }), 1);
});

test("restoreViewAnchor at top of thread (scrollTop <= 4) does not jump to bottom", () => {
  // Regression for the inverted `atEnd = anchor.fromBottom >= scroller.scrollHeight - 4` bug:
  // when scrollTop is 0 and scrollHeight does not grow, the old formula treated top-of-thread
  // as at-end and yanked scrollTop all the way to the bottom.
  const scroller = makeFakeScroller({
    scrollTop: 0,
    scrollHeight: 5100,
    clientHeight: 800,
    rows: [
      { msgIndex: 0, contentTop: 40 },
      { msgIndex: 1, contentTop: 120 },
      { msgIndex: 2, contentTop: 2000 },
      { msgIndex: 3, contentTop: 2074 }
    ]
  });

  const anchor = captureViewAnchor(scroller, scroller as unknown as Element);
  assert.ok(anchor);
  // Same scrollHeight after a token wraps on the same line or collapsed thinking updates
  const changed = restoreViewAnchor(scroller, anchor);
  assert.equal(changed, false);
  assert.equal(scroller.scrollTop, 0);

  // Even when scrollTop is 3 (within 4px of top) and content grows below
  scroller.scrollTop = 3;
  const anchorNearTop = captureViewAnchor(scroller, scroller as unknown as Element);
  assert.ok(anchorNearTop);
  scroller.scrollHeight = 5400;
  assert.equal(restoreViewAnchor(scroller, anchorNearTop), false);
  assert.equal(scroller.scrollTop, 3);
});

test("restoreViewAnchor recovers scrollTop when last message collapses and re-expands", () => {
  // Exact Work-mode scenario: reader is scrolled partway down the last assistant
  // message (not at bottom). An {@html} replacement momentarily collapses the last
  // message's `.answer` to 0px, clamping scrollTop back up to the top of the last
  // message (y ~ 1300). After Svelte inserts the new HTML, scrollHeight is back to
  // 5200 but scrollTop remains clamped at 1300 until restoreViewAnchor puts it back.
  const scroller = makeFakeScroller({
    scrollTop: 3400,
    scrollHeight: 5100,
    clientHeight: 800,
    rows: [
      { msgIndex: 0, contentTop: 40 },
      { msgIndex: 1, contentTop: 120 },
      { msgIndex: 2, contentTop: 2000 }, // last user message
      { msgIndex: 3, contentTop: 2074 }  // long last assistant message
    ]
  });

  const anchor = captureViewAnchor(scroller, scroller as unknown as Element);
  assert.ok(anchor);
  assert.equal(anchor.el, scroller.rows[3] as unknown as HTMLElement);
  assert.equal(anchor.scrollTop, 3400);

  // Browser clamps scrollTop during teardown, then new HTML restores/grows scrollHeight
  scroller.scrollTop = 1300;
  scroller.scrollHeight = 5250;

  const changed = restoreViewAnchor(scroller, anchor);
  assert.equal(changed, true);
  assert.equal(scroller.scrollTop, 3400);
});

test("restoreViewAnchor compensates when content above the viewport grows", () => {
  const scroller = makeFakeScroller({
    scrollTop: 3400,
    scrollHeight: 5100,
    clientHeight: 800,
    rows: [
      { msgIndex: 0, contentTop: 40 },
      { msgIndex: 1, contentTop: 120 },
      { msgIndex: 2, contentTop: 2000 },
      { msgIndex: 3, contentTop: 2074 }
    ]
  });

  const anchor = captureViewAnchor(scroller, scroller as unknown as Element);
  assert.ok(anchor);

  // Content above row 3 grows by 250px (shifting row 3 down by 250px)
  scroller.rows[3].contentTop += 250;
  scroller.scrollHeight += 250;

  const changed = restoreViewAnchor(scroller, anchor);
  assert.equal(changed, true);
  assert.equal(scroller.scrollTop, 3650);
});

test("restoreViewAnchor falls back to captured scrollTop when anchor element is detached", () => {
  const scroller = makeFakeScroller({
    scrollTop: 2800,
    scrollHeight: 5100,
    clientHeight: 800,
    rows: [
      { msgIndex: 0, contentTop: 40 },
      { msgIndex: 1, contentTop: 2000 }
    ]
  });

  const anchor = captureViewAnchor(scroller, scroller as unknown as Element);
  assert.ok(anchor);

  // Simulate full row replacement (detached element reports rect.top = 0) and clamped scrollTop
  scroller.rows[1].isConnected = false;
  scroller.rows[1].getBoundingClientRect = () => ({ top: 0 });
  scroller.scrollTop = 1200;

  const changed = restoreViewAnchor(scroller, anchor);
  assert.equal(changed, true);
  assert.equal(scroller.scrollTop, 2800);
});

test("captureOpenToolDetails and restoreOpenToolDetails preserve open tool cards across re-render", () => {
  const scroller = makeFakeScroller({
    scrollTop: 2000,
    scrollHeight: 4000,
    clientHeight: 800,
    rows: [
      { msgIndex: 0, contentTop: 40 },
      {
        msgIndex: 1,
        contentTop: 120,
        details: [{ open: false }, { open: true }, { open: false }, { open: true }]
      }
    ]
  });

  const snap = captureOpenToolDetails(scroller as unknown as Element);
  assert.deepEqual(snap, [{ msgIndex: 1, openIndices: [1, 3] }]);

  // Simulate Svelte replacing {@html}: all existing details recreated closed + a new 5th card appended
  scroller.rows[1].details = [
    { open: false },
    { open: false },
    { open: false },
    { open: false },
    { open: false }
  ];

  restoreOpenToolDetails(scroller as unknown as Element, snap);
  assert.deepEqual(
    scroller.rows[1].details.map((d) => d.open),
    [false, true, false, true, false]
  );
});
