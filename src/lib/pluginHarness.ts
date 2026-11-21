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
  enabled: boolean;
  installed: boolean;
};

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
  return { id: pluginId, harnesses: harnesses.map((h) => ({ id: h.id, command: h.command, args: [...h.args] })) };
}

export function pendingConsent(p: { pendingConsent?: PluginHarness[] } | null | undefined): PluginHarness[] {
  return p?.pendingConsent ?? [];
}
