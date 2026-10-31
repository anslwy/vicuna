#!/usr/bin/env python3
"""Regenerate src/lib/catalog.ts from the live Ollama registry.

ollama.com sends no CORS headers, so the browser cannot query the registry
at runtime (see AGENTS.md #12). Instead this build-time script scrapes the
registry and rewrites the catalog with fresh tags + sizes:

    npm run catalog:refresh                  # full refresh: all registry models
    npm run catalog:refresh -- --curated-only # only refresh shipped entries
    python3 scripts/refresh-catalog.py --model gemma4 --print  # dry run one

Merge policy: existing blurbs/cats/params are kept (they are hand-curated);
variant tag lists and sizes are refreshed from the registry. New models get
their blurb from the registry page, cats from its capability chips, and a
params range derived from the size tags — curate afterwards. Sizes come from
OCI manifests (sum of layer bytes); cloud tags have no layers and are
marked size "cloud".
"""

import argparse
import concurrent.futures as futures
import datetime
import html
import json
import re
import sys
import urllib.request
from pathlib import Path

LIB = "https://ollama.com/library"
REG = "https://registry.ollama.ai"
UA = {"User-Agent": "vicuna-catalog-refresh/1.0"}
ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "src" / "lib" / "catalog.ts"

# headline tags: plain sizes (8b, 1.1b, 360m, e2b), latest, cloud tags, and
# Apple-silicon MLX builds (27b-mlx, 30b-a3b-mlx, …) — first-class variants
# for Mac users, surfaced by the Discover MLX filter tab. Other
# quant/packaging suffixes (-it-q4_K_M, -fp16, …) stay skipped — the full
# list remains pullable by exact name.
HEADLINE = re.compile(r"^(latest|cloud|[0-9.]+[bm](-a[0-9.]+[bm])?|e[0-9]+b)(-[0-9.]+[bm]-cloud|-cloud|-mlx)?$", re.I)
MAX_VARIANTS = 16


def get(url, accept=None, timeout=20):
    req = urllib.request.Request(url, headers={**UA, **({"Accept": accept} if accept else {})})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read().decode("utf-8", "replace")


def fetch(url, accept=None, tries=2):
    for i in range(tries):
        try:
            return get(url, accept)
        except Exception as e:
            if i == tries - 1:
                print(f"  warn: {url}: {e}", file=sys.stderr)
                return ""
    return ""


def index_models():
    page = fetch(LIB)
    names = re.findall(r'href="/library/([^":/]+)"', page)
    # drop nav/API noise, keep registry model names
    seen, out = set(), []
    for n in names:
        n = html.unescape(n)
        if n in seen or n in ("", "api", "models", "docs", "pricing", "download"):
            continue
        seen.add(n)
        out.append(n)
    return sorted(out)


def model_tags(name):
    """Headline-candidate tags from the model's tags page, order-preserved."""
    tags_page = fetch(f"{LIB}/{name}/tags")
    tags = re.findall(rf'href="/library/{re.escape(name)}:([^"]+)"', tags_page)
    seen, ordered = set(), []
    for t in tags:
        t = html.unescape(t)
        if t not in seen:
            seen.add(t)
            ordered.append(t)
    return ordered


def model_page(name):
    return fetch(f"{LIB}/{name}")


def meta_description(page):
    m = re.search(r'<meta name="description" content="([^"]*)"', page)
    return html.unescape(m.group(1)).strip() if m else ""


def cats_from_chips(page):
    """Registry capability chips (indigo badges) → catalog cats. The `tools`
    chip marks both chat and code models, so it maps to nothing — code models
    land in chat until curated."""
    chips = set(re.findall(r'text-indigo-600 sm:text-\[13px\]">([a-z]+)</span>', page))
    cats = ["chat"]
    if "reasoning" in chips or "thinking" in chips:
        cats.append("reasoning")
    if "vision" in chips:
        cats.append("vision")
    if "embedding" in chips:
        cats.append("embedding")
    return sorted(set(cats), key=cats.index)


