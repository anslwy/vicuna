// Curated Ollama library catalog for the Discover view.
//
// GENERATED — do not hand-edit. Refresh with `npm run catalog:refresh`
// (see scripts/refresh-catalog.py). ollama.com sends no CORS headers,
// so the browser cannot query the registry at runtime; this file is the
// runtime source of truth. Anything not listed is still pullable by
// exact name. Blurbs/cats/params are hand-curated and preserved across
// refreshes; variant tags + sizes come from live OCI manifests.
//
// Sizes are manifest layer totals ("~") except cloud tags (remote).

export type CatalogCat = "chat" | "code" | "reasoning" | "vision" | "embedding";

export interface CatalogVariant {
  /** tag without the model prefix, e.g. "8b" → pull "llama3.1:8b" */
  tag: string;
  /** human display size, e.g. "~4.9 GB" ("cloud" for remote tags) */
  size: string;
  /** numeric MB, used by the Small filter (0 = remote/unknown) */
  sizeMB: number;
}

export interface CatalogEntry {
  /** registry model name, e.g. "llama3.1" */
  name: string;
  blurb: string;
  /** hardware hint, e.g. "8B–405B dense" */
  params: string;
  cats: CatalogCat[];
  variants: CatalogVariant[];
}

export const CATALOG_UPDATED = "2026-10-29";

