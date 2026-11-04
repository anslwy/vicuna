// Headless checks for src/lib/sidebar.ts (pure logic). Run: node scripts/check-sidebar.mjs
import {
  ALL_FILTER,
  CHAT_FILTER,
  WORKSPACE_PREFIX,
  folderOf,
  folderOptionKey,
  workspaceOfFolderKey,
  folderOptions,
  matchesFilter,
  normalizeSessionFilter,
  resolveSessionFilter,
  filterChats,
  groupByDate,
  workspaceBase
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

if (failures > 0) {
  console.error(`\n${failures} sidebar check(s) failed`);
  process.exit(1);
}
console.log("\nall sidebar checks passed");
