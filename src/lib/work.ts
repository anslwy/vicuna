// Vicuna Work mode: workspace agent via Ollama native tool calling.
// The model gets WORK_TOOLS as `tools` on /api/chat; results come back
// as `role: tool` messages. The ```tool fence format is kept only as a
// fallback for models that reject `tools` — parseToolCalls covers it.

import type { ChatMsg } from "./ollama";
import { isTauriRuntime } from "./ollama";
import type { Chat } from "./store";

export interface WorkToolCall {
  name: string;
  args: Record<string, any>;
}

export function buildWorkSystemPrompt(workspace: string): string {
  return `You are Vicuna Work mode, a minimal coding agent operating inside the workspace: ${workspace}

You can inspect and change ONLY files under that workspace, run shell commands there (cwd = workspace), and search the user's past Vicuna chat sessions.

WORKSPACE BOUNDARY (strict):
- The workspace is the limit of what you may touch on your own. Do not list, read, grep, write, or delete anything outside it — no parent folders, home directory, other projects, or system files — and do not run commands that reach outside it.
- Already granted exceptions: read_upload (files the user attached to this session) and sessions/read_session (past chats).
- If a task seems to need anything else outside the workspace (a sibling repo, another project's files, a path the user mentioned), STOP and ask the user for permission first: state the exact path or command you need and why, then wait for a yes.
- Running globally installed tools (git, node, python, rg, …) is fine; using them to inspect or change data outside the workspace is not.
- Use \`ask\` when a decision is genuinely needed (which approach/file/scope, or before anything destructive) instead of guessing. It shows buttons plus an "Other" free-text field and waits for the answer.

Use the provided function tools to act — do not invent results, do not emit manual tool JSON.
- After a tool result arrives, continue reasoning and either call the next tool or answer.
- When the task is done, answer normally with NO further tool calls.
- Never repeat a tool call with the same name and arguments — the result is already in the transcript. Repeating finished work instead of summarizing is a failure.
- Keep file reads targeted; prefer list/grep before reading big files.

RESPONSE STYLE:
- Briefly narrate what you are about to do before each tool call (one line), so the transcript reads naturally.
- End with a concise summary of what you did/found, with file paths and command output that matter.`;
}

