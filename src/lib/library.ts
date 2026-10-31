// Live registry refresh for the Discover view (the in-app "Fetch" button).
//
// JS in a plain browser cannot do this itself — ollama.com and
// registry.ollama.ai send no CORS headers — so the fetches go through the
// `ollama_fetch_text` backend (Tauri IPC when shelled, loopback bridge in a
// plain browser; Rust has no CORS restriction and the URL is allowlisted
// there). With neither present this throws a friendly error.

import { invokeOrBridge, bridgeAvailable } from "./bridge";
import { isTauriRuntime } from "./ollama";
import type { CatalogEntry } from "./catalog";

const LIB = "https://ollama.com/library";
const REG = "https://registry.ollama.ai";

// Keep in sync with scripts/refresh-catalog.py (HEADLINE + MAX_VARIANTS):
// the in-app Fetch rebuilds the override with these rules, so a skew here
// silently drops variants the script ships (happened with -mlx).
const HEADLINE = /^(latest|cloud|[0-9.]+[bm](-a[0-9.]+[bm])?|e[0-9]+b)(-[0-9.]+[bm]-cloud|-cloud|-mlx)?$/i;
const MAX_VARIANTS = 16;

export interface CatalogOverride {
  updated: string;
  entries: CatalogEntry[];
}

async function fetchText(url: string): Promise<string> {
  try {
    return await invokeOrBridge<string>("ollama_fetch_text", { url });
  } catch (e) {
    // plain browser without the bridge: attempted directly, but the
    // registry sends no CORS headers so this fails — surfaced as a
    // friendly note by the caller
    if (/desktop app|bridge/i.test(e instanceof Error ? e.message : String(e))) {
      const res = await fetch(url);
      if (!res.ok) throw new Error(`registry fetch failed: ${res.status}`);
      return await res.text();
    }
    throw e;
  }
}

function decodeEntities(s: string): string {
  return s
    .replace(/&amp;/g, "&")
    .replace(/&lt;/g, "<")
    .replace(/&gt;/g, ">")
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'");
}

export async function fetchLibraryIndex(): Promise<string[]> {
  const page = await fetchText(LIB);
  const names = [...page.matchAll(/href="\/library\/([^":/]+)"/g)].map((m) => m[1]);
  const skip = new Set(["", "api", "models", "docs", "pricing", "download"]);
  return [...new Set(names)].filter((n) => !skip.has(n)).sort();
}

function fmtSize(bytes: number): { size: string; sizeMB: number } {
  if (bytes <= 0) return { size: "cloud", sizeMB: 0 };
  const mb = bytes / 1e6;
  if (mb >= 1000) {
    const gb = mb / 1000;
    return { size: gb >= 20 ? `~${gb.toFixed(0)} GB` : `~${gb.toFixed(1)} GB`, sizeMB: Math.round(mb) };
  }
  return { size: `~${mb.toFixed(0)} MB`, sizeMB: Math.round(mb) };
}

async function manifestSize(name: string, tag: string): Promise<{ size: string; sizeMB: number } | null> {
  try {
    const body = await fetchText(`${REG}/v2/library/${name}/manifests/${tag}`);
    const man = JSON.parse(body) as { layers?: { size?: number }[] };
    const total = (man.layers ?? []).reduce((a, l) => a + (l.size ?? 0), 0);
    return fmtSize(total);
  } catch {
    return null;
  }
}

async function mapPool<T, R>(items: T[], limit: number, fn: (t: T) => Promise<R>): Promise<R[]> {
  const out: R[] = new Array(items.length);
  let i = 0;
  await Promise.all(
    Array.from({ length: Math.min(limit, items.length) }, async () => {
      while (i < items.length) {
        const idx = i++;
        out[idx] = await fn(items[idx]);
      }
    })
  );
  return out;
}

/** Refresh variant tags + sizes for the models we ship, and discover new
 *  registry models (blurb from the registry page, cats from its capability
 *  chips, params derived from the size tags). Anything still missing stays
 *  pullable by exact name. Falls back to the current roster if the library
 *  index itself is unreachable. */
