import assert from "node:assert/strict";
import test from "node:test";
import {
  allThemes,
  browserThemeParams,
  colorToHex,
  readAccentMuted,
  readActiveTheme,
  reconcileActive,
  resolveTerminalColors,
  safeColor,
  scopedKey,
  themeProperties,
  writeAccentMuted,
  writeActiveTheme,
  type InstalledPlugin
} from "../src/lib/pluginTheme.ts";

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

test("the swatch row follows the theme accent until a built-in swatch mutes it", async () => {
  const { selectedAccent, themeAccent, themeProperties } = await import("../src/lib/pluginTheme.ts");
  const t = plugin().themes[0];
  assert.deepEqual(themeAccent(t), { bg: "#88c0d0", hover: "#88c0d0", fg: "#2e3440" });
  assert.equal(selectedAccent(t, false, "orange"), "theme");
  assert.equal(selectedAccent(t, true, "blue"), "blue");
  assert.equal(selectedAccent(null, false, "orange"), "orange");
  assert.equal(selectedAccent({ ...t, accent: null }, false, "green"), "green");
  assert.equal(themeProperties(t, false)["--accent"], undefined);
});

test("Install is usable while the plugin list is still loading", async () => {
  const { installBlocked } = await import("../src/lib/pluginTheme.ts");
  assert.equal(installBlocked("me/repo", null), false);
  assert.equal(installBlocked("me/repo", "list"), false, "a list load never blocks Install");
  assert.equal(installBlocked("  ", null), true);
  assert.equal(installBlocked("me/repo", "install"), true);
  assert.equal(installBlocked("me/repo", "upgrade:x"), true);
});

test("plugin theme and muted accent are isolated per remote host", () => {
  const m = new Map<string, string>();
  const st = { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) };
  const [nord] = allThemes([plugin()]);
  const [rose] = allThemes([{ ...plugin(), id: "rose", name: "Rose", themes: [{ id: "light", name: "Rose Light", base: "light", vars: { "--bg": "#faf4ed" } }] }]);

  assert.equal(scopedKey("vicuna-plugin-theme", null), "vicuna-plugin-theme");
  assert.equal(scopedKey("vicuna-plugin-theme", "host-a"), "vicuna-plugin-theme:server:host-a");

  // Host A picks Nord, client stays on built-in (null), Host B picks Rose
  writeActiveTheme(st, nord, "host-a");
  writeAccentMuted(st, nord.key, "host-a");
  writeActiveTheme(st, rose, "host-b");

  assert.equal(readActiveTheme(st, null), null, "client does not inherit host A's plugin theme");
  assert.equal(readActiveTheme(st, "host-a")?.key, "nord/dark");
  assert.equal(readActiveTheme(st, "host-b")?.key, "rose/light");
  assert.equal(readAccentMuted(st, "host-a"), "nord/dark");
  assert.equal(readAccentMuted(st, null), null);
  assert.equal(readAccentMuted(st, "host-b"), null);

  // Clearing client's theme (e.g. client has no plugins or switches to Dark) leaves hosts intact
  writeActiveTheme(st, null, null);
  writeAccentMuted(st, null, null);
  assert.equal(readActiveTheme(st, "host-a")?.key, "nord/dark");
  assert.equal(readAccentMuted(st, "host-a"), "nord/dark");
  assert.equal(readActiveTheme(st, "host-b")?.key, "rose/light");

  // Clearing Host A leaves Host B intact
  writeActiveTheme(st, null, "host-a");
  writeAccentMuted(st, null, "host-a");
  assert.equal(readActiveTheme(st, "host-a"), null);
  assert.equal(readAccentMuted(st, "host-a"), null);
  assert.equal(readActiveTheme(st, "host-b")?.key, "rose/light");
});

test("colorToHex normalizes hex, rgb, and hsl colors", () => {
  assert.equal(colorToHex("#abc"), "aabbcc");
  assert.equal(colorToHex("#120607"), "120607");
  assert.equal(colorToHex("#12060780"), "12060780");
  assert.equal(colorToHex("rgb(18, 6, 7)"), "120607");
  assert.equal(colorToHex("rgba(255, 0, 0, 0.5)"), "ff000080");
  assert.equal(colorToHex("hsl(0deg 100% 50%)"), "ff0000");
  assert.equal(colorToHex("not-a-color"), null);
});

test("resolveTerminalColors follows built-in and plugin themes", () => {
  assert.deepEqual(resolveTerminalColors("dark", null), {
    background: "#3d3332",
    foreground: "#ece4e0",
    cursor: "#ece4e0",
    cursorAccent: "#3d3332"
  });
  assert.deepEqual(resolveTerminalColors("light", null), {
    background: "#ffffff",
    foreground: "#111111",
    cursor: "#111111",
    cursorAccent: "#ffffff"
  });
  const crimson = plugin("1.0.0", { "--bg": "#120607", "--text": "#f5e9ea" }).themes[0];
  assert.deepEqual(resolveTerminalColors("dark", crimson), {
    background: "#120607",
    foreground: "#f5e9ea",
    cursor: "#f5e9ea",
    cursorAccent: "#120607"
  });
});

test("browserThemeParams encodes the full palette and falls back cleanly for minimal plugin themes", () => {
  const qs = browserThemeParams(
    "dark",
    plugin("1.0.0", { "--bg": "#120607", "--card": "#1d0b0d", "--border": "#3b171b", "--text": "#f5e9ea" }).themes[0],
    { bg: "#ef4444", fg: "#ffffff" }
  );
  assert.equal(qs.get("theme"), "dark");
  assert.equal(qs.get("accent"), "ef4444");
  assert.equal(qs.get("accentfg"), "ffffff");
  assert.equal(qs.get("bg"), "120607");
  assert.equal(qs.get("fg"), "f5e9ea");
  assert.equal(qs.get("card"), "1d0b0d");
  assert.equal(qs.get("border"), "3b171b");
  // omitted secondary surfaces fall back to the plugin's own card/border, not #4e4442/#5d4f4d
  assert.equal(qs.get("inputbg"), "1d0b0d");
  assert.equal(qs.get("inputborder"), "3b171b");
  assert.equal(qs.get("ghostbg"), "1d0b0d");
});
