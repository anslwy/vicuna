<script lang="ts">
  // Work thread header: workspace path & session ID popover, engine badge, harness
  // usage gauge with hover summary, and the context-usage ring.
  import EngineIcon from "$lib/EngineIcon.svelte";
  import { compactWorkspaceLabel, fullWorkspacePath, isNoWorkspacePath } from "$lib/sidebar";
  import { engineId, usageHoverRows, usageGaugeFrac, formatUsageAge, type WorkEngineInfo, type WorkUsageReport } from "$lib/work";
  import { baseEngineId } from "$lib/workGate";
  import { harnessUsageUrl } from "$lib/harnessUsageLinks";
  import { copyText, openExternal } from "$lib/chatDom";

  type UsageCacheEntry = { report: WorkUsageReport | null; error: string | null; at: number };

  let {
    sessionId,
    workspace,
    workPathCopied,
    onCopyPath,
    workEngineInfo,
    workEngineLabel,
    workEngineShort,
    workAccountLabel,
    sessionEngineMissing,
    workEngineList,
    workEngines,
    usageHoverEngine,
    usageStore,
    usageFetching,
    usageNow,
    onPrimeUsage,
    onOpenUsage,
    onRefreshUsage,
    workCtx,
    workSpent
  }: {
    sessionId?: string | null;
    workspace: string | null | undefined;
    workPathCopied: boolean;
    onCopyPath: (path: string | null) => void;
    workEngineInfo: WorkEngineInfo | null;
    workEngineLabel: string;
    workEngineShort: string;
    workAccountLabel: string | null;
    sessionEngineMissing: string | null;
    workEngineList: NonNullable<WorkEngineInfo["engines"]>;
    workEngines: { id: string; label: string }[];
    usageHoverEngine: string | null | undefined;
    usageStore: Record<string, UsageCacheEntry>;
    usageFetching: Record<string, boolean>;
    usageNow: number;
    onPrimeUsage: () => void;
    onOpenUsage: () => void;
    onRefreshUsage: (engine: string) => void;
    workCtx: { used: number; win: number; frac: number } | null;
    workSpent: number;
  } = $props();

  const wsLabel = $derived(compactWorkspaceLabel(workspace));
  const wsFull = $derived(fullWorkspacePath(workspace));
  const wsIsScratch = $derived(isNoWorkspacePath(workspace));
  const trimmedSessionId = $derived((sessionId ?? "").trim());
  const hasPopover = $derived(Boolean(wsFull || trimmedSessionId));

  let popoverPinned = $state(false);
  let pathWrapEl = $state<HTMLSpanElement | null>(null);
  let sessionIdCopied = $state(false);
  let sessionIdCopyTimer: ReturnType<typeof setTimeout> | null = null;

  function copySessionId(id: string) {
    if (!id) return;
    void copyText(id).then((ok) => {
      if (!ok) return;
      sessionIdCopied = true;
      if (sessionIdCopyTimer) clearTimeout(sessionIdCopyTimer);
      sessionIdCopyTimer = setTimeout(() => {
        sessionIdCopied = false;
        sessionIdCopyTimer = null;
      }, 1500);
    });
  }

  function togglePopover(e: MouseEvent) {
    popoverPinned = !popoverPinned;
    if (!popoverPinned) {
      (e.currentTarget as HTMLButtonElement | null)?.blur();
    }
  }

  $effect(() => {
    void sessionId;
    popoverPinned = false;
    sessionIdCopied = false;
    if (sessionIdCopyTimer) {
      clearTimeout(sessionIdCopyTimer);
      sessionIdCopyTimer = null;
    }
    return () => {
      if (sessionIdCopyTimer) {
        clearTimeout(sessionIdCopyTimer);
        sessionIdCopyTimer = null;
      }
    };
  });

  $effect(() => {
    if (!popoverPinned) return;
    const onPointerDown = (e: PointerEvent) => {
      if (pathWrapEl && !pathWrapEl.contains(e.target as Node)) {
        popoverPinned = false;
      }
    };
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") popoverPinned = false;
    };
    window.addEventListener("pointerdown", onPointerDown, true);
    window.addEventListener("keydown", onKeyDown, true);
    return () => {
      window.removeEventListener("pointerdown", onPointerDown, true);
      window.removeEventListener("keydown", onKeyDown, true);
    };
  });
</script>

