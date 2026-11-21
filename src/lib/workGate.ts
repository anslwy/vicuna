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
 *  current engine (opencode, Claude Code, Codex, fx or Antigravity) plus a summary of all engines. */
export interface WorkEngineInfo {
  /** "opencode" | "claude" | "codex" | "fx" | "agy" | "pi" (older backends omit it: opencode) */
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
  return info?.label || (id === "claude" ? "Claude Code" : id === "codex" ? "Codex" : id === "fx" ? "fx" : id === "agy" ? "Antigravity" : id === "pi" ? "pi" : "OpenCode");
}

/** Installed Work engines (for fallback actions, lead delegation and quota tabs). */
export function workEngineChoices(info: WorkEngineInfo | null): EngineSummary[] {
  return (info?.engines ?? []).filter((e) => e.installed);
}

/** All Work engines shown in the harness picker menu (installed and uninstalled,
 *  so missing harnesses still appear disabled with "(not installed)"). */
export function workEngineMenuItems(info: WorkEngineInfo | null): EngineSummary[] {
  return info?.engines ?? [];
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
 *  (backend sends opencode `provider/model`, `claude/<model>`, `codex/<model>`,
 *  `fx/<provider/model>`, `agy/<model>` or `pi/<provider/model>`), else the
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
  // fx/agy ids that are not already engine-prefixed get the engine prefix to
  // keep them apart from the Codex engine's own `codex/<model>`
  if (engine.startsWith("plugin:")) return `${engine.slice(7).split("/")[1] || engine}/${p}`;
  return engine === "claude" || engine === "codex" || engine === "fx" || engine === "agy" || engine === "pi" ? `${engine}/${p}` : p;
}

/** Harnesses whose usage popup shows a subscription quota. OpenCode and pi
 *  only have local stats (or nothing), so they get no tab. */
export const USAGE_ENGINES = ["claude", "codex", "agy"] as const;

/** Installed quota harnesses for the usage popup tabs, plus the tab to open on:
 *  the active engine when it has a quota, else the first installed one
 *  (null when none is installed). */
export function usageTabs<T extends { id: string; installed: boolean }>(
  engines: T[],
  active: string,
): { tabs: T[]; initial: string | null } {
  const tabs = engines.filter((e) => e.installed && (USAGE_ENGINES as readonly string[]).includes(e.id));
  const initial = tabs.some((e) => e.id === active) ? active : (tabs[0]?.id ?? null);
  return { tabs, initial };
}

/** Text rows for the header meter icon's hover popup: one row per quota
 *  meter (prefixed with its group title when the report has several groups,
 *  e.g. agy's per-model quotas), falling back to the plain stats when the
 *  harness reported no meters. */
export function usageHoverRows(report: {
  groups: { title: string; meters: { label: string; valueText: string; resetText: string | null; percent: number; kind: string }[] }[];
  stats: { label: string; value: string }[];
}): { label: string; value: string; reset: string | null; tone: "ok" | "warn" | "low" | null; usedPct: number | null }[] {
  const multi = report.groups.filter((g) => g.meters.length).length > 1;
  const rows = report.groups.flatMap((g) =>
    g.meters.map((m) => {
      const pct = Math.max(0, Math.min(100, Number.isFinite(m.percent) ? m.percent : 0));
      return {
        label: multi && g.title ? `${g.title} · ${m.label}` : m.label,
        value: m.valueText,
        reset: m.resetText,
        tone: usageMeterTone(m.percent, m.kind) as "ok" | "warn" | "low" | null,
        usedPct: (m.kind === "remaining" ? 100 - pct : pct) as number | null,
      };
    }),
  );
  if (rows.length) return rows;
  return report.stats.map((s) => ({ label: s.label, value: s.value, reset: null, tone: null, usedPct: null }));
}

/** Fill (0–1) for the header meter icon: the most-used quota, since that is
 *  the one that runs out first. 0 when nothing was reported. */
export function usageGaugeFrac(rows: { usedPct: number | null }[]): number {
  const used = rows.map((r) => r.usedPct).filter((p): p is number => p !== null);
  return used.length ? Math.max(...used) / 100 : 0;
}

/** Visual tone for a usage/quota bar: green ("ok") when plenty of headroom
 *  remains, amber ("warn") mid-way, red ("low") near exhaustion. */
export function usageMeterTone(percent: number, kind: string): "ok" | "warn" | "low" {
  const pct = Math.max(0, Math.min(100, Number.isFinite(percent) ? percent : 0));
  if (kind === "remaining") {
    return pct >= 50 ? "ok" : pct >= 20 ? "warn" : "low";
  }
  return pct < 50 ? "ok" : pct < 80 ? "warn" : "low";
}
