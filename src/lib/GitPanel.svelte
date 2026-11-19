<script lang="ts">
  // Side panel Git tab (Fork / Sourcetree style, stacked for the narrow
  // panel): Unstaged / Staged folder trees on top, a drag divider, and the
  // selected file's diff (or multi-file summary) below. Stage / unstage /
  // discard work on single or multi-selected files (Shift range, Cmd/Ctrl
  // toggle, Shift+Arrows, Cmd/Ctrl+A), folders, single hunks, or selected
  // lines in a hunk. Patches for hunks and lines come from gitDiff.ts; the
  // backend is src-tauri/src/git_panel.rs.
  import { onDestroy, tick, untrack } from "svelte";
  import hljs from "highlight.js/lib/common";
  import { gitApply, gitDiff, gitDiscard, gitStage, gitStatus, gitUnstage, type GitStatus } from "./git";
  import {
    badgeFor,
    buildPatch,
    buildTree,
    clickTreeFile,
    contextMenuSelection,
    EMPTY_SELECTION,
    formatCopyText,
    gitFileName,
    gitFullPath,
    hljsLanguageFor,
    itemsUnder,
    parseDiff,
    reconcileSelection,
    selectAllInSection,
    selectedItemsInOrder,
    singleSelection,
    splitStatus,
    stepSelection,
    visibleFilePaths,
    type CopyKind,
    type FileDiff,
    type FileSelection,
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

  /** selected file(s) within one section (supports Shift range & Cmd/Ctrl toggle) */
  let selection = $state<FileSelection>(EMPTY_SELECTION);
  let diff = $state<FileDiff | null>(null);
  let diffLoading = $state(false);
  let diffError = $state<string | null>(null);
  let diffTooLarge = $state(false);
  /** selected diff lines, "hunk:line" keys */
  let lineSel = $state<Set<string>>(new Set());
  let anchor: { hi: number; li: number } | null = null;

  let confirm = $state<{ title: string; message: string; label: string; run: () => Promise<void> } | null>(null);
  let menu = $state<{ x: number; y: number; staged: boolean; item: SectionItem | null; dir: string | null } | null>(null);
  let toast = $state<string | null>(null);
  let toastTimer: ReturnType<typeof setTimeout> | null = null;

  /** top pane height as % of the panel (drag divider) */
  let topPct = $state(42);
  let shell: HTMLDivElement | undefined = $state();

  const sections = $derived(splitStatus(status?.files ?? []));
  const unRows = $derived(buildTree(sections.unstaged, collapsedUn));
  const stRows = $derived(buildTree(sections.staged, collapsedSt));
  const selectedItems = $derived.by(() =>
    selectedItemsInOrder(
      selection,
      selection.staged,
      selection.staged ? stRows : unRows,
      selection.staged ? sections.staged : sections.unstaged
    )
  );
  const selItem = $derived(selectedItems.length === 1 ? selectedItems[0] : null);
  const hasDiscardableSelection = $derived(selectedItems.some((i) => i.letter !== "U"));

  function flashToast(msg: string) {
    toast = msg;
    if (toastTimer) clearTimeout(toastTimer);
    toastTimer = setTimeout(() => {
      toast = null;
      toastTimer = null;
    }, 1800);
  }

  async function copyText(text: string): Promise<boolean> {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      try {
        const ta = document.createElement("textarea");
        ta.value = text;
        ta.style.position = "fixed";
        ta.style.opacity = "0";
        document.body.appendChild(ta);
        ta.select();
        const ok = document.execCommand("copy");
        ta.remove();
        return ok;
      } catch {
        return false;
      }
    }
  }

  function errText(e: any): string {
    return typeof e === "string" ? e : (e?.message ?? String(e));
  }

  function escapeHtml(s: string): string {
    return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
  }

  /** per-line highlighted HTML, [hunk][line] */
  const lineHtml = $derived.by(() => {
    if (!diff || !selItem) return [] as string[][];
    const lang = hljsLanguageFor(selItem.path);
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

  function singleKey(sel: FileSelection): string | null {
    return sel.paths.size === 1 && sel.primary ? `${sel.staged}:${sel.primary}` : null;
  }

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
    // display order of the selected file's section before the refresh, so files
    // that leave it (staged / unstaged / went clean) hand the view to their
    // neighbour in the same list
    const prevOrder = selection.paths.size ? visibleFilePaths(selection.staged ? stRows : unRows) : [];
    const prevSingle = singleKey(selection);
    try {
      const s = await gitStatus(r);
      if (seq !== statusSeq) return;
      status = s;
      error = null;
      if (selection.paths.size) {
        const next = reconcileSelection(selection, prevOrder, sections.unstaged, sections.staged);
        selection = next;
        if (singleKey(next) !== prevSingle) diff = null;
      }
      await loadDiff();
    } catch (e) {
      if (seq !== statusSeq) return;
      error = errText(e);
    } finally {
      if (seq === statusSeq) loading = false;
    }
  }

  let diffSeq = 0;
  async function loadDiff() {
    const seq = ++diffSeq;
    lineSel = new Set();
    anchor = null;
    const item = selItem;
    if (!item) {
      diff = null;
      diffError = null;
      diffTooLarge = false;
      diffLoading = false;
      return;
    }
    diffLoading = !diff;
    try {
      const d = await gitDiff(root.trim(), item.path, selection.staged, item.untracked);
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

  function applySelection(next: FileSelection) {
    const prevKey = singleKey(selection);
    const nextKey = singleKey(next);
    selection = next;
    if (nextKey !== prevKey) {
      diff = null;
      void loadDiff();
    }
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
    const x = typeof window === "undefined" ? e.clientX : Math.max(8, Math.min(e.clientX, window.innerWidth - 210));
    const y = typeof window === "undefined" ? e.clientY : Math.max(8, Math.min(e.clientY, window.innerHeight - 280));
    menu = { x, y, staged, item, dir };
  }

  function menuItems(): SectionItem[] {
    if (!menu) return [];
    const list = menu.staged ? sections.staged : sections.unstaged;
    if (menu.item) {
      if (selection.staged === menu.staged && selection.paths.has(menu.item.path) && selection.paths.size > 1) {
        return selectedItemsInOrder(selection, menu.staged, menu.staged ? stRows : unRows, list);
      }
      return [menu.item];
    }
    return menu.dir ? itemsUnder(list, menu.dir) : [];
  }

  const menuTargetItems = $derived(menu ? menuItems() : []);
  const menuCopyPaths = $derived.by(() => {
    if (!menu) return [] as string[];
    if (menu.dir) return [menu.dir];
    return menuTargetItems.map((i) => i.path);
  });
  const menuMulti = $derived(Boolean(menu && !menu.dir && menuTargetItems.length > 1));
  const menuDiscardable = $derived(menuTargetItems.some((i) => i.letter !== "U"));

  function menuAct(kind: "stage" | "unstage" | "discard" | "open" | "reveal" | "copyName" | "copyPath" | "copyFullPath") {
    const items = menuTargetItems;
    const copyPaths = menuCopyPaths;
    const target = menu?.item?.path ?? menu?.dir ?? "";
    const isDir = Boolean(menu?.dir);
    menu = null;
    if (kind === "stage") stageItems(items);
    else if (kind === "unstage") unstageItems(items);
    else if (kind === "discard") discardItems(items);
    else if (kind === "open" && target) onopenfile?.(target);
    else if (kind === "reveal" && target) onreveal?.(target);
    else if (kind === "copyName" || kind === "copyPath" || kind === "copyFullPath") {
      if (!copyPaths.length) return;
      const copyKind: CopyKind = kind === "copyName" ? "name" : kind === "copyPath" ? "path" : "fullPath";
      const text = formatCopyText(root, copyPaths, copyKind);
      void copyText(text).then((ok) => {
        if (!ok) {
          flashToast("Copy failed (clipboard unavailable)");
        } else if (copyPaths.length === 1) {
          const label =
            kind === "copyName"
              ? isDir
                ? "folder name"
                : "file name"
              : kind === "copyPath"
                ? isDir
                  ? "folder path"
                  : "file path"
                : "full path";
          flashToast(`Copied ${label}`);
        } else {
          const label = kind === "copyName" ? "file names" : kind === "copyPath" ? "file paths" : "full paths";
          flashToast(`Copied ${copyPaths.length} ${label}`);
        }
      });
    }
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
      selection = EMPTY_SELECTION;
      diff = null;
      status = null;
      menu = null;
      confirm = null;
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
    if (toastTimer) clearTimeout(toastTimer);
  });

  function onKey(e: KeyboardEvent) {
    if (e.defaultPrevented) return;
    if (e.key === "Escape") {
      if (menu) menu = null;
      else if (confirm) confirm = null;
      else if (lineSel.size) lineSel = new Set();
      else if (selection.paths.size > 1 && selection.primary) {
        applySelection(singleSelection(selection.staged, selection.primary));
      } else return;
      e.preventDefault();
      e.stopPropagation();
      return;
    }
    if (menu || confirm) return;
    const el = e.target instanceof Element ? e.target : null;
    if (el && (el.closest("input, textarea, select") || (el as HTMLElement).isContentEditable)) return;

    if ((e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "a") {
      if (el?.closest(".gp-code")) return;
      const secEl = el?.closest("[data-testid='git-staged'], [data-testid='git-unstaged']");
      const focusedStaged = secEl ? secEl.getAttribute("data-testid") === "git-staged" : null;
      const staged = focusedStaged ?? (selection.paths.size ? selection.staged : unRows.some((r) => r.item) ? false : true);
      const rows = staged ? stRows : unRows;
      if (!rows.some((r) => r.item)) return;
      e.preventDefault();
      applySelection(selectAllInSection(rows, staged, selection.staged === staged ? selection.primary : null));
      return;
    }

    if ((e.key !== "ArrowDown" && e.key !== "ArrowUp") || e.altKey || e.ctrlKey || e.metaKey) return;

    const secEl = el?.closest("[data-testid='git-staged'], [data-testid='git-unstaged']");
    const focusedStaged = secEl ? secEl.getAttribute("data-testid") === "git-staged" : null;
    const staged = focusedStaged ?? (selection.paths.size ? selection.staged : unRows.some((r) => r.item) ? false : true);
    const rows = staged ? stRows : unRows;
    const dir = e.key === "ArrowDown" ? 1 : -1;
    const next = stepSelection(selection, rows, staged, dir, e.shiftKey);
    e.preventDefault();
    if (!next) return;
    applySelection(next);
    void tick().then(() => {
      const row = shell?.querySelector<HTMLButtonElement>(
        `[data-testid="${staged ? "git-staged" : "git-unstaged"}"] .gp-file.active`
      );
      if (!row) return;
      row.scrollIntoView({ block: "nearest" });
      if (el?.closest(".gp-top")) row.focus({ preventScroll: true });
    });
  }
</script>

{#snippet section(title: string, staged: boolean, items: SectionItem[], rows: ReturnType<typeof buildTree>)}
  {@const secSelCount = selection.staged === staged ? selection.paths.size : 0}
  <div class="gp-section" data-testid={staged ? "git-staged" : "git-unstaged"}>
    <div class="gp-sec-head">
      <span class="gp-sec-title">{title}</span>
      {#if items.length}<span class="gp-count">{items.length}</span>{/if}
      {#if secSelCount > 1}<span class="gp-sel-count" data-testid={staged ? "git-staged-sel-count" : "git-unstaged-sel-count"}>{secSelCount} selected</span>{/if}
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
            onclick={(e) => {
              (e.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
              toggleDir(staged, row.path);
            }}
            oncontextmenu={(e) => openMenu(e, staged, null, row.path)}
          >
            <span class="gp-chev">{(staged ? collapsedSt : collapsedUn).has(row.path) ? "▸" : "▾"}</span>
            <svg class="gp-folder" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
            <span class="gp-name">{row.name}</span>
          </button>
        {:else if row.item}
          {@const b = badgeFor(row.item.letter)}
          {@const sel = selection.staged === staged && selection.paths.has(row.item.path)}
          {@const activeRow = selection.staged === staged && selection.primary === row.item.path}
          <button
            class="gp-row gp-file"
            class:sel
            class:active={activeRow}
            data-testid="git-file"
            data-path={row.item.path}
            aria-selected={sel}
            style="padding-left: {22 + row.depth * 14}px"
            title={row.item.origPath ? `${row.item.origPath} → ${row.item.path}` : row.item.path}
            onclick={(e) => {
              (e.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
              applySelection(
                clickTreeFile(selection, rows, staged, row.item!.path, {
                  shift: e.shiftKey,
                  toggle: e.metaKey || e.ctrlKey
                })
              );
            }}
            ondblclick={(e) => {
              if (e.shiftKey || e.metaKey || e.ctrlKey) return;
              if (staged) unstageItems([row.item!]);
              else stageItems([row.item!]);
            }}
            oncontextmenu={(e) => {
              (e.currentTarget as HTMLButtonElement).focus({ preventScroll: true });
              applySelection(contextMenuSelection(selection, staged, row.item!.path));
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

<div class="gp" bind:this={shell} data-testid="git-panel" role="presentation" tabindex="-1" onkeydown={onKey}>
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
      {#if selectedItems.length > 1}
        <div class="gp-diff-head">
          <span class="gp-diff-name" data-testid="git-multi-count">{selectedItems.length} files selected</span>
          <span class="gp-grow"></span>
          {#if selection.staged}
            <button class="gp-btn" data-testid="git-file-unstage" disabled={busy} onclick={() => unstageItems(selectedItems)}>Unstage ({selectedItems.length})</button>
          {:else}
            <button class="gp-btn" data-testid="git-file-stage" disabled={busy} onclick={() => stageItems(selectedItems)}>Stage ({selectedItems.length})</button>
            <button class="gp-btn ghost" data-testid="git-file-discard" disabled={busy || !hasDiscardableSelection} onclick={() => discardItems(selectedItems)}>Discard…</button>
          {/if}
        </div>
        <div class="gp-multi-list" data-testid="git-multi-list">
          {#each selectedItems as it (it.path)}
            {@const b = badgeFor(it.letter)}
            <button
              class="gp-multi-row"
              title={it.path}
              onclick={() => applySelection(singleSelection(selection.staged, it.path))}
            >
              <span class="gp-badge {b.tone}" aria-label="status {it.letter}">{b.text}</span>
              <span class="gp-multi-path">{it.path}</span>
            </button>
          {/each}
        </div>
      {:else if !selItem}
        <div class="gp-msg">Select a file to see its changes.</div>
      {:else}
        <div class="gp-diff-head">
          <span class="gp-diff-name" title={selItem.path}>{selItem.path.split("/").pop()}</span>
          <span class="gp-grow"></span>
          {#if selection.staged}
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
                  {#if selection.staged}
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

  {#if toast}
    <div class="gp-toast" data-testid="git-toast" role="status">{toast}</div>
  {/if}

  {#if menu}
    {@const count = menuTargetItems.length}
    {@const singleCopy = menuCopyPaths.length === 1 ? menuCopyPaths[0] : null}
    <button class="gp-backdrop" aria-label="Close menu" tabindex="-1" onclick={() => (menu = null)} oncontextmenu={(e) => { e.preventDefault(); menu = null; }}></button>
    <div class="gp-menu" style="left: {menu.x}px; top: {menu.y}px" data-testid="git-menu">
      {#if menu.staged}
        <button class="gp-mi" data-testid="git-menu-unstage" disabled={busy || !count} onclick={() => menuAct("unstage")}>
          {count > 1 ? `Unstage ${count} files` : "Unstage"}
        </button>
      {:else}
        <button class="gp-mi" data-testid="git-menu-stage" disabled={busy || !count} onclick={() => menuAct("stage")}>
          {count > 1 ? `Stage ${count} files` : "Stage"}
        </button>
        <button class="gp-mi danger" data-testid="git-menu-discard" disabled={busy || !menuDiscardable} onclick={() => menuAct("discard")}>
          {count > 1 ? `Discard ${count} files…` : "Discard changes…"}
        </button>
      {/if}
      {#if menuCopyPaths.length}
        <div class="gp-sep"></div>
        <button
          class="gp-mi"
          data-testid="git-menu-copy-name"
          title={singleCopy ? gitFileName(singleCopy) : undefined}
          onclick={() => menuAct("copyName")}
        >{menu.dir ? "Copy folder name" : menuMulti ? "Copy file names" : "Copy file name"}</button>
        <button
          class="gp-mi"
          data-testid="git-menu-copy-path"
          title={singleCopy ?? undefined}
          onclick={() => menuAct("copyPath")}
        >{menu.dir ? "Copy folder path" : menuMulti ? "Copy file paths" : "Copy file path"}</button>
        <button
          class="gp-mi"
          data-testid="git-menu-copy-full-path"
          title={singleCopy ? gitFullPath(root, singleCopy) : undefined}
          onclick={() => menuAct("copyFullPath")}
        >{menuMulti ? "Copy full paths" : "Copy full path"}</button>
      {/if}
      {#if !menuMulti && ((menu.item && menu.item.letter !== "D") || (onreveal && !(menu.item && menu.item.letter === "D")))}
        <div class="gp-sep"></div>
      {/if}
      {#if !menuMulti && menu.item && menu.item.letter !== "D"}
        <button class="gp-mi" data-testid="git-menu-open" onclick={() => menuAct("open")}>Open in editor</button>
      {/if}
      {#if !menuMulti && onreveal && !(menu.item && menu.item.letter === "D")}
        <button class="gp-mi" data-testid="git-menu-reveal" onclick={() => menuAct("reveal")}>Reveal in Finder</button>
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
  .gp:focus { outline: none; }
  .gp-bar { display: flex; align-items: center; gap: 6px; padding: 6px 8px 6px 12px; border-bottom: 1px solid var(--border); color: var(--muted); }
  .gp-branch { font-size: 12.5px; color: var(--text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .gp-grow { flex: 1; }
  .gp-icon { border: none; background: transparent; color: var(--icon-dim); width: 26px; height: 26px; border-radius: 6px; display: grid; place-items: center; cursor: pointer; }
  .gp-icon:hover:not(:disabled) { background: var(--hover); color: var(--text); }
  .gp-msg { padding: 18px 14px; color: var(--muted); font-size: 12.5px; text-align: center; }
  .gp-err { display: flex; align-items: flex-start; gap: 6px; margin: 6px 8px 0; padding: 6px 8px; border-radius: 8px; background: var(--err-bg); color: var(--err-text); font-size: 12px; white-space: pre-wrap; word-break: break-word; }
  .gp-err span { flex: 1; }
  .gp-x { border: none; background: transparent; color: inherit; cursor: pointer; font-size: 15px; line-height: 1; }

  .gp-top { display: flex; flex-direction: column; min-height: 0; flex-shrink: 0; -webkit-user-select: none; user-select: none; }
  .gp-section { flex: 1 1 0; min-height: 0; display: flex; flex-direction: column; }
  .gp-section + .gp-section { border-top: 1px solid var(--border); }
  .gp-sec-head { display: flex; align-items: center; gap: 6px; padding: 6px 8px 6px 12px; }
  .gp-sec-title { font-weight: 600; font-size: 12.5px; }
  .gp-count { font-size: 11px; color: var(--muted); background: var(--chip); border-radius: 999px; padding: 0 6px; }
  .gp-sel-count { font-size: 11px; font-weight: 500; color: var(--accent); background: color-mix(in srgb, var(--accent) 14%, transparent); border-radius: 999px; padding: 0 6px; white-space: nowrap; }
  .gp-tree { flex: 1; min-height: 0; overflow: auto; padding-bottom: 4px; }
  .gp-empty-row { padding: 4px 12px; color: var(--faint); font-size: 12px; }
  .gp-row {
    display: flex; align-items: center; gap: 6px; width: 100%; border: none; background: transparent; color: var(--text);
    font: inherit; font-size: 12.5px; text-align: left; padding-top: 3px; padding-bottom: 3px; padding-right: 8px; cursor: pointer; border-radius: 6px;
  }
  .gp-row:hover { background: var(--hover); }
  .gp-row:focus:not(:focus-visible) { outline: none; }
  .gp-row:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .gp-row.sel { background: color-mix(in srgb, var(--accent) 22%, transparent); }
  .gp-row.sel:hover { background: color-mix(in srgb, var(--accent) 28%, transparent); }
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
  .gp-multi-list { flex: 1; min-height: 0; overflow: auto; padding: 6px 8px; display: flex; flex-direction: column; gap: 2px; }
  .gp-multi-row {
    display: flex; align-items: center; gap: 6px; width: 100%; border: none; background: transparent; color: var(--text);
    font: inherit; font-size: 12px; text-align: left; padding: 4px 6px; border-radius: 6px; cursor: pointer;
  }
  .gp-multi-row:hover { background: var(--hover); }
  .gp-multi-path { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11.5px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
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

  .gp-toast {
    position: absolute; left: 12px; right: 12px; bottom: 12px; z-index: 8;
    background: var(--card); border: 1px solid var(--border-2); border-radius: 10px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.18); padding: 7px 11px; font-size: 12px; color: var(--text);
    pointer-events: none; text-align: center;
  }

  .gp-backdrop { position: fixed; inset: 0; z-index: 40; border: 0; background: transparent; cursor: default; }
  .gp-menu {
    position: fixed; z-index: 41; min-width: 170px; padding: 5px; background: var(--card); border: 1px solid var(--border-2);
    border-radius: 10px; box-shadow: 0 10px 30px rgba(0, 0, 0, 0.18);
  }
  .gp-mi { display: block; width: 100%; text-align: left; border: none; background: transparent; color: var(--text); font: inherit; font-size: 13px; padding: 6px 10px; border-radius: 6px; cursor: pointer; }
  .gp-mi:hover:not(:disabled) { background: var(--hover); }
  .gp-mi:disabled { opacity: 0.45; cursor: default; }
  .gp-mi.danger { color: var(--danger, #cf222e); }
  .gp-sep { height: 1px; margin: 4px 6px; background: var(--border); }

  .gp-confirm-wrap { position: absolute; inset: 0; z-index: 42; display: grid; place-items: center; background: rgba(0, 0, 0, 0.25); padding: 12px; }
  .gp-confirm { background: var(--card); border: 1px solid var(--border-2); border-radius: 12px; padding: 14px 16px; max-width: 320px; box-shadow: 0 12px 40px rgba(0, 0, 0, 0.25); }
  .gp-confirm p { margin: 0 0 12px; font-size: 13px; word-break: break-word; }
  .gp-confirm .gp-confirm-title { font-weight: 600; margin-bottom: 6px; }
  .gp-confirm-row { display: flex; justify-content: flex-end; gap: 8px; }
</style>
