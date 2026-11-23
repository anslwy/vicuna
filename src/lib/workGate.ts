export interface WorkAccountInfo {
  id: string;
  /** Engine ID including the account. */
  engine: string;
  label: string;
  /** Profile directory on the machine running the turns. */
  home: string;
  signedIn: boolean;
  /** CLI command to sign in on that machine. */
  signInCommand: string;
}

// Pure Work-mode helpers (no imports, so `npm test` can load them with
// Node's built-in test runner): the engine send gate, engine picker and the
// model-picker rows. Re-exported from ./work.

/** One Work engine as detection reports it. */
export interface EngineSummary {
  id: string;
  /** Validated SVG data URI, supplied by plugin harnesses. */
  icon?: string | null;
  label: string;
  installed: boolean;
  version: string | null;
  missing: string | null;
  /** The Usage view has a tab for it (quota harness or plugin usage map). */
  usage?: boolean;
  /** Set on an extra account row. */
  account?: WorkAccountInfo | null;
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
  if (engine.startsWith("plugin:")) return `${isAccountEngine(engine) ? engine : engine.slice(7).split("/")[1] || engine}/${p}`;
  if (engine.startsWith("claude@") || engine.startsWith("codex@")) return `${engine}/${p}`;
  return engine === "claude" || engine === "codex" || engine === "fx" || engine === "agy" || engine === "pi" ? `${engine}/${p}` : p;
}

/** Harnesses whose usage popup shows a subscription quota. OpenCode and pi
 *  only have local stats (or nothing), so they get no tab. */
export const USAGE_ENGINES = ["claude", "codex", "agy"] as const;

/** Background polling interval & staleness window for usage reports (5 min). */
export const USAGE_POLL_INTERVAL_MS = 5 * 60_000;

/** Discard persisted usage snapshots older than 24 hours on startup. */
export const USAGE_CACHE_MAX_AGE_MS = 24 * 60 * 60_000;

/** Human-readable relative age for the usage badge & modal ("just now", "3m ago", "2h ago", "1d ago"). */
export function formatUsageAge(atMs: number, nowMs: number = Date.now()): string {
  const diff = Math.max(0, nowMs - atMs);
  const mins = Math.floor(diff / 60_000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/** Whether a cached usage timestamp is missing or older than `maxAgeMs`. */
export function isUsageStale(
  atMs: number | null | undefined,
  nowMs: number = Date.now(),
  maxAgeMs: number = USAGE_POLL_INTERVAL_MS,
): boolean {
  return typeof atMs !== "number" || !Number.isFinite(atMs) || nowMs - atMs >= maxAgeMs;
}

/** Order in which background polling refreshes installed quota harnesses:
 *  the active engine first (so the header badge updates fastest), then the rest. */
export function usagePollOrder<T extends { id: string; installed: boolean; usage?: boolean }>(
  engines: T[],
  active: string,
): string[] {
  const { tabs, initial } = usageTabs(engines, active);
  if (!initial) return [];
  return [initial, ...tabs.map((t) => t.id).filter((id) => id !== initial)];
}

/** Hydrate persisted per-engine usage snapshots from localStorage JSON,
 *  dropping malformed entries, errors, and snapshots older than `maxAgeMs`. */
export function parseUsageCache<T>(
  raw: string | null,
  nowMs: number = Date.now(),
  maxAgeMs: number = USAGE_CACHE_MAX_AGE_MS,
): Record<string, { report: T | null; error: string | null; at: number }> {
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const out: Record<string, { report: T | null; error: string | null; at: number }> = {};
    for (const [k, v] of Object.entries(parsed as Record<string, unknown>)) {
      if (!v || typeof v !== "object") continue;
      const rec = v as { report?: unknown; at?: unknown };
      if (!rec.report || typeof rec.report !== "object") continue;
      if (typeof rec.at !== "number" || !Number.isFinite(rec.at)) continue;
      if (nowMs - rec.at > maxAgeMs || rec.at > nowMs + 60_000) continue;
      out[k] = { report: rec.report as T, error: null, at: rec.at };
    }
    return out;
  } catch {
    return {};
  }
}

/** Installed quota harnesses for the usage popup tabs, plus the tab to open on:
 *  the active engine when it has a quota, else the first installed one
 *  (null when none is installed). */
export function usageTabs<T extends { id: string; installed: boolean; usage?: boolean }>(
  engines: T[],
  active: string,
): { tabs: T[]; initial: string | null } {
  const tabs = engines.filter((e) => e.installed && (e.usage || (USAGE_ENGINES as readonly string[]).includes(e.id)));
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

const ACCOUNT_HARNESSES = ["claude", "codex", "agy"] as const;

/** `claude@work` → `claude`; other harness IDs pass through. */
export function baseEngineId(id: string): string {
  if (id.startsWith("plugin:")) return id.split("@", 1)[0];
  return ACCOUNT_HARNESSES.find((base) => id.startsWith(`${base}@`)) ?? id;
}

/** A Claude, Codex or plugin account handoff replays the transcript in a fresh native
 *  session, including when the user returns to an earlier account. */
export function changesCliAccount(engine: string, previousModel: string | null | undefined): boolean {
  if (!previousModel) return false;
  const previous = previousModel.split("/", previousModel.startsWith("plugin:") ? 2 : 1).join("/");
  const isCliAccount = (id: string) =>
    id.startsWith("claude@") || id.startsWith("codex@") || (id.startsWith("plugin:") && isAccountEngine(id));
  return engine !== previous && (isCliAccount(engine) || isCliAccount(previous));
}

export function isAccountEngine(id: string): boolean {
  return baseEngineId(id) !== id;
}

/** Usage view: one tab per harness, plus account chips for the selected
 *  harness when it has extra accounts (`Default` = the base engine). */
export function usageAccountView<T extends { id: string; label: string; account?: WorkAccountInfo | null }>(
  tabs: T[],
  selected: string,
): { harnessTabs: T[]; selectedBase: string; chips: { id: string; label: string }[] } {
  const harnessTabs = tabs.filter((t) => !isAccountEngine(t.id));
  const selectedBase = baseEngineId(selected);
  const accounts = tabs.filter((t) => isAccountEngine(t.id) && baseEngineId(t.id) === selectedBase);
  const chips = accounts.length
    ? [{ id: selectedBase, label: "Default" }, ...accounts.map((a) => ({ id: a.id, label: a.account?.label ?? a.id.slice(selectedBase.length + 1) }))]
    : [];
  return { harnessTabs, selectedBase, chips };
}
