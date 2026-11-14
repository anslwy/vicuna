// Ask card helpers (pure, node-tested in node-tests/askCard.test.ts).
//
// The card serves two kinds of asks: permission requests (`perm-*`, Allow /
// Deny; the backend notes the verdict) and agent questions from vicuna-mcp's
// `ask_user` tool (`ask-*`; the UI records the Q/A in the transcript). A
// multi-select question is answered with a JSON array of the picked labels.

/** An agent question (vicuna-mcp ask_user), not a permission request. */
export function isAgentQuestion(askId: string): boolean {
  return askId.startsWith("ask-");
}

/** Toggle one choice in a multi-select pick, keeping the choices' order. */
export function toggleChoice(options: string[], picked: string[], opt: string): string[] {
  const next = picked.includes(opt) ? picked.filter((p) => p !== opt) : [...picked, opt];
  return options.filter((o) => next.includes(o));
}

/** Wire answer (sent to the backend) + display text (transcript record). */
export function multiAnswer(picked: string[]): { answer: string; display: string } {
  return { answer: JSON.stringify(picked), display: picked.length ? picked.join(", ") : "(none)" };
}

/**
 * The agent-question answer from the card's answer box.
 * - text only: the text as-is (single-select: an edited/typed answer)
 * - multi-select with picks and no text/images: the JSON array (as before)
 * - multi-select with picks plus a note: "Selected: a; b" + the note
 * - images: the envelope `{"vicunaAskAnswer": {text, images}}` that
 *   vicuna-mcp turns into MCP image blocks (images are data URLs)
 * Returns null when there is nothing to send.
 */
export function buildAskAnswer(input: {
  text: string;
  images: string[];
  multi: boolean;
  picked: string[];
}): { answer: string; display: string } | null {
  const note = input.text.trim();
  const images = input.images;
  if (input.multi && input.picked.length && !note && !images.length) return multiAnswer(input.picked);
  const parts: string[] = [];
  if (input.multi && input.picked.length) parts.push(`Selected: ${input.picked.join("; ")}`);
  if (note) parts.push(note);
  const text = parts.join("\n");
  if (!text && !images.length) return null;
  const n = images.length;
  const imgNote = n ? `(${n} image${n === 1 ? "" : "s"})` : "";
  const display = [text, imgNote].filter(Boolean).join(" ");
  const answer = n ? JSON.stringify({ vicunaAskAnswer: { text, images } }) : text;
  return { answer, display };
}
