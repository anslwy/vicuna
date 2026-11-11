// Vicuna Work mode: the user's opencode CLI, driven by the backend over ACP
// (src-tauri/src/acp.rs). This module holds the IPC wrappers (turns, engine
// detection, opencode's model list), the workspace/file-explorer helpers and
// the transcript formatting the event stream renders with.

import type { ChatMsg } from "./ollama";
import { isTauriRuntime } from "./ollama";
import { invokeOrBridge, BridgeChannel, bridgeInvoke } from "./bridge";
import type { Chat } from "./store";
import type { OpencodeModels, WorkEngineInfo } from "./workGate";
export { workSendGate, opencodeModelChoices, workReplyModel, engineId, engineLabel, workEngineChoices } from "./workGate";
export type { WorkEngineInfo, OpencodeModel, OpencodeModels, EngineSummary } from "./workGate";

async function invokeCmd<T>(cmd: string, args: Record<string, any>): Promise<T> {
  // Tauri IPC when shelled, loopback bridge when plain-browser-with-bridge,
  // desktop-needed error otherwise (see src/lib/bridge.ts).
  return await invokeOrBridge<T>(cmd, args);
}

export interface WorkTreeEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size?: number;
  depth: number;
}

/** Recursive workspace listing for the explorer panel (desktop or bridge). */
export async function listWorkTree(root: string, maxDepth = 4): Promise<WorkTreeEntry[]> {
  return await invokeCmd<WorkTreeEntry[]>("work_tree", { root, maxDepth });
}

/** Import an OS-dropped file into the workspace (base64 bytes, desktop or bridge). */
export async function importWorkFile(root: string, path: string, base64: string): Promise<string> {
  return await invokeCmd<string>("work_import_file", { root, path, base64 });
}

/** Rename a workspace file or folder within its own directory (desktop or bridge).
 *  Returns the new workspace-relative path. */
export async function renameWorkFile(root: string, path: string, newName: string): Promise<string> {
  return await invokeCmd<string>("work_rename", { root, path, newName });
}

