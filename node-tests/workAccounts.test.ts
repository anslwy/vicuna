// Work account grouping and session handoff.
import { test } from "node:test";
import assert from "node:assert/strict";
import { baseEngineId, changesCliAccount, isAccountEngine, usageAccountView, workReplyModel } from "../src/lib/workGate.ts";

test("account engine ids map to their harness", () => {
  assert.equal(baseEngineId("agy@free"), "agy");
  assert.equal(baseEngineId("agy"), "agy");
  assert.equal(baseEngineId("claude"), "claude");
  assert.equal(isAccountEngine("agy@work-paid"), true);
  assert.equal(isAccountEngine("plugin:muse/muse"), false);
  assert.equal(baseEngineId("claude@work"), "claude");
  assert.equal(baseEngineId("codex@work"), "codex");
});

const acct = (id: string, label: string) => ({ id: `agy@${id}`, label: `Antigravity · ${label}`, account: { id, engine: `agy@${id}`, label, home: "/h", signedIn: true, signInCommand: "x" } });
const tabs = [{ id: "claude", label: "Claude Code" }, { id: "agy", label: "Antigravity" }, acct("free", "Free"), acct("paid", "Paid")];

test("usage view: one tab per harness, account chips only for a harness with accounts", () => {
  const v = usageAccountView(tabs, "agy@paid");
  assert.deepEqual(v.harnessTabs.map((t) => t.id), ["claude", "agy"]);
  assert.equal(v.selectedBase, "agy");
  assert.deepEqual(v.chips, [{ id: "agy", label: "Default" }, { id: "agy@free", label: "Free" }, { id: "agy@paid", label: "Paid" }]);
  assert.deepEqual(usageAccountView(tabs, "claude").chips, []);
  // no extra accounts: the view is unchanged
  assert.deepEqual(usageAccountView(tabs.slice(0, 2), "agy").chips, []);
});


test("Claude and Codex usage chips keep same-named accounts separate", () => {
  const rows = [
    { id: "claude", label: "Claude Code" },
    { id: "claude@work", label: "Claude Code · Work" },
    { id: "codex", label: "Codex" },
    { id: "codex@work", label: "Codex · Work" },
  ];
  for (const harness of ["claude", "codex"]) {
    const v = usageAccountView(rows, `${harness}@work`);
    assert.deepEqual(v.harnessTabs.map((tab) => tab.id), ["claude", "codex"]);
    assert.deepEqual(v.chips, [{ id: harness, label: "Default" }, { id: `${harness}@work`, label: "work" }]);
  }
});

test("Claude and Codex remember the account that produced each reply", () => {
  for (const harness of ["claude", "codex"]) {
    const work = `${harness}@work`;
    const personal = `${harness}@personal`;
    const reply = workReplyModel(null, "shared-model", work);
    assert.equal(reply, `${work}/shared-model`);
    assert.equal(changesCliAccount(work, reply), false);
    assert.equal(changesCliAccount(personal, reply), true);
    assert.equal(changesCliAccount(work, `${personal}/shared-model`), true);
    assert.equal(changesCliAccount(harness, reply), true);
    assert.equal(changesCliAccount(work, null), false);
  }
});
