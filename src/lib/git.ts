// Git panel adapter: Tauri invokes in the desktop app, the headless bridge
// in a plain browser (see bridge.ts). Logic lives in src-tauri/src/git_panel.rs.
import { invokeOrBridge } from "./bridge";
import type { StatusEntry } from "./gitDiff";

export interface GitStatus {
  repo: boolean;
  branch: string | null;
  files: StatusEntry[];
  truncated: boolean;
}

export interface GitDiffText {
  text: string;
  tooLarge: boolean;
}

export const gitStatus = (root: string) => invokeOrBridge<GitStatus>("git_status", { root });

export const gitDiff = (root: string, path: string, staged: boolean, untracked: boolean) =>
  invokeOrBridge<GitDiffText>("git_diff", { root, path, staged, untracked });

export const gitStage = (root: string, paths: string[]) => invokeOrBridge<void>("git_stage", { root, paths });

export const gitUnstage = (root: string, paths: string[]) => invokeOrBridge<void>("git_unstage", { root, paths });

/** `paths` are restored from the index; `untracked` files are deleted. */
export const gitDiscard = (root: string, paths: string[], untracked: string[]) =>
  invokeOrBridge<void>("git_discard", { root, paths, untracked });

export const gitApply = (root: string, patch: string, cached: boolean, reverse: boolean, intentToAdd: string[] = []) =>
  invokeOrBridge<void>("git_apply", { root, patch, cached, reverse, intentToAdd });
