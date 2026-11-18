// Tool-result and ask-record markup written into Work transcripts.
// Pure (no imports) so node tests load it, and mirrored byte for byte by the
// headless server's recorder (bridge/src/persist.rs, fixture
// node-tests/fixtures/server-format.json): rows written by the window and by
// the server must render identically. Change both together.

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** A harness display note ("Auto-approved: …", engine notices) appended to
 *  the live reply. Between a tool's head and tail the reply sits inside the
 *  card's raw `<pre><code>`, where markdown never applies, so the note must be
 *  escaped there: an approved command containing `<!--` once opened an HTML
 *  comment that hid every later card and the final answer. */
export function formatDisplayNote(text: string, inToolCard: boolean): string {
  return inToolCard ? escapeHtml(text) : text;
}

/** One-line label for a tool call, shared by the result header and the
 *  running indicator. */
export function toolLabel(name: string, args: Record<string, any>): string {
  const argSummary = name === "exec" ? String(args.command ?? "") :
    name === "exec_input" ? String(args.text ?? "") :
    name === "exec_status" ? String(args.id ?? "(shell)") :
    name === "exec_kill" ? String(args.id ?? "(shell)") :
    name === "read" || name === "write" || name === "list" || name === "view_image" ? String(args.path ?? ".") :
    name === "grep" ? String(args.pattern ?? "") :
    name === "sessions" ? String(args.query ?? "") :
    name === "read_session" ? String(args.id ?? "") :
    name === "read_upload" ? String(args.name ?? "") :
    name === "fetch_url" ? String(args.url ?? "") :
    // MCP management shows the name only — args may carry API keys in
    // headers/env, and the label lands in the visible transcript
    name === "mcp_add" || name === "mcp_remove" || name === "mcp_enable" || name === "mcp_disable" ? String(args.name ?? "") :
    name === "mcp_list" ? "" :
    name === "say" ? String(args.text ?? "") :
    name === "ask" ? String(args.question ?? "") : JSON.stringify(args);
  return `${name} — ${argSummary.slice(0, 120)}`;
}

/** Visible record of an answered or dismissed `ask` card. Emitted as harness
 *  HTML (not markdown prose) so the transcript reads as tool output — the old
 *  `> Question/Answer` blockquote taught the model to imitate the format and
 *  fabricate answers instead of calling the tool. */
export function formatAskRecord(question: string, answer: string, dismissed: boolean): string {
  const one = (s: string) => s.replace(/\s+/g, " ").trim();
  const q = escapeHtml(one(question));
  const a = dismissed
    ? `<span class="ask-log-dismissed">Dismissed</span>`
    : `<strong>Answer:</strong> ${escapeHtml(one(answer))}`;
  return `\n\n<div class="ask-log"><div class="ask-log-q">${q}</div><div class="ask-log-a">${a}</div></div>\n\n`;
}

/** Head of a tool-result block, appended as soon as the call is known so the
 *  tag is visible while a slow tool runs; `formatToolCallTail` closes it. */
export function formatToolCallHead(name: string, args: Record<string, any>): string {
  const head = toolLabel(name, args);
  return `\n\n<details class="tool-result"><summary>${escapeHtml(head)}</summary><pre><code>`;
}

export function formatToolCallTail(output: string): string {
  return `${escapeHtml(output.slice(0, 6000))}</code></pre></details>`;
}

/** Tool results render as collapsed <details> (closed by default): the head
 *  line stays visible, output expands on click. Raw HTML rather than a fence
 *  — marked leaves block HTML untouched, and DOMPurify allows details/summary.
 *  Summary is plain text (no markdown inside <summary> renders). */
export function formatToolResult(name: string, args: Record<string, any>, output: string): string {
  return formatToolCallHead(name, args) + formatToolCallTail(output);
}