/** Ollama native `tools` payload for Work mode (mirrors dispatchWorkTool). */
export const WORK_TOOLS = [
  {
    type: "function",
    function: {
      name: "list",
      description: "List a workspace-relative directory (default '.').",
      parameters: {
        type: "object",
        properties: { path: { type: "string", description: "relative dir, default '.'" } }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "read",
      description: "Read a workspace-relative text file (64KB cap).",
      parameters: {
        type: "object",
        required: ["path"],
        properties: { path: { type: "string", description: "relative file path" } }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "grep",
      description: "Substring search (case-insensitive) across workspace text files.",
      parameters: {
        type: "object",
        required: ["pattern"],
        properties: {
          pattern: { type: "string", description: "substring to search for" },
          include: { type: "string", description: "filename substring filter" }
        }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "write",
      description: "Create/overwrite a workspace-relative file with full new text. Read first before overwriting.",
      parameters: {
        type: "object",
        required: ["path", "content"],
        properties: {
          path: { type: "string", description: "relative file path" },
          content: { type: "string", description: "full new file text" }
        }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "exec",
      description: "Run a shell command with cwd=workspace. Prefer non-interactive, time-bounded commands. Never run interactive commands. Do not access paths outside the workspace without asking the user first.",
      parameters: {
        type: "object",
        required: ["command"],
        properties: {
          command: { type: "string", description: "shell command" },
          timeout_ms: { type: "integer", description: "timeout 1000-120000ms, default 30000" }
        }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "ask",
      description: "Ask the user a multiple-choice question and wait for their answer. Use when a decision is genuinely needed before continuing (approach, file, scope, or anything destructive). Provide 2-8 short options; the UI always adds an 'Other' free-text choice. Returns the chosen option or the typed answer.",
      parameters: {
        type: "object",
        required: ["question", "options"],
        properties: {
          question: { type: "string", description: "question text" },
          options: {
            type: "array",
            items: { type: "string" },
            description: "2-8 short choices"
          }
        }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "sessions",
      description: "Search past Vicuna chat sessions (all modes). Returns matching ids, titles, excerpts.",
      parameters: {
        type: "object",
        required: ["query"],
        properties: { query: { type: "string", description: "search text" } }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "read_session",
      description: "Read messages from a past session.",
      parameters: {
        type: "object",
        required: ["id"],
        properties: {
          id: { type: "string", description: "session uuid" },
          limit: { type: "integer", description: "max messages, 1-100, default 30" }
        }
      }
    }
  },
  {
    type: "function",
    function: {
      name: "read_upload",
      description: "Read a text file the user attached to THIS session (by bare file name, e.g. \"report.pdf\"). Use for files listed in the [Attached files ...] note on the user message. 64KB cap.",
      parameters: {
        type: "object",
        required: ["name"],
        properties: {
          name: { type: "string", description: "file name exactly as listed" }
        }
      }
    }
  }
];

/** Loose detection of ```tool blocks (valid or not) for the
 *  "model wrote a call-shaped block that never ran" warning. */
export function hasToolFence(text: string): boolean {
  return /```+\s*tool\b/.test(text);
}

/** Normalize an Ollama native tool_call into a WorkToolCall. */
export function nativeToWorkCall(name: string, args: unknown): WorkToolCall | null {
  if (!name) return null;
  const a = args && typeof args === "object" && !Array.isArray(args) ? (args as Record<string, any>) : {};
  return { name, args: a };
}

export function workHistory(
  chat: Chat,
  extra: ChatMsg[],
  workspace: string
): ChatMsg[] {
  return [
    { role: "system", content: buildWorkSystemPrompt(workspace) },
    ...chat.messages.slice(0, -1).map((m) => ({
      role: m.role,
      content: m.content
    }) as ChatMsg),
    ...extra
  ];
}

/** Extract ```tool fences (JSON inside) from assistant text. */
export function parseToolCalls(text: string): WorkToolCall[] {
  const out: WorkToolCall[] = [];
  const re = /```tool\s*\n([\s\S]*?)\n?```/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    try {
      const obj = JSON.parse(m[1].trim());
      if (obj && typeof obj.name === "string" && obj.args && typeof obj.args === "object") {
        out.push({ name: obj.name, args: obj.args });
      }
    } catch {
      // malformed fence: leave it as visible text, don't act on it
    }
  }
  return out;
}

export function hasUnresolvedToolCall(fullText: string): boolean {
  // the caller tracks executed fences by count; any fence beyond that is new
  return /```tool\s*\n[\s\S]*?\n?```/.test(fullText);
}

async function invokeCmd<T>(cmd: string, args: Record<string, any>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<T>(cmd, args);
}

export interface WorkTreeEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size?: number;
  depth: number;
}

/** Recursive workspace listing for the explorer panel (desktop only). */
export async function listWorkTree(root: string, maxDepth = 4): Promise<WorkTreeEntry[]> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  return await invokeCmd<WorkTreeEntry[]>("work_tree", { root, maxDepth });
}

/** Import an OS-dropped file into the workspace (base64 bytes, desktop only). */
export async function importWorkFile(root: string, path: string, base64: string): Promise<string> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  return await invokeCmd<string>("work_import_file", { root, path, base64 });
}

/** Rename a workspace file or folder within its own directory (desktop only).
 *  Returns the new workspace-relative path. */
export async function renameWorkFile(root: string, path: string, newName: string): Promise<string> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  return await invokeCmd<string>("work_rename", { root, path, newName });
}

/** Delete a workspace file or folder (recursive, desktop only). */
export async function deleteWorkFile(root: string, path: string): Promise<void> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  await invokeCmd<void>("work_delete", { root, path });
}

/** File/Blob → base64 (chunked, safe for large files). */
export async function fileToBase64(file: Blob): Promise<string> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) {
    bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(bin);
}

export interface DroppedEntry {
  path: string;
  file: File;
}

function sanitizeRelPath(p: string): string {
  return p
    .split("/")
    .map((s) => s.trim())
    .filter((s) => s.length > 0 && s !== "." && s !== "..")
    .join("/");
}

/** Flatten a file/folder OS drop into workspace-relative {path, file} pairs.
 *  Uses the FileSystem entry API when present (folders recurse, keeping
 *  relative paths); falls back to the flat FileList otherwise. */
export async function collectDropEntries(dt: DataTransfer): Promise<DroppedEntry[]> {
  const out: DroppedEntry[] = [];
  const items = dt.items ? Array.from(dt.items) : [];
  const entries = items
    .map((it) => {
      try {
        return it.kind === "file" ? it.webkitGetAsEntry() : null;
      } catch {
        return null;
      }
    })
    .filter((e): e is FileSystemEntry => e != null);
  if (!entries.length) {
    for (const f of Array.from(dt.files ?? [])) {
      const rel = sanitizeRelPath(f.name);
      if (rel) out.push({ path: rel, file: f });
    }
    return out;
  }
  const readAll = (reader: FileSystemDirectoryReader): Promise<FileSystemEntry[]> =>
    new Promise((resolve, reject) => {
      const acc: FileSystemEntry[] = [];
      const pump = () =>
        reader.readEntries(
          (batch) => {
            if (!batch.length) {
              resolve(acc);
              return;
            }
            acc.push(...batch);
            pump();
          },
          () => reject(new Error("cannot read folder"))
        );
      pump();
    });
  const fileOf = (e: FileSystemFileEntry): Promise<File> =>
    new Promise((resolve, reject) => e.file(resolve, reject));
  const walk = async (entry: FileSystemEntry, base: string): Promise<void> => {
    if (entry.isFile) {
      const f = await fileOf(entry as FileSystemFileEntry);
      const rel = sanitizeRelPath(`${base}${f.name}`);
      if (rel) out.push({ path: rel, file: f });
    } else if (entry.isDirectory) {
      const kids = await readAll((entry as FileSystemDirectoryEntry).createReader());
      for (const kid of kids) await walk(kid, `${base}${entry.name}/`);
    }
  };
  for (const e of entries) await walk(e, "");
  return out;
}

/** Pure text splice for completing an @-mention: replaces the live query
 *  (up to the next whitespace) with the picked path plus one trailing space.
 *  `start`/`query` come from the mention query; caret is at end of query. */
export function completeMentionText(
  input: string,
  start: number,
  query: string,
  path: string
): { text: string; caret: number } {
  const before = input.slice(0, start);
  const tail = input.slice(start + 1 + query.length).replace(/^[^\s]*/, "");
  const needsSpace = tail !== "" && !tail.startsWith(" ");
  const sep = tail === "" || needsSpace ? " " : "";
  return {
    text: `${before}@${path}${sep}${tail}`,
    caret: before.length + 1 + path.length + sep.length
  };
}

export interface TermStreamEvent {
  type: "output" | "exit";
  text?: string;
  code?: number | null;
}

/** Spawn an interactive shell rooted at the workspace (desktop only).
 *  Output/exit stream through the callbacks; returns the terminal id. */
export async function spawnTerm(
  root: string,
  cols: number,
  rows: number,
  onOutput: (text: string) => void,
  onExit: (code: number | null) => void
): Promise<string> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const ch = new Channel<TermStreamEvent>();
  ch.onmessage = (ev) => {
    if (ev.type === "output") onOutput(ev.text ?? "");
    else if (ev.type === "exit") onExit(ev.code ?? null);
  };
  return await invoke<string>("term_spawn", { root, cols, rows, onEvent: ch });
}

