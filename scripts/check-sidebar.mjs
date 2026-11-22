// Headless checks for src/lib/sidebar.ts (pure logic). Run: node scripts/check-sidebar.mjs
import {
  ALL_FILTER,
  CHAT_FILTER,
  NO_WORKSPACE_FILTER,
  NO_WORKSPACE_SENTINEL,
  WORKSPACE_PREFIX,
  isNoWorkspacePath,
  folderOf,
  folderOptionKey,
  workspaceFilterKey,
  workspaceOfFolderKey,
  folderOptions,
  matchesFilter,
  normalizeSessionFilter,
  resolveSessionFilter,
  filterChats,
  groupByDate,
  workspaceBase,
  visibleChats,
  isPinnedRow,
  showWorkspaceBadge
} from "../src/lib/sidebar.ts";

let failures = 0;
function eq(actual, expected, label) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    failures++;
    console.error(`FAIL ${label}: got ${a}, want ${e}`);
  } else {
    console.log(`ok ${label}`);
  }
}

const MIN = 60 * 1000;
const HOUR = 60 * MIN;

// ---- fixtures ----
function chat(id, opts = {}) {
  const { mode = "chat", workspace = null, age = 0, messages = 2, now = 0 } = opts;
  return {
    id,
    title: id,
    createdAt: now - age,
    updatedAt: now - age,
    messages: new Array(messages).fill({ role: "user", content: "x" }),
    mode,
    workspace
  };
}

// ---- workspaceBase: one label per platform, verbatim prefix stripped ----
eq(workspaceBase("/Users/me/projects/ollama-desktop"), "ollama-desktop", "posix base");
eq(workspaceBase("/Users/me/projects/ollama-desktop/"), "ollama-desktop", "posix trailing slash");
eq(workspaceBase("C:\\Users\\me\\proj"), "proj", "windows base");
eq(workspaceBase("\\\\?\\C:\\Users\\me\\proj"), "proj", "windows verbatim prefix stripped");
eq(workspaceBase("\\\\?\\UNC\\srv\\share\\proj"), "proj", "windows UNC verbatim prefix stripped");
eq(workspaceBase(""), "", "empty workspace has no base");

// ---- folderOf: chats and workspace-less work sessions share the Chat channel ----
eq(folderOf(chat("a")), CHAT_FILTER, "plain chat lives in Chat");
eq(folderOf(chat("b", { mode: "work", workspace: null })), CHAT_FILTER, "workspace-less work session falls back to Chat");
eq(folderOf(chat("c", { mode: "work", workspace: "   " })), CHAT_FILTER, "blank workspace falls back to Chat");
eq(folderOf(chat("d", { mode: "work", workspace: "/p/one" })), "/p/one", "work session lives in its root");
eq(folderOf(chat("e", { mode: "work", workspace: "/p/one/" })), "/p/one", "trailing slash does not split the folder");
eq(folderOf(chat("f", { mode: "work", workspace: "\\\\?\\C:\\p\\one" })), "C:/p/one", "verbatim windows root normalized");
eq(folderOf(chat("g", { mode: "work", workspace: "C:\\p\\one" })), "C:/p/one", "windows root normalized to one key");

// ---- option keys: a workspace root can never collide with a channel ----
eq(folderOptionKey(ALL_FILTER), "all", "All key is itself");
eq(folderOptionKey(CHAT_FILTER), "chat", "Chat key is itself");
eq(folderOptionKey("/p/beta"), "W:/p/beta", "workspace key is prefixed");
eq(workspaceOfFolderKey("all"), null, "All has no workspace");
eq(workspaceOfFolderKey("chat"), null, "Chat has no workspace");
eq(workspaceOfFolderKey("W:/p/beta"), "/p/beta", "prefixed key decodes to the root");
eq(workspaceOfFolderKey("W:"), null, "empty prefixed key decodes to nothing");
// a build that stored the bare root must still resolve
eq(workspaceOfFolderKey("/p/beta"), "/p/beta", "legacy raw root still decodes");
// a workspace literally named "all" or "chat" is not the channel
eq(workspaceOfFolderKey("W:/p/all"), "/p/all", "folder named all is not the All channel");
eq(workspaceOfFolderKey("W:/p/chat"), "/p/chat", "folder named chat is not the Chat channel");
eq(workspaceFilterKey("/p/beta/"), "W:/p/beta", "workspaceFilterKey normalizes trailing slash");
eq(workspaceFilterKey("C:\\p\\beta\\"), "W:/p/beta".replace("/p", "C:/p"), "workspaceFilterKey normalizes windows path");
eq(workspaceFilterKey("   "), null, "blank workspace has no filter key");

