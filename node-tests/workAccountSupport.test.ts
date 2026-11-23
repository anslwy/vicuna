import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { test } from "node:test";

test("connected desktop discovers Antigravity accounts through the server", async (t) => {
  const calls: { id: string; path: string; body: { cmd: string; args: unknown } }[] = [];
  let replies: Record<string, { ok: boolean; data?: unknown }> = {};
  t.mock.method(globalThis, "fetch", () => assert.fail("Account commands must use desktop server_call"));
  const globals = {
    window: {
      location: { search: "?server=fixture" },
      __TAURI_INTERNALS__: {
        invoke: async (cmd: string, request: (typeof calls)[number]) => {
          assert.equal(cmd, "server_call");
          calls.push(request);
          return replies[request.body.cmd] ?? { ok: false };
        }
      }
    },
    sessionStorage: { setItem() {} }
  };
  Object.assign(globalThis, globals);
  t.after(() => {
    Reflect.deleteProperty(globalThis, "window");
    Reflect.deleteProperty(globalThis, "sessionStorage");
  });

  // Node's TS runner needs explicit extensions, while Vite resolves these imports.
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
  const { addWorkAccount, workAccountSupport } = await import("../src/lib/work.ts");

  const generic = (engines: string[]) => ({ ok: true, data: { engines } });
  const legacy = (supported: boolean) => ({ ok: true, data: { supported, accounts: [] } });
  for (const [general, agy, expected, commands] of [
    [generic(["claude", "agy"]), undefined, ["claude", "agy"], ["work_account_support"]],
    [generic(["claude", "codex"]), legacy(true), ["claude", "codex", "agy"], ["work_account_support", "agy_account_list"]],
    [undefined, legacy(true), ["agy"], ["work_account_support", "agy_account_list"]],
    [generic(["codex"]), legacy(false), ["codex"], ["work_account_support", "agy_account_list"]],
    [generic(["codex"]), undefined, ["codex"], ["work_account_support", "agy_account_list"]]
  ] as const) {
    calls.length = 0;
    replies = { work_account_support: general ?? { ok: false }, agy_account_list: agy ?? { ok: false } };
    assert.deepEqual(await workAccountSupport(), expected);
    assert.deepEqual(calls.map(({ id, path, body }) => ({ id, path, body })), commands.map((cmd) => ({
      id: "fixture", path: "/invoke", body: { cmd, args: {} }
    })));
  }

  calls.length = 0;
  const account = { id: "work", engine: "agy@work", label: "Work", home: "/fixture/work", signedIn: false, signInCommand: "agy" };
  replies = { agy_account_add: { ok: true, data: account } };
  assert.deepEqual(await addWorkAccount("agy", "Work"), account);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].id, "fixture");
  assert.deepEqual(calls[0].body, { cmd: "agy_account_add", args: { label: "Work" } });
});
