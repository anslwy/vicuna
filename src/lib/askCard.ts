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
