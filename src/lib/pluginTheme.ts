// Plugin themes (pure; node-tested in node-tests/pluginTheme.test.ts).
// The active plugin theme is cached whole in localStorage so it applies
// before the server answers (and pre-paint in app.html), and survives a
// server that is temporarily unreachable.

export type PluginAccent = { bg: string; hover?: string | null; fg?: string | null };
export type PluginThemeDef = { id: string; name: string; base: "light" | "dark"; vars: Record<string, string>; accent?: PluginAccent | null };
export type InstalledPlugin = {
  id: string;
  name: string;
  version: string;
  description: string;
  source: string;
  installedAt: number;
  themes: PluginThemeDef[];
  /** Side panels (plugins.rs `Panel`) and their server path prefix. */
  panels?: { id: string; title: string; icon?: string | null; entry: string }[];
  panelBase?: string;
  /** Work harnesses (plugin_harness.rs), with consent state. */
  harnesses?: import("./pluginHarness").PluginHarness[];
  pendingConsent?: import("./pluginHarness").PluginHarness[];
  /** Messaging bridge (plugins.rs `BridgeSpec`). */
  bridge?: import("./bridgePlugin").PluginBridgeDef | null;
};
/** What we cache: the theme plus its plugin-qualified key. */
export type ActivePluginTheme = PluginThemeDef & { key: string; pluginName: string };

export const ACTIVE_THEME_KEY = "vicuna-plugin-theme";
export const ACCENT_MUTED_KEY = "vicuna-plugin-accent-muted";

/** Scope a localStorage key to a remote server when `serverId` is set,
 *  matching `serverKey` in src/lib/store.ts and the pre-paint script in
 *  src/app.html. Null/undefined keeps the local-client key. */
export function scopedKey(base: string, serverId?: string | null): string {
  return serverId ? `${base}:server:${serverId}` : base;
}

export const themeKey = (pluginId: string, themeId: string) => `${pluginId}/${themeId}`;

/** Same rules as the server's `valid_color`/`valid_var` (defense in depth:
 *  the cache is local and could be edited by hand). */
export function safeColor(v: unknown): v is string {
  if (typeof v !== "string") return false;
  const s = v.trim();
  return /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.test(s) || /^(rgb|rgba|hsl|hsla)\([0-9 .,%/deg]+\)$/i.test(s);
}
export const safeVar = (n: string) => /^--[a-z0-9-]{1,62}$/i.test(n);

/** Every theme across installed plugins, as cacheable entries. */
export function allThemes(plugins: InstalledPlugin[]): ActivePluginTheme[] {
  return plugins.flatMap((p) => p.themes.map((t) => ({ ...t, key: themeKey(p.id, t.id), pluginName: p.name })));
}

/** The CSS properties a theme sets (accent included), unsafe entries dropped. */
export function themeProperties(t: PluginThemeDef, withAccent = true): Record<string, string> {
  const out: Record<string, string> = {};
  for (const [k, v] of Object.entries(t.vars ?? {})) if (safeVar(k) && safeColor(v)) out[k] = v.trim();
  const a = withAccent ? t.accent : null;
  if (a && safeColor(a.bg)) {
    out["--accent"] = a.bg;
    out["--accent-hover"] = safeColor(a.hover) ? a.hover : a.bg;
    out["--accent-fg"] = safeColor(a.fg) ? a.fg : "#ffffff";
  }
  return out;
}

/** After a list refresh: the fresh copy of the active theme, or null when
 *  its plugin (or the theme) is gone. */
export function reconcileActive(active: ActivePluginTheme | null, plugins: InstalledPlugin[]): ActivePluginTheme | null {
  if (!active) return null;
  return allThemes(plugins).find((t) => t.key === active.key) ?? null;
}

export function readActiveTheme(storage: Pick<Storage, "getItem"> | undefined, serverId?: string | null): ActivePluginTheme | null {
  try {
    const v = JSON.parse(storage?.getItem(scopedKey(ACTIVE_THEME_KEY, serverId)) ?? "null");
    if (v && typeof v.key === "string" && (v.base === "light" || v.base === "dark") && v.vars && typeof v.vars === "object") return v;
  } catch {}
  return null;
}

export function writeActiveTheme(storage: Pick<Storage, "setItem" | "removeItem"> | undefined, t: ActivePluginTheme | null, serverId?: string | null) {
  try {
    const key = scopedKey(ACTIVE_THEME_KEY, serverId);
    if (t) storage?.setItem(key, JSON.stringify(t));
    else storage?.removeItem(key);
  } catch {}
}