def params_from_tags(tags):
    """'0.6B–235B' style range from the numeric headline tags."""
    nums = []
    for t in tags:
        m = re.match(r"^([a-z]?)(\d+(?:\.\d+)?)([bm])$", t, re.I)
        if m:
            nums.append((
                f"{m.group(1).upper()}{m.group(2)}{m.group(3).upper()}",
                float(m.group(2)) * (1000 if m.group(3).lower() == "b" else 1),
            ))
    nums.sort(key=lambda x: x[1])
    if not nums:
        return ""
    return nums[0][0] if len(nums) == 1 else f"{nums[0][0]}–{nums[-1][0]}"


def headline_tags(tags):
    picked = [t for t in tags if HEADLINE.match(t)]
    if not picked:
        picked = tags[:3]
    if len(picked) > MAX_VARIANTS:
        print(f"  warn: truncating {len(picked)} headline tags to {MAX_VARIANTS}", file=sys.stderr)
    return picked[:MAX_VARIANTS]


def manifest_size(name, tag):
    """(display, sizeMB) from the OCI manifest; cloud tags have no layers."""
    body = fetch(
        f"{REG}/v2/library/{name}/manifests/{tag}",
        accept="application/vnd.docker.distribution.manifest.v2+json",
    )
    if not body:
        return None
    try:
        man = json.loads(body)
        total = sum(int(l.get("size", 0)) for l in man.get("layers", []))
    except (ValueError, AttributeError):
        return None
    if total <= 0:
        return ("cloud", 0)
    mb = total / 1e6
    if mb >= 1000:
        gb = mb / 1000
        disp = f"~{gb:.0f} GB" if gb >= 20 else f"~{gb:.1f} GB"
    else:
        disp = f"~{mb:.0f} MB"
    return (disp, int(round(mb)))


def fmt_size(size):
    if size is None:
        return ("~—", 0)
    return size


def load_existing():
    """name -> (blurb, params, cats, {tag: (size, sizeMB)}) from catalog.ts."""
    if not OUT.exists():
        return {}
    src = OUT.read_text()
    entries = {}
    for m in re.finditer(
        r'name:\s*"([^"]+)",\s*blurb:\s*"((?:[^"\\]|\\.)*)",\s*params:\s*"((?:[^"\\]|\\.)*)",\s*cats:\s*\[([^\]]*)\],\s*variants:\s*\[(.*?)\]\s*\}',
        src,
        re.S,
    ):
        name, blurb, params, cats_raw, vars_raw = m.groups()
        cats = re.findall(r'"([^"]+)"', cats_raw)
        variants = {}
        for vm in re.finditer(r'\{\s*tag:\s*"([^"]+)",\s*size:\s*"([^"]+)",\s*sizeMB:\s*(\d+)', vars_raw):
            variants[vm.group(1)] = (vm.group(2), int(vm.group(3)))
        entries[name] = (blurb, params, cats, variants)
    return entries


def refresh_model(name, existing):
    tags = model_tags(name)
    if not tags:
        return None
    old = existing.get(name)
    head = headline_tags(tags)
    if old:
        # keep previously curated tags that still exist, then add new
        # headlines (e.g. latest, *-cloud) — never silently drop a variant
        keep = [t for t in old[3] if t in tags]
        head = keep + [t for t in head if t not in keep]
    variants = []
    with futures.ThreadPoolExecutor(max_workers=8) as pool:
        fut = {pool.submit(manifest_size, name, t): t for t in head[:MAX_VARIANTS]}
        for f in futures.as_completed(fut):
            tag = fut[f]
            try:
                size = f.result()
            except Exception:
                size = None
            if size is None and old and tag in old[3]:
                size = old[3][tag]
            variants.append((tag, *fmt_size(size)))
    order = {t: i for i, t in enumerate(head[:MAX_VARIANTS])}
    variants.sort(key=lambda v: order.get(v[0], 99))
    if old:
        return {"name": name, "blurb": old[0], "params": old[1], "cats": old[2], "variants": variants}
    page = model_page(name)
    return {
        "name": name,
        "blurb": meta_description(page) or f"{name} — pulled from the Ollama registry; blurb pending curation.",
        "params": params_from_tags(head),
        "cats": cats_from_chips(page),
        "variants": variants,
    }


