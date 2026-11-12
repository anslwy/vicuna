// Pure state rules for the explorer's in-app code editor (CodeEditorModal).
// No CodeMirror / DOM imports so node-tests can load it directly.

/** Largest file the editor will open editable: matches the backend write cap
 *  (`work_write_file_logic`, ~512KB) and the raised read clamp. */
export const EDITOR_MAX_BYTES = 524288;

/** Dirty = text differs from what is on disk as of the last load/save, so
 *  undoing back to the saved text clears the indicator (VS Code behavior). */
export function isDirty(doc: string, saved: string): boolean {
  return doc !== saved;
}

/** Saving a truncated read would cut the file short on disk: such files open
 *  read-only. Returns the banner reason, or null when editable. */
export function readOnlyReason(truncated: boolean, size: number): string | null {
  if (truncated || size > EDITOR_MAX_BYTES) {
    const kb = Math.round(size / 1024);
    return `Read-only: file is ${kb}KB, larger than the ${EDITOR_MAX_BYTES / 1024}KB editor limit.`;
  }
  return null;
}

/** Decide what Cmd/Ctrl+S should do given what disk holds right now.
 *  - "clean": nothing to save
 *  - "conflict": disk moved since we loaded/saved (agent or another app wrote it)
 *  - "save": safe to write */
export function saveAction(doc: string, saved: string, onDisk: string | null): "clean" | "conflict" | "save" {
  if (onDisk !== null && onDisk !== saved) return "conflict";
  if (!isDirty(doc, saved)) return "clean";
  return "save";
}

/** Vim keybindings toggle, remembered per browser/app (localStorage). */
export const VIM_PREF_KEY = "vicuna-editor-vim";

type KV = Pick<Storage, "getItem" | "setItem">;

export function readVimPref(store: KV | null | undefined): boolean {
  try {
    return store?.getItem(VIM_PREF_KEY) === "1";
  } catch {
    return false;
  }
}

export function writeVimPref(store: KV | null | undefined, on: boolean): void {
  try {
    store?.setItem(VIM_PREF_KEY, on ? "1" : "0");
  } catch {
    /* storage unavailable (quota / private mode): toggle still works this session */
  }
}

/** Esc closes the editor, except in Vim mode where Esc belongs to Vim
 *  (leave insert/visual mode); there `:q` / × close it instead. Open
 *  overlays (conflict bar, close confirm) still take Esc first. */
export function escapeAction(vim: boolean, conflict: boolean, confirmClose: boolean): "dismiss-conflict" | "dismiss-confirm" | "close" | "none" {
  if (conflict) return "dismiss-conflict";
  if (confirmClose) return "dismiss-confirm";
  return vim ? "none" : "close";
}

/** Basename for the editor title (both separators, like workspaceBase). */
export function fileBase(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}
