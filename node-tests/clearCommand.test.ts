import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { registerHooks } from "node:module";
import {
  canClearContext, isAssistantReply, latestContextStartIndex, undoMessageRange
} from "../src/lib/sessionEngine.ts";

const root = new URL("..", import.meta.url).pathname;
const app = readFileSync(root + "src/lib/ChatApp.svelte", "utf8");
const clearModal = readFileSync(root + "src/lib/ClearContextModal.svelte", "utf8");
const beforeClear = [
  { role: "user", content: "old request" },
  { role: "assistant", content: "old reply" }
];
const marker = { role: "assistant", content: "", contextCleared: true };
const afterClear = [
  { role: "user", content: "fresh request" },
  { role: "assistant", content: "fresh reply" }
];

test("/clear is a Work slash command with a custom confirmation modal", () => {
  assert.match(app, /name: "clear", desc:/);
  assert.match(app, /<ClearContextModal\b/);
  assert.match(clearModal, /data-testid="clear-context-modal"/);
  assert.match(clearModal, /data-testid="confirm-clear-context"/);
  assert.match(clearModal, /Earlier messages stay visible above the line but are no longer sent to the model/);
  assert.match(app, /data-testid="context-cleared-divider"/);
});

test("context clear marker resets history while preserving later turns", () => {
  const messages = [...beforeClear, marker, ...afterClear];
  assert.deepEqual(messages.slice(latestContextStartIndex(messages)), afterClear);
});

test("/clear is disabled when the visible history is empty", () => {
  assert.equal(canClearContext([]), false);
  assert.equal(canClearContext(beforeClear), true);
  assert.equal(canClearContext([...beforeClear, marker]), false);
  assert.equal(canClearContext([...beforeClear, marker, ...afterClear]), true);
});

test("/undo targets only a trailing clear or the latest real turn after it", () => {
  const cleared = [...beforeClear, marker];
  assert.deepEqual(undoMessageRange(cleared), { index: 2, count: 1 });
  assert.deepEqual(undoMessageRange([...cleared, ...afterClear]), { index: 3, count: 2 });
  assert.equal(undoMessageRange([...cleared, afterClear[0]]), null);
  assert.equal(cleared.findLast(isAssistantReply)?.content, "old reply");
});

test("JSON save and reload preserve a clear marker and the history boundary", async (t) => {
  const lib = new URL("../src/lib/", import.meta.url).href;
  const hooks = registerHooks({
    resolve(specifier, context, nextResolve) {
      if (context.parentURL?.startsWith(lib) && /^\.\/\w+$/.test(specifier)) {
        return nextResolve(`${specifier}.ts`, context);
      }
      return nextResolve(specifier, context);
    }
  });
  t.after(() => hooks.deregister());
  const { normalizeServerChat, toSavePayload } = await import("../src/lib/store.ts");
  const chat = normalizeServerChat({
    id: "session", mode: "work", messages: [...beforeClear, marker, ...afterClear]
  });
  const [saved] = JSON.parse(JSON.stringify(toSavePayload([chat])));
  assert.equal(saved.messages[2].contextCleared, true);
  const reloaded = normalizeServerChat(saved);
  assert.equal(reloaded.messages[0].contextCleared, undefined);
  assert.deepEqual(reloaded.messages, chat.messages);
  assert.equal(latestContextStartIndex(reloaded.messages), 3);
});
