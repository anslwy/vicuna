import test from "node:test";
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { chatExtraJson, parseChatExtra, sessionPermissions } from "../src/lib/leadSessions.ts";

test("session permission defaults follow lead status and do not override stored values", () => {
  assert.deepEqual(sessionPermissions({}), { read: true, message: false, remote: false });
  assert.deepEqual(sessionPermissions({ role: "lead" }), { read: true, message: true, remote: false });
  assert.equal(sessionPermissions({ role: "lead", permissions: { remote: true } }).remote, true);
  assert.deepEqual(sessionPermissions({ role: "lead", permissions: { message: false } }), { read: true, message: false, remote: false });
  assert.deepEqual(sessionPermissions({ permissions: { read: false, message: true } }), { read: false, message: true, remote: false });
});

test("permissions persist in session extra JSON", () => {
  const raw = chatExtraJson({ role: "lead", permissions: { read: false, message: false } });
  assert.deepEqual(parseChatExtra(raw), { role: "lead", permissions: { read: false, message: false } });
});

test("the remote-servers permission round-trips through extra JSON", () => {
  const raw = chatExtraJson({ role: "lead", permissions: { remote: true } });
  assert.deepEqual(parseChatExtra(raw), { role: "lead", permissions: { remote: true } });
});

test("sidebar menu exposes the permissions modal and both toggles", async () => {
  const read = (f: string) => readFile(new URL(`../src/lib/${f}`, import.meta.url), "utf8");
  const app = await read("ChatApp.svelte");
  const menu = await read("SessionContextMenu.svelte");
  const modal = await read("PermissionsModal.svelte");
  assert.ok(app.includes("<SessionContextMenu"));
  assert.ok(menu.includes('data-testid="permissions-item"'));
  assert.match(menu, /\{#if serverPersist\}\s*<button class="ctx-item" data-testid="permissions-item"/);
  assert.ok(app.includes("<PermissionsModal"));
  assert.ok(modal.includes('data-testid="permissions-modal"'));
  assert.ok(modal.includes('data-testid="permission-read"'));
  assert.ok(modal.includes('data-testid="permission-message"'));
  assert.ok(modal.includes('data-testid="permission-remote"'));
  assert.ok(app.includes('window.addEventListener("keydown", onEscape, true)'));
  assert.ok(app.includes("[data-testid=\"permission-read\"]')?.focus()"));
});
