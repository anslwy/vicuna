<script lang="ts">
  import { baseEngineId } from "$lib/workGate";
  import { pluginEngineIcon } from "$lib/pluginHarness";
  import type { WorkEngineInfo } from "$lib/work";
  import {
    CLAUDE_ICON_PATH,
    OPENCODE_ICON_PATH,
    OPENCODE_ICON_INNER_PATH,
    CODEX_ICON_OUTLINE_PATH,
    CODEX_ICON_PROMPT_PATH,
    FX_ICON_PATH,
    AGY_ICON_PATH,
    PI_ICON_PATH
  } from "$lib/modelIcons";

  let {
    id,
    workEngineList
  }: {
    id: string;
    workEngineList: NonNullable<WorkEngineInfo["engines"]>;
  } = $props();
</script>

{#if baseEngineId(id) === "claude"}
  <svg class="engine-ico" width="14" height="14" viewBox="0 0 24 24" fill="#D97757" aria-hidden="true"><path d={CLAUDE_ICON_PATH}/></svg>
{:else if baseEngineId(id) === "codex"}
  <svg class="engine-ico" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={CODEX_ICON_OUTLINE_PATH}/><path d={CODEX_ICON_PROMPT_PATH}/></svg>
{:else if id === "fx"}
  <svg class="engine-ico" data-engine-icon="fx" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={FX_ICON_PATH}/></svg>
{:else if id === "pi"}
  <svg class="engine-ico" data-engine-icon="pi" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={PI_ICON_PATH}/></svg>
{:else if id.startsWith("plugin:") && pluginEngineIcon(id, workEngineList)}
  <img class="engine-ico" src={pluginEngineIcon(id, workEngineList)!} width="14" height="14" alt="" aria-hidden="true" />
{:else if id.startsWith("plugin:")}
  <svg class="engine-ico" data-engine-icon="plugin" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 3h4v3a2 2 0 1 0 4 0V3h3v7h-3a2 2 0 1 0 0 4h3v7h-7v-3a2 2 0 1 0-4 0v3H3v-7h3a2 2 0 1 0 0-4H3V3h7z"/></svg>
{:else if baseEngineId(id) === "agy"}
  <svg class="engine-ico" data-engine-icon="agy" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="#4285F4" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d={AGY_ICON_PATH}/></svg>
{:else}
  <svg class="engine-ico" width="14" height="14" viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><path fill-rule="evenodd" d={OPENCODE_ICON_PATH}/><path d={OPENCODE_ICON_INNER_PATH} opacity=".4"/></svg>
{/if}

<style>
  .engine-ico { flex: none; display: block; width: 14px; height: 14px; object-fit: contain; }
</style>
