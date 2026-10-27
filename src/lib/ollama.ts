// Vicuna Ollama client.
//
// Architecture: the Rust backend (src-tauri/src/lib.rs) owns all inference
// traffic to Ollama. These functions are display-layer adapters: inside the
// Tauri webview they invoke backend commands and render streamed events;
// outside Tauri (plain `npm run dev` in a browser) they fall back to direct
// fetch so web-only dev keeps working without the shell.

export const OLLAMA_BASE = "http://localhost:11434";

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
  const res = await fetch(`${OLLAMA_BASE}/api/tags`);
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
  const res = await fetch(`${OLLAMA_BASE}/api/ps`);
  if (!res.ok) throw new Error(`ollama ps failed: ${res.status}`);
  const data = await res.json();
  return (data.models ?? []) as RunningModel[];
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
  const res = await fetch(`${OLLAMA_BASE}/api/generate`, {
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
}

type ChatEvent =
  | { type: "token"; text: string }
  | { type: "think"; text: string }
  | { type: "done"; done_reason?: string | null };

async function chatStreamViaBackend(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal: AbortSignal | undefined,
  onThink: ((t: string) => void) | undefined,
  opts: { think?: boolean; temperature?: number; stop?: string[] }
): Promise<{ text: string; doneReason: string }> {
  const { invoke, Channel } = await tauri();
  if (signal?.aborted) throw new DOMException("aborted", "AbortError");
  let doneReason = "";
  const onEvent = new Channel<ChatEvent>();
  onEvent.onmessage = (ev) => {
    if (signal?.aborted) return;
    if (ev.type === "token") onToken(ev.text);
    else if (ev.type === "think") onThink?.(ev.text);
    else if (ev.type === "done" && ev.done_reason) doneReason = ev.done_reason;
  };
  const invokePromise = invoke<string>("ollama_chat_stream", {
    model,
    messages: messages.map((m) => ({ role: m.role, content: m.content })),
    think: opts.think ?? true,
    temperature: opts.temperature ?? null,
    stop: opts.stop ?? null,
    onEvent
  });
  if (!signal) {
    const text = await invokePromise;
    return { text, doneReason };
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
  return { text, doneReason };
}

async function chatStreamViaFetch(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal: AbortSignal | undefined,
  onThink: ((t: string) => void) | undefined,
  opts: { think?: boolean; temperature?: number; stop?: string[] }
): Promise<{ text: string; doneReason: string }> {
  const options: Record<string, number | string[]> = { num_ctx: 32768 };
  // temperature only set when explicitly requested (continue uses low temp
  // to stay on the most likely next token); otherwise server default
  if (opts.temperature !== undefined) options.temperature = opts.temperature;
  if (opts.stop !== undefined) options.stop = opts.stop;
  const res = await fetch(`${OLLAMA_BASE}/api/chat`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    // num_ctx: long chats (stories, code) exceed Ollama's small default
    // window, which makes continued generations forget the middle and
    // diverge into generic wrap-ups. 32k fits comfortably on this machine.
    body: JSON.stringify({ model, messages, stream: true, think: opts.think ?? true, options }),
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
    return { text: content, doneReason: "" };
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
  return { text: full, doneReason };
}

export async function chatStream(
  model: string,
  messages: ChatMsg[],
  onToken: (t: string) => void,
  signal?: AbortSignal,
  onThink?: (t: string) => void,
  attempt = 0,
  opts: { think?: boolean; temperature?: number; stop?: string[] } = {}
): Promise<string> {
  try {
    const { text, doneReason } = isTauriRuntime()
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
    return text;
  } catch (e: unknown) {
    throw e;
  }
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
    res = await fetch(`${OLLAMA_BASE}/api/generate`, {
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