export async function refreshCatalog(
  current: CatalogEntry[],
  onProgress?: (done: number, total: number) => void
): Promise<CatalogOverride> {
  if (!isTauriRuntime() && !(await bridgeAvailable())) {
    throw new Error("Live fetch needs the desktop app (or `cargo run --bin bridge`) — the registry blocks browser requests (CORS).");
  }
  const known = new Map(current.map((e) => [e.name, e]));
  let names = current.map((e) => e.name);
  try {
    const index = await fetchLibraryIndex();
    if (index.length > 0) names = index;
  } catch {
    /* offline: refresh what we have */
  }
  let done = 0;
  const tick = () => {
    done++;
    onProgress?.(done, names.length);
  };
  const fetched = await mapPool(names, 8, async (name) => {
    const entry = known.get(name);
    try {
      const [tagsPage, mainPage] = await Promise.all([
        fetchText(`${LIB}/${name}/tags`).catch(() => ""),
        entry ? Promise.resolve("") : fetchText(`${LIB}/${name}`).catch(() => "")
      ]);
      const tags = [...new Set(
        [...tagsPage.matchAll(new RegExp(`href="/library/${name}:([^"]+)"`, "g"))].map((m) => decodeEntities(m[1]))
      )];
      if (tags.length > 0) {
        const keep = entry ? entry.variants.map((v) => v.tag).filter((t) => tags.includes(t)) : [];
        let head = keep.concat(tags.filter((t) => HEADLINE.test(t) && !keep.includes(t)));
        if (head.length === 0) head = tags.slice(0, 3);
        head = head.slice(0, MAX_VARIANTS);
        const oldSizes = new Map((entry?.variants ?? []).map((v) => [v.tag, { size: v.size, sizeMB: v.sizeMB }]));
        const variants = await mapPool(head, 8, async (tag) => {
          const live = await manifestSize(name, tag);
          const s = live ?? oldSizes.get(tag) ?? { size: "~—", sizeMB: 0 };
          return { tag, ...s };
        });
        const order = new Map(head.map((t, i) => [t, i]));
        variants.sort((a, b) => (order.get(a.tag) ?? 99) - (order.get(b.tag) ?? 99));
        tick();
        if (entry) return entry.variants.length && !variants.length ? entry : { ...entry, variants };
        return {
          name,
          blurb: metaDescription(mainPage) || `${name} — pulled from the Ollama registry; blurb pending curation.`,
          params: paramsFromTags(head),
          cats: catsFromChips(mainPage),
          variants
        } satisfies CatalogEntry;
      }
    } catch {
      /* keep the bundled entry on any per-model failure */
    }
    tick();
    return entry ?? null;
  });
  const list = fetched.filter((e): e is CatalogEntry => e !== null && e.variants.length > 0);
  // curated order first, newly discovered models appended alphabetically
  const curated = current.map((e) => e.name).filter((n) => list.some((e) => e.name === n));
  const fresh = list.filter((e) => !known.has(e.name)).sort((a, b) => a.name.localeCompare(b.name));
  const byName = new Map(list.map((e) => [e.name, e]));
  const ordered = curated.map((n) => byName.get(n)!).concat(fresh);
  return { updated: new Date().toISOString().slice(0, 10), entries: ordered };
}

function metaDescription(page: string): string {
  const m = page.match(/<meta name="description" content="([^"]*)"/);
  return m ? decodeEntities(m[1]).trim() : "";
}

/** Registry capability chips (indigo badges on the model page) → our cats.
 *  The `tools` chip marks both chat and code models, so it maps to nothing;
 *  code models land in chat until curated. */
function catsFromChips(page: string): CatalogEntry["cats"] {
  const chips = new Set(
    [...page.matchAll(/text-indigo-600 sm:text-\[13px\]">([a-z]+)<\/span>/g)].map((m) => m[1])
  );
  const cats: CatalogEntry["cats"] = ["chat"];
  if (chips.has("reasoning") || chips.has("thinking")) cats.push("reasoning");
  if (chips.has("vision")) cats.push("vision");
  if (chips.has("embedding")) cats.push("embedding");
  return [...new Set(cats)];
}

/** "0.6B–235B" style range from the numeric headline tags. */
function paramsFromTags(tags: string[]): string {
  const nums = [];
  for (const t of tags) {
    const m = t.match(/^([a-z]?)(\d+(?:\.\d+)?)([bm])$/i);
    if (m) nums.push({ disp: `${m[1].toUpperCase()}${m[2]}${m[3].toUpperCase()}`, val: parseFloat(m[2]) * (m[3].toLowerCase() === "b" ? 1000 : 1) });
  }
  nums.sort((a, b) => a.val - b.val);
  if (nums.length === 0) return "";
  return nums.length === 1 ? nums[0].disp : `${nums[0].disp}–${nums[nums.length - 1].disp}`;
}

export function validateOverride(raw: unknown): CatalogOverride | null {
  try {
    const o = raw as CatalogOverride;
    if (!o || typeof o.updated !== "string" || !Array.isArray(o.entries)) return null;
    for (const e of o.entries) {
      // entries with no pullable variant would crash the card render —
      // reject the whole override so the bundled catalog is used instead
      if (typeof e.name !== "string" || !Array.isArray(e.variants) || e.variants.length === 0) return null;
      for (const v of e.variants) {
        if (typeof v.tag !== "string") return null;
      }
    }
    return o;
  } catch {
    return null;
  }
}
