import assert from "node:assert/strict";
import test from "node:test";
import { handlePanelRequest, panelUrl, parsePanelRequest, type PanelHost } from "../src/lib/pluginBridge.ts";

test("only well-formed requests for known methods are accepted", () => {
  assert.deepEqual(parsePanelRequest({ vicuna: 1, id: 1, method: "getTheme" }), { id: 1, method: "getTheme", params: {} });
  assert.equal(parsePanelRequest({ vicuna: 1, id: "a", method: "storage.get", params: { key: "k" } })?.method, "storage.get");
  for (const bad of [
    null, "x", 3, {}, { id: 1, method: "getTheme" },
    { vicuna: 2, id: 1, method: "getTheme" },
    { vicuna: 1, id: {}, method: "getTheme" },
    { vicuna: 1, id: 1, method: "exec" },
    { vicuna: 1, id: 1, method: "storage.get", params: [] },
    { vicuna: 1, id: 1, method: "__proto__" },
    { vicuna: 1, id: "x".repeat(65), method: "getTheme" }
  ]) assert.equal(parsePanelRequest(bad), null, JSON.stringify(bad));
});

function fakeHost() {
  const store = new Map<string, unknown>();
  const calls: string[] = [];
  const titles: (string | null)[] = [];
  const host: PanelHost = {
    theme: () => ({ base: "dark", vars: { "--bg": "#000" } }),
    context: () => ({ sessionId: "s1", title: "T", workspace: "/ws", mode: "work" }),
    setTitle: (t) => { titles.push(t); },
    async storage(cmd, key, value) {
      calls.push(cmd);
      if (cmd === "plugin_storage_get") return store.get(key!) ?? null;
      if (cmd === "plugin_storage_set") return void store.set(key!, value), true;
      if (cmd === "plugin_storage_remove") return void store.delete(key!), true;
      return [...store.keys()];
    }
  };
  return { host, store, calls, titles };
}

test("requests are answered with the same id, errors as messages", async () => {
  const { host, store, titles } = fakeHost();
  const ask = (method: string, params?: object) => handlePanelRequest(parsePanelRequest({ vicuna: 1, id: 7, method, params })!, host);
  assert.deepEqual(await ask("getTheme"), { vicuna: 1, id: 7, result: { base: "dark", vars: { "--bg": "#000" } } });
  assert.equal(((await ask("getContext")).result as any).workspace, "/ws");
  assert.deepEqual(await ask("setTitle", { title: "25:00 - Time to focus!" }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(await ask("setTitle", { title: "  04:56\n-\tBreak  " }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(await ask("setTitle", { title: "x".repeat(80) }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(await ask("setTitle", { title: "   " }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(await ask("setTitle", { title: null }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(titles, ["25:00 - Time to focus!", "04:56 - Break", "x".repeat(64), null, null]);
  assert.match((await ask("setTitle", { title: 42 })).error!, /title must be/);
  assert.deepEqual(await ask("storage.set", { key: "note", value: { text: "hi" } }), { vicuna: 1, id: 7, result: true });
  assert.deepEqual(store.get("note"), { text: "hi" });
  assert.deepEqual((await ask("storage.get", { key: "note" })).result, { text: "hi" });
  assert.deepEqual((await ask("storage.keys")).result, ["note"]);
  await ask("storage.remove", { key: "note" });
  assert.equal((await ask("storage.get", { key: "note" })).result, null);
  assert.match((await ask("storage.get", {})).error!, /key must be/);
  assert.match((await ask("storage.get", { key: "a\nb" })).error!, /key must be/);
  assert.match((await ask("storage.set", { key: "big", value: "x".repeat(70_000) })).error!, /at most 64 KB/);
});

test("panel URLs only come from a well-formed server path", () => {
  const pb = "/plugin-panel/scratchpad/0123abcd/";
  assert.equal(panelUrl("http://127.0.0.1:7421/", pb, "panel.html"), "http://127.0.0.1:7421/plugin-panel/scratchpad/0123abcd/panel.html");
  assert.equal(panelUrl(null, pb, "panel.html"), null);
  assert.equal(panelUrl("http://h", "/plugin-panel/x/../../", "panel.html"), null);
  assert.equal(panelUrl("http://h", pb, "../install.json"), null);
  assert.equal(panelUrl("http://h", pb, ".env"), null);
});
