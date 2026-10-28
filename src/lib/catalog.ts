// Curated Ollama library catalog for the Discover view.
//
// ollama.com/library has no CORS-enabled search API, so the browser cannot
// query the registry live. Instead we ship a curated list (like `lms get`
// staff picks) with client-side search/filter; anything not listed can
// still be pulled by exact name. All tags below were verified against the
// registry (HEAD /v2/library/<name>/manifests/<tag> → 200).
//
// Sizes are approximate default-quant downloads ("~"). Check the registry
// for exact sizes.

export type CatalogCat = "chat" | "code" | "reasoning" | "vision" | "embedding";

export interface CatalogVariant {
  /** tag without the model prefix, e.g. "8b" → pull "llama3.1:8b" */
  tag: string;
  /** human display size, e.g. "~4.9 GB" */
  size: string;
  /** numeric MB, used by the Small filter */
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

export const CATALOG_UPDATED = "2026-10-28";

export const MODEL_CATALOG: CatalogEntry[] = [
  {
    name: "gemma4",
    blurb: "Google's frontier open family: reasoning, multimodal, agentic coding.",
    params: "E2B–31B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "e2b", size: "~4.6 GB", sizeMB: 4600 },
      { tag: "e4b", size: "~6.6 GB", sizeMB: 6600 },
      { tag: "12b", size: "~7.7 GB", sizeMB: 7700 },
      { tag: "26b", size: "~16 GB", sizeMB: 16000 },
      { tag: "31b", size: "~19 GB", sizeMB: 19000 }
    ]
  },
  {
    name: "qwen3",
    blurb: "Alibaba's hybrid thinker: toggle reasoning on/off, strong all-rounder.",
    params: "0.6B–235B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "4b", size: "~2.5 GB", sizeMB: 2500 },
      { tag: "0.6b", size: "~0.5 GB", sizeMB: 500 },
      { tag: "1.7b", size: "~1.4 GB", sizeMB: 1400 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5200 },
      { tag: "14b", size: "~9.3 GB", sizeMB: 9300 },
      { tag: "30b", size: "~19 GB", sizeMB: 19000 },
      { tag: "32b", size: "~20 GB", sizeMB: 20000 },
      { tag: "235b", size: "~142 GB", sizeMB: 142000 }
    ]
  },
  {
    name: "llama3.1",
    blurb: "Meta's workhorse: huge ecosystem, great tool use and multilingual.",
    params: "8B–405B dense",
    cats: ["chat"],
    variants: [
      { tag: "8b", size: "~4.9 GB", sizeMB: 4900 },
      { tag: "70b", size: "~40 GB", sizeMB: 40000 },
      { tag: "405b", size: "~231 GB", sizeMB: 231000 }
    ]
  },
  {
    name: "deepseek-r1",
    blurb: "DeepSeek's reasoning specialist: shows its thinking, tops math/code.",
    params: "1.5B–671B",
    cats: ["reasoning", "chat"],
    variants: [
      { tag: "7b", size: "~4.7 GB", sizeMB: 4700 },
      { tag: "1.5b", size: "~1.1 GB", sizeMB: 1100 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5200 },
      { tag: "14b", size: "~9 GB", sizeMB: 9000 },
      { tag: "32b", size: "~20 GB", sizeMB: 20000 },
      { tag: "70b", size: "~43 GB", sizeMB: 43000 },
      { tag: "671b", size: "~404 GB", sizeMB: 404000 }
    ]
  },
  {
    name: "gemma3",
    blurb: "Google's efficient multimodal: 128K context, sips memory.",
    params: "1B–27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "4b", size: "~3.3 GB", sizeMB: 3300 },
      { tag: "1b", size: "~815 MB", sizeMB: 815 },
      { tag: "12b", size: "~8.1 GB", sizeMB: 8100 },
      { tag: "27b", size: "~17 GB", sizeMB: 17000 }
    ]
  },
  {
    name: "qwen2.5-coder",
    blurb: "Code specialist for completion, review and repo-scale edits.",
    params: "1.5B–32B",
    cats: ["code"],
    variants: [
      { tag: "7b", size: "~4.7 GB", sizeMB: 4700 },
      { tag: "1.5b", size: "~986 MB", sizeMB: 986 },
      { tag: "32b", size: "~20 GB", sizeMB: 20000 }
    ]
  },
  {
    name: "llama3.2",
    blurb: "Meta's small on-device pair: fast, cheap, surprisingly capable.",
    params: "1B–3B",
    cats: ["chat"],
    variants: [
      { tag: "3b", size: "~2.0 GB", sizeMB: 2000 },
      { tag: "1b", size: "~1.3 GB", sizeMB: 1300 }
    ]
  },
  {
    name: "mistral",
    blurb: "Mistral's lean 7B: punches above its weight for chat and RAG.",
    params: "7B dense",
    cats: ["chat"],
    variants: [{ tag: "latest", size: "~4.1 GB", sizeMB: 4100 }]
  },
  {
    name: "gpt-oss",
    blurb: "OpenAI's open reasoning models: harmony format, agentic by design.",
    params: "20B–120B MoE",
    cats: ["reasoning", "chat"],
    variants: [
      { tag: "20b", size: "~14 GB", sizeMB: 14000 },
      { tag: "120b", size: "~70 GB", sizeMB: 70000 }
    ]
  },
  {
    name: "phi4",
    blurb: "Microsoft's 14B: textbook-quality training, strong reasoning.",
    params: "14B dense",
    cats: ["chat", "reasoning"],
    variants: [{ tag: "latest", size: "~9.1 GB", sizeMB: 9100 }]
  },
  {
    name: "qwen2.5",
    blurb: "Previous-gen Alibaba all-rounder: 29 languages, solid tools.",
    params: "1.5B–32B",
    cats: ["chat"],
    variants: [
      { tag: "7b", size: "~4.7 GB", sizeMB: 4700 },
      { tag: "1.5b", size: "~986 MB", sizeMB: 986 },
      { tag: "32b", size: "~20 GB", sizeMB: 20000 }
    ]
  },
  {
    name: "gemma2",
    blurb: "Google's previous gen: efficient 2B for edge, 27B for quality.",
    params: "2B–27B",
    cats: ["chat"],
    variants: [
      { tag: "9b", size: "~5.4 GB", sizeMB: 5400 },
      { tag: "2b", size: "~1.6 GB", sizeMB: 1600 },
      { tag: "27b", size: "~16 GB", sizeMB: 16000 }
    ]
  },
  {
    name: "llama3.3",
    blurb: "Meta's refined 70B: text-only flagship performance, open weights.",
    params: "70B dense",
    cats: ["chat"],
    variants: [{ tag: "70b", size: "~42 GB", sizeMB: 42000 }]
  },
  {
    name: "mixtral",
    blurb: "Mistral's sparse MoE: 8 experts, fast per-token inference.",
    params: "8×7B MoE",
    cats: ["chat"],
    variants: [{ tag: "latest", size: "~26 GB", sizeMB: 26000 }]
  },
  {
    name: "phi3",
    blurb: "Microsoft's Mini: tiny, fast, great for laptops and edge.",
    params: "3.8B dense",
    cats: ["chat"],
    variants: [{ tag: "latest", size: "~2.3 GB", sizeMB: 2300 }]
  },
  {
    name: "tinyllama",
    blurb: "The 1.1B classic: instant loads, perfect for smoke-testing setup.",
    params: "1.1B dense",
    cats: ["chat"],
    variants: [{ tag: "1.1b", size: "~637 MB", sizeMB: 637 }]
  },
  {
    name: "smollm2",
    blurb: "HuggingFace's tiny family: from 135M toys to a usable 1.7B.",
    params: "135M–1.7B",
    cats: ["chat"],
    variants: [
      { tag: "360m", size: "~700 MB", sizeMB: 700 },
      { tag: "135m", size: "~270 MB", sizeMB: 270 },
      { tag: "1.7b", size: "~1.0 GB", sizeMB: 1000 }
    ]
  },
  {
    name: "starcoder2",
    blurb: "BigCode's transparent code model: 600+ languages in training.",
    params: "3B–15B",
    cats: ["code"],
    variants: [
      { tag: "7b", size: "~4.0 GB", sizeMB: 4000 },
      { tag: "3b", size: "~1.7 GB", sizeMB: 1700 },
      { tag: "15b", size: "~9.1 GB", sizeMB: 9100 }
    ]
  },
  {
    name: "codellama",
    blurb: "Meta's code Llama: infilling and long-context code completion.",
    params: "7B–34B",
    cats: ["code"],
    variants: [
      { tag: "7b", size: "~3.8 GB", sizeMB: 3800 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7400 },
      { tag: "34b", size: "~19 GB", sizeMB: 19000 }
    ]
  },
  {
    name: "deepseek-coder-v2",
    blurb: "DeepSeek's 16B coder MoE: 128K context for big refactors.",
    params: "16B MoE",
    cats: ["code"],
    variants: [{ tag: "16b", size: "~8.9 GB", sizeMB: 8900 }]
  },
  {
    name: "llava",
    blurb: "The original open vision chat: ask questions about images.",
    params: "7B–34B",
    cats: ["vision", "chat"],
    variants: [
      { tag: "7b", size: "~4.7 GB", sizeMB: 4700 },
      { tag: "13b", size: "~8 GB", sizeMB: 8000 },
      { tag: "34b", size: "~20 GB", sizeMB: 20000 }
    ]
  },
  {
    name: "moondream",
    blurb: "Tiny 1.8B vision: captions and VQA on almost any hardware.",
    params: "1.8B",
    cats: ["vision"],
    variants: [{ tag: "latest", size: "~1.7 GB", sizeMB: 1700 }]
  },
  {
    name: "bakllava",
    blurb: "Mistral-based LLaVA: sharp vision chat in 4.7 GB.",
    params: "7B",
    cats: ["vision", "chat"],
    variants: [{ tag: "latest", size: "~4.7 GB", sizeMB: 4700 }]
  },
  {
    name: "command-r",
    blurb: "Cohere's RAG specialist: citations, tools, 128K context.",
    params: "35B",
    cats: ["chat"],
    variants: [{ tag: "latest", size: "~20 GB", sizeMB: 20000 }]
  },
  {
    name: "zephyr",
    blurb: "HuggingFace's tuned Mistral: snappy aligned chat.",
    params: "7B dense",
    cats: ["chat"],
    variants: [{ tag: "latest", size: "~4.1 GB", sizeMB: 4100 }]
  },
  {
    name: "nomic-embed-text",
    blurb: "Embeddings for search/RAG: 8K context, tiny and fast.",
    params: "137M embed",
    cats: ["embedding"],
    variants: [{ tag: "latest", size: "~274 MB", sizeMB: 274 }]
  },
  {
    name: "mxbai-embed-large",
    blurb: "Top-tier open embeddings: multilingual retrieval quality.",
    params: "334M embed",
    cats: ["embedding"],
    variants: [{ tag: "latest", size: "~669 MB", sizeMB: 669 }]
  },
  {
    name: "all-minilm",
    blurb: "The 46 MB classic: sentence embeddings for quick prototypes.",
    params: "33M embed",
    cats: ["embedding"],
    variants: [{ tag: "latest", size: "~46 MB", sizeMB: 46 }]
  }
];
