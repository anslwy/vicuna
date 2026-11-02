// OpenRouter runtime catalog + API-key helpers (Settings → OpenRouter).
//
// OpenRouter's public API sends CORS headers (`access-control-allow-origin: *`
// and `Authorization`/`X-Title` in allow-headers — verified), so both the dev
// browser and the Tauri webview call it directly. No Rust command is involved;
// the key never leaves the app except as a Bearer header to openrouter.ai.

const API_BASE = "https://openrouter.ai/api/v1";
const CACHE_KEY = "vicuna-openrouter-models";
const CACHE_VERSION = 1;
/** Descriptions are long and only used for search/tooltips; trim them so the
 *  450+ model cache stays comfortably inside the localStorage budget. */
const DESCRIPTION_LIMIT = 320;

/** Optional attribution header OpenRouter uses for its app rankings. */
const APP_HEADERS: Record<string, string> = { "X-Title": "Vicuna" };

export type OpenRouterFilter = "added" | "all" | "free" | "vision" | "tools" | "reasoning";

export const OPENROUTER_FILTERS: OpenRouterFilter[] = ["added", "all", "free", "vision", "tools", "reasoning"];

export interface OpenRouterModel {
  id: string;
  name: string;
  description: string;
  contextLength: number | null;
  /** Per-token USD strings exactly as the API sends them ("" → unknown). */
  promptPrice: string | null;
  completionPrice: string | null;
  inputModalities: string[];
  supportedParameters: string[];
  reasoning: boolean;
  created: number | null;
}

export interface OpenRouterKeyInfo {
  usage: number | null;
  limit: number | null;
  limitRemaining: number | null;
  isFreeTier: boolean;
  rateRequests: number | null;
  rateInterval: string | null;
}

export interface OpenRouterCache {
  updated: string;
  models: OpenRouterModel[];
}

// ---- selection ids ----

/** Model-menu selections carry this prefix so they can never collide with an
 *  Ollama tag (Ollama names may contain `/` and `:`, e.g. `ifm/k2-horizon:7b`)
 *  and the transport can pick a provider from the string alone. */
export const OPENROUTER_PREFIX = "openrouter:";

export function openRouterSelection(modelId: string): string {
  return `${OPENROUTER_PREFIX}${modelId}`;
}

export function isOpenRouterSelection(selection: string | null | undefined): boolean {
  return typeof selection === "string" && selection.startsWith(OPENROUTER_PREFIX);
}

/** Strip the provider prefix (`openrouter:`); harmless for Ollama tags. */
export function openRouterModelId(selection: string): string {
  return isOpenRouterSelection(selection) ? selection.slice(OPENROUTER_PREFIX.length) : selection;
}

/** Display form of a catalog id: OpenRouter prefixes rolling `-latest`
 *  pointers with `~` (verified live in the registry response — and the
 *  transport must send the id verbatim, tilde included). */
export function displayOpenRouterId(id: string): string {
  return id.startsWith("~") ? id.slice(1) : id;
}

export function findOpenRouterModel(models: OpenRouterModel[], selection: string): OpenRouterModel | null {
  if (!isOpenRouterSelection(selection)) return null;
  const id = openRouterModelId(selection);
  return models.find((m) => m.id === id) ?? null;
}

// ---- key accessor (shared by chat + Work transports) ----

let activeKey = "";

/** ChatApp syncs the saved prefs key here on load/save/clear. */
export function setOpenRouterApiKey(key: string): void {
  activeKey = key.trim();
}

export function openRouterApiKey(): string {
  return activeKey;
}

function str(v: unknown): string {
  return typeof v === "string" ? v : "";
}

function num(v: unknown): number | null {
  const n = typeof v === "number" ? v : typeof v === "string" && v.trim() !== "" ? Number(v) : NaN;
  return Number.isFinite(n) ? n : null;
}

function stringList(v: unknown): string[] {
  return Array.isArray(v) ? v.filter((s): s is string => typeof s === "string") : [];
}

