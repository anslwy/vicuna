import assert from "node:assert/strict";
import test from "node:test";
import { allThemes, reconcileActive, readActiveTheme, safeColor, themeProperties, writeActiveTheme, type InstalledPlugin } from "../src/lib/pluginTheme.ts";

const plugin = (version = "1.0.0", vars: Record<string, string> = { "--bg": "#2e3440" }): InstalledPlugin => ({
  id: "nord", name: "Nord", version, description: "", source: "me/nord", installedAt: 0,
  themes: [{ id: "dark", name: "Nord Dark", base: "dark", vars, accent: { bg: "#88c0d0", fg: "#2e3440" } }]
});

test("colors are plain colors only", () => {
  for (const ok of ["#fff", "#2e3440ff", "rgb(1, 2, 3)", "hsl(210deg 40% 20% / .5)"]) assert.ok(safeColor(ok), ok);
  for (const bad of ["red", "url(x)", "var(--a)", "#fff;}", 3]) assert.ok(!safeColor(bad), String(bad));
});

test("theme properties include the accent and drop unsafe entries", () => {
  const p = themeProperties({ ...plugin().themes[0], vars: { "--bg": "#000", "--x": "url(evil)", "bad name": "#fff" } });
  assert.deepEqual(p, { "--bg": "#000", "--accent": "#88c0d0", "--accent-hover": "#88c0d0", "--accent-fg": "#2e3440" });
});

test("themes are keyed by plugin and reconcile after upgrade / uninstall", () => {
  const [t] = allThemes([plugin()]);
  assert.equal(t.key, "nord/dark");
  assert.equal(reconcileActive(t, [plugin("1.1.0", { "--bg": "#111111" })])?.vars["--bg"], "#111111");
  assert.equal(reconcileActive(t, []), null);
  assert.equal(reconcileActive(null, [plugin()]), null);
});

test("the active theme round-trips through storage", () => {
  const m = new Map<string, string>();
  const st = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
  const [t] = allThemes([plugin()]);
  writeActiveTheme(st, t);
  assert.equal(readActiveTheme(st)?.key, "nord/dark");
  writeActiveTheme(st, null);
  assert.equal(readActiveTheme(st), null);
  m.set("vicuna-plugin-theme", "{bad");
  assert.equal(readActiveTheme(st), null);
});
