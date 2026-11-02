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
