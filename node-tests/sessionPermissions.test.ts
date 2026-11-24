import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { chatExtraJson, parseChatExtra, sessionPermissions } from "../src/lib/leadSessions.ts";

test("session permission defaults follow lead status and do not override stored values", () => {
  assert.deepEqual(sessionPermissions({}), { read: true, message: false });
  assert.deepEqual(sessionPermissions({ role: "lead" }), { read: true, message: true });
  assert.deepEqual(sessionPermissions({ role: "lead", permissions: { message: false } }), { read: true, message: false });
  assert.deepEqual(sessionPermissions({ permissions: { read: false, message: true } }), { read: false, message: true });
});

test("permissions persist in session extra JSON", () => {
  const raw = chatExtraJson({ role: "lead", permissions: { read: false, message: false } });
  assert.deepEqual(parseChatExtra(raw), { role: "lead", permissions: { read: false, message: false } });
});

test("sidebar menu exposes the permissions modal and both toggles", async () => {
  const app = await readFile(new URL("../src/lib/ChatApp.svelte", import.meta.url), "utf8");
  assert.ok(app.includes('data-testid="permissions-item"'));
  assert.ok(app.includes('data-testid="permissions-modal"'));
  assert.ok(app.includes('data-testid="permission-read"'));
  assert.ok(app.includes('data-testid="permission-message"'));
});