export function readAccentMuted(storage: Pick<Storage, "getItem"> | undefined, serverId?: string | null): string | null {
  try {
    return storage?.getItem(scopedKey(ACCENT_MUTED_KEY, serverId)) ?? null;
  } catch {
    return null;
  }
}

export function writeAccentMuted(storage: Pick<Storage, "setItem" | "removeItem"> | undefined, mutedFor: string | null, serverId?: string | null) {
  try {
    const key = scopedKey(ACCENT_MUTED_KEY, serverId);
    if (mutedFor) storage?.setItem(key, mutedFor);
    else storage?.removeItem(key);
  } catch {}
}

/** The theme's own accent, when it brings a usable one. */
export function themeAccent(t: PluginThemeDef | null): { bg: string; hover: string; fg: string } | null {
  const a = t?.accent;
  if (!a || !safeColor(a.bg)) return null;
  return { bg: a.bg, hover: safeColor(a.hover) ? a.hover : a.bg, fg: safeColor(a.fg) ? a.fg : "#ffffff" };
}

/** Which accent swatch is selected: the theme's own ("theme") unless the
 *  user picked a built-in swatch while this theme was active (muted). */
export function selectedAccent(t: PluginThemeDef | null, muted: boolean, builtin: string): string {
  return themeAccent(t) && !muted ? "theme" : builtin;
}

/** Settings → Plugins: Install works as soon as a source is typed; only an
 *  install/upgrade/uninstall in flight blocks it (loading the list does not). */
export function installBlocked(source: string, busy: string | null): boolean {
  return !source.trim() || (busy !== null && busy !== "list");
}

/** Built-in Light / Dark base tokens (mirrored from ChatApp.svelte's :root and
 *  html[data-theme="dark"]). Used by pure helpers (terminal + live-browser) so
 *  they never depend on DOM timing or stale computed styles. */
export const BASE_PALETTES: Record<"light" | "dark", Record<string, string>> = {
  light: {
    "--bg": "#ffffff",
    "--text": "#111111",
    "--card": "#ffffff",
    "--border": "#ececec",
    "--composer": "#f4f4f5",
    "--input-border": "#dddddd",
    "--hover": "#f0f0f0",
    "--link": "#0b57d0",
    "--danger": "#b3261e",
    "--muted": "#9a9a9a"
  },
  dark: {
    "--bg": "#3d3332",
    "--text": "#ece4e0",
    "--card": "#4e4442",
    "--border": "#665b58",
    "--composer": "#4e4442",
    "--input-border": "#756866",
    "--hover": "#5d4f4d",
    "--link": "#7ed491",
    "--danger": "#ff8a80",
    "--muted": "#b3a6a3"
  }
};

function clampByte(n: number): string {
  return Math.max(0, Math.min(255, Math.round(n))).toString(16).padStart(2, "0");
}

/** Convert any CSS color accepted by `safeColor` (`#rgb`, `#rrggbb`,
 *  `#rrggbbaa`, `rgb(...)`, `rgba(...)`, `hsl(...)`, `hsla(...)`) to lowercase
 *  hex without a leading `#` (6 or 8 hex digits). Returns null if unparseable. */