/** Delete a workspace file or folder (recursive, desktop or bridge). */
export async function deleteWorkFile(root: string, path: string): Promise<void> {
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

/** Stage a composer attachment for a session (desktop or bridge). Returns the
 *  absolute staged path for the prompt note. Files keep their bytes as-is
 *  (just `cp` semantics) under ~/vicuna/uploads/<session-id>/. */
export async function stageUploadFile(sessionId: string, fileName: string, base64: string): Promise<string> {
  return await invokeCmd<string>("upload_stage_file", { sessionId, fileName, base64 });
}

export interface BackgroundJobStatus {
  id: string;
  command: string;
  running: boolean;
  exit_code: number | null;
  stdout: string;
  stderr: string;
  truncated: boolean;
}

export interface BackgroundJobSummary {
  id: string;
  command: string;
  running: boolean;
  exit_code: number | null;
}

/** Poll one background exec job (desktop or bridge). */
export async function backgroundStatus(id: string): Promise<BackgroundJobStatus> {
  return await invokeCmd<BackgroundJobStatus>("work_exec_status", { id });
}

/** List background exec jobs (desktop or bridge, no output payload). */
export async function backgroundList(): Promise<BackgroundJobSummary[]> {
  return await invokeCmd<BackgroundJobSummary[]>("work_exec_list", {});
}

/** Render a background job snapshot the same shape as foreground exec. */
export function formatBackgroundStatus(s: BackgroundJobStatus): string {
  const parts: string[] = [];
  if (s.stdout.trim()) parts.push(s.stdout);
  if (s.stderr.trim()) parts.push(`[stderr]\n${s.stderr}`);
  parts.push(s.running ? `[job ${s.id} still running]` : `[job ${s.id} exit ${s.exit_code}]`);
  return parts.join("\n");
}

/** First `bg-N` id mentioned in a harness string, if any. */
export function extractBackgroundId(text: string): string | null {
  const m = text.match(/\bbg-\d+\b/);
  return m ? m[0] : null;
}

/** Rough token estimate for context budgeting: ~4 chars per token plus a
 *  flat per-image vision cost. Counts tool-call arguments too — a round of
 *  big `write`/`exec` calls otherwise hides most of its tokens in JSON the
 *  estimator never sees (that blind spot once let a transcript sail 189K
 *  tokens past the window before compaction noticed). Conservative by
 *  design — the compaction trigger sits at 70% of the window, so
 *  overcounting only compacts early. */
export function estimateWorkTokens(messages: Array<{ content?: string; images?: string[]; tool_calls?: unknown }>): number {
  let n = 0;
  for (const m of messages) {
    n += Math.ceil((m.content?.length ?? 0) / 4);
    n += (m.images?.length ?? 0) * 1500;
    if (m.tool_calls != null) n += Math.ceil(JSON.stringify(m.tool_calls).length / 4);
    n += 4; // role overhead
  }
  return n;
}

/** Split `view_image` vision payloads out of a tool result.
 *  `dispatchWorkTool` embeds data URLs as `[[VICUNA_IMAGE:...]]` markers
 *  (tool results are text-only); the caller strips them from the display/tool
 *  transcript and re-feeds the pictures as Ollama `images[]`. */
export function extractViewedImages(text: string): { clean: string; images: string[] } {
  const images: string[] = [];
  const clean = text.replace(/\[\[VICUNA_IMAGE:([\s\S]*?)\]\]/g, (_m, url: string) => {
    if (url.startsWith("data:image/")) images.push(url);
    return "";
  }).trim();
  return { clean, images };
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

/** One-line activity note for a tool call, shown as normal assistant prose
 *  when the round had no narration of its own. Models that emit tool calls
 *  with no text would otherwise leave a bare chain of collapsed tags, so the
 *  harness speaks for them. Returns null for calls that own their display
 *  (`ask`, `say`) or are unknown. */
export function formatToolActivity(name: string, args: Record<string, any>): string | null {
  const clean = (v: unknown, max = 80) =>
    String(v ?? "").replace(/[\r\n`]+/g, " ").trim().slice(0, max) || null;
  switch (name) {
    case "list": {
      const p = clean(args.path, 60);
      return p && p !== "." ? `Looking through \`${p}\`…` : "Looking through the workspace…";
    }
    case "read": {
      const p = clean(args.path, 60);
      return p ? `Reading \`${p}\`…` : "Reading a file…";
    }
    case "view_image": {
      const p = clean(args.path, 60);
      return p ? `Viewing \`${p}\`…` : "Viewing an image…";
    }
    case "grep": {
      const q = clean(args.pattern, 60);
      return q ? `Searching for "${q}"…` : "Searching the workspace…";
    }
    case "write": {
      const p = clean(args.path, 60);
      return p ? `Writing \`${p}\`…` : "Writing a file…";
    }
    case "exec": {
      const c = clean(args.command, 80);
      return args.background === true
        ? c ? `Starting \`${c}\` in the background…` : "Starting a background command…"
        : c ? `Running \`${c}\`…` : "Running a command…";
    }
    case "exec_status": {
      const id = clean(args.id, 20);
      return id ? `Checking background job ${id}…` : "Listing background jobs…";
    }
    case "exec_kill": {
      const id = clean(args.id, 20);
      return id ? `Stopping background job ${id}…` : "Stopping a background job…";
    }
    case "sessions": return "Searching past chats…";
    case "read_session": return "Reading a past session…";
    case "read_upload": {
      const n = clean(args.name, 60);
      return n ? `Reading attached file \`${n}\`…` : "Reading an attached file…";
    }
    case "fetch_url": {
      const u = clean(args.url, 60);
      return u ? `Fetching \`${u}\`…` : "Fetching a page…";
    }
    default: return null;
  }
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

// ---------- Rust work-agent turn client (src-tauri/src/work_agent.rs) ----------
//
// The agent rounds now live in the backend. These are thin transport helpers:
// the frontend builds one turn request, mirrors the semantic event stream
// into the chat bubble with the builders above, and answers `ask` + steering
// through the commands below. Field names are snake_case to match the Rust
// command signatures exactly.

/** Semantic agent events, mirroring WorkAgentEvent (serde tag `type`,
 *  lowercase variant names). */
export type WorkAgentEvent =
  | { type: "token"; text: string }
  | { type: "think"; text: string }
  | { type: "status"; label: string | null }
  | { type: "roundstart"; mode: string }
  | { type: "model"; model: string }
  | { type: "roundend"; gen_ms: number; gen_tps: number | null; gen_cost?: number | null }
  | { type: "displaynote"; text: string }
  | { type: "queuedrained"; ids: string[]; messages: ChatMsg[] }
  | { type: "toolstart"; name: string; args: Record<string, any> }
  | { type: "toolend"; name: string; output: string }
  | { type: "askrequest"; ask_id: string; question: string; options: string[] }
  | { type: "bgcompleted"; job_id: string; command: string; output: string }
  | { type: "contextcompacted"; note: string }
  | { type: "done"; stopped: boolean }
  | { type: "error"; message: string };

export async function workEngineInfo(): Promise<WorkEngineInfo> {
  return await invokeCmd<WorkEngineInfo>("work_engine_info", {});
}

/** The models opencode itself offers (`opencode models`, run by the backend
 *  with Vicuna's per-process config) plus the saved pick. */
export async function listOpencodeModels(): Promise<OpencodeModels> {
  return await invokeCmd<OpencodeModels>("opencode_models", {});
}

/** Persist the Work model (backend file, so it survives restarts and works
 *  through the headless bridge too). `null` = back to opencode's default. */
export async function setOpencodeModel(model: string | null): Promise<void> {
  await invokeCmd<void>("opencode_set_model", { model });
}

/** Work models of `engine` (opencode: `opencode models`; Claude: the CLI's
 *  own model list from its initialize response; Codex: app-server
 *  `model/list`) plus that engine's pick. */
export async function listWorkModels(engine: string): Promise<OpencodeModels> {
  return await invokeCmd<OpencodeModels>("work_models", { engine });
}

/** Persist the Work model for `engine` (`null` = the engine's default). */
export async function setWorkModel(engine: string, model: string | null): Promise<void> {
  await invokeCmd<void>("work_set_model", { engine, model });
}

/** Tool approval mode: true = each permission request becomes an ask card
 *  in the chat; false = auto-approve (noted in the transcript). */
export async function setWorkApproval(ask: boolean): Promise<WorkEngineInfo> {
  return await invokeCmd<WorkEngineInfo>("work_set_approval", { ask });
}

/** Switch the Work engine; returns fresh detection for the new choice. */
export async function selectWorkEngine(engine: string): Promise<WorkEngineInfo> {
  return await invokeCmd<WorkEngineInfo>("work_engine_select", { engine });
}

export interface WorkTurnParams {
  /** "opencode" | "claude" | "codex" (absent = the backend's saved choice) */
  engine?: string;
  /** engine model id; empty = saved pick / engine default */
  model: string;
  /** Start a new opencode session instead of resuming (regenerate/edit). */
  freshSession?: boolean;
  workspace: string;
  messages: ChatMsg[];
  sessionId?: string;
  goalText?: string | null;
  onEvent: (ev: WorkAgentEvent) => void;
}

/** Start a backend agent turn. Resolves with the turn id once the loop is
 *  running; events stream through onEvent until done/error. Tauri IPC when
 *  shelled, loopback bridge when plain-browser-with-bridge. */
export async function startWorkAgentTurn(p: WorkTurnParams): Promise<string> {
  const args = {
    engine: p.engine ?? null,
    model: p.model,
    freshSession: p.freshSession ?? false,
    workspace: p.workspace,
    messages: p.messages,
    sessionId: p.sessionId ?? null,
    goalText: p.goalText ?? null
  };
  if (isTauriRuntime()) {
    const { invoke, Channel } = await import("@tauri-apps/api/core");
    const ch = new Channel<WorkAgentEvent>();
    ch.onmessage = (ev) => p.onEvent(ev);
    return await invoke<string>("work_agent_start", { ...args, onEvent: ch });
  }
  const ch = new BridgeChannel();
  ch.onmessage = (ev) => p.onEvent(ev as WorkAgentEvent);
  try {
    return await bridgeInvoke("work_agent_start", { ...args, onEvent: ch.marker() });
  } catch (e) {
    ch.stop();
    throw e;
  }
}

/** Interrupt a running turn (double-Esc, Stop, chat deleted). */
export async function stopWorkAgentTurn(turnId: string): Promise<void> {
  if (isTauriRuntime()) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("work_agent_stop", { turnId });
    return;
  }
  try {
    await bridgeInvoke("work_agent_stop", { turnId });
  } catch {
    /* turn already gone (bridge down or settled) */
  }
}

/** Queue a steering prompt into a live turn (joins at the next round). */
export async function queueWorkAgentMsg(turnId: string, id: string, message: ChatMsg): Promise<void> {
  await invokeCmd<void>("work_agent_queue", { turnId, id, message });
}

/** Drop a still-queued steering prompt (composer chip removed). */
export async function removeWorkAgentMsg(turnId: string, id: string): Promise<void> {
  try {
    await invokeCmd<void>("work_agent_remove", { turnId, id });
  } catch {
    /* turn already gone */
  }
}

/** Answer a live `ask` card (answer or dismissal text). */
export async function answerWorkAgentAsk(turnId: string, askId: string, answer: string): Promise<void> {
  await invokeCmd<void>("work_agent_answer", { turnId, askId, answer });
}
