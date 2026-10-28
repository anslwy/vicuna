// Vicuna Ollama client.
//
// Architecture: the Rust backend (src-tauri/src/lib.rs) owns all inference
// traffic to Ollama. These functions are display-layer adapters: inside the
// Tauri webview they invoke backend commands and render streamed events;
// outside Tauri (plain `npm run dev` in a browser) they fall back to direct
// fetch so web-only dev keeps working without the shell.

export const OLLAMA_BASE = "http://localhost:11434";
/** Plain-browser override for the Ollama endpoint (Settings → General →
 *  Ollama host). Empty = default above. The Tauri backend always uses the
 *  local Ollama and ignores this. */
let customBase = "";

export function setOllamaBase(url: string): void {
  customBase = (url || "").trim().replace(/\/+$/, "");
}

export function ollamaBase(): string {
  return customBase || OLLAMA_BASE;
}

/** Cloud-hosted models (tags like `cloud`, `31b-cloud`) run on Ollama's
 *  servers: they never appear in /api/ps, so Load/Unload has no meaning
 *  for them — chatting just works. */
export function isCloudModel(name: string): boolean {
  const tag = name.includes(":") ? (name.split(":").pop() ?? "") : "latest";
  return tag === "cloud" || tag.endsWith("-cloud");
}

export function isTauriRuntime(): boolean {
  return (
    typeof window !== "undefined" &&
    ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}

type TauriCore = typeof import("@tauri-apps/api/core");

async function tauri(): Promise<TauriCore> {
  return await import("@tauri-apps/api/core");
}

export interface OllamaTag {
  name: string;
  model: string;
  size: number;
  modified_at: string;
}

export async function listModels(): Promise<OllamaTag[]> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    const data = await invoke<{ models?: OllamaTag[] }>("ollama_list_models");
    return data.models ?? [];
  }
  const res = await fetch(`${ollamaBase()}/api/tags`);
  if (!res.ok) throw new Error(`ollama tags failed: ${res.status}`);
  const data = await res.json();
  return (data.models ?? []) as OllamaTag[];
}

export interface RunningModel {
  name: string;
  model: string;
  size: number;
  digest: string;
  expires_at: string;
  size_vram: number;
}

export async function listRunning(): Promise<RunningModel[]> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    const data = await invoke<{ models?: RunningModel[] }>("ollama_list_running");
    return data.models ?? [];
  }
  const res = await fetch(`${ollamaBase()}/api/ps`);
  if (!res.ok) throw new Error(`ollama ps failed: ${res.status}`);
  const data = await res.json();
  return (data.models ?? []) as RunningModel[];
}

/** Local Ollama server version (`/api/version`). Throws when unreachable. */
export async function serverVersion(): Promise<string> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    return await invoke<string>("ollama_server_version");
  }
  const res = await fetch(`${ollamaBase()}/api/version`).catch(() => {
    throw new Error("ollama server not reachable");
  });
  if (!res.ok) throw new Error("ollama server not reachable");
  const data = await res.json();
  return data?.version ?? "unknown";
}

/**
 * Start a local `ollama serve` when none is reachable (Tauri only — a plain
 * browser cannot spawn processes). Adopts an already-running server instead
 * of spawning a second one. Resolves with the server version once ready.
 */
export async function startServer(): Promise<string> {
  if (!isTauriRuntime()) {
    throw new Error("start the server in a terminal: `ollama serve`");
  }
  const { invoke } = await tauri();
  return await invoke<string>("ollama_start_server");
}

/**
 * Stop the `ollama serve` this app launched. Refuses when Vicuna didn't
 * start one — foreign servers die in their own terminal.
 */
export async function stopServer(): Promise<void> {
  if (!isTauriRuntime()) {
    throw new Error("stop your own `ollama serve` in its terminal");
  }
  const { invoke } = await tauri();
  await invoke("ollama_stop_server");
}
/** Load a model into memory (or extend its residency). Generates a single
 *  throwaway token; uses the same context size as chat so follow-up chats
 *  don't trigger a reload. */
export async function loadModelIntoMemory(name: string): Promise<void> {
  await warmModel(name, "30m");
}

/** Unload a model from memory. */
export async function unloadModelFromMemory(name: string): Promise<void> {
  await warmModel(name, "0");
}

async function warmModel(name: string, keepAlive: string): Promise<void> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    await invoke("ollama_warm_model", { model: name, keepAlive });
    return;
  }
  const res = await fetch(`${ollamaBase()}/api/generate`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      model: name,
      prompt: "hi",
      stream: false,
      think: false,
      keep_alive: keepAlive,
      options: { num_predict: 1, num_ctx: 32768 }
    })
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`model ${keepAlive === "0" ? "unload" : "load"} failed: ${res.status} ${txt}`);
  }
  await res.json().catch(() => {});
}

