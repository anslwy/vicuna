import type { ChatMsg } from "./ollama";

/** How Continue extends an assistant reply.
 *  raw    — default: prefill the family's own prompt through /api/generate
 *           with raw:true (gemma4/gemma2/3). Falls back to native when the
 *           model family has no known prompt format, or the thread has
 *           images.
 *  auto   — raw only for gemma4, native /api/chat for everyone else.
 *  native — always /api/chat with the partial reply as the last message. */
export type ContinueMode = "auto" | "raw" | "native";

export const CONTINUE_MODES: { id: ContinueMode; label: string }[] = [
  { id: "auto", label: "Auto" },
  { id: "raw", label: "Raw prompt" },
  { id: "native", label: "Native chat" }
];

/** Missing/legacy values land on the default route (raw); explicit choices
 *  are preserved. */
export function normalizeContinueMode(v: unknown): ContinueMode {
  return v === "auto" || v === "native" ? v : "raw";
}

/** Chat prompt format needed to prefill a reply through /api/generate.
 *  Only families whose turn markers are known are listed; anything else
 *  keeps using /api/chat. */
export type PromptStyle = "gemma4" | "gemma3";

export function promptStyleForFamily(family: string | null | undefined): PromptStyle | null {
  const f = (family ?? "").trim().toLowerCase();
  if (!f) return null;
  if (f === "gemma4" || f.startsWith("gemma4.")) return "gemma4";
  if (f === "gemma3" || f.startsWith("gemma3.") || f === "gemma2" || f.startsWith("gemma2.")) return "gemma3";
  return null;
}

/** Validate a persisted prompt style (pending-stream marker). */
export function normalizePromptStyle(v: unknown): PromptStyle | null {
  return v === "gemma4" || v === "gemma3" ? v : null;
}

/** Resolve the configured mode against the model family. Auto only takes the
 *  raw path for gemma4 (verified: gemma4's chat template appends
 *  `<turn|>` after every assistant message, so /api/chat always starts a new
 *  turn instead of continuing). Forced raw also covers the gemma2/3 format. */
export function useRawContinue(mode: ContinueMode, style: PromptStyle | null): boolean {
  if (!style || mode === "native") return false;
  if (mode === "raw") return true;
  return style === "gemma4";
}

/** Turn markers that end a continuation. These are structural, so they are
 *  always sent alongside the user's custom stop words. */
function styleStops(style: PromptStyle): string[] {
  return style === "gemma4"
    ? ["<turn|>", "<|turn>", "<|channel>", "<channel|>"]
    : ["<end_of_turn>", "<start_of_turn>"];
}

/** Build the raw prompt for a transcript whose last message is a partial
 *  assistant reply. Returns null when that isn't the case (caller falls back
 *  to /api/chat). */
export function buildRawContinuePrompt(
  messages: ChatMsg[],
  style: PromptStyle
): { prompt: string; stop: string[] } | null {
  const last = messages[messages.length - 1];
  if (!last || last.role !== "assistant") return null;
  const head = messages.slice(0, -1);

  if (style === "gemma4") {
    let prompt = "<bos>";
    for (const m of head) {
      const role = m.role === "assistant" ? "model" : m.role === "system" ? "system" : "user";
      prompt += `<|turn>${role}\n${m.content}<turn|>\n`;
    }
    prompt += `<|turn>model\n${last.content}`;
    return { prompt, stop: styleStops(style) };
  }

  // gemma2/3: system text rides in the first user turn (the template has no
  // system role).
  let prompt = "<bos>";
  let system = "";
  for (const m of head) {
    if (m.role === "system") {
      system = system ? `${system}\n\n${m.content}` : m.content;
      continue;
    }
    const role = m.role === "assistant" ? "model" : "user";
    const body = role === "user" && system ? `${system}\n\n${m.content}` : m.content;
    if (role === "user") system = "";
    prompt += `<start_of_turn>${role}\n${body}<end_of_turn>\n`;
  }
  prompt += `<start_of_turn>model\n${last.content}`;
  return { prompt, stop: styleStops(style) };
}

/** Find the repeated unit at the end of `tail`. Units made only of ASCII
 *  punctuation/whitespace are ignored so markdown rules and code fences
 *  don't trip the guard, while emoji/CJK/word units still count. */
function runawayRun(tail: string, minRepeats: number): { unit: number; count: number } | null {
  for (let unit = 2; unit <= 32; unit++) {
    if (tail.length < unit * minRepeats) continue;
    const seg = tail.slice(-unit);
    const meaningful = /[A-Za-z0-9]/.test(seg) || /[^\x00-\x7F\s]/.test(seg);
    if (!meaningful) continue;
    let count = 0;
    for (let end = tail.length; end - unit >= 0; end -= unit) {
      if (tail.slice(end - unit, end) === seg) count++;
      else break;
    }
    // 24+ repeated chars: "hahaha" stays, a runaway tail gets cut
    if (count >= minRepeats && unit * count >= 24) return { unit, count };
  }
  return null;
}

/** The raw path can overrun a finished turn on some builds (MLX gemma4
 *  repeats a short phrase instead of emitting `<turn|>`). Detect a tail that
 *  is a short unit repeated many times so the caller can stop the stream
 *  instead of saving megabytes of "owns owns owns". */
export function isRunawayRepetition(text: string, minRepeats = 6): boolean {
  return runawayRun(text.slice(-4000), minRepeats) !== null;
}

/** Trim the repeated suffix that triggered `isRunawayRepetition`, keeping
 *  everything before it. */
export function stripRunawayTail(text: string, minRepeats = 6): string {
  let out = text;
  for (let pass = 0; pass < 64 && out.length > 0; pass++) {
    const run = runawayRun(out.slice(-4000), minRepeats);
    if (!run) return out;
    out = out.slice(0, out.length - run.unit * run.count);
  }
  return out;
}
