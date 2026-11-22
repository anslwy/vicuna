import test from "node:test";
import assert from "node:assert/strict";
import {
  ALL_FILTER,
  CHAT_FILTER,
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
