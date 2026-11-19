// Vicuna Work mode: the user's opencode CLI, driven by the backend over ACP
// (src-tauri/src/acp.rs). This module holds the IPC wrappers (turns, engine
// detection, opencode's model list), the workspace/file-explorer helpers and
// the transcript formatting the event stream renders with.

import type { ChatMsg } from "./ollama";
import { isTauriRuntime } from "./ollama";
import { invokeOrBridge, BridgeChannel, bridgeInvoke, bridgeAvailable, isRemoteServer, isServerBacked, useLocalIpc } from "./bridge";
import type { Chat } from "./store";
import type { OpencodeModels, WorkEngineInfo } from "./workGate";
// pure formatters live in toolFormat.ts (node-testable; the server ports them)
export { toolLabel, formatAskRecord, formatToolCallHead, formatToolCallTail, formatToolResult, formatDisplayNote } from "./toolFormat";
export { workSendGate, opencodeModelChoices, workReplyModel, engineId, engineLabel, workEngineChoices, workEngineMenuItems, usageMeterTone, usageTabs, usageHoverRows, usageGaugeFrac } from "./workGate";
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

export interface WorkFileContent {
  path: string;
  content: string;
  truncated: boolean;
  size: number;
}

/** Read a workspace file for the in-app editor (desktop or bridge). */
export async function readWorkFile(root: string, path: string, maxBytes = 524288): Promise<WorkFileContent> {
  return await invokeCmd<WorkFileContent>("work_read_file", { root, path, maxBytes });
}

/** Save a workspace file from the in-app editor (desktop or bridge). */
export async function writeWorkFile(root: string, path: string, content: string): Promise<void> {
  await invokeCmd<string>("work_write_file", { root, path, content });
}

/** Open a workspace path with the OS default app (desktop only; sandboxed
 *  backend-side via work_join). */
