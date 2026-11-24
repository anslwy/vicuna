// Harness plugins (plugin_harness.rs): pure helpers for the consent dialog,
// the Plugins panel and reply labels.

export type PluginHarness = {
  id: string;
  label: string;
  /** Engine id: `plugin:<plugin-id>/<harness-id>` */
  engine: string;
  command: string;
  args: string[];
  models?: { id: string; label?: string | null }[];
  installHint?: string;
  installUrl?: string | null;
  /** Usage command (runs only after consent, shown in the dialog). */
  usage?: { command: string; args: string[] } | null;
  accounts?: "muse" | AccountSpec | null;
  enabled: boolean;
  installed: boolean;
};

/** Declarative named accounts (plugin_harness.rs `AccountSpec`): `env`
 *  values are folder names inside each account's own folder. */
export type AccountSpec = {
  env: Record<string, string>;
  clear?: string[];
  login: { command: string; args: string[] }[];
  signedIn: { file: string; pointer: string };
};

/** What the consent dialog lists for named accounts. `"muse"` is Vicuna's
 *  fixed adapter, shown with the same fields. */
export function accountSummary(a: PluginHarness["accounts"]): { folders: string[]; clear: string[]; login: string[] } | null {
  if (!a) return null;
  const spec: AccountSpec = a === "muse"
    ? { env: { XDG_CACHE_HOME: "cache", XDG_CONFIG_HOME: "config", XDG_DATA_HOME: "data", XDG_STATE_HOME: "state" }, clear: ["META_API_KEY"], login: [{ command: "muse", args: ["login"] }], signedIn: { file: "config/muse/auth.json", pointer: "/providers/meta" } }
    : a;
  return {
    folders: Object.entries(spec.env).map(([k, v]) => `${k} → <account>/${v}`),
    clear: [...(spec.clear ?? [])],
    login: spec.login.map((l) => commandLine(l)),
  };
}

export const PLUGIN_ENGINE_PREFIX = "plugin:";

export function isPluginEngine(id: string | null | undefined): boolean {
  return !!id && id.startsWith(PLUGIN_ENGINE_PREFIX);
}

/** `plugin:a/b` -> "b" (used as the reply-label prefix, like `claude/…`). */
export function pluginHarnessName(id: string): string {
  const rest = id.slice(PLUGIN_ENGINE_PREFIX.length);
  return rest.split("/")[1] || rest;
}

/** How the dialog shows the exact launch: argv joined, args with spaces quoted. */
export function commandLine(h: Pick<PluginHarness, "command" | "args">): string {
  return [h.command, ...h.args].map((a) => (/\s/.test(a) || a === "" ? JSON.stringify(a) : a)).join(" ");
}

/** What the window echoes back on Accept: exactly what it displayed. The
 *  server refuses a grant that doesn't match the installed manifest. */
export function consentPayload(pluginId: string, harnesses: PluginHarness[]) {
  return { id: pluginId, harnesses: harnesses.map((h) => ({ id: h.id, command: h.command, args: [...h.args], ...(h.accounts ? { accounts: h.accounts } : {}), ...(h.usage ? { usage: { command: h.usage.command, args: [...h.usage.args] } } : {}) })) };
}

export function pendingConsent(p: { pendingConsent?: PluginHarness[] } | null | undefined): PluginHarness[] {
  return p?.pendingConsent ?? [];
}

/** Only backend SVG data URIs may reach the image sink; null uses the puzzle icon. */
export function pluginEngineIcon(id: string, engines: readonly { id: string; icon?: string | null }[]): string | null {
  if (!isPluginEngine(id)) return null;
  const icon = engines.find((e) => e.id === id)?.icon;
  return icon && /^data:image\/svg\+xml;base64,[A-Za-z0-9+/]+={0,2}$/.test(icon) ? icon : null;
}
