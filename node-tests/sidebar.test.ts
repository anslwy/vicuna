import test from "node:test";
import assert from "node:assert/strict";
import {
  ALL_FILTER,
  CHAT_FILTER,
  NO_WORKSPACE_FILTER,
  NO_WORKSPACE_SENTINEL,
  compactWorkspaceLabel,
  fullWorkspacePath,
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

test("compactWorkspaceLabel handles empty and no-workspace / scratch paths", () => {
  assert.equal(compactWorkspaceLabel(null), "No workspace");
  assert.equal(compactWorkspaceLabel(undefined), "No workspace");
  assert.equal(compactWorkspaceLabel(""), "No workspace");
  assert.equal(compactWorkspaceLabel("   "), "No workspace");
  assert.equal(compactWorkspaceLabel(NO_WORKSPACE_SENTINEL), "No workspace");
  assert.equal(compactWorkspaceLabel(NO_WORKSPACE_FILTER), "No workspace");
  assert.equal(
    compactWorkspaceLabel("/run/media/deck/SDCARD/distrobox/omni/vicuna/scratch/c19ce8f0-02c5-49e6-9744-123456789abc"),
    "No workspace"
  );
  assert.equal(
    compactWorkspaceLabel("C:\\Users\\alice\\vicuna\\scratch\\abc-123"),
    "No workspace"
  );
});

test("compactWorkspaceLabel formats nested Unix and Windows paths cleanly", () => {
  assert.equal(
    compactWorkspaceLabel("/run/media/deck/SDCARD/distrobox/omni/tmp/vic-work-path-ui"),
    "tmp/vic-work-path-ui"
  );
  assert.equal(compactWorkspaceLabel("/Users/alice/projects/vicuna"), "projects/vicuna");
  assert.equal(compactWorkspaceLabel("/Users/alice/projects/vicuna/"), "projects/vicuna");
  assert.equal(compactWorkspaceLabel("C:\\Users\\alice\\projects\\vicuna"), "projects/vicuna");
  assert.equal(compactWorkspaceLabel("\\\\?\\C:\\dev\\vicuna"), "dev/vicuna");
  // Generic OS parent dirs and Windows drive letters are omitted
  assert.equal(compactWorkspaceLabel("/home/deck"), "deck");
  assert.equal(compactWorkspaceLabel("/Users/alice"), "alice");
  assert.equal(compactWorkspaceLabel("C:\\Users\\alice"), "alice");
  assert.equal(compactWorkspaceLabel("C:\\vicuna"), "vicuna");
});

test("compactWorkspaceLabel handles root, single-segment, and very long folder names", () => {
  assert.equal(compactWorkspaceLabel("/"), "/");
  assert.equal(compactWorkspaceLabel("///"), "/");
  assert.equal(compactWorkspaceLabel("/vicuna"), "vicuna");
  assert.equal(compactWorkspaceLabel("vicuna"), "vicuna");
  assert.equal(compactWorkspaceLabel("C:\\"), "C:");

  // When parent + folder exceeds max length, drop parent
  assert.equal(
    compactWorkspaceLabel("/a/very-long-parent-directory-name/vicuna"),
    "vicuna"
  );

  // When a single folder name is huge, cap with ellipsis
  const huge = "super-extremely-long-workspace-folder-name-exceeding-limit";
  const label = compactWorkspaceLabel(`/Users/alice/projects/${huge}`);
  assert.equal(label.length, 36);
  assert.equal(label.endsWith("…"), true);
  assert.equal(label, `${huge.slice(0, 35)}…`);
});

test("fullWorkspacePath preserves materialized scratch paths and hides sentinels/empty", () => {
  assert.equal(fullWorkspacePath(null), null);
  assert.equal(fullWorkspacePath(""), null);
  assert.equal(fullWorkspacePath("   "), null);
  assert.equal(fullWorkspacePath(NO_WORKSPACE_SENTINEL), null);
  assert.equal(fullWorkspacePath(NO_WORKSPACE_FILTER), null);
  assert.equal(
    fullWorkspacePath("/run/media/deck/SDCARD/distrobox/omni/vicuna/scratch/c19ce8f0"),
    "/run/media/deck/SDCARD/distrobox/omni/vicuna/scratch/c19ce8f0"
  );
  assert.equal(
    fullWorkspacePath("\\\\?\\C:\\Users\\alice\\vicuna"),
    "C:\\Users\\alice\\vicuna"
  );
  assert.equal(
    fullWorkspacePath("\\\\?\\UNC\\server\\share\\proj"),
    "\\server\\share\\proj"
  );
});