export interface ChatMsg {
  role: "user" | "assistant" | "system";
  content: string;
  thinking?: string;
  thinkingOpen?: boolean;
  /** model that generated this message (assistant only) */
  model?: string;
  /** attached images as data URLs (user only). Stripped to raw base64 at
   *  the transport layer — Ollama's `images[]` takes base64, not data URLs. */
  images?: string[];
  /** wall-clock ms spent generating this message (accumulates across Continue) */
  genMs?: number;
  /** generation speed in tok/s from Ollama's eval stats (latest run) */
  genTps?: number;
}

/** Raw base64 for Ollama: drop any `data:...;base64,` prefix. */
function toB64(u: string): string {
  const i = u.indexOf(",");
  return i >= 0 ? u.slice(i + 1) : u;
}

/** Transport shape: only fields Ollama understands. */
function toWireMessages(messages: ChatMsg[]) {
  return messages.map((m) => ({
    role: m.role,
    content: m.content,
    ...(m.images?.length ? { images: m.images.map(toB64) } : {})
  }));
}

function stripImages(messages: ChatMsg[]): ChatMsg[] {
  return messages.map((m) => (m.images?.length ? { ...m, images: undefined } : m));
}

type ChatEvent =
  | { type: "token"; text: string }
  | { type: "think"; text: string }
  | { type: "done"; done_reason?: string | null; eval_count?: number | null; eval_duration?: number | null };

async function chatStreamViaBackend(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal: AbortSignal | undefined,
  onThink: ((t: string) => void) | undefined,
  opts: { think?: boolean; temperature?: number; stop?: string[] }
): Promise<{ text: string; doneReason: string; evalCount: number; evalDurationNs: number }> {
  const { invoke, Channel } = await tauri();
  if (signal?.aborted) throw new DOMException("aborted", "AbortError");
  let doneReason = "";
  let evalCount = 0;
  let evalDurationNs = 0;
  const onEvent = new Channel<ChatEvent>();
  onEvent.onmessage = (ev) => {
    if (signal?.aborted) return;
    if (ev.type === "token") onToken(ev.text);
    else if (ev.type === "think") onThink?.(ev.text);
    else if (ev.type === "done") {
      if (ev.done_reason) doneReason = ev.done_reason;
      if (typeof ev.eval_count === "number") evalCount = ev.eval_count;
      if (typeof ev.eval_duration === "number") evalDurationNs = ev.eval_duration;
    }
  };
  const invokePromise = invoke<string>("ollama_chat_stream", {
    model,
    messages: toWireMessages(messages),
    think: opts.think ?? true,
    temperature: opts.temperature ?? null,
    stop: opts.stop ?? null,
    onEvent
  });
  if (!signal) {
    const text = await invokePromise;
    return { text, doneReason, evalCount, evalDurationNs };
  }
  // Abort: stop rendering and surface AbortError; the backend request runs
  // to completion detached (documented limitation — no cancel propagation).
  const text = await Promise.race([
    invokePromise,
    new Promise<never>((_, reject) => {
      signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), {
        once: true
      });
    })
  ]);
  return { text, doneReason, evalCount, evalDurationNs };
}

async function chatStreamViaFetch(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal: AbortSignal | undefined,
  onThink: ((t: string) => void) | undefined,
  opts: { think?: boolean; temperature?: number; stop?: string[] }
): Promise<{ text: string; doneReason: string; evalCount: number; evalDurationNs: number }> {
  const options: Record<string, number | string[]> = { num_ctx: 32768 };
  // temperature only set when explicitly requested (continue uses low temp
  // to stay on the most likely next token); otherwise server default
  if (opts.temperature !== undefined) options.temperature = opts.temperature;
  if (opts.stop !== undefined) options.stop = opts.stop;
  const res = await fetch(`${ollamaBase()}/api/chat`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    // num_ctx: long chats (stories, code) exceed Ollama's small default
    // window, which makes continued generations forget the middle and
    // diverge into generic wrap-ups. 32k fits comfortably on this machine.
    body: JSON.stringify({ model, messages: toWireMessages(messages), stream: true, think: opts.think ?? true, options }),
    signal
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`ollama chat failed: ${res.status} ${txt}`);
  }
  if (!res.body) {
    // fallback non-stream
    const data = await res.json();
    const content = data?.message?.content ?? "";
    const thinking = data?.message?.thinking ?? "";
    if (thinking && onThink) onThink(thinking);
    onToken(content);
    return { text: content, doneReason: "", evalCount: 0, evalDurationNs: 0 };
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  let full = "";
  let doneReason = "";
  let evalCount = 0;
  let evalDurationNs = 0;
  const feed = (line: string) => {
    const l = line.trim();
    if (!l) return;
    try {
      const obj = JSON.parse(l);
      if (typeof obj?.done_reason === "string") doneReason = obj.done_reason;
      if (typeof obj?.eval_count === "number") evalCount = obj.eval_count;
      if (typeof obj?.eval_duration === "number") evalDurationNs = obj.eval_duration;
      const thinkPiece: string = obj?.message?.thinking ?? "";
      if (thinkPiece && onThink) onThink(thinkPiece);
      const piece: string = obj?.message?.content ?? "";
      if (piece) {
        full += piece;
        onToken(piece);
      }
    } catch {
      // ignore partial json
    }
  };
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) feed(line);
  }
  if (buf.trim()) feed(buf);
  return { text: full, doneReason, evalCount, evalDurationNs };
}

