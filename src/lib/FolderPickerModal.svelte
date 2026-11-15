<script lang="ts">
  // Folder picker for places the native dialog can't browse: a remote Vicuna
  // server (the folders live on that machine) and plain browsers with the
  // dev bridge. Lists folders through `fs_list_dirs` (invokeOrBridge routes
  // it to the active server). Click a folder to open it; "Select this folder"
  // picks the folder you are in.
  import { onMount, tick } from "svelte";
  import { invokeOrBridge } from "$lib/bridge";

  interface Listing {
    path: string;
    parent: string | null;
    home: string | null;
    sep: string;
    entries: string[];
    truncated: boolean;
  }

  let {
    start = "",
    where = "",
    pickError = null,
    onPick,
    onClose
  }: {
    /** folder to open first (falls back to the home folder) */
    start?: string;
    /** where the folders live, e.g. the server name (shown in the title) */
    where?: string;
    /** the caller rejected the last pick (e.g. not a valid workspace) */
    pickError?: string | null;
    onPick: (path: string) => void | Promise<void>;
    onClose: () => void;
  } = $props();

  let listing = $state<Listing | null>(null);
  let pathDraft = $state("");
  let filter = $state("");
  let showHidden = $state(false);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let sel = $state(0);
  let picking = $state(false);
  let listEl = $state<HTMLDivElement | null>(null);
  let filterEl = $state<HTMLInputElement | null>(null);

  const shown = $derived(
    (listing?.entries ?? []).filter((n) => !filter.trim() || n.toLowerCase().includes(filter.trim().toLowerCase()))
  );

  function join(dir: string, name: string, sep: string): string {
    return dir.endsWith(sep) || dir.endsWith("/") ? dir + name : dir + sep + name;
  }

  async function load(path: string | null, fallbackHome = false) {
    loading = true;
    error = null;
    try {
      const l = await invokeOrBridge<Listing>("fs_list_dirs", { path, hidden: showHidden });
      listing = l;
      pathDraft = l.path;
      filter = "";
      sel = 0;
      await tick();
      listEl?.scrollTo({ top: 0 });
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (fallbackHome) return load(null);
      error = msg;
      if (listing) pathDraft = listing.path;
    } finally {
      loading = false;
    }
  }

  function enter(name: string) {
    if (listing) void load(join(listing.path, name, listing.sep));
  }

  function up() {
    if (listing?.parent) void load(listing.parent);
  }

  /** Pick the folder in view — or, when the path bar was edited and not
   *  yet opened, the typed path (after confirming it exists). */
  async function pick() {
    if (!listing || picking) return;
    picking = true;
    try {
      if (pathDraft.trim() && pathDraft.trim() !== listing.path) {
        await load(pathDraft.trim());
        if (error) return;
      }
      await onPick(listing.path);
    } finally {
      picking = false;
    }
  }

  function moveSel(d: number) {
    if (!shown.length) return;
    sel = (sel + d + shown.length) % shown.length;
    void tick().then(() => listEl?.querySelector<HTMLElement>(`[data-idx="${sel}"]`)?.scrollIntoView({ block: "nearest" }));
  }

  function onKey(e: KeyboardEvent) {
    // the global double-Esc / shortcut handlers must not see keys typed here
    e.stopPropagation();
    const inPath = (e.target as HTMLElement | null)?.dataset?.role === "path";
    if (e.key === "Escape") {
      e.preventDefault();
      onClose();
    } else if (inPath) {
      if (e.key === "Enter") {
        e.preventDefault();
        void load(pathDraft);
      }
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      moveSel(1);
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      moveSel(-1);
    } else if (e.key === "Enter" || (e.key === "ArrowRight" && !filter)) {
      e.preventDefault();
      if ((e.metaKey || e.ctrlKey) && e.key === "Enter") void pick();
      else if (shown[sel]) enter(shown[sel]);
    } else if ((e.key === "Backspace" || e.key === "ArrowLeft") && !filter) {
      e.preventDefault();
      up();
    }
  }

  onMount(() => {
    void load(start.trim() || null, true);
    void tick().then(() => filterEl?.focus());
  });
</script>