// ---- folderOptions: derived from sessions, never from prefs ----
const now = new Date(2024, 4, 15, 12, 0, 0).getTime();
const opts = folderOptions([
  chat("a", { now, age: 1 * MIN }),                                  // Chat, newest
  chat("b", { now, age: 2 * HOUR, mode: "work", workspace: "/p/beta" }),
  chat("c", { now, age: 30 * HOUR, mode: "work", workspace: "/p/alpha" }),
  chat("d", { now, age: 40 * HOUR, mode: "work", workspace: "/p/beta/", messages: 0 }) // empty draft
]);
eq(opts.map((o) => o.label), ["All", "Chat", "beta", "alpha"], "All first, then Chat, then workspaces by recency");
eq(opts.map((o) => o.count), [3, 1, 1, 1], "All counts everything; empty drafts count toward nothing");
eq(opts[0].key, ALL_FILTER, "All option key");
eq(opts[1].key, CHAT_FILTER, "chat option key");
eq(opts[2].key, "W:/p/beta", "workspace option key is prefixed");
eq(opts[2].title, "/p/beta", "option tooltip carries the full path");
eq(opts[0].title, "", "All carries no path");

// Chat is offered even when empty, so the dropdown never jumps around
const onlyWork = folderOptions([chat("w", { now, mode: "work", workspace: "/p/solo" })]);
eq(onlyWork.map((o) => o.label), ["All", "Chat", "solo"], "Chat entry present with zero chat sessions");
eq(onlyWork[1].count, 0, "empty Chat entry counts zero");
eq(onlyWork[0].count, 1, "All counts only real sessions");

// Two sessions in one folder collapse to one entry
const sameFolder = folderOptions([
  chat("x", { now, mode: "work", workspace: "/p/one" }),
  chat("y", { now, age: HOUR, mode: "work", workspace: "/p/one/" })
]);
eq(sameFolder.map((o) => o.label), ["All", "Chat", "one"], "one entry per folder");
eq(sameFolder[2].count, 2, "entry counts every session in the folder");

eq(folderOptions([]).map((o) => o.label), ["All", "Chat"], "no sessions still offers All and Chat");
eq(folderOptions([]).map((o) => o.count), [0, 0], "no sessions counts zero");

// Same final segment in two places: the dropdown shows one label at a time,
// so the two entries must be distinguishable
const twins = folderOptions([
  chat("t1", { now, mode: "work", workspace: "/Users/me/work/api" }),
  chat("t2", { now, age: HOUR, mode: "work", workspace: "/Users/me/side/api" })
]);
eq(twins.map((o) => o.label), ["All", "Chat", "work › api", "side › api"], "duplicate bases are parent-qualified");

// extraWorkspace includes a 0-session workspace (e.g. synced from composer) without duplicating existing ones
const withExtra = folderOptions(
  [chat("b", { now, mode: "work", workspace: "/p/beta" })],
  "/p/fresh/"
);
eq(withExtra.map((o) => o.label), ["All", "Chat", "fresh", "beta"], "extraWorkspace appears with 0 count");
eq(withExtra[2], { key: "W:/p/fresh", label: "fresh", title: "/p/fresh", count: 0 }, "extraWorkspace entry shape");
eq(resolveSessionFilter("W:/p/fresh", withExtra), "W:/p/fresh", "extraWorkspace resolves instead of falling back to All");
const dupExtra = folderOptions(
  [chat("b", { now, mode: "work", workspace: "/p/beta" })],
  "/p/beta/"
);
eq(dupExtra.map((o) => o.label), ["All", "Chat", "beta"], "extraWorkspace matching existing folder is not duplicated");
eq(dupExtra[2].count, 1, "existing folder keeps its count");

// ---- normalize / resolve: a stale filter cannot blank the sidebar ----
eq(normalizeSessionFilter(undefined), ALL_FILTER, "undefined -> all");
eq(normalizeSessionFilter(null), ALL_FILTER, "null -> all");
eq(normalizeSessionFilter("   "), ALL_FILTER, "blank -> all");
eq(normalizeSessionFilter(42), ALL_FILTER, "non-string -> all");
eq(normalizeSessionFilter(" W:/p/beta "), "W:/p/beta", "value trimmed");