export const MODEL_CATALOG: CatalogEntry[] = [
  {
    name: "gemma4",
    blurb: "Google's frontier open family: reasoning, multimodal, agentic coding.",
    params: "E2B–31B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~6.6 GB", sizeMB: 6584 },
      { tag: "cloud", size: "cloud", sizeMB: 0 },
      { tag: "e2b", size: "~4.6 GB", sizeMB: 4587 },
      { tag: "e4b", size: "~6.6 GB", sizeMB: 6584 },
      { tag: "12b", size: "~8.0 GB", sizeMB: 8022 },
      { tag: "26b", size: "~18.7 GB", sizeMB: 18731 },
      { tag: "31b", size: "~20 GB", sizeMB: 20385 },
      { tag: "31b-cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "qwen3",
    blurb: "Alibaba's hybrid thinker: toggle reasoning on/off, strong all-rounder.",
    params: "0.6B–235B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~5.2 GB", sizeMB: 5225 },
      { tag: "0.6b", size: "~523 MB", sizeMB: 523 },
      { tag: "1.7b", size: "~1.4 GB", sizeMB: 1359 },
      { tag: "4b", size: "~2.5 GB", sizeMB: 2497 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5225 },
      { tag: "14b", size: "~9.3 GB", sizeMB: 9276 },
      { tag: "30b", size: "~18.6 GB", sizeMB: 18557 },
      { tag: "32b", size: "~20 GB", sizeMB: 20201 },
      { tag: "235b", size: "~142 GB", sizeMB: 142154 },
    ]
  },
  {
    name: "llama3.1",
    blurb: "Meta's workhorse: huge ecosystem, great tool use and multilingual.",
    params: "8B–405B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
      { tag: "405b", size: "~243 GB", sizeMB: 243070 },
    ]
  },
  {
    name: "deepseek-r1",
    blurb: "DeepSeek's reasoning specialist: shows its thinking, tops math/code.",
    params: "1.5B–671B",
    cats: ["reasoning", "chat"],
    variants: [
      { tag: "latest", size: "~5.2 GB", sizeMB: 5225 },
      { tag: "1.5b", size: "~1.1 GB", sizeMB: 1117 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5225 },
      { tag: "14b", size: "~9.0 GB", sizeMB: 8988 },
      { tag: "32b", size: "~19.9 GB", sizeMB: 19851 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
      { tag: "671b", size: "~404 GB", sizeMB: 404430 },
    ]
  },
  {
    name: "gemma3",
    blurb: "Google's efficient multimodal: 128K context, sips memory.",
    params: "1B–27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~3.4 GB", sizeMB: 3350 },
      { tag: "270m", size: "~292 MB", sizeMB: 292 },
      { tag: "1b", size: "~806 MB", sizeMB: 806 },
      { tag: "4b", size: "~3.4 GB", sizeMB: 3350 },
      { tag: "12b", size: "~8.2 GB", sizeMB: 8164 },
      { tag: "27b", size: "~17.4 GB", sizeMB: 17413 },
    ]
  },
  {
    name: "qwen2.5-coder",
    blurb: "Code specialist for completion, review and repo-scale edits.",
    params: "1.5B–32B",
    cats: ["code"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "0.5b", size: "~398 MB", sizeMB: 398 },
      { tag: "1.5b", size: "~986 MB", sizeMB: 986 },
      { tag: "3b", size: "~1.9 GB", sizeMB: 1930 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "14b", size: "~9.0 GB", sizeMB: 8988 },
      { tag: "32b", size: "~19.9 GB", sizeMB: 19851 },
    ]
  },
  {
    name: "llama3.2",
    blurb: "Meta's small on-device pair: fast, cheap, surprisingly capable.",
    params: "1B–3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 2019 },
      { tag: "1b", size: "~1.3 GB", sizeMB: 1321 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 2019 },
    ]
  },
  {
    name: "mistral",
    blurb: "Mistral's lean 7B: punches above its weight for chat and RAG.",
    params: "7B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.4 GB", sizeMB: 4373 },
      { tag: "7b", size: "~4.4 GB", sizeMB: 4373 },
    ]
  },
  {
    name: "gpt-oss",
    blurb: "OpenAI's open reasoning models: harmony format, agentic by design.",
    params: "20B–120B MoE",
    cats: ["reasoning", "chat"],
    variants: [
      { tag: "latest", size: "~13.8 GB", sizeMB: 13793 },
      { tag: "20b", size: "~13.8 GB", sizeMB: 13793 },
      { tag: "120b", size: "~65 GB", sizeMB: 65370 },
      { tag: "20b-cloud", size: "cloud", sizeMB: 0 },
      { tag: "120b-cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "phi4",
    blurb: "Microsoft's 14B: textbook-quality training, strong reasoning.",
    params: "14B dense",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~9.1 GB", sizeMB: 9053 },
      { tag: "14b", size: "~9.1 GB", sizeMB: 9053 },
    ]
  },
  {
    name: "qwen2.5",
    blurb: "Previous-gen Alibaba all-rounder: 29 languages, solid tools.",
    params: "1.5B–32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "0.5b", size: "~398 MB", sizeMB: 398 },
      { tag: "1.5b", size: "~986 MB", sizeMB: 986 },
      { tag: "3b", size: "~1.9 GB", sizeMB: 1930 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "14b", size: "~9.0 GB", sizeMB: 8988 },
      { tag: "32b", size: "~19.9 GB", sizeMB: 19851 },
      { tag: "72b", size: "~47 GB", sizeMB: 47416 },
    ]
  },
  {
    name: "gemma2",
    blurb: "Google's previous gen: efficient 2B for edge, 27B for quality.",
    params: "2B–27B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.4 GB", sizeMB: 5443 },
      { tag: "2b", size: "~1.6 GB", sizeMB: 1630 },
      { tag: "9b", size: "~5.4 GB", sizeMB: 5443 },
      { tag: "27b", size: "~15.6 GB", sizeMB: 15628 },
    ]
  },
  {
    name: "llama3.3",
    blurb: "Meta's refined 70B: text-only flagship performance, open weights.",
    params: "70B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~43 GB", sizeMB: 42520 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
    ]
  },
  {
    name: "mixtral",
    blurb: "Mistral's sparse MoE: 8 experts, fast per-token inference.",
    params: "8×7B MoE",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~26 GB", sizeMB: 26444 },
    ]
  },
  {
    name: "phi3",
    blurb: "Microsoft's Mini: tiny, fast, great for laptops and edge.",
    params: "3.8B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.2 GB", sizeMB: 2176 },
      { tag: "3.8b", size: "~2.2 GB", sizeMB: 2176 },
      { tag: "14b", size: "~7.9 GB", sizeMB: 7897 },
    ]
  },
  {
    name: "tinyllama",
    blurb: "The 1.1B classic: instant loads, perfect for smoke-testing setup.",
    params: "1.1B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~638 MB", sizeMB: 638 },
      { tag: "1.1b", size: "~638 MB", sizeMB: 638 },
    ]
  },
  {
    name: "smollm2",
    blurb: "HuggingFace's tiny family: from 135M toys to a usable 1.7B.",
    params: "135M–1.7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.8 GB", sizeMB: 1820 },
      { tag: "135m", size: "~271 MB", sizeMB: 271 },
      { tag: "360m", size: "~726 MB", sizeMB: 726 },
      { tag: "1.7b", size: "~1.8 GB", sizeMB: 1820 },
    ]
  },
  {
    name: "starcoder2",
    blurb: "BigCode's transparent code model: 600+ languages in training.",
    params: "3B–15B",
    cats: ["code"],
    variants: [
      { tag: "latest", size: "~1.7 GB", sizeMB: 1710 },
      { tag: "3b", size: "~1.7 GB", sizeMB: 1710 },
      { tag: "7b", size: "~4.0 GB", sizeMB: 4043 },
      { tag: "15b", size: "~9.1 GB", sizeMB: 9065 },
    ]
  },
  {
    name: "codellama",
    blurb: "Meta's code Llama: infilling and long-context code completion.",
    params: "7B–34B",
    cats: ["code"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "34b", size: "~19.1 GB", sizeMB: 19052 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "deepseek-coder-v2",
    blurb: "DeepSeek's 16B coder MoE: 128K context for big refactors.",
    params: "16B MoE",
    cats: ["code"],
    variants: [
      { tag: "latest", size: "~8.9 GB", sizeMB: 8905 },
      { tag: "16b", size: "~8.9 GB", sizeMB: 8905 },
      { tag: "236b", size: "~133 GB", sizeMB: 132912 },
    ]
  },
  {
    name: "llava",
    blurb: "The original open vision chat: ask questions about images.",
    params: "7B–34B",
    cats: ["vision", "chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4733 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4733 },
      { tag: "13b", size: "~8.0 GB", sizeMB: 8011 },
      { tag: "34b", size: "~20 GB", sizeMB: 20166 },
    ]
  },
  {
    name: "moondream",
    blurb: "Tiny 1.8B vision: captions and VQA on almost any hardware.",
    params: "1.8B",
    cats: ["vision"],
    variants: [
      { tag: "latest", size: "~1.7 GB", sizeMB: 1738 },
      { tag: "1.8b", size: "~1.7 GB", sizeMB: 1738 },
    ]
  },
  {
    name: "bakllava",
    blurb: "Mistral-based LLaVA: sharp vision chat in 4.7 GB.",
    params: "7B",
    cats: ["vision", "chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4733 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4733 },
    ]
  },
  {
    name: "command-r",
    blurb: "Cohere's RAG specialist: citations, tools, 128K context.",
    params: "35B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~18.7 GB", sizeMB: 18720 },
      { tag: "35b", size: "~18.7 GB", sizeMB: 18720 },
    ]
  },
  {
    name: "zephyr",
    blurb: "HuggingFace's tuned Mistral: snappy aligned chat.",
    params: "7B dense",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4110 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4110 },
      { tag: "141b", size: "~80 GB", sizeMB: 79509 },
    ]
  },
  {
    name: "nomic-embed-text",
    blurb: "Embeddings for search/RAG: 8K context, tiny and fast.",
    params: "137M embed",
    cats: ["embedding"],
    variants: [
      { tag: "latest", size: "~274 MB", sizeMB: 274 },
    ]
  },
  {
    name: "mxbai-embed-large",
    blurb: "Top-tier open embeddings: multilingual retrieval quality.",
    params: "334M embed",
    cats: ["embedding"],
    variants: [
      { tag: "latest", size: "~670 MB", sizeMB: 670 },
      { tag: "335m", size: "~670 MB", sizeMB: 670 },
    ]
  },
  {
    name: "all-minilm",
    blurb: "The 46 MB classic: sentence embeddings for quick prototypes.",
    params: "33M embed",
    cats: ["embedding"],
    variants: [
      { tag: "latest", size: "~46 MB", sizeMB: 46 },
      { tag: "22m", size: "~46 MB", sizeMB: 46 },
      { tag: "33m", size: "~67 MB", sizeMB: 67 },
    ]
  },
];