/** Normalize one `/models` entry; null when it has no usable id. */
function normalizeModel(raw: unknown): OpenRouterModel | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = str(r.id).trim();
  if (!id) return null;
  const arch = (r.architecture ?? {}) as Record<string, unknown>;
  const pricing = (r.pricing ?? {}) as Record<string, unknown>;
  const params = stringList(r.supported_parameters);
  const description = str(r.description).trim();
  return {
    id,
    name: str(r.name).trim() || id,
    description: description.length > DESCRIPTION_LIMIT ? `${description.slice(0, DESCRIPTION_LIMIT).trimEnd()}…` : description,
    contextLength: num(r.context_length),
    promptPrice: typeof pricing.prompt === "string" ? pricing.prompt : null,
    completionPrice: typeof pricing.completion === "string" ? pricing.completion : null,
    inputModalities: stringList(arch.input_modalities),
    supportedParameters: params,
    reasoning: Boolean(r.reasoning) || params.includes("reasoning") || params.includes("include_reasoning"),
    created: num(r.created)
  };
}

function normalizeKeyInfo(raw: unknown): OpenRouterKeyInfo {
  const r = (raw ?? {}) as Record<string, unknown>;
  const rate = (r.rate_limit ?? {}) as Record<string, unknown>;
  return {
    usage: num(r.usage),
    limit: num(r.limit),
    limitRemaining: num(r.limit_remaining),
    isFreeTier: r.is_free_tier === true,
    rateRequests: num(rate.requests),
    rateInterval: str(rate.interval) || null
  };
}

async function describeError(res: Response): Promise<string> {
  let detail = "";
  try {
    const body = (await res.json()) as { error?: unknown; message?: unknown };
    const err = body.error;
    if (typeof err === "string") detail = err;
    else if (err && typeof err === "object") detail = str((err as Record<string, unknown>).message);
    if (!detail) detail = str(body.message);
  } catch {
    /* not JSON — fall back to the status code */
  }
  if (res.status === 401) {
    return detail ? `OpenRouter rejected the API key (401): ${detail}` : "OpenRouter rejected the API key (401). Check it and try again.";
  }
  if (res.status === 402) return "OpenRouter says this key is out of credit.";
  if (res.status === 403) return detail ? `OpenRouter refused this key (403): ${detail}` : "OpenRouter refused this key (403).";
  if (res.status === 429) return "OpenRouter is rate-limiting this key — wait a moment and retry.";
  const suffix = detail ? `: ${detail}` : "";
  return `OpenRouter request failed (${res.status})${suffix}`;
}

async function fetchJson(url: string, headers: Record<string, string>): Promise<unknown> {
  let res: Response;
  try {
    res = await fetch(url, { headers });
  } catch {
    throw new Error("Couldn't reach OpenRouter — check your internet connection.");
  }
  if (!res.ok) throw new Error(await describeError(res));
  try {
    return await res.json();
  } catch {
    throw new Error("OpenRouter returned an unreadable response.");
  }
}

function authHeaders(apiKey: string): Record<string, string> {
  const key = apiKey.trim();
  return key ? { ...APP_HEADERS, Authorization: `Bearer ${key}` } : { ...APP_HEADERS };
}

/** Runtime model catalog. The endpoint is public; a key (when configured) is
 *  sent along so account-specific availability is reflected. */
export async function fetchOpenRouterModels(apiKey = ""): Promise<OpenRouterModel[]> {
  const body = (await fetchJson(`${API_BASE}/models`, authHeaders(apiKey))) as { data?: unknown };
  const list = Array.isArray(body?.data) ? body.data : [];
  const models = list.map(normalizeModel).filter((m): m is OpenRouterModel => m !== null);
  if (models.length === 0) throw new Error("OpenRouter returned no models.");
  return models.sort((a, b) => a.name.localeCompare(b.name) || a.id.localeCompare(b.id));
}

/** Validate a key and return its account/usage status. Empty keys never hit
 *  the network. Throws with a user-facing message on failure. */
export async function testOpenRouterKey(apiKey: string): Promise<OpenRouterKeyInfo> {
  const key = apiKey.trim();
  if (!key) throw new Error("Enter an API key first.");
  const body = (await fetchJson(`${API_BASE}/key`, authHeaders(key))) as { data?: unknown };
  return normalizeKeyInfo(body?.data);
}