eq(resolveSessionFilter("W:/p/beta", opts), "W:/p/beta", "existing folder resolves");
eq(resolveSessionFilter("/p/beta", opts), "W:/p/beta", "legacy raw root resolves to the prefixed key");
eq(resolveSessionFilter("W:/p/gone", opts), ALL_FILTER, "deleted folder falls back to All");
eq(resolveSessionFilter(CHAT_FILTER, opts), CHAT_FILTER, "Chat resolves");
eq(resolveSessionFilter(ALL_FILTER, opts), ALL_FILTER, "All resolves");
eq(resolveSessionFilter("garbage", opts), ALL_FILTER, "unknown key falls back to All");

// ---- filterChats: scoping rules ----
const list = [
  chat("chat1", { now }),
  chat("work-beta", { now, mode: "work", workspace: "/p/beta" }),
  chat("work-alpha", { now, mode: "work", workspace: "/p/alpha" }),
  chat("work-loose", { now, mode: "work", workspace: null })
];
eq(filterChats(list, ALL_FILTER).map((c) => c.id), ["chat1", "work-beta", "work-alpha", "work-loose"], "All keeps everything, in order");
eq(filterChats(list, CHAT_FILTER).map((c) => c.id), ["chat1", "work-loose"], "Chat = plain chats + workspace-less work");
eq(filterChats(list, "W:/p/beta").map((c) => c.id), ["work-beta"], "workspace entry hides chat sessions");
eq(filterChats(list, "W:/p/alpha").map((c) => c.id), ["work-alpha"], "workspace entry shows only its own folder");
eq(filterChats(list, "W:/p/none").map((c) => c.id), [], "absent folder matches nothing");
eq(filterChats(list, "/p/beta").map((c) => c.id), ["work-beta"], "legacy raw root filters the same way");

// a chat session must never appear under a workspace entry
eq(list.filter((c) => c.mode !== "work").filter((c) => matchesFilter(c, "W:/p/beta")), [], "chat never matches a workspace entry");

// ---- groupByDate: buckets, empty drafts, and ordering ----
const g = groupByDate([
  chat("today", { now, age: 30 * MIN }),
  chat("yest", { now, age: 20 * HOUR }),
  chat("week", { now, age: 3 * 24 * HOUR }),
  chat("old", { now, age: 40 * 24 * HOUR }),
  chat("draft", { now, age: 1 * MIN, messages: 0 })
], now);
eq(g.today.map((c) => c.id), ["today"], "today bucket");
eq(g.yesterday.map((c) => c.id), ["yest"], "yesterday bucket");
eq(g.week.map((c) => c.id), ["week"], "week bucket");
eq(g.older.map((c) => c.id), ["old"], "older bucket");
eq([...g.today, ...g.yesterday, ...g.week, ...g.older].some((c) => c.id === "draft"), false, "empty drafts are never listed");

// updatedAt wins over createdAt: a reply to an old session re-times it
const revived = {
  id: "revived",
  title: "revived",
  createdAt: now - 40 * 24 * HOUR,
  updatedAt: now - 5 * MIN,
  messages: [{ role: "user", content: "x" }],
  mode: "chat",
  workspace: null
};
eq(groupByDate([revived], now).today.map((c) => c.id), ["revived"], "updatedAt re-times a session into Today");

// sessions without updatedAt fall back to createdAt
const legacy = { id: "legacy", title: "legacy", createdAt: now - 3 * 24 * HOUR, messages: [{ role: "user", content: "x" }] };
eq(groupByDate([legacy], now).week.map((c) => c.id), ["legacy"], "missing updatedAt falls back to createdAt");

// ---- ordering inside a bucket ----
const ordered = groupByDate([
  chat("a", { now, age: 3 * HOUR }),
  chat("b", { now, age: 1 * HOUR }),
  chat("c", { now, age: 2 * HOUR })
], now);
eq(ordered.today.map((c) => c.id), ["b", "c", "a"], "newest first inside a bucket");

// ---- archive ----
const arch = [{ id: "a", archived: true }, { id: "b" }, { id: "c", archived: true }];
eq(visibleChats(arch, true).map((c) => c.id), ["a", "b", "c"], "show archived lists everything");
eq(visibleChats(arch, false).map((c) => c.id), ["b"], "hide archived drops archived sessions");
eq(visibleChats(arch, false, "c").map((c) => c.id), ["b", "c"], "the open session stays listed while hidden");
eq(isPinnedRow({ pinned: true, messages: [1] }), true, "pinned row");
eq(isPinnedRow({ pinned: true, archived: true, messages: [1] }), false, "archived leaves Pinned");
eq(isPinnedRow({ pinned: true, messages: [] }), false, "empty never pinned");