export function colorToHex(color: string | undefined | null): string | null {
  if (!color) return null;
  const s = color.trim();
  const hex = /^#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i.exec(s);
  if (hex) {
    const h = hex[1].toLowerCase();
    if (h.length === 3 || h.length === 4) {
      return h.split("").map((c) => c + c).join("");
    }
    return h;
  }
  const rgb = /^rgba?\(\s*([0-9.]+%?)\s*[,\s]\s*([0-9.]+%?)\s*[,\s]\s*([0-9.]+%?)(?:\s*[,/]\s*([0-9.]+%?))?\s*\)$/i.exec(s);
  if (rgb) {
    const ch = (v: string) => (v.endsWith("%") ? (parseFloat(v) / 100) * 255 : parseFloat(v));
    const base = clampByte(ch(rgb[1])) + clampByte(ch(rgb[2])) + clampByte(ch(rgb[3]));
    if (rgb[4] !== undefined) {
      const a = rgb[4].endsWith("%") ? parseFloat(rgb[4]) / 100 : parseFloat(rgb[4]);
      if (a < 1) return base + clampByte(a * 255);
    }
    return base;
  }
  const hsl = /^hsla?\(\s*([0-9.]+)(?:deg)?\s*[,\s]\s*([0-9.]+)%\s*[,\s]\s*([0-9.]+)%(?:\s*[,/]\s*([0-9.]+%?))?\s*\)$/i.exec(s);
  if (hsl) {
    const h = (((parseFloat(hsl[1]) % 360) + 360) % 360) / 360;
    const sat = Math.max(0, Math.min(1, parseFloat(hsl[2]) / 100));
    const l = Math.max(0, Math.min(1, parseFloat(hsl[3]) / 100));
    const q = l < 0.5 ? l * (1 + sat) : l + sat - l * sat;
    const p = 2 * l - q;
    const hue2rgb = (t: number) => {
      const u = t < 0 ? t + 1 : t > 1 ? t - 1 : t;
      if (u < 1 / 6) return p + (q - p) * 6 * u;
      if (u < 1 / 2) return q;
      if (u < 2 / 3) return p + (q - p) * (2 / 3 - u) * 6;
      return p;
    };
    const r = sat === 0 ? l : hue2rgb(h + 1 / 3);
    const g = sat === 0 ? l : hue2rgb(h);
    const b = sat === 0 ? l : hue2rgb(h - 1 / 3);
    const base = clampByte(r * 255) + clampByte(g * 255) + clampByte(b * 255);
    if (hsl[4] !== undefined) {
      const a = hsl[4].endsWith("%") ? parseFloat(hsl[4]) / 100 : parseFloat(hsl[4]);
      if (a < 1) return base + clampByte(a * 255);
    }
    return base;
  }
  return null;
}

/** Effective chrome colors for xterm.js so the terminal matches the active
 *  built-in or plugin theme (light, dark, or custom palette). */
export function resolveTerminalColors(
  base: "light" | "dark",
  pluginTheme: PluginThemeDef | null
): { background: string; foreground: string; cursor: string; cursorAccent: string } {
  const b = BASE_PALETTES[base];
  const p = pluginTheme ? themeProperties(pluginTheme, false) : {};
  const bgHex = colorToHex(p["--bg"]) ?? colorToHex(b["--bg"])!;
  const fgHex = colorToHex(p["--text"]) ?? colorToHex(b["--text"])!;
  const background = `#${bgHex}`;
  const foreground = `#${fgHex}`;
  return { background, foreground, cursor: foreground, cursorAccent: background };
}

/** Build the query string parameters for the embedded `live-browser` iframe
 *  so its entire palette matches Vicuna's active theme (including plugin
 *  themes) pre-paint. Older `lb` builds ignore the extra palette params and
 *  keep using `theme`/`accent`/`accentfg`. */
export function browserThemeParams(
  base: "light" | "dark",
  pluginTheme: PluginThemeDef | null,
  accent: { bg: string; fg: string }
): URLSearchParams {
  const b = BASE_PALETTES[base];
  const p = pluginTheme ? themeProperties(pluginTheme, false) : {};
  const qs = new URLSearchParams();
  qs.set("theme", base);
  const acc = colorToHex(accent.bg);
  const accFg = colorToHex(accent.fg);
  if (acc) qs.set("accent", acc);
  if (accFg) qs.set("accentfg", accFg);

  // When a plugin theme omits secondary surface tokens (e.g. --composer or
  // --hover), fall back through the plugin's own surfaces before the built-in
  // base palette so brownish dark defaults never bleed into a custom theme.
  const pick = (...vals: (string | undefined)[]) => {
    for (const v of vals) {
      const h = colorToHex(v);
      if (h) return h;
    }
    return null;
  };
  const entries: [string, string | null][] = [
    ["bg", pick(p["--bg"], b["--bg"])],
    ["fg", pick(p["--text"], b["--text"])],
    ["card", pick(p["--card"], p["--item-bg"], p["--bg"], b["--card"])],
    ["border", pick(p["--border"], b["--border"])],
    ["inputbg", pick(p["--composer"], p["--item-bg"], p["--card"], b["--composer"])],
    ["inputborder", pick(p["--input-border"], p["--border-2"], p["--border"], b["--input-border"])],
    ["ghostbg", pick(p["--hover"], p["--active-item"], p["--item-bg"], p["--card"], b["--hover"])],
    ["link", pick(p["--link"], b["--link"])],
    ["danger", pick(p["--danger"], b["--danger"])],
    ["marker", pick(p["--muted"], p["--text-dim"], b["--muted"])]
  ];
  for (const [k, v] of entries) {
    if (v) qs.set(k, v);
  }
  return qs;
}