// ---- cache (localStorage; survives restarts, shared by desktop + browser) ----

/** Cached entries are already normalized (contextLength, promptPrice, …),
 *  unlike the raw `/models` payload — re-running `normalizeModel` on them
 *  silently blanks every field it doesn't recognize (hit live: context and
 *  pricing vanished after a reload). */
function normalizeCachedModel(raw: unknown): OpenRouterModel | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  const id = str(r.id).trim();
  if (!id) return null;
  return {
    id,
    name: str(r.name).trim() || id,
    description: str(r.description),
    contextLength: num(r.contextLength),
    promptPrice: typeof r.promptPrice === "string" ? r.promptPrice : null,
    completionPrice: typeof r.completionPrice === "string" ? r.completionPrice : null,
    inputModalities: stringList(r.inputModalities),
    supportedParameters: stringList(r.supportedParameters),
    reasoning: r.reasoning === true,
    created: num(r.created)
  };
}

export function readOpenRouterCache(): OpenRouterCache | null {
  try {
    const raw = localStorage.getItem(CACHE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as { version?: unknown; updated?: unknown; models?: unknown };
    if (parsed.version !== CACHE_VERSION || !Array.isArray(parsed.models)) return null;
    const models = parsed.models.map(normalizeCachedModel).filter((m): m is OpenRouterModel => m !== null);
    if (models.length === 0) return null;
    return { updated: str(parsed.updated) || new Date().toISOString(), models };
  } catch {
    return null;
  }
}

export function writeOpenRouterCache(models: OpenRouterModel[]): string {
  const updated = new Date().toISOString();
  try {
    localStorage.setItem(CACHE_KEY, JSON.stringify({ version: CACHE_VERSION, updated, models }));
  } catch {
    /* quota or private mode — the catalog is still usable this session */
  }
  return updated;
}

// ---- display helpers ----

function trimNumber(n: number): string {
  return n >= 10 ? String(Math.round(n)) : String(Math.round(n * 10) / 10);
}

/** 1048576 → "1M", 128000 → "128K", 8000 → "8K". */
export function formatContextLength(n: number | null): string {
  if (n === null || n <= 0) return "—";
  if (n >= 1_000_000) return `${trimNumber(n / 1_000_000)}M`;
  if (n >= 1000) return `${Math.round(n / 1000)}K`;
  return String(n);
}

function priceValue(v: string | null): number | null {
  if (v === null || v.trim() === "") return null;
  const n = Number(v);
  return Number.isFinite(n) ? n : null;
}

function perMillion(v: number): string {
  if (v < 0) return "varies";
  if (v === 0) return "$0";
  const perM = v * 1_000_000;
  if (perM >= 1) return `$${perM.toFixed(2)}`;
  if (perM >= 0.01) return `$${String(Number(perM.toFixed(3)))}`;
  return `$${perM.toPrecision(2)}`;
}

/** "Free", or "$0.15 in · $0.60 out" (prices are per million tokens). */
export function formatModelPricing(model: OpenRouterModel): string {
  const prompt = priceValue(model.promptPrice);
  const completion = priceValue(model.completionPrice);
  if (prompt === 0 && completion === 0) return "Free";
  const bits: string[] = [];
  if (prompt !== null) bits.push(`${perMillion(prompt)} in`);
  if (completion !== null) bits.push(`${perMillion(completion)} out`);
  return bits.length > 0 ? bits.join(" · ") : "Pricing varies";
}

export function isFreeModel(model: OpenRouterModel): boolean {
  return priceValue(model.promptPrice) === 0 && priceValue(model.completionPrice) === 0;
}

export function supportsVision(model: OpenRouterModel): boolean {
  return model.inputModalities.includes("image");
}

export function supportsTools(model: OpenRouterModel): boolean {
  return model.supportedParameters.includes("tools");
}

/** Keep only the tail of a key so it can be shown in settings safely. */
export function maskKey(key: string): string {
  const k = key.trim();
  if (!k) return "";
  if (k.length <= 10) return "•".repeat(k.length);
  return `${k.slice(0, 7)}…${k.slice(-4)}`;
}

export function formatUsd(v: number | null): string {
  if (v === null) return "—";
  return v >= 1 ? `$${v.toFixed(2)}` : `$${v.toFixed(4)}`;
}

/** Compact footer cost: "$0" for free, 2 decimals ≥$1, 4 decimals ≥$0.01,
 *  otherwise up to 6 decimals trimmed (0.00014 → "$0.00014"). Null → null
 *  so Ollama/local footers stay unchanged. */
export function formatCostUsd(cost: number | null | undefined): string | null {
  if (cost == null || !Number.isFinite(cost)) return null;
  if (cost === 0) return "$0";
  if (cost >= 1) return `$${cost.toFixed(2)}`;
  if (cost >= 0.01) return `$${cost.toFixed(4)}`;
  return `$${cost.toFixed(6).replace(/0+$/, "").replace(/\.$/, "")}`;
}

/** "just now" / "12m ago" / "2h ago" / "3d ago". */
export function formatCacheAge(iso: string): string {
  const t = Date.parse(iso);
  if (!Number.isFinite(t)) return "unknown";
  const mins = Math.max(0, Math.round((Date.now() - t) / 60000));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.round(hours / 24)}d ago`;
}

/** Search + capability filter shared by the settings list. Every whitespace
 *  token must match the id/name/description. */
export function filterOpenRouterModels(models: OpenRouterModel[], query: string, filter: OpenRouterFilter): OpenRouterModel[] {
  const tokens = query.toLowerCase().split(/\s+/).filter(Boolean);
  return models.filter((m) => {
    if (filter === "free" && !isFreeModel(m)) return false;
    if (filter === "vision" && !supportsVision(m)) return false;
    if (filter === "tools" && !supportsTools(m)) return false;
    if (filter === "reasoning" && !m.reasoning) return false;
    if (tokens.length === 0) return true;
    const hay = `${m.id} ${m.name} ${m.description}`.toLowerCase();
    return tokens.every((t) => hay.includes(t));
  });
}

// ---- chat transport (OpenAI-compatible SSE) ----
//
// The frontend calls this directly (OpenRouter sends CORS headers), so chat
// mode needs no Rust command. Work mode reuses the same request shape in
// `src-tauri/src/work_agent.rs` because the agent loop owns tool execution.

/** Structurally compatible with `OllamaToolCall` / `ChatMsg` in ollama.ts;
 *  duplicated here so the transport has no import cycle with the adapter. */
export interface OpenRouterToolCall {
  id?: string;
  function: { index?: number; name: string; arguments: Record<string, unknown> };
}

export interface OpenRouterToolDef {
  type: "function";
  function: { name: string; description: string; parameters: Record<string, unknown> };
}

export interface OpenRouterChatMessage {
  role: "user" | "assistant" | "system" | "tool";
  content: string;
  images?: string[];
  tool_calls?: Array<{ id?: string; function: { name: string; arguments: unknown } }>;
  tool_name?: string;
}

export interface OpenRouterChatOptions {
  think?: boolean | string;
  temperature?: number;
  stop?: string[];
  tools?: OpenRouterToolDef[];
}

export interface OpenRouterChatResult {
  text: string;
  evalCount: number;
  evalDurationNs: number;
  toolCalls: OpenRouterToolCall[];
  /** Authoritative charge for this request in USD (`usage.cost`). Null when
   *  the API omitted it (e.g. aborted streams) — callers may fall back to
   *  token × catalog pricing. */
  cost: number | null;
  promptTokens: number | null;
  completionTokens: number | null;
}

const EFFORT_LEVELS = new Set(["minimal", "low", "medium", "high", "xhigh"]);

/** Map Vicuna's `think` value onto OpenRouter's `reasoning` object.
 *  Auto/true means "model default" (omit the param): forcing `enabled: true`
 *  would switch reasoning on for models that default it off. */
function reasoningParam(think: boolean | string | undefined): Record<string, unknown> | null {
  if (think === false) return { enabled: false };
  if (typeof think === "string" && EFFORT_LEVELS.has(think)) return { effort: think };
  return null;
}

function toolArgsString(args: unknown): string {
  if (typeof args === "string") return args;
  if (args && typeof args === "object") return JSON.stringify(args);
  return "{}";
}

/** History → OpenAI chat messages. Tool results need a `tool_call_id`; the
 *  app's ChatMsg only carries `tool_name`, so ids are assigned deterministically
 *  (or reused when a prior call already had one) and matched in order. */
export function toOpenRouterMessages(messages: OpenRouterChatMessage[], stripImages = false): unknown[] {
  const out: unknown[] = [];
  const pending: Array<{ name: string; id: string }> = [];
  let seq = 0;
  for (const m of messages) {
    if (m.role === "assistant") {
      const wire: Record<string, unknown> = { role: "assistant", content: m.content ?? "" };
      if (m.tool_calls?.length) {
        wire.tool_calls = m.tool_calls.map((tc) => {
          const name = tc?.function?.name ?? "";
          const id = tc.id || `call_${seq++}`;
          if (name) pending.push({ name, id });
          return {
            id,
            type: "function",
            function: { name, arguments: toolArgsString(tc?.function?.arguments) }
          };
        });
      }
      out.push(wire);
      continue;
    }
    if (m.role === "tool") {
      const name = m.tool_name ?? "";
      const at = pending.findIndex((p) => p.name === name);
      const pick = at >= 0 ? pending.splice(at, 1)[0] : pending.shift();
      out.push({ role: "tool", tool_call_id: pick?.id ?? `call_${seq++}`, content: m.content });
      continue;
    }
    if (!stripImages && m.images?.length) {
      const parts: unknown[] = [];
      if (m.content) parts.push({ type: "text", text: m.content });
      for (const url of m.images) parts.push({ type: "image_url", image_url: { url } });
      out.push({ role: m.role, content: parts });
      continue;
    }
    out.push({ role: m.role, content: m.content });
  }
  return out;
}

function contentText(v: unknown): string {
  if (typeof v === "string") return v;
  if (Array.isArray(v)) {
    return v
      .map((p) => (p && typeof p === "object" && typeof (p as Record<string, unknown>).text === "string" ? String((p as Record<string, unknown>).text) : ""))
      .join("");
  }
  return "";
}

/** Stream one OpenRouter chat completion. Throws user-facing errors. */
export async function openRouterChatStream(
  model: string,
  messages: OpenRouterChatMessage[],
  onToken: (t: string) => void,
  signal: AbortSignal | undefined,
  onThink: ((t: string) => void) | undefined,
  opts: OpenRouterChatOptions
): Promise<OpenRouterChatResult> {
  const key = activeKey;
  if (!key) throw new Error("No OpenRouter API key — add one in Settings → OpenRouter.");
  const body: Record<string, unknown> = {
    model,
    messages: toOpenRouterMessages(messages),
    stream: true,
    usage: { include: true }
  };
  const reasoning = reasoningParam(opts.think);
  if (reasoning) body.reasoning = reasoning;
  if (opts.temperature !== undefined) body.temperature = opts.temperature;
  if (opts.stop?.length) body.stop = opts.stop.slice(0, 4);
  if (opts.tools?.length) body.tools = opts.tools;
  let res: Response;
  try {
    res = await fetch(`${API_BASE}/chat/completions`, {
      method: "POST",
      headers: { ...authHeaders(key), "Content-Type": "application/json" },
      body: JSON.stringify(body),
      signal
    });
  } catch (e: unknown) {
    if (e instanceof Error && e.name === "AbortError") throw e;
    throw new Error("Couldn't reach OpenRouter — check your internet connection.");
  }
  if (!res.ok) throw new Error(await describeError(res));

  const calls = new Map<number, { id?: string; name: string; args: string }>();
  let usageOut: number | null = null;
  let promptOut: number | null = null;
  let costOut: number | null = null;
  let full = "";
  let genStart = 0;
  let genEnd = 0;
  const takeUsage = (obj: Record<string, unknown>) => {
    const u = obj.usage as Record<string, unknown> | undefined;
    if (!u) return;
    if (typeof u.completion_tokens === "number") usageOut = u.completion_tokens;
    if (typeof u.prompt_tokens === "number") promptOut = u.prompt_tokens;
    if (typeof u.cost === "number" && Number.isFinite(u.cost)) costOut = u.cost;
  };
  const feed = (payload: string) => {
    if (!payload || payload === "[DONE]") return;
    let obj: Record<string, unknown>;
    try {
      obj = JSON.parse(payload) as Record<string, unknown>;
    } catch {
      return;
    }
    if (obj.error) {
      const err = obj.error;
      const detail = typeof err === "string" ? err : str((err as Record<string, unknown>)?.message) || JSON.stringify(err);
      throw new Error(`OpenRouter error: ${detail}`);
    }
    takeUsage(obj);
    const choices = Array.isArray(obj.choices) ? obj.choices : [];
    const choice = choices[0] as Record<string, unknown> | undefined;
    if (!choice) return;
    const delta = (choice.delta ?? {}) as Record<string, unknown>;
    const thinkPiece = contentText(delta.reasoning) || contentText(delta.reasoning_content);
    if (thinkPiece) {
      if (!genStart) genStart = performance.now();
      onThink?.(thinkPiece);
    }
    const piece = contentText(delta.content);
    if (piece) {
      if (!genStart) genStart = performance.now();
      genEnd = performance.now();
      full += piece;
      onToken(piece);
    }
    const deltas = delta.tool_calls;
    if (Array.isArray(deltas)) {
      if (!genStart) genStart = performance.now();
      genEnd = performance.now();
      for (const raw of deltas) {
        if (!raw || typeof raw !== "object") continue;
        const tc = raw as Record<string, unknown>;
        const index = typeof tc.index === "number" ? tc.index : calls.size;
        let slot = calls.get(index);
        if (!slot) {
          slot = { name: "", args: "" };
          calls.set(index, slot);
        }
        if (typeof tc.id === "string" && tc.id) slot.id = tc.id;
        const fn = (tc.function ?? {}) as Record<string, unknown>;
        if (typeof fn.name === "string" && fn.name) slot.name += fn.name;
        if (typeof fn.arguments === "string") slot.args += fn.arguments;
        else if (fn.arguments && typeof fn.arguments === "object") slot.args += JSON.stringify(fn.arguments);
      }
    }
  };

  if (!res.body) {
    const data = (await res.json().catch(() => ({}))) as Record<string, unknown>;
    takeUsage(data);
    const choice = (Array.isArray(data.choices) ? data.choices[0] : null) as Record<string, unknown> | null;
    const msg = (choice?.message ?? {}) as Record<string, unknown>;
    const text = contentText(msg.content);
    if (text) onToken(text);
    full = text;
  } else {
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += decoder.decode(value, { stream: true });
      let nl: number;
      while ((nl = buf.indexOf("\n")) >= 0) {
        const line = buf.slice(0, nl).replace(/\r$/, "");
        buf = buf.slice(nl + 1);
        if (!line.startsWith("data:")) continue;
        feed(line.slice(5).trim());
      }
    }
    if (buf.trim().startsWith("data:")) feed(buf.trim().slice(5).trim());
  }

  const toolCalls: OpenRouterToolCall[] = [];
  for (const [index, c] of [...calls.entries()].sort((a, b) => a[0] - b[0])) {
    if (!c.name) continue;
    let args: Record<string, unknown> = {};
    if (c.args.trim()) {
      try {
        const parsed = JSON.parse(c.args) as unknown;
        if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) args = parsed as Record<string, unknown>;
      } catch {
        /* leave empty — the agent reports the malformed call */
      }
    }
    toolCalls.push({ id: c.id, function: { index, name: c.name, arguments: args } });
  }
  const durationMs = genStart > 0 ? Math.max(0, genEnd - genStart) : 0;
  const evalCount = usageOut ?? Math.max(0, Math.round(full.length / 4));
  return { text: full, evalCount, evalDurationNs: Math.round(durationMs * 1e6), toolCalls, cost: costOut, promptTokens: promptOut, completionTokens: usageOut };
}