// ---- showWorkspaceBadge: hide redundant pill when already in that workspace scope ----
const wsBeta = chat("wb", { now, mode: "work", workspace: "/p/beta" });
const wsBetaSlash = chat("wbs", { now, mode: "work", workspace: "/p/beta/" });
const wsAlpha = chat("wa", { now, mode: "work", workspace: "/p/alpha" });
const wsLoose = chat("wl", { now, mode: "work", workspace: null });
eq(showWorkspaceBadge(wsBeta, ALL_FILTER), true, "All scope shows workspace pill");
eq(showWorkspaceBadge(wsBeta, CHAT_FILTER), true, "Chat scope keeps workspace pill (e.g. Pinned work session)");
eq(showWorkspaceBadge(wsBeta, "W:/p/beta"), false, "matching workspace scope hides redundant pill");
eq(showWorkspaceBadge(wsBetaSlash, "W:/p/beta"), false, "trailing slash still matches scope and hides pill");
eq(showWorkspaceBadge(wsBeta, "/p/beta"), false, "legacy raw root filter still hides pill");
eq(showWorkspaceBadge(wsAlpha, "W:/p/beta"), true, "different workspace (e.g. Pinned) keeps its pill");
eq(showWorkspaceBadge(wsLoose, "W:/p/beta"), true, "workspace-less work session keeps Work badge");

// ---- No workspace mode (scratch per-session folder) ----
eq(isNoWorkspacePath(NO_WORKSPACE_SENTINEL), true, "sentinel is no-workspace");
eq(isNoWorkspacePath("/home/me/vicuna/scratch/fe0e2692-f7d3"), true, "posix scratch folder is no-workspace");
eq(isNoWorkspacePath("C:\\Users\\me\\vicuna\\scratch\\abc_123\\"), true, "windows scratch folder is no-workspace");
eq(isNoWorkspacePath("/home/me/repos/vicuna"), false, "normal repo is not no-workspace");
eq(folderOf(chat("nw", { mode: "work", workspace: "/home/me/vicuna/scratch/sess-1" })), NO_WORKSPACE_FILTER, "scratch session lives in No workspace channel");
eq(workspaceFilterKey(NO_WORKSPACE_SENTINEL), NO_WORKSPACE_FILTER, "sentinel maps to No workspace filter");
eq(workspaceOfFolderKey(NO_WORKSPACE_FILTER), null, "No workspace filter has no single repo root");
const withNoWs = folderOptions([
  chat("nw1", { now, age: 1 * MIN, mode: "work", workspace: "/home/me/vicuna/scratch/sess-1" }),
  chat("nw2", { now, age: 2 * MIN, mode: "work", workspace: "/home/me/vicuna/scratch/sess-2" }),
  chat("w1", { now, age: 3 * MIN, mode: "work", workspace: "/p/beta" })
]);
eq(withNoWs.map((o) => o.label), ["All", "Chat", "No workspace", "beta"], "No workspace groups all scratch sessions into one option");
eq(withNoWs.find((o) => o.key === NO_WORKSPACE_FILTER)?.count, 2, "No workspace counts all scratch sessions");
const pendingNoWs = folderOptions([chat("w1", { now, mode: "work", workspace: "/p/beta" })], NO_WORKSPACE_SENTINEL);
eq(pendingNoWs.map((o) => o.label), ["All", "Chat", "No workspace", "beta"], "extraWorkspace sentinel offers No workspace with 0 count");
eq(pendingNoWs.find((o) => o.key === NO_WORKSPACE_FILTER)?.count, 0, "pending No workspace counts zero");
eq(resolveSessionFilter(NO_WORKSPACE_FILTER, pendingNoWs), NO_WORKSPACE_FILTER, "No workspace resolves");
eq(filterChats([
  chat("nw1", { now, mode: "work", workspace: "/home/me/vicuna/scratch/sess-1" }),
  chat("c1", { now }),
  chat("w1", { now, mode: "work", workspace: "/p/beta" })
], NO_WORKSPACE_FILTER).map((c) => c.id), ["nw1"], "No workspace filter matches only scratch sessions");
eq(showWorkspaceBadge(chat("nw1", { mode: "work", workspace: "/home/me/vicuna/scratch/sess-1" }), NO_WORKSPACE_FILTER), false, "No workspace scope hides redundant badge");
eq(showWorkspaceBadge(chat("nw1", { mode: "work", workspace: "/home/me/vicuna/scratch/sess-1" }), ALL_FILTER), true, "All scope shows No workspace badge");

if (failures > 0) {
  console.error(`\n${failures} sidebar check(s) failed`);
  process.exit(1);
}
console.log("\nall sidebar checks passed");
