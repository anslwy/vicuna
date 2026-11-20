// Lead session helpers + per-session extra fields (run: `npm test`).
import { test } from "node:test";
import assert from "node:assert/strict";
import { chatExtraJson, demoteOtherLeads, parseChatExtra, parseWorkerDone, sessionListText, tailText, workerDoneMessage } from "../src/lib/leadSessions.ts";

test("worker-done messages round-trip and carry the reply", () => {
  const msg = workerDoneMessage({ id: "abc-1", title: 'Fix "tests"', engine: "codex", model: "gpt-6-luna" }, "All green.", false);
  assert.ok(msg.includes("codex/gpt-6-luna"));
  assert.ok(msg.includes("All green."));
  assert.deepEqual(parseWorkerDone(msg), { title: 'Fix "tests"', id: "abc-1" });
  assert.ok(workerDoneMessage({ id: "x", title: "t", engine: "claude", model: null }, "", true).includes("stopped"));
  assert.equal(parseWorkerDone("hello"), null);
});

test("long replies keep their tail", () => {
  const long = "a".repeat(100) + "\nEND";
  const t = tailText(long, 50);
  assert.ok(t.startsWith("…") && t.endsWith("END") && t.length <= 51);
  assert.equal(tailText(" short ", 50), "short");
});

test("list text tags the lead and its workers", () => {
  const txt = sessionListText(
    [
      { id: "L", title: "lead", status: "running", engine: "claude", model: null, workspace: "/w", role: "lead", updatedAt: 2 },
      { id: "W", title: "w", status: "idle", engine: "codex", model: "m", workspace: "/w", role: "worker", updatedAt: 1 },
      { id: "A", title: "old", status: "idle", engine: "claude", model: null, workspace: null, role: "other", updatedAt: 0, archived: true }
    ],
    ["claude", "codex"],
    "claude"
  );
  assert.ok(txt.includes("Installed engines: claude, codex (default: claude)"));
  assert.ok(txt.includes('- L · "lead" [lead, you] · running · claude · /w'));
  assert.ok(txt.includes('- W · "w" [your worker] · idle · codex/m'));
  assert.ok(txt.includes('- A · "old" [archived] · idle'));
  assert.ok(sessionListText([], [], "opencode").includes("No other Work sessions."));
});

test("extra column keeps only set fields and survives a round trip", () => {
  const base = { id: "1", title: "t", createdAt: 0, messages: [] };
  assert.equal(chatExtraJson(base), null);
  const raw = chatExtraJson({ ...base, engine: "codex", workModel: "m", role: "lead", leadId: "L", pinned: true, archived: true });
  assert.deepEqual(parseChatExtra(raw), { engine: "codex", workModel: "m", role: "lead", leadId: "L", pinned: true, archived: true });
  assert.equal(chatExtraJson({ ...base, archived: false }), null);
  assert.deepEqual(parseChatExtra({ archived: "yes" }), {});
  assert.deepEqual(parseChatExtra("not json"), {});
  assert.deepEqual(parseChatExtra({ role: "boss", pinned: "yes" }), {});
});

test("demoteOtherLeads enforces a single lead session", () => {
  const chats: Array<{ id: string; role?: "lead" | null; pinned?: boolean }> = [
    { id: "a", role: "lead", pinned: true },
    { id: "b", role: null, pinned: false },
    { id: "c", role: "lead", pinned: true }
  ];
  assert.deepEqual(demoteOtherLeads(chats, "b"), ["a", "c"]);
  assert.equal(chats[0].role, null);
  assert.equal(chats[0].pinned, true);
  assert.equal(chats[2].role, null);
  chats[1].role = "lead";
  assert.deepEqual(demoteOtherLeads(chats, "b"), []);
  assert.equal(chats[1].role, "lead");
});
