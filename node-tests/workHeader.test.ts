import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const workHeader = readFileSync(root + "src/lib/WorkHeader.svelte", "utf8");
const chatApp = readFileSync(root + "src/lib/ChatApp.svelte", "utf8");

test("WorkHeader renders Session ID block above the directory block with independent copy feedback", () => {
  const popupStart = workHeader.indexOf('data-testid="work-path-popup"');
  assert.ok(popupStart >= 0, "work-path-popup exists");

  const sessionSectionIdx = workHeader.indexOf('data-testid="work-session-id-section"', popupStart);
  const sessionCopyIdx = workHeader.indexOf('data-testid="work-session-id-copy"', popupStart);
  const sessionCodeIdx = workHeader.indexOf('data-testid="work-session-id"', popupStart);
  const dirSectionIdx = workHeader.indexOf('data-testid="work-path-section"', popupStart);
  const dirCopyIdx = workHeader.indexOf('data-testid="work-path-copy"', popupStart);
  const dirCodeIdx = workHeader.indexOf('data-testid="work-path-full"', popupStart);

  assert.ok(sessionSectionIdx > popupStart, "Session ID section is inside popover");
  assert.ok(dirSectionIdx > sessionSectionIdx, "Session ID block is rendered above the directory block");
  assert.ok(sessionCopyIdx > sessionSectionIdx && sessionCopyIdx < dirSectionIdx, "Session ID copy button is inside Session ID block");
  assert.ok(sessionCodeIdx > sessionCopyIdx && sessionCodeIdx < dirSectionIdx, "Session ID code box follows header row");
  assert.ok(dirCopyIdx > dirSectionIdx && dirCodeIdx > dirCopyIdx, "Directory copy button and code box remain inside directory block");

  const sessionBlock = workHeader.slice(sessionSectionIdx, dirSectionIdx);
  assert.ok(sessionBlock.includes("Session ID"), "Session ID label present");
  assert.ok(sessionBlock.includes("sessionIdCopied"), "Session ID copy button has its own copied state");
  assert.ok(sessionBlock.includes("copySessionId(trimmedSessionId)"), "Session ID copy button copies the session ID");
  assert.equal(sessionBlock.includes("workPathCopied"), false, "Session ID copy state is independent from directory copy state");

  const dirBlock = workHeader.slice(dirSectionIdx, workHeader.indexOf("</span>\n    </span>", dirSectionIdx));
  assert.ok(dirBlock.includes("Scratch directory") && dirBlock.includes("Workspace directory"), "Directory header labels present");
  assert.ok(dirBlock.includes("workPathCopied"), "Directory copy button uses workPathCopied state");
  assert.ok(dirBlock.includes("onCopyPath(wsFull)"), "Directory copy button copies wsFull");
  assert.equal(dirBlock.includes("sessionIdCopied"), false, "Directory copy state is independent from Session ID copy state");

  assert.ok(workHeader.includes("{#if trimmedSessionId}"), "Session ID block renders only when sessionId is non-empty");
});

test("WorkHeader top workspace trigger pill toggles popover without copying", () => {
  const triggerIdx = workHeader.indexOf('data-testid="work-path"');
  assert.ok(triggerIdx >= 0, "work-path trigger exists");
  const btnStart = workHeader.lastIndexOf("<button", triggerIdx);
  const btnEnd = workHeader.indexOf(">", triggerIdx);
  assert.ok(btnStart >= 0 && btnEnd > btnStart, "work-path trigger is a button");

  const triggerTag = workHeader.slice(btnStart, btnEnd + 1);
  assert.ok(triggerTag.includes("onclick={togglePopover}"), "trigger pill toggles/pins the popover");
  assert.equal(triggerTag.includes("onCopyPath"), false, "trigger pill does not call onCopyPath");
  assert.equal(triggerTag.includes("copyText"), false, "trigger pill does not call copyText");
  assert.equal(triggerTag.includes("copySessionId"), false, "trigger pill does not call copySessionId");
  assert.equal(triggerTag.includes("Click to copy"), false, "trigger aria-label does not claim click to copy");

  const toggleStart = workHeader.indexOf("function togglePopover(");
  assert.ok(toggleStart >= 0, "togglePopover function exists");
  const toggleEnd = workHeader.indexOf("\n  }", toggleStart);
  const toggleBody = workHeader.slice(toggleStart, toggleEnd);
  assert.ok(toggleBody.includes("popoverPinned = !popoverPinned"), "togglePopover flips popoverPinned");
  assert.equal(toggleBody.includes("onCopyPath"), false, "togglePopover never copies workspace path");
  assert.equal(toggleBody.includes("copyText"), false, "togglePopover never writes to clipboard");
});

test("ChatApp passes activeChat.id as sessionId to WorkHeader", () => {
  const start = chatApp.indexOf("<WorkHeader");
  assert.ok(start >= 0, "ChatApp renders WorkHeader");
  const end = chatApp.indexOf("/>", start);
  const usage = chatApp.slice(start, end);
  assert.ok(usage.includes("sessionId={activeChat.id}"), "ChatApp passes sessionId={activeChat.id} to WorkHeader");
});
