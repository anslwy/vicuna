import test from "node:test";
import assert from "node:assert/strict";
import {
  ALL_FILTER,
  CHAT_FILTER,
  NO_WORKSPACE_SENTINEL,
  resolveDraftWorkspace,
  showWorkspaceBadge,
} from "../src/lib/sidebar.ts";
import type { Chat } from "../src/lib/store.ts";

function makeChat(id: string, workspace: string | null): Chat {
  return {
    id,
    title: id,
    createdAt: 1,
    updatedAt: 1,
    messages: [{ role: "user", content: "hi" }],
    mode: "work",
    workspace,
  };
}

test("fresh workspace prefs default Work drafts to the host home dir when known", () => {
  assert.equal(resolveDraftWorkspace(undefined, "/home/deck"), "/home/deck");
  assert.equal(resolveDraftWorkspace("", "/root"), "/root");
  assert.equal(resolveDraftWorkspace("/", "/Users/alice"), "/Users/alice");
  assert.equal(resolveDraftWorkspace("", "C:\\Users\\alice"), "C:\\Users\\alice");
});

test("fresh workspace prefs fall back to No workspace when home dir is unavailable", () => {
  assert.equal(resolveDraftWorkspace(undefined), NO_WORKSPACE_SENTINEL);
  assert.equal(resolveDraftWorkspace(""), NO_WORKSPACE_SENTINEL);
  assert.equal(resolveDraftWorkspace("/"), NO_WORKSPACE_SENTINEL);
  assert.equal(resolveDraftWorkspace("", ""), NO_WORKSPACE_SENTINEL);
  assert.equal(resolveDraftWorkspace("", "/"), NO_WORKSPACE_SENTINEL);
});

test("Work drafts retain a saved default workspace or explicit No workspace choice", () => {
  assert.equal(resolveDraftWorkspace("/projects/vicuna"), "/projects/vicuna");
  assert.equal(resolveDraftWorkspace("/projects/vicuna", "/home/deck"), "/projects/vicuna");
  assert.equal(resolveDraftWorkspace(NO_WORKSPACE_SENTINEL, "/home/deck"), NO_WORKSPACE_SENTINEL);
});

test("showWorkspaceBadge hides pill when session is already in the selected workspace scope", () => {
  const beta = makeChat("b", "/Users/me/projects/ollama-desktop");
  const betaSlash = makeChat("bs", "/Users/me/projects/ollama-desktop/");
  const betaWin = makeChat("bw", "\\\\?\\C:\\projects\\ollama-desktop");
  const other = makeChat("o", "/Users/me/projects/vicuna");
  const loose = makeChat("l", null);

  assert.equal(showWorkspaceBadge(beta, ALL_FILTER), true);
  assert.equal(showWorkspaceBadge(beta, CHAT_FILTER), true);
  assert.equal(showWorkspaceBadge(beta, "W:/Users/me/projects/ollama-desktop"), false);
  assert.equal(showWorkspaceBadge(betaSlash, "W:/Users/me/projects/ollama-desktop"), false);
  assert.equal(showWorkspaceBadge(beta, "/Users/me/projects/ollama-desktop"), false);
  assert.equal(showWorkspaceBadge(betaWin, "W:C:/projects/ollama-desktop"), false);
  assert.equal(showWorkspaceBadge(other, "W:/Users/me/projects/ollama-desktop"), true);
  assert.equal(showWorkspaceBadge(loose, "W:/Users/me/projects/ollama-desktop"), true);
});
