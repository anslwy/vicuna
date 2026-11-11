// Pure Work-mode helpers (no imports, so `npm test` can load them with
// Node's built-in test runner): the opencode send gate and the model-picker
// rows. Re-exported from ./work.

/** opencode CLI detection (re-run on every call: Refresh, window focus). */
export interface WorkEngineInfo {
  installed: boolean;
  path: string | null;
  version: string | null;
  installUrl: string;
  /** model saved from Vicuna's picker (null = opencode's own default) */
  model: string | null;
  configPath: string | null;
}

/** Send gate for Work mode: no opencode, no turn (there is no fallback).
 *  `info === null` means detection has not answered yet (or failed). */
export function workSendGate(info: WorkEngineInfo | null): { blocked: boolean; reason: string | null } {
  if (info === null) return { blocked: true, reason: "Checking for the opencode CLI…" };
  if (!info.installed) return { blocked: true, reason: `opencode CLI not found, install it: ${info.installUrl}` };
  return { blocked: false, reason: null };
}

export interface OpencodeModel {
  /** `provider/model`, exactly as opencode accepts it */
  id: string;
  provider: string;
}

export interface OpencodeModels {
  models: OpencodeModel[];
  current: string | null;
}

/** Picker rows: opencode's order, the saved pick kept visible even when its
 *  provider vanished, filtered by a case-insensitive query. */
export function opencodeModelChoices(list: OpencodeModels | null, selected: string | null, query: string): OpencodeModel[] {
  const q = query.trim().toLowerCase();
  const rows = [...(list?.models ?? [])];
  if (selected && !rows.some((m) => m.id === selected)) {
    rows.unshift({ id: selected, provider: selected.split("/")[0] ?? "" });
  }
  return q ? rows.filter((m) => m.id.toLowerCase().includes(q)) : rows;
}


/** Label for a Work-mode reply: the model opencode reported for the turn,
 *  else the Work picker's choice, else "opencode" (its own default). Never
 *  the Chat-mode model. */
export function workReplyModel(reported: string | null | undefined, picked: string | null | undefined): string {
  const r = reported?.trim();
  if (r) return r;
  const p = picked?.trim();
  return p ? p : "opencode";
}
