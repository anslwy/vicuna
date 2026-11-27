import { type McpEnabledMap, listMcpServers, resolveMcpEnabled } from "./mcp";
import { createReleaseNotesPreview, fetchGithubReleaseNotesMetadata, APP_UPDATE_RELEASES_URL } from "./appUpdate";
import { type OpenRouterKeyInfo, formatUsd } from "./openrouter";
import { type ServerChatHit } from "./bridge";
import { type Chat } from "./store";

/** live multiple-choice question from a work agent turn, per session. The
 *  turn blocks until the user answers; each session keeps its own card so
 *  concurrent turns don't overwrite each other's question. */
export interface PendingAsk {
  chatId: string;
  index: number;
  question: string;
  options: string[];
  /** multi-select question (vicuna-mcp ask_user `multiple`) */
  multi: boolean;
  /** the labels picked so far in a multi-select question */
  picked: string[];
  /** agent question (vicuna-mcp ask_user): choices prefill the answer box
   *  instead of answering, and the box takes pasted images. Permission
   *  asks (Allow/Deny) still answer on click. */
  agentQuestion: boolean;
  /** the answer box's draft text */
  text: string;
  /** images pasted into the answer box (data URLs) */
  images: string[];
  /** Other… opened the box on a permission ask */
  otherOpen: boolean;
  resolve: (answer: string, outcome: "answer" | "dismiss" | "cancel" | "later", display?: string) => void;
}

/** agent questions the user put off with "Answer later": the ask_user call
 *  already returned, so the answer goes to the session as a message */
export interface LaterAsk {
  id: string;
  chatId: string;
  index: number;
  question: string;
  options: string[];
  multi: boolean;
  picked: string[];
  text: string;
  images: string[];
}

export async function newMcpEnabled(draft?: McpEnabledMap): Promise<McpEnabledMap> {
  const defaults = await listMcpServers();
  return resolveMcpEnabled(defaults, { mcpEnabled: draft });
}

export const scheduleTz = (() => {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "";
  } catch {
    return "";
  }
})();

export const USAGE_CACHE_KEY = "vicuna-usage-cache";

export const CLI_INSTALL_CMD = "curl -fsSL https://github.com/anslwy/vicuna/releases/latest/download/install-cli.sh | sh";

export async function resolveAppUpdateReleaseNotes(update: {
  body?: string | null;
}): Promise<{ notesPreview: string | null; releaseNotesUrl: string }> {
  const fallback = createReleaseNotesPreview(update.body ?? null);
  try {
    return await fetchGithubReleaseNotesMetadata();
  } catch (err) {
    console.warn("Failed to fetch GitHub release notes preview:", err);
    return { notesPreview: fallback, releaseNotesUrl: APP_UPDATE_RELEASES_URL };
  }
}

export function openRouterUsageText(info: OpenRouterKeyInfo): string {
  const bits: string[] = [];
  if (info.usage !== null) bits.push(`usage ${formatUsd(info.usage)}`);
  if (info.limit !== null && info.limit > 0) {
    bits.push(info.limitRemaining !== null ? `${formatUsd(info.limitRemaining)} of ${formatUsd(info.limit)} left` : `limit ${formatUsd(info.limit)}`);
  }
  return bits.join(" · ");
}

export const SERVER_SETUP_CMD = "curl -fsSL https://github.com/anslwy/vicuna/releases/latest/download/install-cli.sh | sh\nvicuna serve --tailscale";

export type RemoteHit = ServerChatHit & { serverId: string; serverName: string };

export type SearchItem =
  | { kind: "new" }
  | { kind: "chat"; chat: Chat; snippet?: string }
  | { kind: "remote"; hit: RemoteHit };

export interface ModelChoice {
  id: string;
  name: string;
  sub: string;
  provider: "ollama" | "openrouter";
}

// discover tab: curated registry catalog with client-side search + filters
export const BROWSE_CATS = ["all", "chat", "code", "reasoning", "vision", "embedding", "small", "mlx"] as const;

// generic starter prompts (no tools required on the model's side)
export const SUGGESTIONS = [
  { title: "Explain a concept", prompt: "Explain black holes in simple terms", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 19.5A2.5 2.5 0 0 1 6.5 17H20"/><path d="M6.5 2H20v20H6.5A2.5 2.5 0 0 1 4 19.5v-15A2.5 2.5 0 0 1 6.5 2z"/></svg>' },
  { title: "Write code", prompt: "Write a Python function to sort a list", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="16 18 22 12 16 6"/><polyline points="8 6 2 12 8 18"/></svg>' },
  { title: "Draft a message", prompt: "Write a friendly follow-up email", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 20h9"/><path d="M16.5 3.5a2.121 2.121 0 0 1 3 3L7 19l-4 1 1-4L16.5 3.5z"/></svg>' },
  { title: "Brainstorm ideas", prompt: "Give me ideas for a weekend project", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M9 18h6"/><path d="M10 22h4"/><path d="M12 2a7 7 0 0 0-4 12.7c.6.5 1 1.4 1 2.3h6c0-.9.4-1.8 1-2.3A7 7 0 0 0 12 2z"/></svg>' },
  { title: "Summarize text", prompt: "Summarize this paragraph in two sentences:", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="16" y1="13" x2="8" y2="13"/><line x1="16" y1="17" x2="8" y2="17"/></svg>' },
  { title: "What can you do?", prompt: "What can you do?", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7L4.4 10.6l5.7-1.9z"/></svg>' }
];

// work starters: each one maps to a real Work-mode capability (list/read,
// grep, exec, write, sessions) so the cards demo what the agent can do
export const WORK_SUGGESTIONS = [
  { title: "Explore this folder", prompt: "List the files here and explain what this project is", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>' },
  { title: "Find in files", prompt: "Search the workspace for TODO comments and group them by file", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="11" cy="11" r="7"/><line x1="21" y1="21" x2="16.65" y2="16.65"/></svg>' },
  { title: "Run a command", prompt: "Run `git status` and summarize what changed", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><polyline points="4 17 10 11 4 5"/><line x1="12" y1="19" x2="20" y2="19"/></svg>' },
  { title: "Write a file", prompt: "Create a README.md that explains this project", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z"/><polyline points="14 2 14 8 20 8"/><line x1="12" y1="18" x2="12" y2="12"/><line x1="9" y1="15" x2="15" y2="15"/></svg>' },
  { title: "Search past chats", prompt: "Search my past chats for this project and summarize what we did", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="9"/><polyline points="12 7 12 12 15.5 14"/></svg>' },
  { title: "What can you do?", prompt: "What can you do in Work mode?", icon: '<svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M12 3l1.9 5.7 5.7 1.9-5.7 1.9L12 18.2l-1.9-5.7L4.4 10.6l5.7-1.9z"/></svg>' }
];

export const SHOW_ARCHIVED_KEY = "vicuna-show-archived";

export type ChatTurnEvent =
  | { type: "token" | "think" | "replace"; text: string }
  | { type: "done" | "images_stripped" }
  | { type: "error"; message: string };

export type ChatTurnOperation =
  | { operation: "send"; content: string; images?: string[]; titleSource: string }
  | { operation: "continue" | "regenerate"; index: number }
  | { operation: "edit"; index: number; content: string; images?: string[] }
  | { operation: "fork_edit"; index: number; content: string; images: string[]; forkId: string };

export const WARM_RECENT_COUNT = 12;
