import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync } from "node:fs";
import { latestContextStartIndex } from "../src/lib/sessionEngine.ts";

const root = new URL("..", import.meta.url).pathname;
const app = readFileSync(root + "src/lib/ChatApp.svelte", "utf8");

test("/clear is a Work slash command with a custom confirmation modal", () => {
  assert.match(app, /name: "clear", desc:/);
  assert.match(app, /data-testid="clear-context-modal"/);
  assert.match(app, /data-testid="confirm-clear-context"/);
  assert.match(app, /Earlier messages stay visible above the line but are no longer sent to the model/);
  assert.match(app, /data-testid="context-cleared-divider"/);
});

test("context clear marker resets history while preserving later turns", () => {
  const messages = [
    { role: "user", content: "old request" },
    { role: "assistant", content: "old reply" },
    { role: "assistant", content: "", contextCleared: true },
    { role: "user", content: "fresh request" },
    { role: "assistant", content: "fresh reply" }
  ];
  assert.deepEqual(messages.slice(latestContextStartIndex(messages)), [
    { role: "user", content: "fresh request" },
    { role: "assistant", content: "fresh reply" }
  ]);
});
