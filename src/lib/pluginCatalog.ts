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
  kinds: ("theme" | "panel" | "harness" | "mcp" | "bridge")[];
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

export const KIND_LABEL: Record<string, string> = { theme: "Theme", panel: "Panel", harness: "Harness", mcp: "MCP", bridge: "Bridge" };

/** Turn a GitHub plugin source (`owner/repo`, `github.com/owner/repo`, or
 *  `https://github.com/owner/repo[.git]`) into its canonical https URL,
 *  or null for local paths and non-GitHub / direct-manifest URLs. */
export function githubRepoUrl(source: unknown): string | null {
  if (typeof source !== "string") return null;
  let s = source.trim();
  if (!s || s.startsWith("/") || s.startsWith("./") || s.startsWith("../") || s.startsWith("file://")) return null;
  if (/^[a-zA-Z]:[\\/]/.test(s)) return null;
  s = s.replace(/\/+$/, "");
  let candidate: string | null = null;
  const ghMatch = /^https?:\/\/github\.com\/(.+)$/i.exec(s) ?? /^github\.com\/(.+)$/i.exec(s);
  if (ghMatch) {
    candidate = ghMatch[1].replace(/\.git$/i, "");
  } else if (!s.includes("://")) {
    candidate = s.replace(/\.git$/i, "");
  }
  if (!candidate) return null;
  const parts = candidate.split("/");
  if (parts.length !== 2) return null;
  const [owner, repo] = parts;
  const ok = (p: string) => p.length > 0 && p !== "." && p !== ".." && /^[a-zA-Z0-9._-]+$/.test(p);
  if (!ok(owner) || !ok(repo)) return null;
  return `https://github.com/${owner}/${repo}`;
}