export async function chatStream(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal?: AbortSignal,
  onThink?: (t: string) => void,
  attempt = 0,
  opts: { think?: boolean; temperature?: number; stop?: string[]; onImagesStripped?: () => void } = {}
): Promise<{ text: string; evalCount: number; evalDurationNs: number }> {
  try {
    const { text, doneReason, evalCount, evalDurationNs } = isTauriRuntime()
      ? await chatStreamViaBackend(model, messages, onToken, signal, onThink, opts)
      : await chatStreamViaFetch(model, messages, onToken, signal, onThink, opts);
    if (text === "" && doneReason === "load" && attempt < 3 && !signal?.aborted) {
      // model was (re)loading instead of generating: the request triggered the
      // load, so waiting briefly and retrying generates normally
      await new Promise((r) => setTimeout(r, 2000));
      if (!signal?.aborted) {
        return chatStream(model, messages, onToken, signal, onThink, attempt + 1, opts);
      }
    }
    return { text, evalCount, evalDurationNs };
  } catch (e: unknown) {
    if (isVisionUnsupportedError(e)) {
      // text-only model: retry the same turn without images (the stripped
      // request can't retrigger this, so no attempt gate needed). The caller
      // surfaces a note via onImagesStripped so the user knows the model
      // never saw the picture.
      opts.onImagesStripped?.();
      return chatStream(model, stripImages(messages), onToken, signal, onThink, attempt, opts);
    }
    if (attempt === 0 && isThinkUnsupportedError(e)) {
      // plain (non-reasoning) models reject think:true outright: retry once
      // with thinking disabled instead of failing the turn
      return chatStream(model, messages, onToken, signal, onThink, 1, { ...opts, think: false });
    }
    throw e;
  }
}

function isVisionUnsupportedError(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return msg.includes("does not support multimodal");
}

function isThinkUnsupportedError(e: unknown): boolean {
  const msg = e instanceof Error ? e.message : String(e);
  return msg.includes("does not support thinking");
}

/**
 * Render a chat history as plain "Role: text" transcript lines, ending with
 * the partial assistant reply. Used as a raw /api/generate prompt so the
 * model continues exactly where the reply stopped.
 */
export function chatToTranscript(messages: ChatMsg[]): string {
  return messages
    .map((m) => {
      const who = m.role === "user" ? "User" : m.role === "assistant" ? "Assistant" : "System";
      return `${who}: ${m.content}`;
    })
    .join("\n\n");
}

type GenerateEvent =
  | { type: "token"; text: string }
  | { type: "done"; done_reason?: string | null };

/**
 * Raw text completion via /api/generate (no chat template applied).
 * Best-effort continuation: the caller passes a transcript ending mid-reply.
 * Custom stop words end generation if the model tries to start a new turn.
 */