<div class="work-banner" data-testid="work-banner">
  <span class="work-badge">Work</span>
  {#if hasPopover}
    <span class="ctx-wrap work-path-wrap" class:pinned={popoverPinned} bind:this={pathWrapEl}>
      <button
        type="button"
        class="work-path-btn"
        data-testid="work-path"
        aria-expanded={popoverPinned}
        aria-label={wsFull ? `Workspace path: ${wsFull}. Click to toggle details.` : "Session details. Click to toggle."}
        onclick={togglePopover}
      >
        <span class="work-path-label">{wsLabel}</span>
      </button>
      <span class="ctx-popup work-path-popup" data-testid="work-path-popup">
        {#if trimmedSessionId}
          <span class="work-path-popup-section" data-testid="work-session-id-section">
            <span class="work-path-popup-head">
              <span class="work-path-popup-title">Session ID</span>
              <button
                type="button"
                class="usage-hover-refresh work-path-copy-btn"
                class:copied={sessionIdCopied}
                data-testid="work-session-id-copy"
                onclick={(e) => { e.stopPropagation(); copySessionId(trimmedSessionId); }}
              >{sessionIdCopied ? "Copied" : "Copy"}</button>
            </span>
            <code class="work-path-full" data-testid="work-session-id">{trimmedSessionId}</code>
          </span>
        {/if}
        {#if wsFull}
          <span class="work-path-popup-section" data-testid="work-path-section">
            <span class="work-path-popup-head">
              <span class="work-path-popup-title">{wsIsScratch ? "Scratch directory" : "Workspace directory"}</span>
              <button
                type="button"
                class="usage-hover-refresh work-path-copy-btn"
                class:copied={workPathCopied}
                data-testid="work-path-copy"
                onclick={(e) => { e.stopPropagation(); onCopyPath(wsFull); }}
              >{workPathCopied ? "Copied" : "Copy"}</button>
            </span>
            <code class="work-path-full" data-testid="work-path-full">{wsFull}</code>
          </span>
        {/if}
      </span>
    </span>
  {:else}
    <span class="work-path work-path-empty" data-testid="work-path">{wsLabel}</span>
  {/if}
  <span class="work-badge engine-badge" data-testid="work-engine-badge" data-engine={engineId(workEngineInfo)} title={workEngineInfo?.installed ? `${workEngineLabel} ${workEngineInfo.version ?? ""} — ${workEngineInfo.path ?? ""}` : (workEngineInfo?.missing ?? `${workEngineLabel} not found`)}>{#if workEngineInfo && !sessionEngineMissing}<EngineIcon id={engineId(workEngineInfo)} {workEngineList} />{/if}{sessionEngineMissing ? `no ${sessionEngineMissing}` : workEngineInfo ? (workEngineInfo.installed ? (workAccountLabel ? workEngineShort : engineId(workEngineInfo)) : `no ${engineId(workEngineInfo)}`) : "…"}</span>
  <span class="hdr-meters">
  {#if usageHoverEngine}
    {@const hov = usageStore[usageHoverEngine]}
    {@const hovLoading = !!usageFetching[usageHoverEngine]}
    {@const hovLabel = workEngines.find((e) => e.id === usageHoverEngine)?.label ?? usageHoverEngine}
    {@const hovRows = hov?.report ? usageHoverRows(hov.report) : []}
    {@const gauge = usageGaugeFrac(hovRows)}
    <span class="ctx-wrap usage-wrap">
      <!-- same 18px / r=7 / 2.5 stroke as the context ring, but a 270° gauge arc (gap at the bottom) filled by the most-used quota -->
      <button class="usage-meter-btn" data-testid="usage-meter-btn" aria-label={`${hovLabel} usage${hovRows.length ? ` ${Math.round(gauge * 100)}%` : ""}`} onmouseenter={() => onPrimeUsage()} onfocus={() => onPrimeUsage()} onclick={() => onOpenUsage()}>
        <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
          <circle cx="9" cy="9" r="7" fill="none" stroke="var(--border-2)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray="33 44" transform="rotate(135 9 9)" />
          {#if gauge > 0}
            <circle cx="9" cy="9" r="7" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray={`${(gauge * 33).toFixed(1)} 44`} transform="rotate(135 9 9)" />
          {/if}
        </svg>
      </button>
      <span class="ctx-popup usage-hover" data-testid="usage-hover">
        <span class="usage-hover-head">
          <span class="usage-hover-title">{hovLabel}{hov?.report?.plan ? ` · ${hov.report.plan}` : ""}</span>
          <button
            class="usage-hover-refresh"
            data-testid="usage-hover-refresh"
            disabled={hovLoading}
            onclick={(e) => { e.stopPropagation(); onRefreshUsage(usageHoverEngine); }}
            title="Refresh usage"
          >{hovLoading ? "Refreshing…" : "Refresh"}</button>
        </span>
        {#if hov?.report}
          {#if hov.error}
            <span class="usage-hover-note usage-tone-low">{hov.error}</span>
          {/if}
          {#each hovRows as r}
            <span class="usage-hover-row">
              <span class="usage-hover-line"><span class="usage-hover-label" title={r.label}>{r.label}</span><span class="usage-hover-val {r.tone ? `usage-tone-${r.tone}` : ''}">{r.value}</span></span>
              {#if r.usedPct !== null}
                <span class="usage-hover-bar"><span class="usage-hover-fill {r.tone ?? ''}" style="width: {r.usedPct}%"></span></span>
              {/if}
              {#if r.reset}<span class="usage-hover-reset">{r.reset}</span>{/if}
            </span>
          {:else}
            <span class="usage-hover-note">{hov.report.note ?? "No usage reported"}</span>
          {/each}
        {:else if hovLoading}
          <span class="usage-hover-note">Fetching usage…</span>
        {:else if hov?.error}
          <span class="usage-hover-note">{hov.error}</span>
        {:else}
          <span class="usage-hover-note">Hover to load usage</span>
        {/if}
        <span class="usage-hover-hint">
          <span data-testid="usage-hover-updated">
            {#if hov?.report}
              Updated {formatUsageAge(hov.at, usageNow)}{hovLoading ? " · updating…" : ""}
            {:else if hovLoading}
              Updating…
            {:else}
              Not updated yet
            {/if}
          </span>
          <span>Click for details</span>
        </span>
        {#if harnessUsageUrl(baseEngineId(usageHoverEngine))}
          <button class="usage-hover-refresh usage-provider-link" data-testid="usage-hover-provider-link" onclick={(e) => { e.stopPropagation(); void openExternal(harnessUsageUrl(baseEngineId(usageHoverEngine))!); }}>Open usage page</button>
        {/if}
      </span>
    </span>
  {/if}
  {#if workCtx}
    {@const pct = Math.round(workCtx.frac * 100)}
    <span class="ctx-wrap" data-testid="ctx-ring" role="img" aria-label={`Context usage ${pct}%, ${workCtx.used.toLocaleString()} tokens`}>
      <svg width="18" height="18" viewBox="0 0 18 18" aria-hidden="true">
        <circle cx="9" cy="9" r="7" fill="none" stroke="var(--border-2)" stroke-width="2.5" />
        <circle cx="9" cy="9" r="7" fill="none" stroke="var(--accent)" stroke-width="2.5" stroke-linecap="round" stroke-dasharray={`${(workCtx.frac * 43.98).toFixed(1)} 44`} transform="rotate(-90 9 9)" />
      </svg>
      <span class="ctx-popup" data-testid="ctx-popup">
        <span class="ctx-row"><span>Usage</span><span>{pct}%</span></span>
        <span class="ctx-row"><span>Tokens</span><span>{workCtx.used.toLocaleString()}</span></span>
        <span class="ctx-row"><span>Spent</span><span data-testid="ctx-spent">${workSpent.toFixed(2)}</span></span>
      </span>
    </span>
  {/if}
  </span>
</div>

<style>
  .work-badge { flex-shrink: 0; font-size: 11px; font-weight: 700; color: var(--accent-fg); background: var(--accent); border-radius: 999px; padding: 1px 8px; margin-left: 8px; }
  .work-badge.engine-badge { display: inline-flex; align-items: center; gap: 4px; background: transparent; color: var(--muted-2); border: 1px solid var(--border-2); font-weight: 600; cursor: default; }
  .work-banner { position: relative; z-index: 20; display: flex; align-items: center; gap: 8px; max-width: 860px; width: 100%; margin: 0 auto; padding: 14px 54px 6px 24px; box-sizing: border-box; font-size: 12.5px; color: var(--muted-2); }
  .work-banner .work-badge { margin-left: 0; }
  .hdr-meters { margin-left: auto; display: inline-flex; align-items: center; gap: 8px; }
  .ctx-wrap { position: relative; display: inline-flex; cursor: default; }
  .usage-meter-btn { display: inline-flex; align-items: center; justify-content: center; padding: 0; border: 0; background: none; cursor: pointer; }
  .usage-meter-btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 50%; }
  .usage-hover { width: 320px; padding: 12px 16px; }
  .usage-hover::before { content: ""; position: absolute; left: 0; right: 0; top: -10px; height: 10px; }
  .usage-hover-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-size: 13px; font-weight: 600; color: var(--text); padding-bottom: 8px; }
  .usage-hover-title { min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .usage-hover-refresh { flex-shrink: 0; border: 1px solid var(--border-strong); background: transparent; color: var(--text-dim); border-radius: 6px; padding: 2px 7px; font: inherit; font-size: 11px; font-weight: 500; cursor: pointer; }
  .usage-hover-refresh:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .usage-hover-refresh:disabled { opacity: .55; cursor: default; }
  .usage-hover-row { display: block; padding: 6px 0; }
  .usage-hover-line { display: flex; align-items: baseline; justify-content: space-between; gap: 12px; font-size: 13px; }
  .usage-hover-label { color: var(--muted-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0; }
  .usage-hover-val { white-space: nowrap; font-variant-numeric: tabular-nums; flex-shrink: 0; color: var(--text); }
  .usage-hover-bar { display: block; height: 4px; margin-top: 5px; border-radius: 2px; background: var(--border-2); overflow: hidden; }
  .usage-hover-fill { display: block; height: 100%; border-radius: 2px; background: var(--accent); }
  .usage-hover-fill.ok { background: #16a34a; }
  .usage-hover-fill.warn { background: #b45309; }
  .usage-hover-fill.low { background: var(--danger); }
  .usage-hover-reset { display: block; font-size: 11px; color: var(--muted-2); margin-top: 4px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .usage-hover-note { display: block; font-size: 12px; color: var(--muted-2); padding: 2px 0; max-width: 280px; }
  .usage-hover-hint { display: flex; align-items: center; justify-content: space-between; gap: 8px; font-size: 11px; color: var(--muted-2); border-top: 1px solid var(--border-2); margin-top: 6px; padding-top: 6px; }
  .usage-tone-ok { color: #15803d; }
  .usage-tone-warn { color: #b45309; }
  .usage-tone-low { color: var(--danger); }
  .ctx-popup { display: none; position: absolute; right: 0; top: calc(100% + 8px); background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; box-shadow: 0 12px 40px rgba(0,0,0,.14); padding: 10px 14px; z-index: 30; min-width: 170px; }
  .ctx-wrap:hover .ctx-popup, .ctx-wrap:focus-within .ctx-popup { display: block; }
  .ctx-row { display: flex; justify-content: space-between; gap: 24px; font-size: 13px; color: var(--text); padding: 2px 0; font-variant-numeric: tabular-nums; }
  .ctx-row span:first-child { color: var(--muted-2); }
  .work-path { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .work-path-empty { font-size: 12px; color: var(--muted-2); }
  .work-path-wrap { min-width: 0; max-width: min(260px, 50cqi); }
  .work-path-btn { display: inline-flex; align-items: center; gap: 5px; max-width: 100%; min-width: 0; padding: 2px 7px; border: 1px solid transparent; border-radius: 6px; background: transparent; color: var(--muted-2); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12px; line-height: 1.4; cursor: pointer; transition: background .12s, color .12s, border-color .12s; }
  .work-path-btn:hover, .work-path-btn:focus-visible, .work-path-wrap.pinned .work-path-btn { background: var(--hover); color: var(--text); border-color: var(--border); outline: none; }
  .work-path-wrap.pinned .work-path-popup { display: block; }
  .work-path-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .work-path-popup { left: 0; right: auto; width: max-content; min-width: 200px; max-width: min(440px, calc(100vw - 48px), calc(100cqi - 110px)); padding: 10px 12px; }
  .work-path-popup::before { content: ""; position: absolute; left: 0; right: 0; top: -10px; height: 10px; }
  .work-path-popup-section { display: block; }
  .work-path-popup-section + .work-path-popup-section { margin-top: 10px; padding-top: 10px; border-top: 1px solid var(--border-2); }
  .work-path-popup-head { display: flex; align-items: center; justify-content: space-between; gap: 10px; margin-bottom: 6px; }
  .work-path-popup-title { font-size: 11.5px; font-weight: 600; color: var(--muted-2); }
  .work-path-copy-btn.copied { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, transparent); }
  .work-path-full { display: block; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 11.5px; line-height: 1.45; color: var(--text); background: var(--bg); border: 1px solid var(--border); border-radius: 6px; padding: 6px 8px; word-break: break-all; overflow-wrap: anywhere; user-select: text; cursor: text; }
  @container (max-width: 920px) {
    .work-banner { padding-left: 60px; }
  }
  @media (max-width: 700px) {
    .work-banner { padding-left: 60px; }
  }
</style>