def emit(entries, date):
    lines = [
        "// Curated Ollama library catalog for the Discover view.",
        "//",
        "// GENERATED — do not hand-edit. Refresh with `npm run catalog:refresh`",
        "// (see scripts/refresh-catalog.py). ollama.com sends no CORS headers,",
        "// so the browser cannot query the registry at runtime; this file is the",
        "// runtime source of truth. Anything not listed is still pullable by",
        "// exact name. Blurbs/cats/params are hand-curated and preserved across",
        "// refreshes; variant tags + sizes come from live OCI manifests.",
        "//",
        '// Sizes are manifest layer totals ("~") except cloud tags (remote).',
        "",
        'export type CatalogCat = "chat" | "code" | "reasoning" | "vision" | "embedding";',
        "",
        "export interface CatalogVariant {",
        '  /** tag without the model prefix, e.g. "8b" → pull "llama3.1:8b" */',
        '  tag: string;',
        '  /** human display size, e.g. "~4.9 GB" ("cloud" for remote tags) */',
        '  size: string;',
        "  /** numeric MB, used by the Small filter (0 = remote/unknown) */",
        "  sizeMB: number;",
        "}",
        "",
        "export interface CatalogEntry {",
        '  /** registry model name, e.g. "llama3.1" */',
        "  name: string;",
        "  blurb: string;",
        '  /** hardware hint, e.g. "8B–405B dense" */',
        "  params: string;",
        "  cats: CatalogCat[];",
        "  variants: CatalogVariant[];",
        "}",
        "",
        f'export const CATALOG_UPDATED = "{date}";',
        "",
        "export const MODEL_CATALOG: CatalogEntry[] = [",
    ]
    for e in entries:
        cats = ", ".join(f'"{c}"' for c in e["cats"])
        lines.append("  {")
        lines.append(f'    name: "{e["name"]}",')
        lines.append(f'    blurb: "{e["blurb"]}",')
        lines.append(f'    params: "{e["params"]}",')
        lines.append(f"    cats: [{cats}],")
        lines.append("    variants: [")
        for tag, size, mb in e["variants"]:
            lines.append(f'      {{ tag: "{tag}", size: "{size}", sizeMB: {mb} }},')
        lines.append("    ]")
        lines.append("  },")
    lines.append("];")
    lines.append("")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--curated-only", dest="all", action="store_false", help="only refresh shipped entries")
    ap.add_argument("--all", dest="all", action="store_true", help="fetch all registry models (default)")
    ap.set_defaults(all=True)
    ap.add_argument("--model", help="refresh a single model (implied dry-run with --print)")
    ap.add_argument("--print", dest="show", action="store_true", help="print to stdout, don't write")
    ap.add_argument("--write", action="store_true", help="write catalog.ts (default unless --print/--model)")
    args = ap.parse_args()

    existing = load_existing()
    if args.model:
        names = [args.model]
    elif args.all:
        names = index_models()
        print(f"registry lists {len(names)} models", file=sys.stderr)
    else:
        names = [n for n in index_models() if n in existing] or list(existing)
        print(f"refreshing {len(names)} curated models", file=sys.stderr)

    results = {}
    with futures.ThreadPoolExecutor(max_workers=8) as pool:
        fut = {pool.submit(refresh_model, n, existing): n for n in names}
        for f in futures.as_completed(fut):
            n = fut[f]
            try:
                e = f.result()
            except Exception as ex:
                print(f"  warn: {n}: {ex}", file=sys.stderr)
                continue
            if e:
                results[n] = e
                print(f"  ok: {n} ({len(e['variants'])} tags)", file=sys.stderr)
            else:
                print(f"  skip: {n} (no tags found)", file=sys.stderr)

    # curated order first, newly discovered models appended alphabetically
    prev_order = [n for n in re.findall(r'name:\s*"([^"]+)"', OUT.read_text())] if OUT.exists() else []
    ordered = [results[n] for n in prev_order if n in results]
    ordered += [results[n] for n in sorted(results) if n not in prev_order]

    date = datetime.date.today().isoformat()
    out = emit(ordered, date if not args.model else existing and date)
    if args.show or args.model and not args.write:
        if not args.write:
            sys.stdout.write(out)
            return
    OUT.write_text(out)
    print(f"wrote {OUT} ({len(ordered)} models)", file=sys.stderr)


if __name__ == "__main__":
    main()
