// The postMessage API between the app and plugin panels (pure; node-tested in
// node-tests/pluginBridge.test.ts). Panels run in sandboxed iframes with an
// opaque origin and no network (plugins.rs PANEL_CSP), so this is their only
// channel. Every request is shape-checked here; the plugin a request acts on
// is bound by the window from the iframe that sent it, never taken from the
// message, so a panel can only read its own storage.
//
// Panel → app:  { vicuna: 1, id, method, params? }
// App → panel:  { vicuna: 1, id, result } | { vicuna: 1, id, error }
// App → panel:  { vicuna: 1, event: "theme" | "context", data }   (pushed on change)
//
// Methods: getTheme, getContext, storage.get {key}, storage.set {key, value},
// storage.remove {key}, storage.keys. Nothing else exists in this version:
// no file access, commands, network or agent control.

export const PANEL_METHODS = ["getTheme", "getContext", "storage.get", "storage.set", "storage.remove", "storage.keys"] as const;
export type PanelMethod = (typeof PANEL_METHODS)[number];

export type PanelRequest = { id: string | number; method: PanelMethod; params: Record<string, unknown> };

export type PanelTheme = { base: "light" | "dark"; vars: Record<string, string> };
export type PanelContext = { sessionId: string | null; title: string | null; workspace: string | null; mode: "chat" | "work" | null };

export interface PanelHost {
  theme(): PanelTheme;
  context(): PanelContext;
  storage(cmd: "plugin_storage_get" | "plugin_storage_set" | "plugin_storage_remove" | "plugin_storage_keys", key?: string, value?: unknown): Promise<unknown>;
}

const MAX_KEY = 128;
const MAX_VALUE = 64 * 1024;

/** CSS variables a panel gets in `getTheme` (enough to match the app). */
export const THEME_VARS = [
  "--bg", "--text", "--text-dim", "--muted", "--faint", "--hover", "--card", "--composer", "--border", "--border-2",
  "--accent", "--accent-hover", "--accent-fg", "--danger", "--ok", "--link", "--code-bg", "--code-inline", "--msg-font-size"
];

/** A well-formed request, or null for anything else (ignored silently). */
export function parsePanelRequest(data: unknown): PanelRequest | null {
  if (!data || typeof data !== "object") return null;
  const d = data as Record<string, unknown>;
  if (d.vicuna !== 1) return null;
  const id = d.id;
  if (!(typeof id === "string" && id.length <= 64) && !(typeof id === "number" && Number.isFinite(id))) return null;
  if (typeof d.method !== "string" || !(PANEL_METHODS as readonly string[]).includes(d.method)) return null;
  const params = d.params === undefined ? {} : d.params;
  if (!params || typeof params !== "object" || Array.isArray(params)) return null;
  return { id, method: d.method as PanelMethod, params: params as Record<string, unknown> };
}

function key(params: Record<string, unknown>): string {
  const k = params.key;
  if (typeof k !== "string" || !k || k.length > MAX_KEY || /[\u0000-\u001f\u007f]/.test(k)) {
    throw new Error(`key must be a string of 1-${MAX_KEY} characters`);
  }
  return k;
}

/** Run one request against the host; the reply to post back to the panel. */
export async function handlePanelRequest(req: PanelRequest, host: PanelHost): Promise<{ vicuna: 1; id: string | number; result?: unknown; error?: string }> {
  try {
    let result: unknown;
    switch (req.method) {
      case "getTheme":
        result = host.theme();
        break;
      case "getContext":
        result = host.context();
        break;
      case "storage.get":
        result = await host.storage("plugin_storage_get", key(req.params));
        break;
      case "storage.keys":
        result = await host.storage("plugin_storage_keys");
        break;
      case "storage.remove":
        result = await host.storage("plugin_storage_remove", key(req.params));
        break;
      case "storage.set": {
        const k = key(req.params);
        const value = req.params.value;
        let size: number;
        try {
          size = JSON.stringify(value ?? null).length;
        } catch {
          throw new Error("value must be JSON");
        }
        if (size > MAX_VALUE) throw new Error(`a value is at most ${MAX_VALUE / 1024} KB`);
        result = await host.storage("plugin_storage_set", k, value ?? null);
        break;
      }
    }
    return { vicuna: 1, id: req.id, result: result ?? null };
  } catch (e: any) {
    return { vicuna: 1, id: req.id, error: String(e?.message ?? e) };
  }
}

/** Absolute iframe URL for a panel file: the backend base + server path. */
export function panelUrl(base: string | null, panelBase: string | undefined, file: string): string | null {
  if (!base || !panelBase || !/^\/plugin-panel\/[a-z0-9-]+\/[0-9a-f]+\/$/.test(panelBase)) return null;
  if (!/^[A-Za-z0-9._-]+$/.test(file) || file.startsWith(".")) return null;
  return `${base.replace(/\/+$/, "")}${panelBase}${file}`;
}
