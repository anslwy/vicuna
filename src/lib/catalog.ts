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
  {
    name: "alfred",
    blurb: "A robust conversational model designed to be used for both chat and instruct use cases.",
    params: "40B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~24 GB", sizeMB: 23808 },
      { tag: "40b", size: "~24 GB", sizeMB: 23808 },
    ]
  },
  {
    name: "athene-v2",
    blurb: "Athene-V2 is a 72B parameter model which excels at code completion, mathematics, and log extraction tasks.",
    params: "72B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~47 GB", sizeMB: 47416 },
      { tag: "72b", size: "~47 GB", sizeMB: 47416 },
    ]
  },
  {
    name: "aya",
    blurb: "Aya 23, released by Cohere, is a new family of state-of-the-art, multilingual models that support 23 languages.",
    params: "8B–35B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.8 GB", sizeMB: 4797 },
      { tag: "8b", size: "~4.8 GB", sizeMB: 4797 },
      { tag: "35b", size: "~20 GB", sizeMB: 20229 },
    ]
  },
  {
    name: "aya-expanse",
    blurb: "Cohere For AI's language models trained to perform well across 23 different languages.",
    params: "8B–32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.1 GB", sizeMB: 5057 },
      { tag: "8b", size: "~5.1 GB", sizeMB: 5057 },
      { tag: "32b", size: "~19.8 GB", sizeMB: 19801 },
    ]
  },
  {
    name: "beam",
    blurb: "Beam is an open model by Reflection designed to deliver strong performance for areas such as agentic tool use, coding and general reasoning with an emphasis on token efficient usage.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "bespoke-minicheck",
    blurb: "A state-of-the-art fact-checking model developed by Bespoke Labs.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4713 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4713 },
    ]
  },
  {
    name: "bge-large",
    blurb: "Embedding model from BAAI mapping texts to vectors.",
    params: "335M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~671 MB", sizeMB: 671 },
      { tag: "335m", size: "~671 MB", sizeMB: 671 },
    ]
  },
  {
    name: "bge-m3",
    blurb: "BGE-M3 is a new model from BAAI distinguished for its versatility in Multi-Functionality, Multi-Linguality, and Multi-Granularity.",
    params: "567M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~1.2 GB", sizeMB: 1158 },
      { tag: "567m", size: "~1.2 GB", sizeMB: 1158 },
    ]
  },
  {
    name: "bonsai-2",
    blurb: "Bonsai 2 from Prism ML is a family of vision models built for ternary weights – smaller models with close to full-precision quality.",
    params: "2B–27B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~3.2 GB", sizeMB: 3181 },
      { tag: "2b", size: "~911 MB", sizeMB: 911 },
      { tag: "4b", size: "~1.6 GB", sizeMB: 1579 },
      { tag: "9b", size: "~3.2 GB", sizeMB: 3181 },
      { tag: "27b", size: "~8.3 GB", sizeMB: 8255 },
    ]
  },
  {
    name: "clef",
    blurb: "Clef is a 27B multimodal model created by Cloudflare that turns a state and a schema of typed questions into decisions.",
    params: "27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~20 GB", sizeMB: 20163 },
      { tag: "cloud", size: "cloud", sizeMB: 0 },
      { tag: "27b", size: "~20 GB", sizeMB: 20163 },
      { tag: "27b-cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "clef-flash",
    blurb: "Clef-Flash is a 9B multimodal model that turns a state and a schema of typed questions into decisions.",
    params: "9B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~10.6 GB", sizeMB: 10579 },
      { tag: "9b", size: "~10.6 GB", sizeMB: 10579 },
    ]
  },
  {
    name: "codebooga",
    blurb: "A high-performing code instruct model created by merging two existing code models.",
    params: "34B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~19.1 GB", sizeMB: 19052 },
      { tag: "34b", size: "~19.1 GB", sizeMB: 19052 },
    ]
  },
  {
    name: "codegeex4",
    blurb: "A versatile model for AI software development scenarios, including code completion.",
    params: "9B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.5 GB", sizeMB: 5455 },
      { tag: "9b", size: "~5.5 GB", sizeMB: 5455 },
    ]
  },
  {
    name: "codegemma",
    blurb: "CodeGemma is a collection of powerful, lightweight models that can perform a variety of coding tasks like fill-in-the-middle code completion, code generation, natural language understanding, mathematical reasoning, and instruction following.",
    params: "2B–7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.0 GB", sizeMB: 5012 },
      { tag: "2b", size: "~1.6 GB", sizeMB: 1551 },
      { tag: "7b", size: "~5.0 GB", sizeMB: 5012 },
    ]
  },
  {
    name: "codeqwen",
    blurb: "CodeQwen1.5 is a large language model pretrained on a large amount of code data.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.2 GB", sizeMB: 4179 },
      { tag: "7b", size: "~4.2 GB", sizeMB: 4179 },
    ]
  },
  {
    name: "codestral",
    blurb: "Codestral is Mistral AI’s first-ever code model designed for code generation tasks.",
    params: "22B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~12.6 GB", sizeMB: 12569 },
      { tag: "22b", size: "~12.6 GB", sizeMB: 12569 },
    ]
  },
  {
    name: "codeup",
    blurb: "Great code generation model based on Llama2.",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "cogito",
    blurb: "Cogito v1 Preview is a family of hybrid reasoning models by Deep Cogito that outperform the best available open models of the same size, including counterparts from LLaMA, DeepSeek, and Qwen across most standard benchmarks.",
    params: "3B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "3b", size: "~2.2 GB", sizeMB: 2241 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "14b", size: "~9.0 GB", sizeMB: 8985 },
      { tag: "32b", size: "~19.8 GB", sizeMB: 19849 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
    ]
  },
  {
    name: "cogito-2.1",
    blurb: "The Cogito v2.1 LLMs are instruction tuned generative models. All models are released under MIT license for commercial use.",
    params: "671B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1342 GB", sizeMB: 1342260 },
      { tag: "671b", size: "~1342 GB", sizeMB: 1342260 },
    ]
  },
  {
    name: "command-a",
    blurb: "111 billion parameter model optimized for demanding enterprises that require fast, secure, and high-quality AI",
    params: "111B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~67 GB", sizeMB: 67136 },
      { tag: "111b", size: "~67 GB", sizeMB: 67136 },
    ]
  },
  {
    name: "command-r-plus",
    blurb: "Command R+ is a powerful, scalable large language model purpose-built to excel at real-world enterprise use cases.",
    params: "104B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~59 GB", sizeMB: 59221 },
      { tag: "104b", size: "~59 GB", sizeMB: 59221 },
    ]
  },
  {
    name: "command-r7b",
    blurb: "The smallest model in Cohere's R series delivers top-tier speed, efficiency, and quality to build powerful AI applications on commodity GPUs and edge devices.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.1 GB", sizeMB: 5057 },
      { tag: "7b", size: "~5.1 GB", sizeMB: 5057 },
    ]
  },
  {
    name: "command-r7b-arabic",
    blurb: "A new state-of-the-art version of the lightweight Command R7B model that excels in advanced Arabic language capabilities for enterprises in the Middle East and Northern Africa.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.1 GB", sizeMB: 5057 },
      { tag: "7b", size: "~5.1 GB", sizeMB: 5057 },
    ]
  },
  {
    name: "d1",
    blurb: "d1 is a 3B decision model from Liquid AI, built on LFM2.5-VL-3B",
    params: "2B–3B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~3.7 GB", sizeMB: 3731 },
      { tag: "2b", size: "~3.9 GB", sizeMB: 3854 },
      { tag: "3b", size: "~3.7 GB", sizeMB: 3731 },
    ]
  },
  {
    name: "d1-omni",
    blurb: "d1-omni is a multimodal 600M decision model from Liquid AI",
    params: "600M–2B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~850 MB", sizeMB: 850 },
      { tag: "600m", size: "~850 MB", sizeMB: 850 },
      { tag: "2b", size: "~4.4 GB", sizeMB: 4352 },
    ]
  },
  {
    name: "d3",
    blurb: "Turn text, structured data, and images into scored choices, yes/no answers, and ratings.",
    params: "2B–27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~5.8 GB", sizeMB: 5833 },
      { tag: "2b", size: "~3.2 GB", sizeMB: 3224 },
      { tag: "4b", size: "~5.8 GB", sizeMB: 5833 },
      { tag: "9b", size: "~10.4 GB", sizeMB: 10449 },
      { tag: "27b", size: "~30 GB", sizeMB: 29527 },
    ]
  },
  {
    name: "dbrx",
    blurb: "DBRX is an open, general-purpose LLM created by Databricks.",
    params: "132B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~74 GB", sizeMB: 74193 },
      { tag: "132b", size: "~74 GB", sizeMB: 74193 },
    ]
  },
  {
    name: "deepcoder",
    blurb: "DeepCoder is a fully open-Source 14B coder model at O3-mini level, with a 1.5B version also available.",
    params: "1.5B–14B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~9.0 GB", sizeMB: 8988 },
      { tag: "1.5b", size: "~1.1 GB", sizeMB: 1117 },
      { tag: "14b", size: "~9.0 GB", sizeMB: 8988 },
    ]
  },
  {
    name: "deepscaler",
    blurb: "A fine-tuned version of Deepseek-R1-Distilled-Qwen-1.5B that surpasses the performance of OpenAI’s o1-preview with just 1.5B parameters on popular math evaluations.",
    params: "1.5B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.6 GB", sizeMB: 3560 },
      { tag: "1.5b", size: "~3.6 GB", sizeMB: 3560 },
    ]
  },
  {
    name: "deepseek-coder",
    blurb: "DeepSeek Coder is a capable coding model trained on two trillion code and natural language tokens.",
    params: "1.3B–33B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~776 MB", sizeMB: 776 },
      { tag: "1.3b", size: "~776 MB", sizeMB: 776 },
      { tag: "6.7b", size: "~3.8 GB", sizeMB: 3828 },
      { tag: "33b", size: "~18.8 GB", sizeMB: 18819 },
    ]
  },
  {
    name: "deepseek-llm",
    blurb: "An advanced language model crafted with 2 trillion bilingual tokens.",
    params: "7B–67B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.0 GB", sizeMB: 4000 },
      { tag: "7b", size: "~4.0 GB", sizeMB: 4000 },
      { tag: "67b", size: "~38 GB", sizeMB: 38153 },
    ]
  },
  {
    name: "deepseek-ocr",
    blurb: "DeepSeek-OCR is a vision-language model that can perform token-efficient OCR.",
    params: "3B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~6.7 GB", sizeMB: 6687 },
      { tag: "3b", size: "~6.7 GB", sizeMB: 6687 },
    ]
  },
  {
    name: "deepseek-v2",
    blurb: "A strong, economical, and efficient Mixture-of-Experts language model.",
    params: "16B–236B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~8.9 GB", sizeMB: 8905 },
      { tag: "16b", size: "~8.9 GB", sizeMB: 8905 },
      { tag: "236b", size: "~133 GB", sizeMB: 132912 },
    ]
  },
  {
    name: "deepseek-v2.5",
    blurb: "An upgraded version of DeekSeek-V2  that integrates the general and coding abilities of both DeepSeek-V2-Chat and DeepSeek-Coder-V2-Instruct.",
    params: "236B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~133 GB", sizeMB: 132912 },
      { tag: "236b", size: "~133 GB", sizeMB: 132912 },
    ]
  },
  {
    name: "deepseek-v3",
    blurb: "A strong Mixture-of-Experts (MoE) language model with 671B total parameters with 37B activated for each token.",
    params: "671B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~404 GB", sizeMB: 404430 },
      { tag: "671b", size: "~404 GB", sizeMB: 404430 },
    ]
  },
  {
    name: "deepseek-v3.1",
    blurb: "DeepSeek-V3.1-Terminus is a hybrid model that supports both thinking mode and non-thinking mode.",
    params: "671B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~404 GB", sizeMB: 404494 },
      { tag: "671b", size: "~404 GB", sizeMB: 404494 },
    ]
  },
  {
    name: "deepseek-v4-pro",
    blurb: "DeepSeek-V4-Pro is a frontier Mixture-of-Experts model with a large context window and three reasoning modes.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "deepseek-v4.1-flash",
    blurb: "Designed for greater capability, faster inference, higher throughput, and scaling to larger models.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "devstral",
    blurb: "Devstral: the best open source model for coding agents",
    params: "24B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~14.3 GB", sizeMB: 14334 },
      { tag: "24b", size: "~14.3 GB", sizeMB: 14334 },
    ]
  },
  {
    name: "devstral-2",
    blurb: "123B model that excels at using tools to explore codebases, editing multiple files and power software engineering agents.",
    params: "123B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~75 GB", sizeMB: 74898 },
      { tag: "123b", size: "~75 GB", sizeMB: 74898 },
    ]
  },
  {
    name: "devstral-small-2",
    blurb: "24B model that excels at using tools to explore codebases, editing multiple files and power software engineering agents.",
    params: "24B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~15.2 GB", sizeMB: 15177 },
      { tag: "24b", size: "~15.2 GB", sizeMB: 15177 },
      { tag: "24b-cloud", size: "~—", sizeMB: 0 },
    ]
  },
  {
    name: "dolphin-llama3",
    blurb: "Dolphin 2.9 is a new model with 8B and 70B sizes by Eric Hartford based on Llama 3 that has a variety of instruction, conversational, and coding skills.",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "dolphin-mistral",
    blurb: "The uncensored Dolphin model based on Mistral that excels at coding tasks. Updated to version 2.8.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "dolphin-mixtral",
    blurb: "Uncensored, 8x7b and 8x22b fine-tuned models based on the Mixtral mixture of experts models that excels at coding tasks. Created by Eric Hartford.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~26 GB", sizeMB: 26444 },
    ]
  },
  {
    name: "dolphin-phi",
    blurb: "2.7B uncensored Dolphin model by Eric Hartford, based on the Phi language model by Microsoft Research.",
    params: "2.7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1602 },
      { tag: "2.7b", size: "~1.6 GB", sizeMB: 1602 },
    ]
  },
  {
    name: "dolphin3",
    blurb: "Dolphin 3.0 Llama 3.1 8B 🐬 is the next generation of the Dolphin series of instruct-tuned models designed to be the ultimate general purpose local model, enabling coding, math, agentic, function calling, and general use cases.",
    params: "8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4921 },
    ]
  },
  {
    name: "dolphincoder",
    blurb: "A 7B and 15B uncensored variant of the Dolphin model family that excels at coding, based on StarCoder2.",
    params: "7B–15B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.2 GB", sizeMB: 4229 },
      { tag: "7b", size: "~4.2 GB", sizeMB: 4229 },
      { tag: "15b", size: "~9.1 GB", sizeMB: 9065 },
    ]
  },
  {
    name: "duckdb-nsql",
    blurb: "7B parameter text-to-SQL model made by MotherDuck and Numbers Station.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
    ]
  },
  {
    name: "embeddinggemma",
    blurb: "EmbeddingGemma is a 300M parameter embedding model from Google.",
    params: "300M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~622 MB", sizeMB: 622 },
      { tag: "300m", size: "~622 MB", sizeMB: 622 },
    ]
  },
  {
    name: "embeddinggemma-2",
    blurb: "EmbeddingGemma 2 is a multimodal embedding model from Google built on the Gemma 4 architecture.",
    params: "270M–740M",
    cats: ["chat", "vision", "embedding"],
    variants: [
      { tag: "latest", size: "~1.3 GB", sizeMB: 1285 },
      { tag: "270m", size: "~304 MB", sizeMB: 304 },
      { tag: "440m", size: "~671 MB", sizeMB: 671 },
      { tag: "570m", size: "~918 MB", sizeMB: 918 },
      { tag: "740m", size: "~1.3 GB", sizeMB: 1285 },
    ]
  },
  {
    name: "everythinglm",
    blurb: "Uncensored Llama2 based model with support for a 16K context window.",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7367 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7367 },
    ]
  },
  {
    name: "exaone-deep",
    blurb: "EXAONE Deep exhibits superior capabilities in various reasoning tasks including math and coding benchmarks, ranging from 2.4B to 32B parameters developed and released by LG AI Research.",
    params: "2.4B–32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.8 GB", sizeMB: 4771 },
      { tag: "2.4b", size: "~1.6 GB", sizeMB: 1645 },
      { tag: "7.8b", size: "~4.8 GB", sizeMB: 4771 },
      { tag: "32b", size: "~19.3 GB", sizeMB: 19344 },
    ]
  },
  {
    name: "exaone3.5",
    blurb: "EXAONE 3.5 is a collection of instruction-tuned bilingual (English and Korean) generative models ranging from 2.4B to 32B parameters, developed and released by LG AI Research.",
    params: "2.4B–32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.8 GB", sizeMB: 4771 },
      { tag: "2.4b", size: "~1.6 GB", sizeMB: 1645 },
      { tag: "7.8b", size: "~4.8 GB", sizeMB: 4771 },
      { tag: "32b", size: "~19.3 GB", sizeMB: 19344 },
    ]
  },
  {
    name: "falcon",
    blurb: "A large language model built by the Technology Innovation Institute (TII) for use in summarization, text generation, and chat bots.",
    params: "7B–180B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.2 GB", sizeMB: 4211 },
      { tag: "7b", size: "~4.2 GB", sizeMB: 4211 },
      { tag: "40b", size: "~24 GB", sizeMB: 23808 },
      { tag: "180b", size: "~101 GB", sizeMB: 101483 },
    ]
  },
  {
    name: "falcon2",
    blurb: "Falcon2 is an 11B parameters causal decoder-only model built by TII and trained over 5T tokens.",
    params: "11B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~6.4 GB", sizeMB: 6383 },
      { tag: "11b", size: "~6.4 GB", sizeMB: 6383 },
    ]
  },
  {
    name: "falcon3",
    blurb: "A family of efficient AI models under 10B parameters performant in science, math, and coding through innovative training techniques.",
    params: "1B–10B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.6 GB", sizeMB: 4570 },
      { tag: "1b", size: "~1.8 GB", sizeMB: 1779 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 2006 },
      { tag: "7b", size: "~4.6 GB", sizeMB: 4570 },
      { tag: "10b", size: "~6.3 GB", sizeMB: 6288 },
    ]
  },
  {
    name: "firefunction-v2",
    blurb: "An open weights function calling model based on Llama 3, competitive with GPT-4o function calling capabilities.",
    params: "70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~40 GB", sizeMB: 39970 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "flint",
    blurb: "Flint is a 39.8 billion-parameter mixture-of-experts model from Recursive, built for coding agents.",
    params: "40B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~23 GB", sizeMB: 22863 },
      { tag: "40b", size: "~23 GB", sizeMB: 22863 },
    ]
  },
  {
    name: "functiongemma",
    blurb: "FunctionGemma is a specialized version of Google's Gemma 3 270M model fine-tuned explicitly for function calling.",
    params: "270M",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~301 MB", sizeMB: 301 },
      { tag: "270m", size: "~301 MB", sizeMB: 301 },
    ]
  },
  {
    name: "gemma",
    blurb: "Gemma is a family of lightweight, state-of-the-art open models built by Google DeepMind. Updated to version 1.1",
    params: "2B–7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.0 GB", sizeMB: 5012 },
      { tag: "2b", size: "~1.7 GB", sizeMB: 1678 },
      { tag: "7b", size: "~5.0 GB", sizeMB: 5012 },
    ]
  },
  {
    name: "gemma3n",
    blurb: "Gemma 3n models are designed for efficient execution on everyday devices such as laptops, tablets or phones.",
    params: "E2B–E4B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~9.8 GB", sizeMB: 9769 },
      { tag: "e2b", size: "~7.8 GB", sizeMB: 7836 },
      { tag: "e4b", size: "~9.8 GB", sizeMB: 9769 },
    ]
  },
  {
    name: "glm-4.7-flash",
    blurb: "As the strongest model in the 30B class, GLM-4.7-Flash offers a new option for lightweight deployment that balances performance and efficiency.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~19.0 GB", sizeMB: 19019 },
    ]
  },
  {
    name: "glm-5.2",
    blurb: "GLM-5.2 is Z.ai’s flagship model for the era of long-horizon tasks.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "glm-5.3",
    blurb: "Z.ai's flagship model and the most capable open-weights model for coding, with major gains on long-horizon agentic tasks.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "glm-5.3-flash",
    blurb: "Z.ai's first natively multimodal model, approaching Claude Opus 4.8 on coding and agentic benchmarks with just 18B active parameters.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "glm-ocr",
    blurb: "GLM-OCR is a multimodal OCR model for complex document understanding, built on the GLM-V encoder–decoder architecture.",
    params: "",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~2.7 GB", sizeMB: 2655 },
    ]
  },
  {
    name: "glm4",
    blurb: "A strong multi-lingual general language model with competitive performance to Llama 3.",
    params: "9B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.5 GB", sizeMB: 5455 },
      { tag: "9b", size: "~5.5 GB", sizeMB: 5455 },
    ]
  },
  {
    name: "goliath",
    blurb: "A language model created by combining two fine-tuned Llama 2 70B models into one.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~66 GB", sizeMB: 66310 },
    ]
  },
  {
    name: "gpt-oss-safeguard",
    blurb: "gpt-oss-safeguard-20b and gpt-oss-safeguard-120b are safety reasoning models built-upon gpt-oss",
    params: "20B–120B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~13.8 GB", sizeMB: 13793 },
      { tag: "20b", size: "~13.8 GB", sizeMB: 13793 },
      { tag: "120b", size: "~65 GB", sizeMB: 65370 },
    ]
  },
  {
    name: "granite-code",
    blurb: "A family of open foundation models by IBM for Code Intelligence",
    params: "3B–34B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 1997 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 1997 },
      { tag: "8b", size: "~4.6 GB", sizeMB: 4591 },
      { tag: "20b", size: "~11.6 GB", sizeMB: 11553 },
      { tag: "34b", size: "~19.2 GB", sizeMB: 19238 },
    ]
  },
  {
    name: "granite-embedding",
    blurb: "The IBM Granite Embedding 30M and 278M models models are text-only dense biencoder embedding models, with 30M available in English only and 278M serving multilingual use cases.",
    params: "30M–278M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~63 MB", sizeMB: 63 },
      { tag: "30m", size: "~63 MB", sizeMB: 63 },
      { tag: "278m", size: "~563 MB", sizeMB: 563 },
    ]
  },
  {
    name: "granite3-dense",
    blurb: "The IBM Granite 2B and 8B models are designed to support tool-based use cases and support for retrieval augmented generation (RAG), streamlining code generation, translation and bug fixing.",
    params: "2B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1602 },
      { tag: "2b", size: "~1.6 GB", sizeMB: 1602 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4943 },
    ]
  },
  {
    name: "granite3-guardian",
    blurb: "The IBM Granite Guardian 3.0 2B and 8B models are designed to detect risks in prompts and/or responses.",
    params: "2B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.7 GB", sizeMB: 2694 },
      { tag: "2b", size: "~2.7 GB", sizeMB: 2694 },
      { tag: "8b", size: "~5.8 GB", sizeMB: 5797 },
    ]
  },
  {
    name: "granite3-moe",
    blurb: "The IBM Granite 1B and 3B models are the first mixture of experts (MoE) Granite models from IBM designed for low latency usage.",
    params: "1B–3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~822 MB", sizeMB: 822 },
      { tag: "1b", size: "~822 MB", sizeMB: 822 },
      { tag: "3b", size: "~2.1 GB", sizeMB: 2059 },
    ]
  },
  {
    name: "granite3.1-dense",
    blurb: "The IBM Granite 2B and 8B models are text-only dense LLMs trained on over 12 trillion tokens of data, demonstrated significant improvements over their predecessors in performance and speed in IBM’s initial testing.",
    params: "2B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.0 GB", sizeMB: 4992 },
      { tag: "2b", size: "~1.6 GB", sizeMB: 1570 },
      { tag: "8b", size: "~5.0 GB", sizeMB: 4992 },
    ]
  },
  {
    name: "granite3.1-moe",
    blurb: "The IBM Granite 1B and 3B models are long-context mixture of experts (MoE) Granite models from IBM designed for low latency usage.",
    params: "1B–3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 2035 },
      { tag: "1b", size: "~1.4 GB", sizeMB: 1422 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 2035 },
    ]
  },
  {
    name: "granite3.2",
    blurb: "Granite-3.2 is a family of long-context AI models from IBM Granite fine-tuned for thinking capabilities.",
    params: "2B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4943 },
      { tag: "2b", size: "~1.5 GB", sizeMB: 1545 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4943 },
    ]
  },
  {
    name: "granite3.2-vision",
    blurb: "A compact and efficient vision-language model, specifically designed for visual document understanding, enabling automated content extraction from tables, charts, infographics, plots, diagrams, and more.",
    params: "2B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~2.4 GB", sizeMB: 2438 },
      { tag: "2b", size: "~2.4 GB", sizeMB: 2438 },
    ]
  },
  {
    name: "granite3.3",
    blurb: "IBM Granite 2B and 8B models are 128K context length language models that have been fine-tuned for improved reasoning and instruction-following capabilities.",
    params: "2B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4943 },
      { tag: "2b", size: "~1.5 GB", sizeMB: 1545 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4943 },
    ]
  },
  {
    name: "granite4",
    blurb: "Granite 4 features improved instruction following (IF) and tool-calling capabilities, making them more effective in enterprise applications.",
    params: "350M–3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.1 GB", sizeMB: 2100 },
      { tag: "350m", size: "~708 MB", sizeMB: 708 },
      { tag: "1b", size: "~3.3 GB", sizeMB: 3267 },
      { tag: "3b", size: "~2.1 GB", sizeMB: 2100 },
    ]
  },
  {
    name: "granite4.1",
    blurb: "IBM Granite Models are a family of enterprise-ready, open foundation models that support multilingual capabilities, coding, retrieval-augmented generation (RAG), tool use, and structured JSON output. Released under Apache 2.0 license.",
    params: "3B–30B",
    cats: ["chat"],
    variants: [
      { tag: "3b", size: "~2.1 GB", sizeMB: 2100 },
      { tag: "8b", size: "~5.3 GB", sizeMB: 5348 },
      { tag: "30b", size: "~17.5 GB", sizeMB: 17490 },
    ]
  },
  {
    name: "granite4.1-guardian",
    blurb: "Granite Guardian 4.1 is a specialized safety and judging model from IBM Research that evaluates whether LLM prompts and responses meet specified harm criteria.",
    params: "8B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "8b", size: "~6.9 GB", sizeMB: 6879 },
    ]
  },
  {
    name: "granite4.2",
    blurb: "IBM Granite Models are a family of enterprise-ready, open foundation models that support multilingual capabilities, coding, retrieval-augmented generation (RAG), tool use, thinking and structured JSON output. Released under Apache 2.0 license.",
    params: "3B–30B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.3 GB", sizeMB: 5348 },
      { tag: "3b", size: "~2.2 GB", sizeMB: 2244 },
      { tag: "8b", size: "~5.3 GB", sizeMB: 5348 },
      { tag: "30b", size: "~17.7 GB", sizeMB: 17721 },
    ]
  },
  {
    name: "hermes3",
    blurb: "Hermes 3 is the latest version of the flagship Hermes series of LLMs by Nous Research",
    params: "3B–405B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 2019 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
      { tag: "405b", size: "~229 GB", sizeMB: 228856 },
    ]
  },
  {
    name: "internlm2",
    blurb: "InternLM2.5 is a 7B parameter model tailored for practical scenarios with outstanding reasoning capability.",
    params: "1M–20B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.5 GB", sizeMB: 4453 },
      { tag: "1m", size: "~4.5 GB", sizeMB: 4453 },
      { tag: "1.8b", size: "~1.1 GB", sizeMB: 1114 },
      { tag: "7b", size: "~4.5 GB", sizeMB: 4453 },
      { tag: "20b", size: "~11.3 GB", sizeMB: 11323 },
    ]
  },
  {
    name: "kev",
    blurb: "Kev scores answers to structured questions about a supplied text state in a single forward pass.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.6 GB", sizeMB: 5630 },
    ]
  },
  {
    name: "kimi-k2.6",
    blurb: "Kimi K2.6 is an open-source, native multimodal agentic model that advances practical capabilities in long-horizon coding, coding-driven design, proactive autonomous execution, and swarm-based task orchestration.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "kimi-k2.7-code",
    blurb: "Kimi K2.7 Code is Moonshot AI's coding-focused agentic model built upon Kimi K2.6, with substantial improvements on real-world long-horizon coding tasks and roughly 30% lower thinking-token usage.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "kimi-k3",
    blurb: "Kimi K3 is an open-weight, native multimodal agentic model and our most capable model to date.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "laguna-s-2.1",
    blurb: "Our most capable model to date, designed for long-horizon work. 70.2% on Terminal-Bench 2.1 at 118B-A8B.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~96 GB", sizeMB: 96032 },
    ]
  },
  {
    name: "laguna-xs-2.1",
    blurb: "Laguna XS 2.1 is a 33B total parameter Mixture-of-Experts model with 3B activated parameters per token designed for agentic coding and long-horizon work on a local machine.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~20 GB", sizeMB: 20274 },
    ]
  },
  {
    name: "laguna-xs.2",
    blurb: "Laguna XS.2 is a 33B total parameter Mixture-of-Experts model with 3B activated parameters per token designed for agentic coding and long-horizon work on a local machine.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~23 GB", sizeMB: 23098 },
    ]
  },
  {
    name: "laya",
    blurb: "Laya is a 421M decision model from Convai Innovations, fine-tuned from ModernBERT-large.",
    params: "",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~844 MB", sizeMB: 844 },
    ]
  },
  {
    name: "lfm2",
    blurb: "LFM2 is a family of hybrid models designed for on-device deployment. LFM2-24B-A2B is the largest model in the family, scaling the architecture to 24 billion parameters while keeping inference efficient.",
    params: "24B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~14.4 GB", sizeMB: 14416 },
      { tag: "24b", size: "~14.4 GB", sizeMB: 14416 },
    ]
  },
  {
    name: "lfm2.5",
    blurb: "LFM2.5-8B-A1B, an edge model built for fast, reliable tool calling on consumer hardware.",
    params: "8B–24B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~5.2 GB", sizeMB: 5156 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5156 },
      { tag: "24b", size: "~14.5 GB", sizeMB: 14526 },
    ]
  },
  {
    name: "lfm2.5-thinking",
    blurb: "LFM2.5 is a new family of hybrid models designed for on-device deployment.",
    params: "1.2B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~731 MB", sizeMB: 731 },
      { tag: "1.2b", size: "~731 MB", sizeMB: 731 },
    ]
  },
  {
    name: "lfm3",
    blurb: "LFM3-1.7B is a tri-hybrid model for local agents and specialized applications.",
    params: "1.7B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~1.1 GB", sizeMB: 1100 },
      { tag: "1.7b", size: "~1.1 GB", sizeMB: 1100 },
    ]
  },
  {
    name: "lfm3-audio",
    blurb: "LFM3-Audio-2B is a compact, fully featured audio model.",
    params: "2B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~1.7 GB", sizeMB: 1670 },
      { tag: "2b", size: "~1.7 GB", sizeMB: 1670 },
    ]
  },
  {
    name: "lfm3-omni",
    blurb: "a single end-to-end model that understands text, images, video and audio, and answers in text and speech",
    params: "2B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 2049 },
      { tag: "2b", size: "~2.0 GB", sizeMB: 2049 },
    ]
  },
  {
    name: "lfm3-vl",
    blurb: "LFM3-VL-2B is a vision-language model for on-device visual understanding.",
    params: "2B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~1.5 GB", sizeMB: 1479 },
      { tag: "2b", size: "~1.5 GB", sizeMB: 1479 },
    ]
  },
  {
    name: "llama-guard3",
    blurb: "Llama Guard 3 is a series of models fine-tuned for content safety classification of LLM inputs and responses.",
    params: "1B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "1b", size: "~1.6 GB", sizeMB: 1600 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4921 },
    ]
  },
  {
    name: "llama-pro",
    blurb: "An expansion of Llama 2 that specializes in integrating both general language understanding and domain-specific knowledge, particularly in programming and mathematics.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4737 },
    ]
  },
  {
    name: "llama2",
    blurb: "Llama 2 is a collection of foundation language models ranging from 7B to 70B parameters.",
    params: "7B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3827 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3827 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7367 },
      { tag: "70b", size: "~39 GB", sizeMB: 38873 },
    ]
  },
  {
    name: "llama2-chinese",
    blurb: "Llama 2 based model fine tuned to improve Chinese dialogue ability.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "llama2-uncensored",
    blurb: "Uncensored Llama 2 model by George Sung and Jarrad Hope.",
    params: "7B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "llama3",
    blurb: "Meta Llama 3: The most capable openly available LLM to date",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "llama3-chatqa",
    blurb: "A model from NVIDIA based on Llama 3 that excels at conversational question answering (QA) and retrieval-augmented generation (RAG).",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "llama3-gradient",
    blurb: "This model extends LLama-3 8B's context length from 8k to over 1m tokens.",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "llama3-groq-tool-use",
    blurb: "A series of models from Groq that represent a significant advancement in open-source AI capabilities for tool use/function calling.",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4661 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "llama3.2-vision",
    blurb: "Llama 3.2 Vision is a collection of instruction-tuned image reasoning generative models in 11B and 90B sizes.",
    params: "11B–90B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~7.8 GB", sizeMB: 7817 },
      { tag: "11b", size: "~7.8 GB", sizeMB: 7817 },
      { tag: "90b", size: "~55 GB", sizeMB: 54623 },
    ]
  },
  {
    name: "llama4",
    blurb: "Meta's latest collection of multimodal models.",
    params: "",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~67 GB", sizeMB: 67437 },
    ]
  },
  {
    name: "llava-llama3",
    blurb: "A LLaVA model fine-tuned from Llama 3 Instruct with better scores in several benchmarks.",
    params: "8B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~5.5 GB", sizeMB: 5546 },
      { tag: "8b", size: "~5.5 GB", sizeMB: 5546 },
    ]
  },
  {
    name: "llava-phi3",
    blurb: "A new small LLaVA model fine-tuned from Phi 3 Mini.",
    params: "3.8B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~2.9 GB", sizeMB: 2927 },
      { tag: "3.8b", size: "~2.9 GB", sizeMB: 2927 },
    ]
  },
  {
    name: "magicoder",
    blurb: "🎩 Magicoder is a family of 7B parameter models trained on 75K synthetic instruction data using OSS-Instruct, a novel approach to enlightening LLMs with open-source code snippets.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
    ]
  },
  {
    name: "magistral",
    blurb: "Magistral is a small, efficient reasoning model with 24B parameters.",
    params: "24B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~14.3 GB", sizeMB: 14334 },
      { tag: "24b", size: "~14.3 GB", sizeMB: 14334 },
    ]
  },
  {
    name: "marco-o1",
    blurb: "An open large reasoning model for real-world solutions by the Alibaba International Digital Commerce Group (AIDC-AI).",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4683 },
    ]
  },
  {
    name: "mathstral",
    blurb: "MathΣtral: a 7B model designed for math reasoning and scientific discovery by Mistral AI.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4113 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4113 },
    ]
  },
  {
    name: "medgemma",
    blurb: "MedGemma is a collection of Gemma 3 variants that are trained for performance on medical text and image comprehension.",
    params: "4B–27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~3.3 GB", sizeMB: 3339 },
      { tag: "4b", size: "~3.3 GB", sizeMB: 3339 },
      { tag: "27b", size: "~17.4 GB", sizeMB: 17397 },
    ]
  },
  {
    name: "medgemma1.5",
    blurb: "MedGemma 1.5 4B is an updated version of the MedGemma 4B model.",
    params: "4B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~3.3 GB", sizeMB: 3339 },
      { tag: "4b", size: "~3.3 GB", sizeMB: 3339 },
    ]
  },
  {
    name: "meditron",
    blurb: "Open-source medical large language model adapted from Llama 2 to the medical domain.",
    params: "7B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "medllama2",
    blurb: "Fine-tuned Llama 2 model to answer medical questions based on an open source medical dataset.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
    ]
  },
  {
    name: "megadolphin",
    blurb: "MegaDolphin-2.2-120b is a transformation of Dolphin-2.2-70b created by interleaving the model with itself.",
    params: "120B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~68 GB", sizeMB: 67755 },
      { tag: "120b", size: "~68 GB", sizeMB: 67755 },
    ]
  },
  {
    name: "minicpm-v",
    blurb: "A series of multimodal LLMs (MLLMs) designed for vision-language understanding.",
    params: "8B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~5.5 GB", sizeMB: 5474 },
      { tag: "8b", size: "~5.5 GB", sizeMB: 5474 },
    ]
  },
  {
    name: "minicpm-v4.5",
    blurb: "A GPT-4o Level MLLM for Single Image, Multi Image and High-FPS Video Understanding on Your Phone",
    params: "8B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~6.1 GB", sizeMB: 6122 },
      { tag: "8b", size: "~6.1 GB", sizeMB: 6122 },
    ]
  },
  {
    name: "minicpm-v4.6",
    blurb: "A Pocket-Sized MLLM for Ultra-Efficient Image and Video Understanding on Your Phone",
    params: "1B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1638 },
      { tag: "1b", size: "~1.6 GB", sizeMB: 1638 },
    ]
  },
  {
    name: "minimax-m2.7",
    blurb: "MiniMax's M2-series model for coding, agentic workflows, and professional productivity.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "minimax-m3",
    blurb: "MiniMax M3: Coding & Agentic Frontier. 1M context window. Native Multimodality.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "minimax-m3.1-flash",
    blurb: "As a next-generation Flash model, M3.1 Flash delivers a generational leap in agentic tasks, coding, Computer Use and cybersecurity, while making online inference nearly three times faster and reducing usage costs by 70%.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "ministral-3",
    blurb: "The Ministral 3 family is designed for edge deployment, capable of running on a wide range of hardware.",
    params: "3B–14B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~6.0 GB", sizeMB: 6022 },
      { tag: "3b", size: "~3.0 GB", sizeMB: 2954 },
      { tag: "8b", size: "~6.0 GB", sizeMB: 6022 },
      { tag: "14b", size: "~9.1 GB", sizeMB: 9083 },
    ]
  },
  {
    name: "mistral-large",
    blurb: "Mistral Large 2 is Mistral's new flagship model that is significantly more capable in code generation, mathematics, and reasoning with 128k context window and support for dozens of languages.",
    params: "123B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~73 GB", sizeMB: 73220 },
      { tag: "123b", size: "~73 GB", sizeMB: 73220 },
    ]
  },
  {
    name: "mistral-large-3",
    blurb: "A general-purpose multimodal mixture-of-experts model for production-grade tasks and enterprise workloads.",
    params: "",
    cats: ["chat", "vision"],
    variants: [
      { tag: "675b-cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "mistral-large-4",
    blurb: "Mistral Large 4 is Mistral's open-weight, general-purpose multimodal model. Its granular Mixture-of-Experts design activates 52B of 1.05T total parameters, with a 1.6B vision encoder handling images.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "mistral-medium-3.5",
    blurb: "Mistral Medium 3.5 is the first flagship model of Mistral AI that merged instruction-following, reasoning, and coding in a single set of 128B weights.",
    params: "128B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~80 GB", sizeMB: 80241 },
      { tag: "128b", size: "~80 GB", sizeMB: 80241 },
    ]
  },
  {
    name: "mistral-nemo",
    blurb: "A state-of-the-art 12B model with 128k context length, built by Mistral AI in collaboration with NVIDIA.",
    params: "12B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.1 GB", sizeMB: 7072 },
      { tag: "12b", size: "~7.1 GB", sizeMB: 7072 },
    ]
  },
  {
    name: "mistral-openorca",
    blurb: "Mistral OpenOrca is a 7 billion parameter model, fine-tuned on top of the Mistral 7B model using the OpenOrca dataset.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "mistral-small",
    blurb: "Mistral Small 3 sets a new benchmark in the “small” Large Language Models category below 70B.",
    params: "22B–24B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~14.3 GB", sizeMB: 14334 },
      { tag: "22b", size: "~12.6 GB", sizeMB: 12569 },
      { tag: "24b", size: "~14.3 GB", sizeMB: 14334 },
    ]
  },
  {
    name: "mistral-small3.1",
    blurb: "Building upon Mistral Small 3, Mistral Small 3.1 (2503) adds state-of-the-art vision understanding and enhances long context capabilities up to 128k tokens without compromising text performance.",
    params: "24B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~15.5 GB", sizeMB: 15487 },
      { tag: "24b", size: "~15.5 GB", sizeMB: 15487 },
    ]
  },
  {
    name: "mistral-small3.2",
    blurb: "An update to Mistral Small that improves on function calling, instruction following, and less repetition errors.",
    params: "24B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~15.2 GB", sizeMB: 15177 },
      { tag: "24b", size: "~15.2 GB", sizeMB: 15177 },
    ]
  },
  {
    name: "mistrallite",
    blurb: "MistralLite is a fine-tuned model based on Mistral with enhanced capabilities of processing long contexts.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "muse-glimmer",
    blurb: "Meta's latest open model built for always-on local agents. 30B parameters, licensed under Apache 2.0 and runs on a single GPU — tuned for tool use, long tasks, and failure recovery.",
    params: "30B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~18.2 GB", sizeMB: 18157 },
      { tag: "30b", size: "~18.2 GB", sizeMB: 18157 },
    ]
  },
  {
    name: "muse-spark-open",
    blurb: "Muse Spark Open is a natively multimodal model with 816B total parameters (42B active) and a 1M-token context window. Built for coding and agentic work, it excels at long-horizon tasks, complex software engineering, and multi-step reasoning.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "nemotron",
    blurb: "Llama-3.1-Nemotron-70B-Instruct is a large language model customized by NVIDIA to improve the helpfulness of LLM generated responses to user queries.",
    params: "70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~43 GB", sizeMB: 42520 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
    ]
  },
  {
    name: "nemotron-3-nano",
    blurb: "Nemotron-3-Nano is a new Standard for Efficient, Open, and Intelligent Agentic Models, now updated with a 4B parameter count model.",
    params: "4B–30B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~24 GB", sizeMB: 24272 },
      { tag: "4b", size: "~2.8 GB", sizeMB: 2838 },
      { tag: "30b", size: "~24 GB", sizeMB: 24272 },
      { tag: "30b-cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "nemotron-3-super",
    blurb: "NVIDIA Nemotron 3 Super is a 120B open MoE model activating just 12B parameters to deliver maximum compute efficiency and accuracy for complex multi-agent applications.",
    params: "120B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~87 GB", sizeMB: 86828 },
      { tag: "cloud", size: "cloud", sizeMB: 0 },
      { tag: "120b", size: "~87 GB", sizeMB: 86828 },
    ]
  },
  {
    name: "nemotron-3-ultra",
    blurb: "NVIDIA Nemotron 3 Ultra is built for high-throughput reasoning and long-running agent workflows.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "cloud", size: "cloud", sizeMB: 0 },
    ]
  },
  {
    name: "nemotron-3.5-bolt",
    blurb: "Nemotron 3.5 Bolt is NVIDIA’s compact vision-language model for local coding, reasoning, and tool use.",
    params: "E4B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~25 GB", sizeMB: 24750 },
      { tag: "e4b", size: "~7.9 GB", sizeMB: 7909 },
    ]
  },
  {
    name: "nemotron-3.5-lightning",
    blurb: "NVIDIA Nemotron 3.5 Lightning is an open 30B mixture-of-experts (MoE) model with 3B active parameters built for always-on agents.",
    params: "30B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~25 GB", sizeMB: 25431 },
      { tag: "30b", size: "~25 GB", sizeMB: 25431 },
    ]
  },
  {
    name: "nemotron-3.5-super",
    blurb: "Nemotron 3.5 Super is NVIDIA’s vision-language model for coding, reasoning, tool use, and document understanding",
    params: "120B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~90 GB", sizeMB: 90154 },
      { tag: "120b", size: "~90 GB", sizeMB: 90154 },
    ]
  },
  {
    name: "nemotron-cascade-2",
    blurb: "An open 30B MoE model from NVIDIA with 3B activated parameters that delivers strong reasoning and agentic capabilities.",
    params: "30B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~24 GB", sizeMB: 24272 },
      { tag: "30b", size: "~24 GB", sizeMB: 24272 },
    ]
  },
  {
    name: "nemotron-mini",
    blurb: "A commercial-friendly small language model by NVIDIA optimized for roleplay, RAG QA, and function calling.",
    params: "4B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.7 GB", sizeMB: 2697 },
      { tag: "4b", size: "~2.7 GB", sizeMB: 2697 },
    ]
  },
  {
    name: "nemotron3",
    blurb: "NVIDIA Nemotron 3 Nano Omni is a multimodal large language model that unifies video, audio, image, and text understanding to support enterprise-grade Q&A, summarization, transcription, and document intelligence workflows.",
    params: "33B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~28 GB", sizeMB: 27639 },
      { tag: "33b", size: "~28 GB", sizeMB: 27639 },
    ]
  },
  {
    name: "neural-chat",
    blurb: "A fine-tuned model based on Mistral with good coverage of domain and language.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4110 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4110 },
    ]
  },
  {
    name: "nexusraven",
    blurb: "Nexus Raven is a 13B instruction tuned model for function calling tasks.",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7367 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7367 },
    ]
  },
  {
    name: "nimble",
    blurb: "A 9B decision model from Bespoke Labs for fast, typed classification.",
    params: "9B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~9.5 GB", sizeMB: 9528 },
      { tag: "9b", size: "~9.5 GB", sizeMB: 9528 },
    ]
  },
  {
    name: "nimble-v3",
    blurb: "A 9B decision model from Bespoke Labs for fast classification, routing, and scoring.",
    params: "9B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~9.5 GB", sizeMB: 9528 },
      { tag: "9b", size: "~9.5 GB", sizeMB: 9528 },
    ]
  },
  {
    name: "nomic-embed-text-v2-moe",
    blurb: "nomic-embed-text-v2-moe is a multilingual MoE text embedding model that excels at multilingual retrieval.",
    params: "",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~958 MB", sizeMB: 958 },
    ]
  },
  {
    name: "north-mini-code-1.0",
    blurb: "North Mini Code is Cohere's first model for developers — a 30B Mixture-of-Experts model with 3B active parameters, built for agentic software engineering.",
    params: "",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~18.6 GB", sizeMB: 18594 },
    ]
  },
  {
    name: "notus",
    blurb: "A 7B chat model fine-tuned with high-quality data and based on Zephyr.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4110 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4110 },
    ]
  },
  {
    name: "notux",
    blurb: "A top-performing mixture of experts model, fine-tuned with high-quality data.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~26 GB", sizeMB: 26442 },
    ]
  },
  {
    name: "nous-hermes",
    blurb: "General use models based on Llama and Llama 2 from Nous Research.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "nous-hermes2",
    blurb: "The powerful family of models by Nous Research that excels at scientific discussion and coding tasks.",
    params: "10.7B–34B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~6.1 GB", sizeMB: 6072 },
      { tag: "10.7b", size: "~6.1 GB", sizeMB: 6072 },
      { tag: "34b", size: "~19.5 GB", sizeMB: 19467 },
    ]
  },
  {
    name: "nous-hermes2-mixtral",
    blurb: "The Nous Hermes 2 model from Nous Research, now trained over Mixtral.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~26 GB", sizeMB: 26444 },
    ]
  },
  {
    name: "nuextract",
    blurb: "A 3.8B model fine-tuned on a private high-quality synthetic dataset for information extraction, based on Phi-3.",
    params: "3.8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.2 GB", sizeMB: 2176 },
      { tag: "3.8b", size: "~2.2 GB", sizeMB: 2176 },
    ]
  },
  {
    name: "olmo-3",
    blurb: "Olmo is a series of Open language models designed to enable the science of language models. These models are pre-trained on the Dolma 3 dataset and post-trained on the Dolci datasets.",
    params: "7B–32B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~4.5 GB", sizeMB: 4472 },
      { tag: "7b", size: "~4.5 GB", sizeMB: 4472 },
      { tag: "32b", size: "~19.5 GB", sizeMB: 19482 },
    ]
  },
  {
    name: "olmo-3.1",
    blurb: "Olmo is a series of Open language models designed to enable the science of language models. These models are pre-trained on the Dolma 3 dataset and post-trained on the Dolci datasets.",
    params: "32B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~19.5 GB", sizeMB: 19482 },
      { tag: "32b", size: "~19.5 GB", sizeMB: 19482 },
    ]
  },
  {
    name: "olmo2",
    blurb: "OLMo 2 is a new family of 7B and 13B models trained on up to 5T tokens. These models are on par with or better than equivalently sized fully open models, and competitive with open-weight models such as Llama 3.1 on English academic benchmarks.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.5 GB", sizeMB: 4472 },
      { tag: "7b", size: "~4.5 GB", sizeMB: 4472 },
      { tag: "13b", size: "~8.4 GB", sizeMB: 8354 },
    ]
  },
  {
    name: "open-orca-platypus2",
    blurb: "Merge of the Open Orca OpenChat model and the Garage-bAInd Platypus 2 model. Designed for chat and code generation.",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "openchat",
    blurb: "A family of open-source models trained on a wide variety of data, surpassing ChatGPT on various benchmarks. Updated to version 3.5-0106.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4110 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4110 },
    ]
  },
  {
    name: "opencoder",
    blurb: "OpenCoder is an open and reproducible code LLM family which includes 1.5B and 8B models, supporting chat in English and Chinese languages.",
    params: "1.5B–8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4736 },
      { tag: "1.5b", size: "~1.4 GB", sizeMB: 1418 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4736 },
    ]
  },
  {
    name: "openhermes",
    blurb: "OpenHermes 2.5 is a 7B model fine-tuned by Teknium on Mistral with fully open datasets.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "openthinker",
    blurb: "A fully open-source family of reasoning models built using a dataset derived by distilling DeepSeek-R1.",
    params: "7B–32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "7b", size: "~4.7 GB", sizeMB: 4683 },
      { tag: "32b", size: "~19.9 GB", sizeMB: 19851 },
    ]
  },
  {
    name: "orca-mini",
    blurb: "A general-purpose model ranging from 3 billion parameters to 70 billion, suitable for entry-level hardware.",
    params: "3B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 1980 },
      { tag: "3b", size: "~2.0 GB", sizeMB: 1980 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "orca2",
    blurb: "Orca 2 is built by Microsoft research, and are a fine-tuned version of Meta's Llama 2 models.  The model is designed to excel particularly in reasoning.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "ornith",
    blurb: "A self-improving family of open-source models for agentic coding",
    params: "9B–35B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~5.6 GB", sizeMB: 5629 },
      { tag: "9b", size: "~5.6 GB", sizeMB: 5629 },
      { tag: "35b", size: "~21 GB", sizeMB: 21167 },
    ]
  },
  {
    name: "ornith-1.5",
    blurb: "Chirp Chirp! 🐦 We are introducing Ornith-1.5, a major step toward building foundation models through end-to-end self-improvement.",
    params: "9B–397B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "9b", size: "~6.6 GB", sizeMB: 6551 },
      { tag: "35b", size: "~23 GB", sizeMB: 22616 },
      { tag: "122b", size: "~77 GB", sizeMB: 76756 },
      { tag: "397b", size: "~242 GB", sizeMB: 241527 },
    ]
  },
  {
    name: "paraphrase-multilingual",
    blurb: "Sentence-transformers model that can be used for tasks like clustering or semantic search.",
    params: "278M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~563 MB", sizeMB: 563 },
      { tag: "278m", size: "~563 MB", sizeMB: 563 },
    ]
  },
  {
    name: "phi",
    blurb: "Phi-2: a 2.7B language model by Microsoft Research that demonstrates outstanding reasoning and language understanding capabilities.",
    params: "2.7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1602 },
      { tag: "2.7b", size: "~1.6 GB", sizeMB: 1602 },
    ]
  },
  {
    name: "phi3.5",
    blurb: "A lightweight AI model with 3.8 billion parameters with performance overtaking similarly and larger sized models.",
    params: "3.8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.2 GB", sizeMB: 2176 },
      { tag: "3.8b", size: "~2.2 GB", sizeMB: 2176 },
    ]
  },
  {
    name: "phi4-mini",
    blurb: "Phi-4-mini brings significant enhancements in multilingual support, reasoning, and mathematics, and now, the long-awaited function calling feature is finally supported.",
    params: "3.8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.5 GB", sizeMB: 2492 },
      { tag: "3.8b", size: "~2.5 GB", sizeMB: 2492 },
    ]
  },
  {
    name: "phi4-mini-reasoning",
    blurb: "Phi 4 mini reasoning is a lightweight open model that balances efficiency with advanced reasoning ability.",
    params: "3.8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.2 GB", sizeMB: 3152 },
      { tag: "3.8b", size: "~3.2 GB", sizeMB: 3152 },
    ]
  },
  {
    name: "phi4-reasoning",
    blurb: "Phi 4 reasoning and reasoning plus are 14-billion parameter open-weight reasoning models that rival much larger models on complex reasoning tasks.",
    params: "14B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~11.1 GB", sizeMB: 11118 },
      { tag: "14b", size: "~11.1 GB", sizeMB: 11118 },
    ]
  },
  {
    name: "phind-codellama",
    blurb: "Code generation model based on Code Llama.",
    params: "34B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~19.1 GB", sizeMB: 19052 },
      { tag: "34b", size: "~19.1 GB", sizeMB: 19052 },
    ]
  },
  {
    name: "phonon",
    blurb: "Phonon is Gradium’s text-to-speech family. It turns written text into spoken audio as it generates, making it useful for reading assistant replies, narrating passages, and adding a voice to local apps.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~113 MB", sizeMB: 113 },
    ]
  },
  {
    name: "pplx-decider",
    blurb: "pplx-decider is a 27B decision model from Perplexity, fine-tuned from Qwen3.8-27B",
    params: "27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~17.5 GB", sizeMB: 17475 },
      { tag: "27b", size: "~17.5 GB", sizeMB: 17475 },
    ]
  },
  {
    name: "qwen",
    blurb: "Qwen 1.5 is a series of large language models by Alibaba Cloud spanning from 0.5B to 110B parameters",
    params: "0.5B–110B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.3 GB", sizeMB: 2330 },
      { tag: "0.5b", size: "~395 MB", sizeMB: 395 },
      { tag: "1.8b", size: "~1.1 GB", sizeMB: 1120 },
      { tag: "4b", size: "~2.3 GB", sizeMB: 2330 },
      { tag: "7b", size: "~4.5 GB", sizeMB: 4512 },
      { tag: "14b", size: "~8.2 GB", sizeMB: 8179 },
      { tag: "32b", size: "~18.5 GB", sizeMB: 18499 },
      { tag: "72b", size: "~41 GB", sizeMB: 41000 },
      { tag: "110b", size: "~63 GB", sizeMB: 62890 },
    ]
  },
  {
    name: "qwen2",
    blurb: "Qwen2 is a new series of large language models from Alibaba group",
    params: "0.5B–72B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.4 GB", sizeMB: 4431 },
      { tag: "0.5b", size: "~352 MB", sizeMB: 352 },
      { tag: "1.5b", size: "~935 MB", sizeMB: 935 },
      { tag: "7b", size: "~4.4 GB", sizeMB: 4431 },
      { tag: "72b", size: "~41 GB", sizeMB: 41232 },
    ]
  },
  {
    name: "qwen2-math",
    blurb: "Qwen2 Math is a series of specialized math language models built upon the Qwen2 LLMs, which significantly outperforms the mathematical capabilities of open-source models and even closed-source models (e.g., GPT4o).",
    params: "1.5B–72B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.4 GB", sizeMB: 4431 },
      { tag: "1.5b", size: "~935 MB", sizeMB: 935 },
      { tag: "7b", size: "~4.4 GB", sizeMB: 4431 },
      { tag: "72b", size: "~41 GB", sizeMB: 41232 },
    ]
  },
  {
    name: "qwen2.5vl",
    blurb: "Flagship vision-language model of Qwen and also a significant leap from the previous Qwen2-VL.",
    params: "3B–72B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~6.0 GB", sizeMB: 5969 },
      { tag: "3b", size: "~3.2 GB", sizeMB: 3201 },
      { tag: "7b", size: "~6.0 GB", sizeMB: 5969 },
      { tag: "32b", size: "~21 GB", sizeMB: 21159 },
      { tag: "72b", size: "~49 GB", sizeMB: 48709 },
    ]
  },
  {
    name: "qwen3-coder",
    blurb: "Alibaba's performant long context models for agentic and coding tasks.",
    params: "30B–480B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~18.6 GB", sizeMB: 18557 },
      { tag: "30b", size: "~18.6 GB", sizeMB: 18557 },
      { tag: "480b", size: "~290 GB", sizeMB: 290059 },
    ]
  },
  {
    name: "qwen3-coder-next",
    blurb: "Qwen3-Coder-Next is a coding-focused language model from Alibaba's Qwen team, optimized for agentic coding workflows and local development.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~52 GB", sizeMB: 51742 },
    ]
  },
  {
    name: "qwen3-embedding",
    blurb: "Building upon the foundational models of the Qwen3 series, Qwen3 Embedding provides a comprehensive range of text embeddings models in various sizes",
    params: "0.6B–8B",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~4.7 GB", sizeMB: 4677 },
      { tag: "0.6b", size: "~639 MB", sizeMB: 639 },
      { tag: "4b", size: "~2.5 GB", sizeMB: 2497 },
      { tag: "8b", size: "~4.7 GB", sizeMB: 4677 },
    ]
  },
  {
    name: "qwen3-next",
    blurb: "The first installment in the Qwen3-Next series with strong performance in terms of both parameter efficiency and inference speed.",
    params: "80B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~50 GB", sizeMB: 50351 },
      { tag: "80b", size: "~50 GB", sizeMB: 50351 },
    ]
  },
  {
    name: "qwen3-vl",
    blurb: "The most powerful vision-language model in the Qwen model family to date.",
    params: "2B–235B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~6.2 GB", sizeMB: 6191 },
      { tag: "2b", size: "~1.9 GB", sizeMB: 1890 },
      { tag: "4b", size: "~3.3 GB", sizeMB: 3337 },
      { tag: "8b", size: "~6.2 GB", sizeMB: 6191 },
      { tag: "30b", size: "~19.6 GB", sizeMB: 19595 },
      { tag: "32b", size: "~21 GB", sizeMB: 20910 },
      { tag: "235b", size: "~143 GB", sizeMB: 143269 },
    ]
  },
  {
    name: "qwen3.5",
    blurb: "Qwen 3.5 is a family of open-source multimodal models that delivers exceptional utility and performance.",
    params: "0.8B–122B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~6.6 GB", sizeMB: 6551 },
      { tag: "0.8b", size: "~1.3 GB", sizeMB: 1322 },
      { tag: "2b", size: "~2.7 GB", sizeMB: 2683 },
      { tag: "4b", size: "~3.3 GB", sizeMB: 3324 },
      { tag: "9b", size: "~6.6 GB", sizeMB: 6551 },
      { tag: "27b", size: "~17.5 GB", sizeMB: 17479 },
      { tag: "35b", size: "~22 GB", sizeMB: 22070 },
      { tag: "122b", size: "~81 GB", sizeMB: 81370 },
    ]
  },
  {
    name: "qwen3.6",
    blurb: "Qwen3.6 delivers substantial upgrades in agentic coding and thinking preservation than previous Qwen models.",
    params: "27B–35B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~23 GB", sizeMB: 22621 },
      { tag: "27b", size: "~17.8 GB", sizeMB: 17769 },
      { tag: "35b", size: "~23 GB", sizeMB: 22621 },
    ]
  },
  {
    name: "qwen3.8",
    blurb: "Qwen3.8 delivers substantial gains across coding, professional work, research, and long-horizon agentic tasks.",
    params: "27B",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "latest", size: "~17.7 GB", sizeMB: 17742 },
      { tag: "27b", size: "~17.7 GB", sizeMB: 17742 },
    ]
  },
  {
    name: "qwen3.8-flash-next",
    blurb: "This experimental preview of the architecture that will underpin Qwen4.",
    params: "",
    cats: ["chat", "reasoning", "vision"],
    variants: [
      { tag: "125b-a6b-nvfp4", size: "~105 GB", sizeMB: 104852 },
      { tag: "125b-a6b-q4_K_M", size: "~120 GB", sizeMB: 120058 },
      { tag: "125b-a6b-q8_0", size: "~189 GB", sizeMB: 189132 },
    ]
  },
  {
    name: "qwq",
    blurb: "QwQ is the reasoning model of the Qwen series.",
    params: "32B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~19.9 GB", sizeMB: 19851 },
      { tag: "32b", size: "~19.9 GB", sizeMB: 19851 },
    ]
  },
  {
    name: "r1-1776",
    blurb: "A version of the DeepSeek-R1 model that has been post trained to provide unbiased, accurate, and factual information by Perplexity.",
    params: "70B–671B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~43 GB", sizeMB: 42520 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
      { tag: "671b", size: "~404 GB", sizeMB: 404430 },
    ]
  },
  {
    name: "reader-lm",
    blurb: "A series of models that convert HTML content to Markdown content, which is useful for content conversion tasks.",
    params: "0.5B–1.5B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~935 MB", sizeMB: 935 },
      { tag: "0.5b", size: "~352 MB", sizeMB: 352 },
      { tag: "1.5b", size: "~935 MB", sizeMB: 935 },
    ]
  },
  {
    name: "reflection",
    blurb: "A high-performing model trained with a new technique called Reflection-tuning that teaches a LLM to detect mistakes in its reasoning and correct course.",
    params: "70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~40 GB", sizeMB: 39970 },
      { tag: "70b", size: "~40 GB", sizeMB: 39970 },
    ]
  },
  {
    name: "rnj-1",
    blurb: "Rnj-1 is a family of 8B parameter open-weight, dense models trained from scratch by Essential AI, optimized for code and STEM with capabilities on par with SOTA open-weight models.",
    params: "8B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.1 GB", sizeMB: 5114 },
      { tag: "8b", size: "~5.1 GB", sizeMB: 5114 },
    ]
  },
  {
    name: "sailor2",
    blurb: "Sailor2 are multilingual language models made for South-East Asia. Available in 1B, 8B, and 20B parameter sizes.",
    params: "1B–20B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.2 GB", sizeMB: 5243 },
      { tag: "1b", size: "~1.1 GB", sizeMB: 1056 },
      { tag: "8b", size: "~5.2 GB", sizeMB: 5243 },
      { tag: "20b", size: "~11.6 GB", sizeMB: 11622 },
    ]
  },
  {
    name: "samantha-mistral",
    blurb: "A companion assistant trained in philosophy, psychology, and personal relationships. Based on Mistral.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "shieldgemma",
    blurb: "ShieldGemma is set of instruction tuned models for evaluating the safety of text prompt input and text output responses against a set of defined safety policies.",
    params: "2B–27B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.8 GB", sizeMB: 5761 },
      { tag: "2b", size: "~1.7 GB", sizeMB: 1709 },
      { tag: "9b", size: "~5.8 GB", sizeMB: 5761 },
      { tag: "27b", size: "~16.6 GB", sizeMB: 16645 },
    ]
  },
  {
    name: "smallthinker",
    blurb: "A new small reasoning model fine-tuned from the Qwen 2.5 3B Instruct model.",
    params: "3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.6 GB", sizeMB: 3616 },
      { tag: "3b", size: "~3.6 GB", sizeMB: 3616 },
    ]
  },
  {
    name: "smollm",
    blurb: "🪐 A family of small models with 135M, 360M, and 1.7B parameters, trained on a new high-quality dataset.",
    params: "135M–1.7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~991 MB", sizeMB: 991 },
      { tag: "135m", size: "~92 MB", sizeMB: 92 },
      { tag: "360m", size: "~229 MB", sizeMB: 229 },
      { tag: "1.7b", size: "~991 MB", sizeMB: 991 },
    ]
  },
  {
    name: "snowflake-arctic-embed",
    blurb: "A suite of text embedding models by Snowflake, optimized for performance.",
    params: "22M–335M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~669 MB", sizeMB: 669 },
      { tag: "22m", size: "~46 MB", sizeMB: 46 },
      { tag: "33m", size: "~67 MB", sizeMB: 67 },
      { tag: "110m", size: "~219 MB", sizeMB: 219 },
      { tag: "137m", size: "~274 MB", sizeMB: 274 },
      { tag: "335m", size: "~669 MB", sizeMB: 669 },
    ]
  },
  {
    name: "snowflake-arctic-embed2",
    blurb: "Snowflake's frontier embedding model. Arctic Embed 2.0 adds multilingual support without sacrificing English performance or scalability.",
    params: "568M",
    cats: ["chat", "embedding"],
    variants: [
      { tag: "latest", size: "~1.2 GB", sizeMB: 1160 },
      { tag: "568m", size: "~1.2 GB", sizeMB: 1160 },
    ]
  },
  {
    name: "solar",
    blurb: "A compact, yet powerful 10.7B large language model designed for single-turn conversation.",
    params: "10.7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~6.1 GB", sizeMB: 6072 },
      { tag: "10.7b", size: "~6.1 GB", sizeMB: 6072 },
    ]
  },
  {
    name: "solar-pro",
    blurb: "Solar Pro Preview: an advanced large language model (LLM) with 22 billion parameters designed to fit into a single GPU",
    params: "22B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~13.3 GB", sizeMB: 13310 },
      { tag: "22b", size: "~13.3 GB", sizeMB: 13310 },
    ]
  },
  {
    name: "sqlcoder",
    blurb: "SQLCoder is a code completion model fined-tuned on StarCoder for SQL generation tasks",
    params: "7B–15B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "15b", size: "~9.0 GB", sizeMB: 8988 },
    ]
  },
  {
    name: "stable-beluga",
    blurb: "Llama 2 based model fine tuned on an Orca-style dataset. Originally called Free Willy.",
    params: "7B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "stable-code",
    blurb: "Stable Code 3B is a coding model with instruct and code completion variants on par with models such as Code Llama 7B that are 2.5x larger.",
    params: "3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1609 },
      { tag: "3b", size: "~1.6 GB", sizeMB: 1609 },
    ]
  },
  {
    name: "stablelm-zephyr",
    blurb: "A lightweight chat model allowing accurate, and responsive output without requiring high-end hardware.",
    params: "3B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.6 GB", sizeMB: 1609 },
      { tag: "3b", size: "~1.6 GB", sizeMB: 1609 },
    ]
  },
  {
    name: "stablelm2",
    blurb: "Stable LM 2 is a state-of-the-art 1.6B and 12B parameter language model trained on multilingual data in English, Spanish, German, Italian, French, Portuguese, and Dutch.",
    params: "1.6B–12B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~983 MB", sizeMB: 983 },
      { tag: "1.6b", size: "~983 MB", sizeMB: 983 },
      { tag: "12b", size: "~7.0 GB", sizeMB: 6969 },
    ]
  },
  {
    name: "starcoder",
    blurb: "StarCoder is a code generation model trained on 80+ programming languages.",
    params: "1B–15B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~1.8 GB", sizeMB: 1831 },
      { tag: "1b", size: "~726 MB", sizeMB: 726 },
      { tag: "3b", size: "~1.8 GB", sizeMB: 1831 },
      { tag: "7b", size: "~4.3 GB", sizeMB: 4295 },
      { tag: "15b", size: "~9.0 GB", sizeMB: 8988 },
    ]
  },
  {
    name: "starling-lm",
    blurb: "Starling is a large language model trained by reinforcement learning from AI feedback focused on improving chatbot helpfulness.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "strands-decider",
    blurb: "A 2B decision model from Strands Agents for fast classification, routing, and scoring.",
    params: "2B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~2.0 GB", sizeMB: 1975 },
      { tag: "2b", size: "~2.0 GB", sizeMB: 1975 },
    ]
  },
  {
    name: "tev1",
    blurb: "A 4B decision model from Together AI for fast classification.",
    params: "0.8B–4B",
    cats: ["chat", "reasoning"],
    variants: [
      { tag: "latest", size: "~4.6 GB", sizeMB: 4611 },
      { tag: "0.8b", size: "~834 MB", sizeMB: 834 },
      { tag: "4b", size: "~4.6 GB", sizeMB: 4611 },
    ]
  },
  {
    name: "tinydolphin",
    blurb: "An experimental 1.1B parameter model trained on the new Dolphin 2.8 dataset by Eric Hartford and based on TinyLlama.",
    params: "1.1B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~637 MB", sizeMB: 637 },
      { tag: "1.1b", size: "~637 MB", sizeMB: 637 },
    ]
  },
  {
    name: "translategemma",
    blurb: "A new collection of open translation models built on Gemma 3, helping people communicate across 55 languages.",
    params: "4B–27B",
    cats: ["chat", "vision"],
    variants: [
      { tag: "latest", size: "~3.3 GB", sizeMB: 3299 },
      { tag: "4b", size: "~3.3 GB", sizeMB: 3299 },
      { tag: "12b", size: "~8.1 GB", sizeMB: 8110 },
      { tag: "27b", size: "~17.4 GB", sizeMB: 17359 },
    ]
  },
  {
    name: "tulu3",
    blurb: "Tülu 3 is a leading instruction following model family, offering fully open-source data, code, and recipes by the The Allen Institute for AI.",
    params: "8B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "8b", size: "~4.9 GB", sizeMB: 4921 },
      { tag: "70b", size: "~43 GB", sizeMB: 42520 },
    ]
  },
  {
    name: "vicuna",
    blurb: "General use chat model based on Llama and Llama 2 with 2K to 16K context sizes.",
    params: "7B–33B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "33b", size: "~18.4 GB", sizeMB: 18356 },
    ]
  },
  {
    name: "wizard-math",
    blurb: "Model focused on math and logic problems",
    params: "7B–70B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "70b", size: "~39 GB", sizeMB: 38872 },
    ]
  },
  {
    name: "wizard-vicuna",
    blurb: "Wizard Vicuna is a 13B parameter model based on Llama 2 trained by MelodysDreamj.",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "wizard-vicuna-uncensored",
    blurb: "Wizard Vicuna Uncensored is a 7B, 13B, and 30B parameter model based on Llama 2 uncensored by Eric Hartford.",
    params: "7B–30B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "30b", size: "~18.4 GB", sizeMB: 18356 },
    ]
  },
  {
    name: "wizardcoder",
    blurb: "State-of-the-art code generation model",
    params: "33B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "33b", size: "~18.8 GB", sizeMB: 18819 },
    ]
  },
  {
    name: "wizardlm",
    blurb: "General use model based on Llama 2.",
    params: "",
    cats: ["chat"],
    variants: [
      { tag: "7b-q2_K", size: "~2.8 GB", sizeMB: 2826 },
      { tag: "7b-q3_K_S", size: "~2.9 GB", sizeMB: 2948 },
      { tag: "7b-q3_K_M", size: "~3.3 GB", sizeMB: 3298 },
    ]
  },
  {
    name: "wizardlm-uncensored",
    blurb: "Uncensored version of Wizard LM model",
    params: "13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~7.4 GB", sizeMB: 7366 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "wizardlm2",
    blurb: "State of the art large language model from Microsoft AI with improved performance on complex chat, multilingual, reasoning and agent use cases.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "xwinlm",
    blurb: "Conversational model based on Llama 2 that performs competitively on various benchmarks.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "yarn-llama2",
    blurb: "An extension of Llama 2 that supports a context of up to 128k tokens.",
    params: "7B–13B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "7b", size: "~3.8 GB", sizeMB: 3826 },
      { tag: "13b", size: "~7.4 GB", sizeMB: 7366 },
    ]
  },
  {
    name: "yarn-mistral",
    blurb: "An extension of Mistral to support context windows of 64K or 128K.",
    params: "7B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~4.1 GB", sizeMB: 4109 },
      { tag: "7b", size: "~4.1 GB", sizeMB: 4109 },
    ]
  },
  {
    name: "yi",
    blurb: "Yi 1.5 is a high-performing, bilingual language model.",
    params: "6B–34B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~3.5 GB", sizeMB: 3479 },
      { tag: "6b", size: "~3.5 GB", sizeMB: 3479 },
      { tag: "9b", size: "~5.0 GB", sizeMB: 5037 },
      { tag: "34b", size: "~19.5 GB", sizeMB: 19467 },
    ]
  },
  {
    name: "yi-coder",
    blurb: "Yi-Coder is a series of open-source code language models that delivers state-of-the-art coding performance with fewer than 10 billion parameters.",
    params: "1.5B–9B",
    cats: ["chat"],
    variants: [
      { tag: "latest", size: "~5.0 GB", sizeMB: 5037 },
      { tag: "1.5b", size: "~866 MB", sizeMB: 866 },
      { tag: "9b", size: "~5.0 GB", sizeMB: 5037 },
    ]
  },
];