/** Send keystrokes to a terminal (desktop only). */
export async function writeTerm(id: string, data: string): Promise<void> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  await invokeCmd<void>("term_write", { id, data });
}

/** Resize a terminal (desktop only). */
export async function resizeTerm(id: string, cols: number, rows: number): Promise<void> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  await invokeCmd<void>("term_resize", { id, cols, rows });
}

/** Kill a terminal's shell (desktop only, best-effort). */
export async function killTerm(id: string): Promise<void> {
  if (!isTauriRuntime()) return;
  try {
    await invokeCmd<void>("term_kill", { id });
  } catch {
    /* already gone */
  }
}

export interface SessionSearchHit {
  id: string;
  title: string;
  mode: string;
  excerpt: string;
}

/** Frontend-local tool: search past sessions (works in browser too). */
export function searchSessions(chats: Chat[], query: string, limit = 8): SessionSearchHit[] {
  const q = query.trim().toLowerCase();
  if (!q) return [];
  const hits: SessionSearchHit[] = [];
  for (const c of chats) {
    if (hits.length >= limit) break;
    if (c.id.toLowerCase().includes(q) || c.title.toLowerCase().includes(q)) {
      const first = c.messages.find((m) => m.role === "user")?.content ?? "";
      hits.push({ id: c.id, title: c.title, mode: c.mode ?? "chat", excerpt: first.slice(0, 200) });
      continue;
    }
    const msg = c.messages.find((m) => m.content.toLowerCase().includes(q));
    if (msg) {
      const idx = msg.content.toLowerCase().indexOf(q);
      const start = Math.max(0, idx - 60);
      const excerpt = (start > 0 ? "…" : "") + msg.content.slice(start, idx + q.length + 120);
      hits.push({ id: c.id, title: c.title, mode: c.mode ?? "chat", excerpt });
    }
  }
  return hits;
}

