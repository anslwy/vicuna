// Settings → Plugins → Explore (pure; node-tested in node-tests/pluginCatalog.test.ts).
// The server (`plugin_catalog`, plugins.rs) fetches and validates the
// official index; these helpers re-check what reaches the DOM and drive
// search + the per-card button.

export type CatalogEntry = {
  id: string;
  name: string;
  description: string;
  author: string;
  source: string;
  kinds: ("theme" | "panel" | "harness" | "bridge")[];
  icon?: string;
  screenshot?: string;
  tags: string[];
  minVicuna?: string;
  /** installed version, or null */
  installed: string | null;
  /** same id installed from another source */
  conflict: boolean;
  compatible: boolean;
};
export type Catalog = { url: string; plugins: CatalogEntry[]; skipped: number; ageSecs: number; vicunaVersion: string };

/** Same rule as the server's `valid_image_url`: https on GitHub only. */
export function safeImageUrl(u: unknown): string | null {
  if (typeof u !== "string" || u.length > 512 || /[\s"'<>\\`\u007f-￿]/.test(u)) return null;
  return /^https:\/\/(raw\.githubusercontent\.com|github\.com)\/[^/]/.test(u) ? u : null;
}

/** Case-insensitive match on name, description, author, id, source, tags, kinds. */
export function filterCatalog(list: CatalogEntry[], query: string): CatalogEntry[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return list;
  return list.filter((e) => {
    const hay = [e.name, e.description, e.author, e.id, e.source, ...(e.tags ?? []), ...(e.kinds ?? [])].join(" ").toLowerCase();
    return words.every((w) => hay.includes(w));
  });
}

export type CardAction = "install" | "upgrade" | "conflict" | "incompatible";

/** What the card's button does. Installed plugins re-check their source. */
export function cardAction(e: CatalogEntry): CardAction {
  if (e.conflict) return "conflict";
  if (e.installed) return "upgrade";
  if (!e.compatible) return "incompatible";
  return "install";
}

export const KIND_LABEL: Record<string, string> = { theme: "Theme", panel: "Panel", harness: "Harness", bridge: "Bridge" };
