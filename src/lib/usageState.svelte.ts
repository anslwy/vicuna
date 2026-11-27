// Harness quota (Work header meter + Usage modal): cached reports per engine,
// lazy hover fetch, background polling and the modal's tab state.
// Created once by ChatApp.
import { type WorkUsageReport, parseUsageCache, isUsageStale, USAGE_POLL_INTERVAL_MS, fetchWorkUsage, usagePollOrder, engineId, usageTabs } from "./work";
import { USAGE_CACHE_KEY } from "./chatAppData";
import type { WorkEngineInfo, workEngineChoices } from "./work";

export interface UsageStateDeps {
  readonly workEngines: ReturnType<typeof workEngineChoices>;
  readonly workEngineInfo: WorkEngineInfo | null;
  readonly usageHoverEngine: string | null | undefined;
  readonly refreshWorkEngine: () => Promise<void>;
}

export function createUsageState(deps: UsageStateDeps) {
  type UsageCacheEntry = { report: WorkUsageReport | null; error: string | null; at: number };
  let usageOpen = $state(false);
  let usageEngine = $state("agy");
  let usageNoEngineError = $state<string | null>(null);
  /** Unified per-engine usage snapshot shared by the header badge, hover popover, and modal. */
  let usageStore = $state<Record<string, UsageCacheEntry>>({});
  /** Per-engine in-flight flags so hover, background poll, and modal refreshes deduplicate. */
  let usageFetching = $state<Record<string, boolean>>({});
  /** Ticks every 30s so "Updated Xm ago" labels stay fresh without extra fetches. */
  let usageNow = $state(Date.now());
  const usageInFlight = new Map<string, Promise<void>>();
  let usagePollRunning = false;
  const usageEntry = $derived(usageStore[usageEngine] ?? null);
  const usageReport = $derived(usageEntry?.report ?? null);
  const usageError = $derived(usageNoEngineError ?? usageEntry?.error ?? null);
  const usageUpdatedAt = $derived(usageEntry?.at ?? null);
  const usageLoading = $derived(!!usageFetching[usageEngine]);
  function persistUsageStore() {
    try {
      const toSave: Record<string, { report: WorkUsageReport; at: number }> = {};
      for (const [k, v] of Object.entries(usageStore)) {
        if (v.report) toSave[k] = { report: v.report, at: v.at };
      }
      localStorage.setItem(USAGE_CACHE_KEY, JSON.stringify(toSave));
    } catch {}
  }
  function hydrateUsageStore() {
    try {
      const loaded = parseUsageCache<WorkUsageReport>(localStorage.getItem(USAGE_CACHE_KEY));
      if (Object.keys(loaded).length) usageStore = loaded;
    } catch {}
  }
  function fetchUsageEngine(engine: string, opts?: { force?: boolean; background?: boolean }): Promise<void> {
    if (!engine) return Promise.resolve();
    const force = opts?.force ?? false;
    const background = opts?.background ?? false;
    const existing = usageStore[engine];
    if (!force && existing?.report && !isUsageStale(existing.at, Date.now(), USAGE_POLL_INTERVAL_MS)) {
      return Promise.resolve();
    }
    const inflight = usageInFlight.get(engine);
    if (inflight) return inflight;

    usageFetching = { ...usageFetching, [engine]: true };
    const p = (async () => {
      try {
        const res = await fetchWorkUsage(engine);
        const now = Date.now();
        const prev = usageStore[engine];
        if (!res.error) {
          usageStore = { ...usageStore, [engine]: { report: res, error: null, at: now } };
          persistUsageStore();
        } else if (background && prev?.report) {
          // keep last good snapshot on transient background poll failures
        } else {
          usageStore = {
            ...usageStore,
            [engine]: { report: prev?.report ?? null, error: res.error, at: prev?.report ? prev.at : now },
          };
        }
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        const prev = usageStore[engine];
        if (!(background && prev?.report)) {
          usageStore = {
            ...usageStore,
            [engine]: { report: prev?.report ?? null, error: msg, at: prev?.report ? prev.at : Date.now() },
          };
        }
      } finally {
        usageInFlight.delete(engine);
        const { [engine]: _, ...rest } = usageFetching;
        usageFetching = rest;
        usageNow = Date.now();
      }
    })();
    usageInFlight.set(engine, p);
    return p;
  }
  async function pollUsageBackground() {
    if (usagePollRunning) return;
    if (typeof document !== "undefined" && document.visibilityState === "hidden") return;
    const order = usagePollOrder(deps.workEngines, engineId(deps.workEngineInfo));
    if (!order.length) return;
    usagePollRunning = true;
    try {
      for (const eng of order) {
        if (typeof document !== "undefined" && document.visibilityState === "hidden") break;
        await fetchUsageEngine(eng, { background: true });
      }
    } finally {
      usagePollRunning = false;
    }
  }
  function primeUsageHover() {
    usageNow = Date.now();
    const engine = deps.usageHoverEngine;
    if (!engine) return;
    void fetchUsageEngine(engine, { background: true });
  }
  function closeUsageModal() {
    usageOpen = false;
  }
  function selectUsageTab(engine: string) {
    usageNoEngineError = null;
    usageEngine = engine;
    usageNow = Date.now();
    void fetchUsageEngine(engine, { background: true });
  }
  async function openUsageModal(engine?: string) {
    usageOpen = true;
    usageNoEngineError = null;
    usageNow = Date.now();
    if (!engine && !deps.workEngineInfo) await deps.refreshWorkEngine();
    if (!usageOpen) return;
    const target = engine ?? usageTabs(deps.workEngines, engineId(deps.workEngineInfo)).initial;
    if (!target) {
      usageNoEngineError = "Usage is available for Claude Code, Codex and Antigravity; none of them is installed.";
      return;
    }
    usageEngine = target;
    void fetchUsageEngine(target, { background: true });
  }
  $effect(() => {
    const key = usagePollOrder(deps.workEngines, engineId(deps.workEngineInfo)).join(",");
    if (!key) return;
    void pollUsageBackground();
  });

  return {
    get usageOpen() { return usageOpen; },
    set usageOpen(v) { usageOpen = v; },
    get usageEngine() { return usageEngine; },
    set usageEngine(v) { usageEngine = v; },
    get usageNoEngineError() { return usageNoEngineError; },
    set usageNoEngineError(v) { usageNoEngineError = v; },
    get usageStore() { return usageStore; },
    set usageStore(v) { usageStore = v; },
    get usageFetching() { return usageFetching; },
    set usageFetching(v) { usageFetching = v; },
    get usageNow() { return usageNow; },
    set usageNow(v) { usageNow = v; },
    get usageInFlight() { return usageInFlight; },
    get usagePollRunning() { return usagePollRunning; },
    set usagePollRunning(v) { usagePollRunning = v; },
    get usageEntry() { return usageEntry; },
    get usageReport() { return usageReport; },
    get usageError() { return usageError; },
    get usageUpdatedAt() { return usageUpdatedAt; },
    get usageLoading() { return usageLoading; },
    persistUsageStore,
    hydrateUsageStore,
    fetchUsageEngine,
    pollUsageBackground,
    primeUsageHover,
    closeUsageModal,
    selectUsageTab,
    openUsageModal
  };
}

export type UsageState = ReturnType<typeof createUsageState>;
