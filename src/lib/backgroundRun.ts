// Session wake-up messages that render as tool-style cards (pure,
// node-tested in node-tests/backgroundRun.test.ts).
//
// "Background command finished (…)" messages were produced by the removed
// vicuna-mcp `run` tool. Nothing creates them any more, but chats saved before
// the removal still hold them, so the parser stays to render them as a
// collapsed tool result instead of a raw user bubble.

const BG_DONE_HEAD = /^Background command finished \(((?:exit -?\d+)|killed), after ([^,]+), job ([^)]+)\):\n/;

export interface BgDoneParts {
  /** "exit 0", "exit 1", "killed" */
  status: string;
  ok: boolean;
  elapsed: string;
  jobId: string;
  command: string;
  output: string;
  stderr: string;
  truncated: boolean;
}

/** Pull the fenced block that follows `label` (or the first one when null). */
function fencedAfter(text: string, label: string | null): string | null {
  const start = label == null ? 0 : text.indexOf(`\n${label}\n`);
  if (start < 0) return null;
  const m = /(`{3,})\n([\s\S]*?)\n\1(?:\n|$)/.exec(text.slice(start));
  return m ? m[2] : null;
}

/** Is this session message a finished-background-job wake-up (rendered as a
 *  tool result, not a user bubble)? */
export function isBgDoneMessage(content: string): boolean {
  return BG_DONE_HEAD.test(content);
}

/** Parse a `bgDoneMessage` back for display; null when it isn't one. */
export function parseBgDoneMessage(content: string): BgDoneParts | null {
  const head = BG_DONE_HEAD.exec(content);
  if (!head) return null;
  const rest = content.slice(head[0].length);
  return {
    status: head[1],
    ok: head[1] === "exit 0",
    elapsed: head[2],
    jobId: head[3],
    command: fencedAfter(rest, null) ?? "",
    output: fencedAfter(rest, "Output:") ?? "",
    stderr: fencedAfter(rest, "Stderr:") ?? "",
    truncated: rest.includes("(output was truncated")
  };
}

const MCP_WAKE_RE = /^MCP server\(s\) now active: (.+?)\. You can call their tools now\.$/;

export interface McpWakeServer {
  name: string;
  tools: number | null;
}

/** The user message that wakes the agent when a newly added/enabled MCP server is ready. */
export function mcpWakeMessage(servers: McpWakeServer[]): string {
  const summary = servers
    .map((s) => (typeof s.tools === "number" ? `\`${s.name}\` (${s.tools} tool${s.tools === 1 ? "" : "s"})` : `\`${s.name}\``))
    .join(", ");
  return `MCP server(s) now active: ${summary}. You can call their tools now.`;
}

/** Parse an `mcpWakeMessage` back for display as a tool-style card; null when it isn't one. */
export function parseMcpWakeMessage(content: string): { summary: string } | null {
  const m = MCP_WAKE_RE.exec(content.trim());
  if (!m) return null;
  return { summary: m[1].replace(/`/g, "") };
}
