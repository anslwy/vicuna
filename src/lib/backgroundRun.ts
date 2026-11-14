// Backgrounded vicuna-mcp `run` jobs (pure, node-tested in
// node-tests/backgroundRun.test.ts).
//
// A Work agent runs long commands through vicuna-mcp's `run` tool; the user
// (or the agent itself) can move one to the background. The tool call then
// returns at once, the job keeps running in Vicuna, and when it finishes its
// output goes back to the session as a user message — queued into the live
// turn, or starting a new one when the session is idle.

export interface BgRun {
  chatId: string;
  jobId: string;
  command: string;
  /** ms epoch the job was started (for the elapsed label) */
  startedAt: number;
}

export interface BgRunStatus {
  running: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
  truncated: boolean;
}

/** "45s", "2m 13s", "1h 4m" */
export function formatRunElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}

/** Fence that can't be closed by the output itself. */
function fence(body: string): string {
  let f = "```";
  while (body.includes(f)) f += "`";
  return `${f}\n${body}\n${f}`;
}

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

/** The user message that wakes the agent when a background job finishes. */
export function bgDoneMessage(run: BgRun, st: BgRunStatus, now: number): string {
  const exit = st.exit_code == null ? "killed" : `exit ${st.exit_code}`;
  const lines = [
    `Background command finished (${exit}, after ${formatRunElapsed(now - run.startedAt)}, job ${run.jobId}):`,
    "",
    fence(run.command)
  ];
  const out = st.stdout.trimEnd();
  const err = st.stderr.trimEnd();
  if (out) lines.push("", "Output:", fence(out));
  if (err) lines.push("", "Stderr:", fence(err));
  if (!out && !err) lines.push("", "(no output)");
  if (st.truncated) lines.push("", "(output was truncated at 32KB per stream)");
  lines.push("", "Continue from here.");
  return lines.join("\n");
}
