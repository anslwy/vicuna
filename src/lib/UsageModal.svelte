<script lang="ts">
  import EngineIcon from "$lib/EngineIcon.svelte";
  import {
    usageTabs,
    usageMeterTone,
    formatUsageAge,
    type WorkEngineInfo,
    type WorkUsageReport
  } from "$lib/work";

  let {
    workEngines,
    workEngineList,
    usageEngine,
    usageReport,
    usageUpdatedAt,
    usageNow,
    usageLoading,
    usageError,
    onClose,
    onSelectTab,
    onRefresh
  }: {
    workEngines: NonNullable<WorkEngineInfo["engines"]>;
    workEngineList: NonNullable<WorkEngineInfo["engines"]>;
    usageEngine: string;
    usageReport: WorkUsageReport | null;
    usageUpdatedAt: number | null;
    usageNow: number;
    usageLoading: boolean;
    usageError: string | null;
    onClose: () => void;
    onSelectTab: (engine: string) => void;
    onRefresh: (engine: string) => void;
  } = $props();

  let installedEngines = $derived(usageTabs(workEngines, usageEngine).tabs);
</script>

<button class="confirm-backdrop sched-dim" data-testid="usage-backdrop" aria-label="Close" onclick={onClose} tabindex="-1"></button>
<div class="confirm-card sched-card usage-card" data-testid="usage-dialog" role="dialog" aria-modal="true" aria-label="Harness usage">
  <button class="modal-x" data-testid="usage-close" aria-label="Close" onclick={onClose}>×</button>
  <div class="usage-head">
    <div class="usage-title-row">
      <p class="confirm-text sched-title">Usage</p>
      {#if usageReport?.plan}
        <span class="usage-plan">{usageReport.plan}</span>
      {/if}
      <span class="usage-title-right">
        {#if usageUpdatedAt && usageReport}
          <span class="usage-updated" data-testid="usage-updated">Updated {formatUsageAge(usageUpdatedAt, usageNow)}</span>
        {/if}
        <button class="usage-refresh" data-testid="usage-refresh" disabled={usageLoading} onclick={() => onRefresh(usageEngine)} title="Refresh usage">
          {usageLoading ? "Refreshing…" : "Refresh"}
        </button>
      </span>
    </div>
    {#if installedEngines.length > 1}
      <div class="usage-tabs" role="tablist" aria-label="Work harnesses">
        {#each installedEngines as eng (eng.id)}
          <button
            class="usage-tab {usageEngine === eng.id ? 'sel' : ''}"
            data-testid="usage-tab-{eng.id}"
            role="tab"
            aria-selected={usageEngine === eng.id}
            onclick={() => onSelectTab(eng.id)}
          ><EngineIcon id={eng.id} {workEngineList} />{eng.label}</button>
        {/each}
      </div>
    {/if}
  </div>
  <div class="usage-body">
    {#if usageReport}
      {#if usageReport.account}
        <p class="usage-account" data-testid="usage-account">{usageReport.account}</p>
      {/if}
      {#if usageError}
        <p class="sched-warn" data-testid="usage-error">{usageError}</p>
      {/if}
      {#if usageReport.groups.length}
        <div class="usage-groups">
          {#each usageReport.groups as grp}
            <div class="usage-group">
              <div class="usage-group-title">{grp.title}</div>
              {#if grp.subtitle}
                <div class="usage-group-sub">{grp.subtitle}</div>
              {/if}
              <div class="usage-meters">
                {#each grp.meters as m}
                  {@const tone = usageMeterTone(m.percent, m.kind)}
                  <div class="usage-meter" data-testid="usage-meter">
                    <div class="usage-meter-top">
                      <span class="usage-meter-label">{m.label}</span>
                      <span class="usage-meter-val">{m.valueText}</span>
                    </div>
                    <div class="usage-bar">
                      <div class="usage-fill {tone}" style="width: {Math.max(0, Math.min(100, m.percent))}%"></div>
                    </div>
                    {#if m.resetText}
                      <div class="usage-meter-reset {tone}">{m.resetText}</div>
                    {/if}
                  </div>
                {/each}
              </div>
            </div>
          {/each}
        </div>
      {/if}
      {#if usageReport.stats.length}
        <div class="usage-stats" data-testid="usage-stats">
          {#each usageReport.stats as st}
            <div class="usage-stat">
              <span class="usage-stat-label">{st.label}</span>
              <span class="usage-stat-val">{st.value}</span>
            </div>
          {/each}
        </div>
      {/if}
      {#if usageReport.note}
        <p class="usage-note" data-testid="usage-note">{usageReport.note}</p>
      {/if}
      {#if !usageReport.groups.length && !usageReport.stats.length && !usageReport.note && !usageReport.raw}
        <p class="sched-empty">No usage data returned for {usageReport.engineLabel}.</p>
      {/if}
      {#if usageReport.raw}
        <details class="usage-raw" open={!usageReport.groups.length && !usageReport.stats.length}>
          <summary>Raw output</summary>
          <pre>{usageReport.raw}</pre>
        </details>
      {/if}
    {:else if usageLoading}
      <p class="usage-loading" data-testid="usage-loading">Fetching usage from {installedEngines.find((e) => e.id === usageEngine)?.label ?? usageEngine}…</p>
    {:else if usageError}
      <p class="sched-warn" data-testid="usage-error">{usageError}</p>
    {/if}
  </div>
</div>

<style>
  .confirm-backdrop { position: fixed; inset: 0; background: transparent; border: none; cursor: default; z-index: 50; }
  .confirm-backdrop.sched-dim { background: rgba(0,0,0,.35); }
  .confirm-card { position: fixed; left: 50%; top: 38%; transform: translate(-50%, -50%); z-index: 51; background: var(--card); border-radius: 24px; box-shadow: 0 24px 80px rgba(0,0,0,.18); padding: 26px 26px 22px 26px; width: 340px; max-width: calc(100vw - 48px); box-sizing: border-box; }
  .confirm-card.sched-card { width: min(560px, calc(100vw - 48px)); max-height: 76vh; top: 50%; display: flex; flex-direction: column; }
  .confirm-card.usage-card { width: min(600px, calc(100vw - 48px)); height: min(640px, 80vh); max-height: 80vh; }
  .modal-x { position: absolute; top: 10px; right: 10px; width: 32px; height: 32px; border: none; background: transparent; border-radius: 50%; cursor: pointer; color: var(--icon-dim); font-size: 20px; line-height: 1; display: flex; align-items: center; justify-content: center; }
  .modal-x:hover { background: var(--hover); color: var(--text); }
  .confirm-text { font-size: 17px; font-weight: 700; line-height: 1.4; margin: 0 0 20px 0; color: var(--text); }
  .confirm-text.sched-title { margin-bottom: 6px; }
  .sched-warn { font-size: 13px; line-height: 1.45; color: var(--danger); margin: 0 0 12px 0; }
  .sched-empty { font-size: 13.5px; line-height: 1.5; color: var(--text-dim); margin: 6px 0 4px 0; }
  .usage-head { flex-shrink: 0; display: flex; flex-direction: column; gap: 6px; padding-bottom: 10px; border-bottom: 1px solid var(--border); margin-bottom: 12px; }
  .usage-title-row { display: flex; align-items: center; gap: 8px; padding-right: 28px; }
  .usage-title-row .sched-title { margin-bottom: 0; }
  .usage-plan { font-size: 11.5px; font-weight: 600; padding: 2px 8px; border-radius: 999px; background: var(--hover); color: var(--text-dim); white-space: nowrap; }
  .usage-title-right { margin-left: auto; display: inline-flex; align-items: center; gap: 8px; }
  .usage-updated { font-size: 11.5px; color: var(--muted); white-space: nowrap; }
  .usage-refresh { border: 1px solid var(--border-strong); background: transparent; color: var(--text-dim); border-radius: 8px; padding: 4px 10px; font: inherit; font-size: 12px; cursor: pointer; }
  .usage-refresh:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .usage-refresh:disabled { opacity: .55; cursor: default; }
  .usage-account { font-size: 12.5px; color: var(--muted); margin: 0 0 -4px 0; }
  .usage-tabs { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 2px; }
  .usage-tab { display: inline-flex; align-items: center; gap: 5px; border: 1px solid var(--border); background: transparent; color: var(--text-dim); border-radius: 999px; padding: 4px 11px; font: inherit; font-size: 12px; cursor: pointer; }
  .usage-tab:hover { background: var(--hover); color: var(--text); }
  .usage-tab.sel { background: var(--accent); color: var(--accent-fg); border-color: var(--accent); }
  .usage-body { overflow-y: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 14px; padding-right: 2px; }
  .usage-loading { font-size: 13.5px; color: var(--muted); margin: 8px 0; }
  .usage-groups { display: flex; flex-direction: column; gap: 16px; }
  .usage-group { border: 1px solid var(--border); border-radius: 12px; padding: 12px 14px; display: flex; flex-direction: column; gap: 10px; }
  .usage-group-title { font-size: 12px; font-weight: 700; letter-spacing: .04em; color: var(--text); }
  .usage-group-sub { font-size: 12px; color: var(--muted); margin-top: -6px; line-height: 1.4; }
  .usage-meters { display: flex; flex-direction: column; gap: 10px; }
  .usage-meter { display: flex; flex-direction: column; gap: 4px; }
  .usage-meter-top { display: flex; justify-content: space-between; align-items: baseline; gap: 10px; font-size: 13px; }
  .usage-meter-label { font-weight: 500; color: var(--text); }
  .usage-meter-val { font-variant-numeric: tabular-nums; font-size: 12.5px; color: var(--text-dim); }
  .usage-bar { height: 8px; border-radius: 999px; background: var(--hover); overflow: hidden; }
  .usage-fill { height: 100%; border-radius: 999px; transition: width .2s ease; }
  .usage-fill.ok { background: #16a34a; }
  .usage-fill.warn { background: #b45309; }
  .usage-fill.low { background: var(--danger); }
  .usage-meter-reset { font-size: 11.5px; color: var(--muted); }
  .usage-meter-reset.ok { color: #15803d; }
  .usage-meter-reset.warn { color: #b45309; }
  .usage-meter-reset.low { color: var(--danger); }
  .usage-stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; }
  .usage-stat { border: 1px solid var(--border); border-radius: 10px; padding: 8px 10px; display: flex; flex-direction: column; gap: 2px; }
  .usage-stat-label { font-size: 11.5px; color: var(--muted); }
  .usage-stat-val { font-size: 13.5px; font-weight: 600; color: var(--text); font-variant-numeric: tabular-nums; }
  .usage-note { font-size: 12.5px; line-height: 1.45; color: var(--muted); margin: 0; }
  .usage-raw { font-size: 12px; color: var(--muted); }
  .usage-raw summary { cursor: pointer; user-select: none; }
  .usage-raw pre { margin: 8px 0 0 0; padding: 10px; border-radius: 8px; background: var(--hover); color: var(--text-dim); font-size: 11.5px; line-height: 1.4; overflow-x: auto; white-space: pre-wrap; user-select: text; }
  @media (max-width: 720px) {
    .confirm-card { width: calc(100vw - 32px); }
  }
</style>