export async function openWorkPathExternal(root: string, path: string): Promise<void> {
  if (isRemoteServer()) throw new Error("That file is on the server; open it in the in-app editor instead.");
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke("work_open_path", { root, path });
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

/** Remote-server terminals: their poll channels (stopped on kill/exit) and
 *  per-terminal write queues. Each keystroke is an HTTP request there, and
 *  concurrent requests can land out of order, so writes are serialized and
 *  keystrokes typed while one is in flight coalesce into the next send. */
const remoteTermChannels = new Map<string, BridgeChannel>();
const termWriteQueues = new Map<string, { pending: string; busy: boolean }>();

/** Decide where terminal commands go: remote server windows and plain-browser
 *  dev-bridge sessions use `BridgeChannel`/`bridgeInvoke`; desktop main
 *  windows stay on their own Tauri pty (never the local embedded server — the
 *  main window is server-backed (`local`) since 0.0.23, so `invokeOrBridge`
 *  would send writes to a server that never heard of the Tauri pty's id). */
async function termUsesBridge(): Promise<boolean> {
  if (isRemoteServer()) return true;
  if (isTauriRuntime()) return false;
  if (await bridgeAvailable()) return true;
  throw new Error("workspace tools need the Vicuna desktop app.");
}

/** Spawn an interactive shell rooted at the workspace (desktop or dev bridge;
 *  on a remote server the shell runs on the server). Output/exit stream through
 *  the callbacks; returns the terminal id. */
export async function spawnTerm(
  root: string,
  cols: number,
  rows: number,
  onOutput: (text: string) => void,
  onExit: (code: number | null) => void
): Promise<string> {
  const useBridge = await termUsesBridge();
  const handle = (ev: TermStreamEvent) => {
    if (ev.type === "output") onOutput(ev.text ?? "");
    else if (ev.type === "exit") onExit(ev.code ?? null);
  };
  if (useBridge) {
    const ch = new BridgeChannel();
    let id = "";
    ch.onmessage = (ev) => {
      handle(ev as TermStreamEvent);
      if ((ev as TermStreamEvent).type === "exit") {
        ch.stop();
        if (id) remoteTermChannels.delete(id);
      }
    };
    try {
      id = await bridgeInvoke("term_spawn", { root, cols, rows, onEvent: ch.marker() });
    } catch (e) {
      ch.stop();
      throw e;
    }
    remoteTermChannels.set(id, ch);
    return id;
  }
  const { invoke, Channel } = await import("@tauri-apps/api/core");
  const ch = new Channel<TermStreamEvent>();
  ch.onmessage = handle;
  return await invoke<string>("term_spawn", { root, cols, rows, onEvent: ch });
}

async function termCmd(cmd: string, args: Record<string, any>): Promise<void> {
  if (await termUsesBridge()) {
    await bridgeInvoke(cmd, args);
    return;
  }
  const { invoke } = await import("@tauri-apps/api/core");
  await invoke(cmd, args);
}

/** Send keystrokes to a terminal (desktop or dev bridge). Serialized per terminal. */
export async function writeTerm(id: string, data: string): Promise<void> {
  await termUsesBridge();
  let q = termWriteQueues.get(id);
  if (!q) {
    q = { pending: "", busy: false };
    termWriteQueues.set(id, q);
  }
  q.pending += data;
  if (q.busy) return;
  q.busy = true;
  try {
    while (q.pending) {
      const chunk = q.pending;
      q.pending = "";
      await termCmd("term_write", { id, data: chunk });
    }
  } finally {
    q.busy = false;
  }
}

/** Resize a terminal (desktop or dev bridge). */
export async function resizeTerm(id: string, cols: number, rows: number): Promise<void> {
  await termCmd("term_resize", { id, cols, rows });
}

/** Kill a terminal's shell (desktop or dev bridge, best-effort). */
export async function killTerm(id: string): Promise<void> {
  const useBridge = isRemoteServer() || (!isTauriRuntime() && (remoteTermChannels.has(id) || (await bridgeAvailable())));
  if (!isTauriRuntime() && !useBridge) return;
  remoteTermChannels.get(id)?.stop();
  remoteTermChannels.delete(id);
  termWriteQueues.delete(id);
  try {
    if (useBridge) {
      await bridgeInvoke("term_kill", { id });
    } else {
      const { invoke } = await import("@tauri-apps/api/core");
      await invoke("term_kill", { id });
    }
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
  | { type: "askrequest"; ask_id: string; question: string; options: string[]; multi?: boolean }
  | { type: "schedulewakeup"; req_id: string; message: string; at: string | null; in_minutes: number | null }
  | { type: "sessioncontrol"; req_id: string; action: string; args: Record<string, any> }
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
  /** "opencode" | "claude" | "codex" | "fx" | "agy" | "pi" (absent = the backend's saved choice) */
  engine?: string;
  /** engine model id; empty = saved pick / engine default */
  model: string;
  /** Start a new opencode session instead of resuming (regenerate/edit). */
  freshSession?: boolean;
  workspace: string;
  messages: ChatMsg[];
  sessionId?: string;
  /** the user's lead session: vicuna-mcp manage_sessions is allowed */
  lead?: boolean;
  /** server mode: the session as it stands at turn start (save row with its
   *  revision) and the live bubble's index; the server records the turn */
  persist?: { chat: Record<string, unknown>; index: number } | null;
  /** server mode: lead session the server reports to when this turn ends */
  reportTo?: string | null;
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
    lead: p.lead ?? false,
    // server mode (ignored elsewhere): see WorkTurnParams.persist
    ...(p.persist ? { persistChat: p.persist.chat, persistIndex: p.persist.index, reportTo: p.reportTo ?? null } : {}),
    tzOffsetMin: -new Date().getTimezoneOffset()
  };
  if (useLocalIpc()) {
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

/** A Work turn the server is still running for this session (the window that
 *  started it closed or reloaded). Only remote servers / bridged browsers keep
 *  turns across windows; local Tauri turns end with their window.
 *  Keyed by Vicuna session id. */
export async function liveWorkTurns(): Promise<Record<string, { turnId: string; channel: string; startedMs?: number }>> {
  if (useLocalIpc()) return {};
  try {
    return ((await bridgeInvoke("work_agent_live", {})) ?? {}) as Record<string, { turnId: string; channel: string; startedMs?: number }>;
  } catch {
    return {}; // older server without the command, or bridge down
  }
}

/** Server-side recording of a live turn (server mode): the transcript so far,
 *  the live bubble index, the in-flight tool call and an unanswered ask card.
 *  Taking it drops the already-recorded events from the channel. */
export type WorkTurnSnapshot = {
  turnId: string;
  channel: string;
  startedMs?: number;
  snapshot?: {
    messages: any[];
    index: number;
    pendingHead: { name: string; args: Record<string, any> } | null;
    pendingAsk: WorkAgentEvent | null;
  };
};

export async function attachWorkAgentSnapshot(sessionId: string): Promise<WorkTurnSnapshot | null> {
  try {
    return ((await bridgeInvoke("work_agent_attach", { sessionId })) ?? null) as WorkTurnSnapshot | null;
  } catch {
    return null; // older server without the command
  }
}

let serverPersistCache: Promise<boolean> | null = null;

/** Does this window's server record Work turns itself (and check chat
 *  revisions)? False for This computer, the dev bridge and older servers. */
export function serverRecordsTurns(): Promise<boolean> {
  if (!isServerBacked()) return Promise.resolve(false);
  serverPersistCache ??= bridgeInvoke("server_info", {})
    .then((i: any) => i?.persist === true)
    .catch(() => {
      serverPersistCache = null;
      return false;
    });
  return serverPersistCache;
}

/** Follow an already-running turn's event stream (see liveWorkTurns). */
export function attachWorkAgentTurn(channel: string, onEvent: (ev: WorkAgentEvent) => void): void {
  const ch = new BridgeChannel(channel);
  ch.onmessage = (ev) => onEvent(ev as WorkAgentEvent);
}

/** Interrupt a running turn (double-Esc, Stop, chat deleted). */
export async function stopWorkAgentTurn(turnId: string): Promise<void> {
  if (useLocalIpc()) {
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

/** Is a backend turn still running? null when it can't be asked (an
 *  older shell/server without `work_agent_turn_live`, bridge down) — the
 *  caller must then not assume either way. */
export async function workTurnLive(turnId: string): Promise<boolean | null> {
  try {
    if (useLocalIpc()) {
      const { invoke } = await import("@tauri-apps/api/core");
      return Boolean(await invoke("work_agent_turn_live", { turnId }));
    }
    return Boolean(await bridgeInvoke("work_agent_turn_live", { turnId }));
  } catch {
    return null;
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
export async function answerWorkAgentAsk(
  turnId: string,
  askId: string,
  answer: string,
  record?: { display: string; dismissed: boolean }
): Promise<void> {
  // `record`: the Q/A record this window writes into its copy; a server
  // recording the turn writes the same into its db
  await invokeCmd<void>("work_agent_answer", { turnId, askId, answer, ...(record ? { record } : {}) });
}

/** The ask card's "Answer later": vicuna-mcp returns the waiting ask_user
 *  call at once (must match `vicuna_mcp::ASK_LATER`). */
export const ASK_LATER = '{"vicunaAskLater":true}';

export type UsageMeter = {
  label: string;
  percent: number;
  kind: "remaining" | "used" | string;
  valueText: string;
  resetText: string | null;
};

export type UsageGroup = {
  title: string;
  subtitle: string | null;
  meters: UsageMeter[];
};

export type UsageStat = {
  label: string;
  value: string;
};

export type WorkUsageReport = {
  engine: string;
  engineLabel: string;
  account: string | null;
  plan: string | null;
  groups: UsageGroup[];
  stats: UsageStat[];
  note: string | null;
  raw: string | null;
  error: string | null;
};

/** Fetch quota / usage for a Work harness (defaults to the active engine). */
export async function fetchWorkUsage(engine?: string): Promise<WorkUsageReport> {
  return await invokeCmd<WorkUsageReport>("work_usage", { engine: engine ?? null });
}
