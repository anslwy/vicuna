// Pure Work-mode helpers (no imports, so `npm test` can load them with
// Node's built-in test runner): the engine send gate, engine picker and the
// model-picker rows. Re-exported from ./work.

/** One Work engine as detection reports it. */
export interface EngineSummary {
  id: string;
  label: string;
  installed: boolean;
  version: string | null;
  missing: string | null;
}

/** Work engine detection (re-run on every call: Refresh, window focus): the
 *  current engine (opencode, Claude Code or Codex) plus a summary of all engines. */
export interface WorkEngineInfo {
  /** "opencode" | "claude" | "codex" | "fx" (older backends omit it: opencode) */
  id?: string;
  label?: string;
  /** why the engine can't run, with the install hint */
  missing?: string | null;
  engines?: EngineSummary[];
  installed: boolean;
  path: string | null;
  version: string | null;
  installUrl: string;
  /** model saved from Vicuna's picker (null = the engine's own default) */
  model: string | null;
  configPath: string | null;
  /** other installs detection found but did not pick ("path (version)") */
  others?: string[];
  /** non-blocking warning, e.g. older than the tested minimum version */
  warning?: string | null;
  /** tool approval: true = ask in the chat, false = auto-approve */
  askPermission?: boolean;
}

/** Send gate for Work mode: no opencode, no turn (there is no fallback).
 *  `info === null` means detection has not answered yet (or failed). */
export function workSendGate(info: WorkEngineInfo | null): { blocked: boolean; reason: string | null } {
  if (info === null) return { blocked: true, reason: "Checking for the Work engine…" };
  if (!info.installed) {
    return { blocked: true, reason: info.missing || `${engineLabel(info)} CLI not found, install it: ${info.installUrl}` };
  }
  return { blocked: false, reason: null };
}

export function engineId(info: WorkEngineInfo | null): string {
  return info?.id || "opencode";
}

export function engineLabel(info: WorkEngineInfo | null): string {
  const id = engineId(info);
  return info?.label || (id === "claude" ? "Claude Code" : id === "codex" ? "Codex" : id === "fx" ? "fx" : "OpenCode");
}

/** Engines the picker offers: the installed ones. The picker only shows
 *  when there is a real choice (two or more installed). */
export function workEngineChoices(info: WorkEngineInfo | null): EngineSummary[] {
  return (info?.engines ?? []).filter((e) => e.installed);
}

export interface OpencodeModel {
  /** the engine's model id: opencode `provider/model`, Claude e.g. "haiku", Codex e.g. "gpt-6-luna" */
  id: string;
  provider: string;
  /** display name when the engine gives one (Claude, Codex) */
  name?: string;
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


/** Label for a Work-mode reply: the model the engine reported for the turn
 *  (backend sends opencode `provider/model`, `claude/<model>` or
 *  `codex/<model>`), else the
 *  Work picker's choice in the same shape, else the engine id. Never the
 *  Chat-mode model. */
export function workReplyModel(
  reported: string | null | undefined,
  picked: string | null | undefined,
  engine: string = "opencode"
): string {
  const r = reported?.trim();
  if (r) return r;
  const p = picked?.trim();
  if (!p) return engine;
  // fx ids are `provider/model` already; the engine prefix keeps them apart
  // from the Codex engine's own `codex/<model>`
  return engine === "claude" || engine === "codex" || engine === "fx" ? `${engine}/${p}` : p;
}
