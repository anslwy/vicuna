<script lang="ts">
  import type { Snippet } from "svelte";

  /** Custom hover tooltip in the opencode style: card bubble with an
   *  optional keyboard-hint chip. CSS-only (hover + keyboard focus), so it
   *  works anywhere without positioning JS. */
  let {
    text,
    shortcut = null,
    side = "top",
    children
  }: {
    text: string;
    shortcut?: string | null;
    side?: "top" | "bottom";
    children: Snippet;
  } = $props();
</script>

<span class="tooltip-wrap">
  {@render children()}
  <span class="tooltip-bubble tooltip-{side}" role="tooltip">{text}{#if shortcut}<kbd>{shortcut}</kbd>{/if}</span>
</span>

<style>
  .tooltip-wrap { position: relative; display: inline-flex; }
  .tooltip-bubble {
    display: none;
    position: absolute;
    left: 50%;
    transform: translateX(-50%);
    max-width: 300px;
    width: max-content;
    background: var(--card);
    color: var(--text);
    border: 1px solid var(--border-2);
    border-radius: 10px;
    box-shadow: 0 12px 40px rgba(0,0,0,.14);
    padding: 8px 12px;
    font-size: 12.5px;
    line-height: 1.5;
    text-align: center;
    text-wrap: balance;
    white-space: normal;
    z-index: 40;
    align-items: center;
    justify-content: center;
    gap: 8px;
  }
  .tooltip-top { bottom: calc(100% + 8px); }
  .tooltip-bottom { top: calc(100% + 8px); }
  .tooltip-wrap:hover .tooltip-bubble { display: inline-flex; }
  /* beat any inherited white-space from the anchor site */
  .tooltip-wrap .tooltip-bubble { white-space: normal; }
  .tooltip-bubble kbd {
    font: inherit;
    font-size: 12px;
    border: 1px solid var(--border-2);
    border-radius: 6px;
    background: var(--hover-3);
    padding: 1px 7px;
    white-space: nowrap;
  }
</style>
