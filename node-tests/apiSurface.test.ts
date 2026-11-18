// Every backend command the frontend calls is either a server API action
// (src-tauri/src/server/mod.rs API_ACTIONS) or explicitly desktop-only
// (src/lib/bridge.ts DESKTOP_CMDS). Nothing silently falls back to the app:
// a new feature that touches the database must land as an API action first,
// so the CLI / headless tests / other frontends reach it too.
import assert from "node:assert/strict";
import test from "node:test";
import { readFileSync, readdirSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const read = (p: string) => readFileSync(root + p, "utf8");

function stringSet(src: string, start: string, end: string): Set<string> {
  const a = src.indexOf(start);
  assert.ok(a >= 0, `missing ${start}`);
  const body = src.slice(a, src.indexOf(end, a));
  return new Set([...body.matchAll(/"([a-z_]+)"/g)].map((m) => m[1]));
}

const api = stringSet(read("src-tauri/src/server/mod.rs"), "pub const API_ACTIONS", "];");
const desktop = stringSet(read("src/lib/bridge.ts"), "export const DESKTOP_CMDS", "]);");

function frontendCommands(): Map<string, string> {
  const out = new Map<string, string>();
  const files = [
    ...readdirSync(root + "src/lib").filter((f) => /\.(ts|svelte)$/.test(f)).map((f) => "src/lib/" + f),
    ...readdirSync(root + "src/routes").filter((f) => /\.svelte$/.test(f)).map((f) => "src/routes/" + f)
  ];
  for (const f of files) {
    const src = read(f);
    for (const m of src.matchAll(/\b(?:invokeOrBridge|invokeCmd|bridgeInvoke|serverInvoke|invoke)(?:<[^>()]*>)?\(\s*"([a-z_]+)"/g)) {
      if (!out.has(m[1])) out.set(m[1], f);
    }
  }
  return out;
}

test("every frontend command is an API action or explicitly desktop-only", () => {
  const missing = [...frontendCommands()].filter(([c]) => !api.has(c) && !desktop.has(c) && c !== "chats_save");
  assert.deepEqual(missing, [], "add these as server actions (or, if truly about this device, to DESKTOP_CMDS)");
});

test("desktop-only commands are not also server actions (one owner each)", () => {
  // app-update checks: the desktop asks about ITSELF; the server answers the
  // same for plain browsers on the bridge (#42) — about the server's build
  const sharedOnPurpose = new Set(["app_build_info", "github_latest_release", "github_download_asset"]);
  const both = [...desktop].filter((c) => api.has(c) && !sharedOnPurpose.has(c));
  assert.deepEqual(both, []);
});

test("the registry is non-trivial", () => {
  assert.ok(api.size > 50 && api.has("cli_session") && api.has("api_actions"));
});
