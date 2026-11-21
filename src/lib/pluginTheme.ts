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
