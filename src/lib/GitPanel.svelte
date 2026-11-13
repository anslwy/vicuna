<script lang="ts">
  // Side panel Git tab (Fork / Sourcetree style, stacked for the narrow
  // panel): Unstaged / Staged folder trees on top, a drag divider, and the
  // selected file's diff below. Stage / unstage / discard work on whole
  // files and folders, single hunks, or the lines selected in a hunk
  // (click, shift-click for a range). Patches for hunks and lines come from
  // gitDiff.ts; the backend is src-tauri/src/git_panel.rs.
  import { onDestroy, untrack } from "svelte";
  import hljs from "highlight.js/lib/common";
  import { gitApply, gitDiff, gitDiscard, gitStage, gitStatus, gitUnstage, type GitStatus } from "./git";
  import {
    badgeFor,
    buildPatch,
    buildTree,
    hljsLanguageFor,
    itemsUnder,
    parseDiff,
    splitStatus,
    type FileDiff,
    type PatchMode,
    type Picks,
    type SectionItem
  } from "./gitDiff";

  let {
    root,
    active,
    refreshTick = 0,
    onopenfile,
    onreveal
  }: {
    root: string;
    active: boolean;
    /** bump to refresh (agent turn settled, editor saved, …) */
    refreshTick?: number;
    onopenfile?: (path: string) => void;
    onreveal?: (path: string) => void;
  } = $props();

  let status = $state<GitStatus | null>(null);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let opError = $state<string | null>(null);
  let busy = $state(false);

  let collapsedUn = $state<Set<string>>(new Set());
  let collapsedSt = $state<Set<string>>(new Set());

  /** selected file: path + which section it was picked from */
  let selected = $state<{ path: string; staged: boolean } | null>(null);
  let diff = $state<FileDiff | null>(null);
  let diffLoading = $state(false);
  let diffError = $state<string | null>(null);
  let diffTooLarge = $state(false);
  /** selected diff lines, "hunk:line" keys */
  let lineSel = $state<Set<string>>(new Set());
  let anchor: { hi: number; li: number } | null = null;

  let confirm = $state<{ title: string; message: string; label: string; run: () => Promise<void> } | null>(null);
  let menu = $state<{ x: number; y: number; staged: boolean; item: SectionItem | null; dir: string | null } | null>(null);

  /** top pane height as % of the panel (drag divider) */
  let topPct = $state(42);
  let shell: HTMLDivElement | undefined = $state();

  const sections = $derived(splitStatus(status?.files ?? []));
  const unRows = $derived(buildTree(sections.unstaged, collapsedUn));
  const stRows = $derived(buildTree(sections.staged, collapsedSt));
  const selItem = $derived.by(() => {
    if (!selected) return null;
    const list = selected.staged ? sections.staged : sections.unstaged;
    return list.find((i) => i.path === selected!.path) ?? null;
  });

  function errText(e: any): string {
    return typeof e === "string" ? e : (e?.message ?? String(e));
  }

  function escapeHtml(s: string): string {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  /** per-line highlighted HTML, [hunk][line] */
  const lineHtml = $derived.by(() => {
    if (!diff || !selected) return [] as string[][];
    const lang = hljsLanguageFor(selected.path);
    const total = diff.hunks.reduce((n, h) => n + h.lines.length, 0);
    const useHl = !!lang && !!hljs.getLanguage(lang) && total <= 4000;
    return diff.hunks.map((h) =>
      h.lines.map((l) => {
        if (!useHl) return escapeHtml(l.text);
        try {
          return hljs.highlight(l.text, { language: lang!, ignoreIllegals: true }).value;
        } catch {
          return escapeHtml(l.text);
        }
      })
    );
  });

  let statusSeq = 0;
  async function refresh() {
    const r = root.trim();
    if (!r || r === "/") {
      status = null;
      error = "Pick a workspace folder first.";
      return;
    }
    const seq = ++statusSeq;
    loading = !status;
    // display order of the selected file's section before the refresh, so a
    // file that leaves it (staged / unstaged / went clean) hands the view to
    // its neighbour in the same list
    const prevOrder = selected ? sectionOrder(selected.staged) : [];
    try {
      const s = await gitStatus(r);
      if (seq !== statusSeq) return;
      status = s;
      error = null;
      if (selected && !selItem) {
        const next = neighbourAfterRemoval(prevOrder, selected.path, selected.staged);
        if (next) {
          selected = { path: next, staged: selected.staged };
        } else {
          // its section is empty now: follow the file to the other section
          const other = (selected.staged ? sections.unstaged : sections.staged).find((i) => i.path === selected!.path);
          selected = other ? { path: other.path, staged: !selected.staged } : null;
        }
        diff = null;
      }
      await loadDiff();
    } catch (e) {
      if (seq !== statusSeq) return;
      error = errText(e);
    } finally {
      if (seq === statusSeq) loading = false;
    }
  }

  /** visible file paths of a section in tree order (files in collapsed folders skipped) */
  function sectionOrder(staged: boolean): string[] {
    const rows = staged ? stRows : unRows;
    return rows.flatMap((row) => (row.item ? [row.item.path] : []));
  }

  /** next file after `path` in the old order that is still in the section, else the previous one */
  function neighbourAfterRemoval(prevOrder: string[], path: string, staged: boolean): string | null {
    const live = new Set((staged ? sections.staged : sections.unstaged).map((i) => i.path));
    const at = prevOrder.indexOf(path);
    if (at < 0) return null;
    for (let i = at + 1; i < prevOrder.length; i++) if (live.has(prevOrder[i])) return prevOrder[i];
    for (let i = at - 1; i >= 0; i--) if (live.has(prevOrder[i])) return prevOrder[i];
    return null;
  }

  let diffSeq = 0;
  async function loadDiff() {
    const seq = ++diffSeq;
    lineSel = new Set();
    anchor = null;
    const item = selItem;
    if (!selected || !item) {
      diff = null;
      diffError = null;
      diffTooLarge = false;
      return;
    }
    diffLoading = !diff;
    try {
      const d = await gitDiff(root.trim(), item.path, selected.staged, item.untracked);
      if (seq !== diffSeq) return;
      diffTooLarge = d.tooLarge;
      diff = d.tooLarge ? null : parseDiff(d.text);
      diffError = null;
    } catch (e) {
      if (seq !== diffSeq) return;
      diff = null;
      diffError = errText(e);
    } finally {
      if (seq === diffSeq) diffLoading = false;
    }
  }

  function select(item: SectionItem, staged: boolean) {
    if (selected?.path === item.path && selected.staged === staged) return;
    selected = { path: item.path, staged };
    diff = null;
    void loadDiff();
  }

  async function run(fn: () => Promise<void>) {
    if (busy) return;
    busy = true;
    opError = null;
    try {
      await fn();
    } catch (e) {
      opError = errText(e);
    } finally {
      busy = false;
      await refresh();
    }
  }

  // ---- file / folder actions ----

  function stageItems(items: SectionItem[]) {
    if (!items.length) return;
    void run(() => gitStage(root.trim(), items.map((i) => i.path)));
  }

  function unstageItems(items: SectionItem[]) {
    if (!items.length) return;
    const paths = items.flatMap((i) => (i.origPath ? [i.path, i.origPath] : [i.path]));
    void run(() => gitUnstage(root.trim(), paths));
  }

  // whole workspace in one git call ("." is scoped to the workspace by the
  // backend), so files past the status cap are included too
  function stageAll() {
    void run(() => gitStage(root.trim(), ["."]));
  }

  function unstageAll() {
    void run(() => gitUnstage(root.trim(), ["."]));
  }

  function discardItems(items: SectionItem[]) {
    const list = items.filter((i) => i.letter !== "U");
    if (!list.length) return;
    const newOnes = list.filter((i) => i.newFile);
    const what = list.length === 1 ? list[0].path : `${list.length} files`;
    confirm = {
      title: "Discard changes?",
      message:
        `Discard all unstaged changes to ${what}?` +
        (newOnes.length ? ` ${newOnes.length === 1 ? "The new file" : `${newOnes.length} new files`} will be deleted.` : "") +
        " This can't be undone.",
      label: "Discard",
      run: () =>
        gitDiscard(
          root.trim(),
          list.filter((i) => !i.newFile).map((i) => i.path),
          newOnes.map((i) => i.path)
        )
    };
  }

  // ---- hunk / line actions ----

  function hunkSel(hi: number): Set<number> | null {
    const s = new Set<number>();
    for (const k of lineSel) {
      const [h, l] = k.split(":").map(Number);
      if (h === hi) s.add(l);
    }
    return s.size ? s : null;
  }

  function applyHunk(hi: number, mode: PatchMode) {
    const item = selItem;
    if (!diff || !item) return;
    const picks: Picks = new Map([[hi, hunkSel(hi)]]);
    const built = buildPatch(diff, picks, mode);
    if (!built) return;
    // every change picked: the file-level command handles new/deleted files cleanly
    if (!built.partial) {
      if (mode === "stage") stageItems([item]);
      else if (mode === "unstage") unstageItems([item]);
      else discardItems([item]);
      return;
    }
    const r = root.trim();
    if (mode === "stage") void run(() => gitApply(r, built.patch, true, false, item.untracked ? [item.path] : []));
    else if (mode === "unstage") void run(() => gitApply(r, built.patch, true, true));
    else {
      const n = hunkSel(hi)?.size;
      confirm = {
        title: "Discard changes?",
        message: `Discard ${n ? `${n} selected line${n === 1 ? "" : "s"}` : "this hunk"} in ${item.path}? This can't be undone.`,
        label: "Discard",
        run: () => gitApply(r, built.patch, false, true)
      };
    }
  }

  function clickLine(e: MouseEvent, hi: number, li: number) {
    if (!diff || diff.hunks[hi].lines[li].kind === " ") return;
    const key = `${hi}:${li}`;
    const next = new Set(lineSel);
    if (e.shiftKey && anchor && anchor.hi === hi) {
      const [a, b] = anchor.li < li ? [anchor.li, li] : [li, anchor.li];
      for (let i = a; i <= b; i++) if (diff.hunks[hi].lines[i].kind !== " ") next.add(`${hi}:${i}`);
    } else if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
    }
    anchor = { hi, li };
    lineSel = next;
  }

  // ---- tree ----

  function toggleDir(staged: boolean, dir: string) {
    const cur = staged ? collapsedSt : collapsedUn;
    const next = new Set(cur);
    if (next.has(dir)) next.delete(dir);
    else next.add(dir);
    if (staged) collapsedSt = next;
    else collapsedUn = next;
  }

  function openMenu(e: MouseEvent, staged: boolean, item: SectionItem | null, dir: string | null) {
    e.preventDefault();
    menu = { x: e.clientX, y: e.clientY, staged, item, dir };
  }

  function menuItems(): SectionItem[] {
    if (!menu) return [];
    const list = menu.staged ? sections.staged : sections.unstaged;
    if (menu.item) return [menu.item];
    return menu.dir ? itemsUnder(list, menu.dir) : [];
  }

  function menuAct(kind: "stage" | "unstage" | "discard" | "open" | "reveal") {
    const items = menuItems();
    const target = menu?.item?.path ?? menu?.dir ?? "";
    menu = null;
    if (kind === "stage") stageItems(items);
    else if (kind === "unstage") unstageItems(items);
    else if (kind === "discard") discardItems(items);
    else if (kind === "open" && target) onopenfile?.(target);
    else if (kind === "reveal" && target) onreveal?.(target);
  }

  // ---- divider ----

  function startDrag(e: PointerEvent) {
    if (!shell) return;
    e.preventDefault();
    const rect = shell.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      topPct = Math.min(85, Math.max(15, ((ev.clientY - rect.top) / rect.height) * 100));
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function onDividerKey(e: KeyboardEvent) {
    if (e.key === "ArrowUp") topPct = Math.max(15, topPct - 3);
    else if (e.key === "ArrowDown") topPct = Math.min(85, topPct + 3);
    else return;
    e.preventDefault();
  }

  // ---- refresh triggers: root change, becoming visible, refreshTick, window focus ----

  let lastRoot = "";
  $effect(() => {
    const r = root.trim();
    if (r !== lastRoot) {
      lastRoot = r;
      selected = null;
      diff = null;
      status = null;
      collapsedUn = new Set();
      collapsedSt = new Set();
    }
  });

  $effect(() => {
    void refreshTick;
    void root;
    // refresh() reads status/selection; untracked so it can't re-trigger itself
    if (active) untrack(() => void refresh());
  });

  let lastFocusRefresh = 0;
  function onWindowFocus() {
    if (!active || Date.now() - lastFocusRefresh < 1500) return;
    lastFocusRefresh = Date.now();
    void refresh();
  }
  if (typeof window !== "undefined") window.addEventListener("focus", onWindowFocus);
  onDestroy(() => {
    if (typeof window !== "undefined") window.removeEventListener("focus", onWindowFocus);
  });

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (menu) menu = null;
    else if (confirm) confirm = null;
    else if (lineSel.size) lineSel = new Set();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

{#snippet section(title: string, staged: boolean, items: SectionItem[], rows: ReturnType<typeof buildTree>)}
  <div class="gp-section" data-testid={staged ? "git-staged" : "git-unstaged"}>
    <div class="gp-sec-head">
      <span class="gp-sec-title">{title}</span>
      {#if items.length}<span class="gp-count">{items.length}</span>{/if}
      <span class="gp-grow"></span>
      <button
        class="gp-btn"
        data-testid={staged ? "git-unstage-all" : "git-stage-all"}
        disabled={busy || !items.length}
        onclick={() => (staged ? unstageAll() : stageAll())}
        title={staged ? "Unstage every staged file" : "Stage every changed file"}
      >{staged ? "Unstage All" : "Stage All"}</button>
    </div>
    <div class="gp-tree">
      {#if !items.length}
        <div class="gp-empty-row">{staged ? "Nothing staged" : "No changes"}</div>
      {/if}
      {#each rows as row (row.key)}
        {#if row.dir}
          <button
            class="gp-row gp-dir"
            style="padding-left: {8 + row.depth * 14}px"
            title={row.path}
            onclick={() => toggleDir(staged, row.path)}
            oncontextmenu={(e) => openMenu(e, staged, null, row.path)}
          >
            <span class="gp-chev">{(staged ? collapsedSt : collapsedUn).has(row.path) ? "▸" : "▾"}</span>
            <svg class="gp-folder" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
            <span class="gp-name">{row.name}</span>
          </button>
        {:else if row.item}
          {@const b = badgeFor(row.item.letter)}
          {@const sel = selected?.path === row.item.path && selected.staged === staged}
          <button
            class="gp-row gp-file"
            class:sel
            data-testid="git-file"
            data-path={row.item.path}
            style="padding-left: {22 + row.depth * 14}px"
            title={row.item.origPath ? `${row.item.origPath} → ${row.item.path}` : row.item.path}
            onclick={() => select(row.item!, staged)}
            ondblclick={() => (staged ? unstageItems([row.item!]) : stageItems([row.item!]))}
            oncontextmenu={(e) => {
              select(row.item!, staged);
              openMenu(e, staged, row.item!, null);
            }}
          >
            <span class="gp-badge {b.tone}" aria-label="status {row.item.letter}">{b.text}</span>
            <span class="gp-name">{row.name}</span>
          </button>
        {/if}
      {/each}
    </div>
  </div>
{/snippet}

<div class="gp" bind:this={shell} data-testid="git-panel" role="presentation" onkeydown={onKey}>
  <div class="gp-bar">
    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="6" cy="6" r="2.5"/><circle cx="18" cy="6" r="2.5"/><circle cx="12" cy="18" r="2.5"/><path d="M6 8.5v1a3 3 0 0 0 3 3h6a3 3 0 0 0 3-3v-1"/><path d="M12 12.5v3"/></svg>
    <span class="gp-branch" data-testid="git-branch">{status?.repo ? (status.branch ?? "detached") : "Git"}</span>
    <span class="gp-grow"></span>
    <button class="gp-icon" data-testid="git-refresh" aria-label="Refresh" title="Refresh" disabled={loading} onclick={() => void refresh()}>
      <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M21 12a9 9 0 1 1-2.64-6.36"/><polyline points="21 3 21 9 15 9"/></svg>
    </button>
  </div>
  {#if opError}
    <div class="gp-err" data-testid="git-op-error">
      <span>{opError}</span>
      <button class="gp-x" aria-label="Dismiss" onclick={() => (opError = null)}>×</button>
    </div>
  {/if}

  {#if error}
    <div class="gp-msg">{error}</div>
  {:else if loading && !status}
    <div class="gp-msg">Loading…</div>
  {:else if status && !status.repo}
    <div class="gp-msg" data-testid="git-not-repo">Not a git repository.</div>
  {:else if status}
    <div class="gp-top" style="height: {topPct}%">
      {@render section("Unstaged", false, sections.unstaged, unRows)}
      {@render section("Staged", true, sections.staged, stRows)}
      {#if status.truncated}<div class="gp-empty-row">Showing the first 5000 changed files.</div>{/if}
    </div>
    <div
      class="gp-divider"
      role="slider"
      aria-label="Resize file list"
      aria-orientation="vertical"
      aria-valuenow={Math.round(topPct)}
      aria-valuemin={15}
      aria-valuemax={85}
      tabindex="0"
      onpointerdown={startDrag}
      ondblclick={() => (topPct = 42)}
      onkeydown={onDividerKey}
    ></div>
    <div class="gp-diff" data-testid="git-diff">
      {#if !selected || !selItem}
        <div class="gp-msg">Select a file to see its changes.</div>
      {:else}
        <div class="gp-diff-head">
          <span class="gp-diff-name" title={selItem.path}>{selItem.path.split("/").pop()}</span>
          <span class="gp-grow"></span>
          {#if selected.staged}
            <button class="gp-btn" data-testid="git-file-unstage" disabled={busy} onclick={() => unstageItems([selItem])}>Unstage</button>
          {:else}
            <button class="gp-btn" data-testid="git-file-stage" disabled={busy} onclick={() => stageItems([selItem])}>Stage</button>
            <button class="gp-btn ghost" data-testid="git-file-discard" disabled={busy || selItem.letter === "U"} onclick={() => discardItems([selItem])}>Discard…</button>
          {/if}
        </div>
        {#if diffLoading}
          <div class="gp-msg">Loading diff…</div>
        {:else if diffError}
          <div class="gp-msg">{diffError}</div>
        {:else if diffTooLarge}
          <div class="gp-msg">This diff is too large to show (over 2MB).</div>
        {:else if diff?.binary}
          <div class="gp-msg">Binary file: no text diff.</div>
        {:else if diff && !diff.hunks.length}
          <div class="gp-msg">No text changes (mode or empty file).</div>
        {:else if diff}
          <div class="gp-code">
            <div class="gp-rows">
            {#each diff.hunks as h, hi (hi)}
              {@const picked = hunkSel(hi)}
              <div class="gp-hunk-head" data-testid="git-hunk">
                <span class="gp-hunk-label">@@ -{h.oldStart},{h.oldCount} +{h.newStart},{h.newCount} @@ {h.section}</span>
                <span class="gp-hunk-btns">
                  {#if selected.staged}
                    <button class="gp-btn sm" data-testid="git-hunk-unstage" disabled={busy} onclick={() => applyHunk(hi, "unstage")}>{picked ? "Unstage lines" : "Unstage hunk"}</button>
                  {:else}
                    <button class="gp-btn sm" data-testid="git-hunk-stage" disabled={busy} onclick={() => applyHunk(hi, "stage")}>{picked ? "Stage lines" : "Stage hunk"}</button>
                    <button class="gp-btn sm ghost" data-testid="git-hunk-discard" disabled={busy} onclick={() => applyHunk(hi, "discard")}>{picked ? "Discard lines" : "Discard hunk"}</button>
                  {/if}
                </span>
              </div>
              {#each h.lines as l, li (li)}
                <!-- svelte-ignore a11y_click_events_have_key_events -->
                <div
                  class="gp-line {l.kind === '+' ? 'add' : l.kind === '-' ? 'del' : 'ctx'}"
                  class:picked={lineSel.has(`${hi}:${li}`)}
                  data-testid="git-line"
                  role="option"
                  aria-selected={lineSel.has(`${hi}:${li}`)}
                  tabindex="-1"
                  onclick={(e) => clickLine(e, hi, li)}
                >
                  <span class="gp-gutter">
                    <span class="gp-no">{l.oldNo ?? ""}</span>
                    <span class="gp-no">{l.newNo ?? ""}</span>
                    <span class="gp-sign">{l.kind === " " ? "" : l.kind}</span>
                  </span>
                  <span class="gp-text">{@html lineHtml[hi]?.[li] ?? ""}</span>
                </div>
              {/each}
            {/each}
            </div>
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  {#if menu}
    <button class="gp-backdrop" aria-label="Close menu" tabindex="-1" onclick={() => (menu = null)} oncontextmenu={(e) => { e.preventDefault(); menu = null; }}></button>
    <div class="gp-menu" style="left: {menu.x}px; top: {menu.y}px" data-testid="git-menu">
      {#if menu.staged}
        <button class="gp-mi" onclick={() => menuAct("unstage")}>Unstage</button>
      {:else}
        <button class="gp-mi" onclick={() => menuAct("stage")}>Stage</button>
        <button class="gp-mi danger" onclick={() => menuAct("discard")}>Discard changes…</button>
      {/if}
      {#if menu.item && menu.item.letter !== "D"}
        <div class="gp-sep"></div>
        <button class="gp-mi" onclick={() => menuAct("open")}>Open in editor</button>
      {/if}
      {#if onreveal && !(menu.item && menu.item.letter === "D")}
        <button class="gp-mi" onclick={() => menuAct("reveal")}>Reveal in Finder</button>
      {/if}
    </div>
  {/if}

  {#if confirm}
    <div class="gp-confirm-wrap">
      <div class="gp-confirm" role="alertdialog" aria-label={confirm.title} data-testid="git-confirm">
        <p class="gp-confirm-title">{confirm.title}</p>
        <p>{confirm.message}</p>
        <div class="gp-confirm-row">
          <button class="gp-btn ghost" data-testid="git-confirm-cancel" onclick={() => (confirm = null)}>Cancel</button>
          <button
            class="gp-btn danger"
            data-testid="git-confirm-ok"
            onclick={() => {
              const c = confirm;
              confirm = null;
              if (c) void run(c.run);
            }}
          >{confirm.label}</button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .gp { position: relative; display: flex; flex-direction: column; height: 100%; min-height: 0; font-size: 13px; }
  .gp-bar { display: flex; align-items: center; gap: 6px; padding: 6px 8px 6px 12px; border-bottom: 1px solid var(--border); color: var(--muted); }
  .gp-branch { font-size: 12.5px; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .gp-grow { flex: 1; }
  .gp-icon { border: none; background: transparent; color: var(--icon-dim); width: 26px; height: 26px; border-radius: 6px; display: grid; place-items: center; cursor: pointer; }
  .gp-icon:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .gp-msg { padding: 18px 14px; color: var(--muted); font-size: 12.5px; text-align: center; }
  .gp-err { display: flex; align-items: flex-start; gap: 6px; margin: 6px 8px 0; padding: 6px 8px; border-radius: 8px; background: var(--err-bg); color: var(--err-text); font-size: 12px; white-space: pre-wrap; word-break: break-word; }
  .gp-err span { flex: 1; }
  .gp-x { border: none; background: transparent; color: inherit; cursor: pointer; font-size: 15px; line-height: 1; }

  .gp-top { display: flex; flex-direction: column; min-height: 0; flex-shrink: 0; }
  .gp-section { flex: 1 1 0; min-height: 0; display: flex; flex-direction: column; }
  .gp-section + .gp-section { border-top: 1px solid var(--border); }
  .gp-sec-head { display: flex; align-items: center; gap: 6px; padding: 6px 8px 6px 12px; }
  .gp-sec-title { font-weight: 600; font-size: 12.5px; }
  .gp-count { font-size: 11px; color: var(--muted); background: var(--chip); border-radius: 999px; padding: 0 6px; }
  .gp-tree { flex: 1; min-height: 0; overflow: auto; padding-bottom: 4px; }
  .gp-empty-row { padding: 4px 12px; color: var(--faint); font-size: 12px; }
  .gp-row {
    display: flex; align-items: center; gap: 6px; width: 100%; border: none; background: transparent; color: var(--text);
    font: inherit; font-size: 12.5px; text-align: left; padding-top: 3px; padding-bottom: 3px; padding-right: 8px; cursor: pointer; border-radius: 6px;
  }
  .gp-row:hover { background: var(--hover); }
  .gp-row.sel { background: color-mix(in srgb, var(--accent) 22%, transparent); }
  .gp-chev { width: 10px; color: var(--muted); font-size: 10px; flex-shrink: 0; }
  .gp-folder { color: #d9822b; flex-shrink: 0; }
  .gp-name { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .gp-badge {
    flex-shrink: 0; width: 15px; height: 15px; border-radius: 4px; display: grid; place-items: center;
    font-size: 10px; font-weight: 700; color: #fff; line-height: 1;
  }
  .gp-badge.add { background: #2da44e; }
  .gp-badge.mod { background: #d29922; }
  .gp-badge.del { background: #cf222e; }
  .gp-badge.ren { background: #0969da; }
  .gp-badge.conflict { background: #bc4c00; }

  .gp-btn {
    appearance: none; border: 1px solid var(--border-2); background: var(--item-bg); color: var(--text);
    font: inherit; font-size: 12px; padding: 3px 10px; border-radius: 7px; cursor: pointer; white-space: nowrap;
  }
  .gp-btn:hover:not(:disabled) { background: var(--hover-3, var(--hover)); }
  .gp-btn:disabled { opacity: 0.45; cursor: default; }
  .gp-btn.ghost { background: transparent; }
  .gp-btn.danger { background: var(--danger, #cf222e); border-color: transparent; color: #fff; }
  .gp-btn.sm { font-size: 11px; padding: 1px 8px; }

  .gp-divider { flex-shrink: 0; height: 5px; cursor: row-resize; border-top: 1px solid var(--border); background: transparent; }
  .gp-divider:hover, .gp-divider:focus-visible { background: color-mix(in srgb, var(--accent) 35%, transparent); outline: none; }

  .gp-diff { flex: 1; min-height: 0; display: flex; flex-direction: column; }
  .gp-diff-head { display: flex; align-items: center; gap: 6px; padding: 6px 8px 6px 12px; border-bottom: 1px solid var(--border); }
  .gp-diff-name { font-weight: 600; font-size: 12.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .gp-code {
    flex: 1; min-height: 0; overflow: auto; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    font-size: 12px; line-height: 1.5; -webkit-user-select: text; user-select: text;
    /* cqw below = the visible width, so hunk headers fit the viewport while rows span the longest line */
    container-type: inline-size;
  }
  /* one box as wide as the longest line (never narrower than the view): every row stretches to it, so line backgrounds end flush instead of at each line's own text */
  .gp-rows { width: max-content; min-width: 100%; }
  .gp-hunk-head {
    position: sticky; left: 0; z-index: 2; display: flex; align-items: center; gap: 8px; padding: 4px 8px; width: 100cqw; box-sizing: border-box;
    background: var(--item-bg); color: var(--muted); border-top: 1px solid var(--border); border-bottom: 1px solid var(--border);
  }
  .gp-hunk-label { flex: 1; min-width: 0; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 11.5px; }
  .gp-hunk-btns { display: flex; gap: 4px; flex-shrink: 0; font-family: system-ui, sans-serif; }
  .gp-line { display: grid; grid-template-columns: max-content max-content; background: var(--bg); cursor: default; }
  /* line numbers stay put while the code scrolls sideways; inherits the row tint so it doesn't look cut out */
  .gp-gutter { position: sticky; left: 0; z-index: 1; display: grid; grid-template-columns: 36px 36px 14px; background: inherit; }
  .gp-line.add, .gp-line.del { cursor: pointer; }
  .gp-line.add { background: color-mix(in srgb, #2da44e 17%, var(--bg)); }
  .gp-line.del { background: color-mix(in srgb, #cf222e 15%, var(--bg)); }
  .gp-line.picked .gp-gutter { box-shadow: inset 3px 0 0 var(--accent); }
  .gp-line.add.picked { background: color-mix(in srgb, #2da44e 34%, var(--bg)); }
  .gp-line.del.picked { background: color-mix(in srgb, #cf222e 30%, var(--bg)); }
  .gp-no { color: var(--faint); text-align: right; padding-right: 6px; font-size: 11px; -webkit-user-select: none; user-select: none; }
  .gp-sign { color: var(--muted); -webkit-user-select: none; user-select: none; }
  .gp-text { white-space: pre; tab-size: 4; padding-right: 12px; color: var(--text); }
  .gp-text :global(.hljs-keyword), .gp-text :global(.hljs-selector-tag) { color: var(--code-keyword); }
  .gp-text :global(.hljs-string), .gp-text :global(.hljs-regexp) { color: var(--code-string); }
  .gp-text :global(.hljs-comment), .gp-text :global(.hljs-quote) { color: var(--code-comment); font-style: italic; }
  .gp-text :global(.hljs-number), .gp-text :global(.hljs-literal) { color: var(--code-number); }
  .gp-text :global(.hljs-title), .gp-text :global(.hljs-title.function_) { color: var(--code-title); }
  .gp-text :global(.hljs-built_in) { color: var(--code-builtin); }
  .gp-text :global(.hljs-attr), .gp-text :global(.hljs-attribute), .gp-text :global(.hljs-variable), .gp-text :global(.hljs-template-variable) { color: var(--code-attr); }
  .gp-text :global(.hljs-name), .gp-text :global(.hljs-selector-id), .gp-text :global(.hljs-selector-class) { color: var(--code-builtin); }

  .gp-backdrop { position: fixed; inset: 0; z-index: 40; border: 0; background: transparent; cursor: default; }
  .gp-menu {
    position: fixed; z-index: 41; min-width: 170px; padding: 5px; background: var(--card); border: 1px solid var(--border-2);
    border-radius: 10px; box-shadow: 0 10px 30px rgba(0, 0, 0, 0.18);
  }
  .gp-mi { display: block; width: 100%; text-align: left; border: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding: 6px 10px; border-radius: 6px; cursor: pointer; }
  .gp-mi:hover { background: var(--hover); }
  .gp-mi.danger { color: var(--danger, #cf222e); }
  .gp-sep { height: 1px; margin: 4px 6px; background: var(--border); }

  .gp-confirm-wrap { position: absolute; inset: 0; z-index: 42; display: grid; place-items: center; background: rgba(0, 0, 0, 0.25); padding: 12px; }
  .gp-confirm { background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; padding: 14px 16px; max-width: 320px; box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25); }
  .gp-confirm p { margin: 0 0 12px; font-size: 13px; word-break: break-word; }
  .gp-confirm .gp-confirm-title { font-weight: 600; margin-bottom: 6px; }
  .gp-confirm-row { display: flex; justify-content: flex-end; gap: 8px; }
</style>