export function readSession(chats: Chat[], id: string, limit = 30): string {
  const c = chats.find((x) => x.id === id);
  if (!c) return "No session found for that id.";
  const msgs = c.messages.slice(-limit);
  return `Session "${c.title}" (${c.mode ?? "chat"}), ${c.messages.length} messages:\n\n` +
    msgs.map((m) => `${m.role === "user" ? "User" : "Assistant"}: ${m.content.slice(0, 2000)}`).join("\n\n");
}

/** Stage a composer attachment for a session (desktop only). Returns the
 *  absolute staged path for the prompt note. Files keep their bytes as-is
 *  (just `cp` semantics) under ~/vicuna/uploads/<session-id>/. */
export async function stageUploadFile(sessionId: string, fileName: string, base64: string): Promise<string> {
  if (!isTauriRuntime()) throw new Error("workspace tools need the Vicuna desktop app.");
  return await invokeCmd<string>("upload_stage_file", { sessionId, fileName, base64 });
}

/** Execute one tool call. Returns markdown for the transcript. */
export async function dispatchWorkTool(
  call: WorkToolCall,
  ctx: {
    workspace: string;
    chats: Chat[];
    sessionId?: string;
    /** render a multiple-choice card and wait for the user's answer */
    askUser?: (question: string, options: string[]) => Promise<string>;
  }
): Promise<string> {
  const a = call.args ?? {};
  const str = (v: unknown) => (typeof v === "string" ? v : "");
  try {
    switch (call.name) {
      case "list": {
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        const res = await invokeCmd<Array<{ name: string; path: string; is_dir: boolean; size?: number }>>(
          "work_list_dir", { root: ctx.workspace, path: str(a.path) || null }
        );
        if (res.length === 0) return "Empty directory.";
        return res.map((e) => `${e.path}${e.is_dir ? "/" : e.size != null ? ` (${e.size}B)` : ""}`).join("\n");
      }
      case "read": {
        if (!str(a.path)) return "Error: read needs {\"path\": \"...\"}.";
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        const res = await invokeCmd<{ content: string; truncated: boolean; size: number }>(
          "work_read_file", { root: ctx.workspace, path: str(a.path), maxBytes: null }
        );
        return res.content + (res.truncated ? `\n…[truncated, ${res.size}B total]` : "");
      }
      case "grep": {
        if (!str(a.pattern)) return "Error: grep needs {\"pattern\": \"...\"}.";
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        const res = await invokeCmd<Array<{ path: string; line: number; text: string }>>(
          "work_grep", { root: ctx.workspace, pattern: str(a.pattern), include: str(a.include) || null }
        );
        if (res.length === 0) return "No matches.";
        return res.map((h) => `${h.path}:${h.line}: ${h.text}`).join("\n");
      }
      case "write": {
        if (!str(a.path)) return "Error: write needs {\"path\": \"...\", \"content\": \"...\"}.";
        if (typeof a.content !== "string") return "Error: write needs a string \"content\".";
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        await invokeCmd<string>("work_write_file", { root: ctx.workspace, path: str(a.path), content: a.content });
        return `Wrote ${str(a.path)} (${a.content.length} chars).`;
      }
      case "exec": {
        if (!str(a.command)) return "Error: exec needs {\"command\": \"...\"}.";
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        const res = await invokeCmd<{ stdout: string; stderr: string; exit_code: number | null; timed_out: boolean; truncated: boolean }>(
          "work_exec", { root: ctx.workspace, command: str(a.command), timeoutMs: typeof a.timeout_ms === "number" ? a.timeout_ms : null }
        );
        const parts: string[] = [];
        if (res.stdout.trim()) parts.push(res.stdout);
        if (res.stderr.trim()) parts.push(`[stderr]\n${res.stderr}`);
        parts.push(`[exit ${res.timed_out ? "timed out" : res.exit_code}]`);
        return parts.join("\n");
      }
      case "ask": {
        const question = str(a.question).trim();
        const options = Array.isArray(a.options)
          ? a.options
              .filter((o: unknown): o is string => typeof o === "string" && o.trim().length > 0)
              .map((o: string) => o.trim())
          : [];
        if (!question) return "Error: ask needs a non-empty question.";
        if (options.length < 2) return "Error: ask needs at least 2 options.";
        if (!ctx.askUser) return "Error: asking the user is not available here.";
        return await ctx.askUser(question, options.slice(0, 8));
      }
      case "sessions": {
        if (!str(a.query)) return "Error: sessions needs {\"query\": \"...\"}.";
        const hits = searchSessions(ctx.chats, str(a.query));
        if (hits.length === 0) return "No matching sessions.";
        return hits.map((h) => `• "${h.title}" [${h.mode}] id=${h.id}\n  ${h.excerpt}`).join("\n");
      }
      case "read_session": {
        if (!str(a.id)) return "Error: read_session needs {\"id\": \"...\"}.";
        const limit = typeof a.limit === "number" ? Math.min(100, Math.max(1, Math.round(a.limit))) : 30;
        return readSession(ctx.chats, str(a.id), limit);
      }
      case "read_upload": {
        if (!str(a.name)) return "Error: read_upload needs {\"name\": \"...\"}.";
        if (!isTauriRuntime()) return "Error: workspace tools need the Vicuna desktop app.";
        if (!ctx.sessionId) return "Error: no active session for uploads.";
        const res = await invokeCmd<{ content: string; truncated: boolean; size: number }>(
          "upload_read_file", { sessionId: ctx.sessionId, name: str(a.name) }
        );
        return res.content + (res.truncated ? `\n…[truncated, ${res.size}B total]` : "");
      }
      default:
        return `Error: unknown tool "${call.name}". Available: list, read, grep, write, exec, sessions, read_session, read_upload.`;
    }
  } catch (e: any) {
    return `Error: ${e?.message ?? String(e)}`;
  }
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

/** One-line label for a tool call, shared by the result header and the
 *  running indicator. */
export function toolLabel(name: string, args: Record<string, any>): string {
  const argSummary = name === "exec" ? String(args.command ?? "") :
    name === "read" || name === "write" || name === "list" ? String(args.path ?? ".") :
    name === "grep" ? String(args.pattern ?? "") :
    name === "sessions" ? String(args.query ?? "") :
    name === "read_session" ? String(args.id ?? "") :
    name === "read_upload" ? String(args.name ?? "") :
    name === "ask" ? String(args.question ?? "") : JSON.stringify(args);
  return `${name} — ${argSummary.slice(0, 120)}`;
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