export async function generateStream(
  model: string,
  prompt: string,
  onToken: (t: string) => void,
  signal?: AbortSignal,
  onThink?: (t: string) => void,
  attempt = 0,
  opts: { temperature?: number; stop?: string[] } = {}
): Promise<string> {
  void onThink;
  if (isTauriRuntime()) {
    const { invoke, Channel } = await tauri();
    if (signal?.aborted) throw new DOMException("aborted", "AbortError");
    let doneReason = "";
    const onEvent = new Channel<GenerateEvent>();
    onEvent.onmessage = (ev) => {
      if (signal?.aborted) return;
      if (ev.type === "token") onToken(ev.text);
      else if (ev.type === "done" && ev.done_reason) doneReason = ev.done_reason;
    };
    const invokePromise = invoke<string>("ollama_generate_stream", {
      model,
      prompt,
      temperature: opts.temperature ?? null,
      stop: opts.stop ?? null,
      onEvent
    });
    const text = signal
      ? await Promise.race([
          invokePromise,
          new Promise<never>((_, reject) => {
            signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), {
              once: true
            });
          })
        ])
      : await invokePromise;
    if (text === "" && doneReason === "load" && attempt < 3 && !signal?.aborted) {
      await new Promise((r) => setTimeout(r, 2000));
      if (!signal?.aborted) {
        return generateStream(model, prompt, onToken, signal, onThink, attempt + 1, opts);
      }
    }
    return text;
  }
  const options: Record<string, number> = { num_ctx: 32768 };
  if (opts.temperature !== undefined) options.temperature = opts.temperature;
  let res: Response;
  try {
    res = await fetch(`${ollamaBase()}/api/generate`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        model,
        prompt,
        stream: true,
        raw: true,
        think: false,
        stop: opts.stop ?? ["\nUser:", "\nAssistant:", "<end_of_turn>", "<|im_end|>"],
        options
      }),
      signal
    });
  } catch (e: unknown) {
    throw e;
  }
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`ollama generate failed: ${res.status} ${txt}`);
  }
  if (!res.body) {
    const data = await res.json();
    const content: string = data?.response ?? "";
    if (content) onToken(content);
    return content;
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  let full = "";
  let doneReason = "";
  const feed = (line: string) => {
    const l = line.trim();
    if (!l) return;
    try {
      const obj = JSON.parse(l);
      if (typeof obj?.done_reason === "string") doneReason = obj.done_reason;
      const piece: string = obj?.response ?? "";
      if (piece) {
        full += piece;
        onToken(piece);
      }
    } catch {
      // ignore partial json
    }
  };
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) feed(line);
  }
  if (buf.trim()) feed(buf);
  if (full === "" && doneReason === "load" && attempt < 3 && !signal?.aborted) {
    await new Promise((r) => setTimeout(r, 2000));
    if (!signal?.aborted) {
      return generateStream(model, prompt, onToken, signal, onThink, attempt + 1, opts);
    }
  }
  return full;
}

export interface PullProgress {
  status: string;
  digest?: string;
  total?: number;
  completed?: number;
}

/**
 * Pull (download) a model from the Ollama registry. Progress lines from
 * /api/pull are forwarded to onProgress; the stream ends with
 * {"status":"success"}. Abortable via signal in the plain-browser fallback;
 * in Tauri the backend request runs detached (same limitation as chat abort).
 */
export async function pullModel(
  name: string,
  onProgress: (p: PullProgress) => void,
  signal?: AbortSignal
): Promise<void> {
  if (isTauriRuntime()) {
    const { invoke, Channel } = await tauri();
    if (signal?.aborted) throw new DOMException("aborted", "AbortError");
    const onEvent = new Channel<PullProgress>();
    onEvent.onmessage = (ev) => {
      if (signal?.aborted) return;
      onProgress(ev);
    };
    const invokePromise = invoke<void>("ollama_pull_model", { model: name, onEvent });
    if (!signal) {
      await invokePromise;
      return;
    }
    await Promise.race([
      invokePromise,
      new Promise<never>((_, reject) => {
        signal.addEventListener("abort", () => reject(new DOMException("aborted", "AbortError")), {
          once: true
        });
      })
    ]);
    return;
  }
  const res = await fetch(`${ollamaBase()}/api/pull`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: name, stream: true }),
    signal
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`model pull failed: ${res.status} ${txt}`);
  }
  if (!res.body) {
    await res.json().catch(() => {});
    return;
  }
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  let buf = "";
  const feed = (line: string) => {
    const l = line.trim();
    if (!l) return;
    let obj: Record<string, unknown>;
    try {
      obj = JSON.parse(l) as Record<string, unknown>;
    } catch {
      return; // ignore partial json
    }
    if (typeof obj.error === "string" && obj.error) {
      throw new Error(`model pull failed: ${obj.error}`);
    }
    if (typeof obj.status === "string") {
      onProgress({
        status: obj.status,
        digest: typeof obj.digest === "string" ? obj.digest : undefined,
        total: typeof obj.total === "number" ? obj.total : undefined,
        completed: typeof obj.completed === "number" ? obj.completed : undefined
      });
    }
  };
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    buf += decoder.decode(value, { stream: true });
    const lines = buf.split("\n");
    buf = lines.pop() ?? "";
    for (const line of lines) feed(line);
  }
  if (buf.trim()) feed(buf);
}

/** Delete a model from local storage. The caller should unload it first. */
export async function deleteModel(name: string): Promise<void> {
  if (isTauriRuntime()) {
    const { invoke } = await tauri();
    await invoke("ollama_delete_model", { model: name });
    return;
  }
  const res = await fetch(`${ollamaBase()}/api/delete`, {
    method: "DELETE",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ model: name })
  });
  if (!res.ok) {
    const txt = await res.text().catch(() => "");
    throw new Error(`model delete failed: ${res.status} ${txt}`);
  }
}