<button class="fp-backdrop" data-testid="folder-picker-backdrop" aria-label="Cancel" tabindex="-1" onclick={onClose}></button>
<div class="fp-card" data-testid="folder-picker" role="dialog" aria-modal="true" aria-label="Choose a folder" tabindex="-1" onkeydown={onKey}>
  <div class="fp-title">Choose a workspace folder{#if where}<span class="fp-where"> on {where}</span>{/if}</div>

  <div class="fp-bar">
    <button class="fp-icon" data-testid="folder-picker-up" title="Up one folder (Backspace)" aria-label="Up one folder" disabled={!listing?.parent || loading} onclick={up}>
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="m5 12 7-7 7 7"/></svg>
    </button>
    <button class="fp-icon" data-testid="folder-picker-home" title="Home folder" aria-label="Home folder" disabled={loading} onclick={() => void load(null)}>
      <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m3 10 9-7 9 7"/><path d="M5 9v11h14V9"/></svg>
    </button>
    <input
      class="fp-path"
      data-testid="folder-picker-path"
      data-role="path"
      spellcheck="false"
      autocomplete="off"
      title="Type a path and press Enter"
      bind:value={pathDraft}
    />
  </div>

  <input class="fp-filter" data-testid="folder-picker-filter" placeholder="Filter folders" spellcheck="false" autocomplete="off" bind:this={filterEl} bind:value={filter} oninput={() => (sel = 0)} />

  <div class="fp-list" data-testid="folder-picker-list" bind:this={listEl}>
    {#if error}
      <div class="fp-msg err" data-testid="folder-picker-error">{error}</div>
    {:else if !listing}
      <div class="fp-msg">Loading…</div>
    {:else if shown.length === 0}
      <div class="fp-msg">{filter ? "No folders match." : "No folders here. Select this folder, or go up."}</div>
    {:else}
      {#each shown as name, i (name)}
        <button
          class="fp-row {i === sel ? 'sel' : ''}"
          data-testid="folder-picker-row"
          data-idx={i}
          title={name}
          onmouseenter={() => (sel = i)}
          onclick={() => enter(name)}
        >
          <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
          <span class="fp-name">{name}</span>
          <svg class="fp-chev" width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 6 6 6-6 6"/></svg>
        </button>
      {/each}
      {#if listing.truncated}<div class="fp-msg">Showing the first 2000 folders. Filter or type a path to narrow it down.</div>{/if}
    {/if}
  </div>

  {#if pickError}<div class="fp-msg err" data-testid="folder-picker-pick-error">{pickError}</div>{/if}
  <div class="fp-foot">
    <label class="fp-hidden">
      <input type="checkbox" data-testid="folder-picker-hidden" bind:checked={showHidden} onchange={() => listing && void load(listing.path)} />
      Show hidden
    </label>
    <span class="fp-spacer"></span>
    <button class="fp-btn" data-testid="folder-picker-cancel" onclick={onClose}>Cancel</button>
    <button class="fp-btn primary" data-testid="folder-picker-select" disabled={!listing || loading || picking} title={listing?.path ?? ""} onclick={() => void pick()}>
      {picking ? "Checking…" : "Select this folder"}
    </button>
  </div>
</div>

<style>
  .fp-backdrop { position: fixed; inset: 0; z-index: 60; background: rgba(0, 0, 0, 0.18); border: none; cursor: default; }
  .fp-card { position: fixed; left: 50%; top: 50%; transform: translate(-50%, -50%); z-index: 61; width: min(560px, calc(100vw - 48px)); height: min(560px, calc(100vh - 80px)); display: flex; flex-direction: column; gap: 8px; background: var(--card); border-radius: 20px; box-shadow: 0 24px 80px rgba(0, 0, 0, 0.18); padding: 18px; box-sizing: border-box; outline: none; }
  .fp-title { font-size: 15px; font-weight: 600; color: var(--text); padding: 0 2px 4px; }
  .fp-where { font-weight: 400; color: var(--muted); }
  .fp-bar { display: flex; gap: 6px; align-items: center; }
  .fp-icon { flex: none; width: 34px; height: 34px; display: flex; align-items: center; justify-content: center; border: 1px solid var(--border-2); border-radius: 10px; background: transparent; color: var(--text-dim); cursor: pointer; }
  .fp-icon:hover:not(:disabled) { background: var(--hover-2); }
  .fp-icon:disabled { opacity: 0.4; cursor: default; }
  .fp-path, .fp-filter { border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 10px; font: inherit; font-size: 13.5px; background: transparent; color: var(--text); min-width: 0; box-sizing: border-box; }
  .fp-path { flex: 1; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 12.5px; }
  .fp-path:focus, .fp-filter:focus { outline: none; border-color: var(--accent); }
  .fp-list { flex: 1; min-height: 0; overflow-y: auto; border: 1px solid var(--border); border-radius: 12px; padding: 4px; }
  .fp-row { width: 100%; display: flex; align-items: center; gap: 8px; border: none; background: transparent; text-align: left; font: inherit; font-size: 14px; padding: 7px 10px; border-radius: 8px; cursor: pointer; color: var(--text); }
  .fp-row svg { flex: none; color: var(--muted); }
  .fp-row.sel { background: color-mix(in srgb, var(--accent) 12%, transparent); }
  .fp-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .fp-chev { opacity: 0.5; }
  .fp-msg { padding: 12px; font-size: 13px; color: var(--muted); }
  .fp-msg.err { color: var(--danger); overflow-wrap: anywhere; }
  .fp-foot { display: flex; align-items: center; gap: 8px; padding-top: 4px; }
  .fp-spacer { flex: 1; }
  .fp-hidden { display: flex; align-items: center; gap: 6px; font-size: 13px; color: var(--muted); cursor: pointer; user-select: none; }
  .fp-btn { border: 1px solid var(--border-2); border-radius: 10px; padding: 8px 14px; font: inherit; font-size: 14px; background: transparent; color: var(--text); cursor: pointer; }
  .fp-btn:hover:not(:disabled) { background: var(--hover-2); }
  .fp-btn.primary { background: var(--accent); border-color: var(--accent); color: var(--accent-fg, #fff); font-weight: 600; }
  .fp-btn.primary:hover:not(:disabled) { filter: brightness(1.05); background: var(--accent); }
  .fp-btn:disabled { opacity: 0.55; cursor: default; }
</style>
