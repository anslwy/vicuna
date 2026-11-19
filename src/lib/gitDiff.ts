// Pure logic for the side panel's Git tab (GitPanel.svelte): unified-diff
// parsing, reduced patches for hunk / line staging, status → Unstaged /
// Staged sections, and the folder tree. No DOM / Tauri imports so
// node-tests can load it directly.

export type LineKind = " " | "+" | "-";

export interface DiffLine {
  kind: LineKind;
  text: string;
  oldNo: number | null;
  newNo: number | null;
  /** followed by "\ No newline at end of file" */
  noEol: boolean;
}

export interface Hunk {
  oldStart: number;
  oldCount: number;
  newStart: number;
  newCount: number;
  /** text after the closing @@ (function context) */
  section: string;
  lines: DiffLine[];
}

export interface FileDiff {
  header: string[];
  hunks: Hunk[];
  binary: boolean;
}

const HUNK_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@ ?(.*)$/;

/** Parse a single-file `git diff` output. */
export function parseDiff(text: string): FileDiff {
  const out: FileDiff = { header: [], hunks: [], binary: false };
  const lines = text.split("\n");
  if (lines.length && lines[lines.length - 1] === "") lines.pop();
  let cur: Hunk | null = null;
  let oldNo = 0;
  let newNo = 0;
  for (const raw of lines) {
    const line = raw.endsWith("\r") ? raw.slice(0, -1) : raw;
    const m = HUNK_RE.exec(line);
    if (m) {
      cur = {
        oldStart: +m[1],
        oldCount: m[2] === undefined ? 1 : +m[2],
        newStart: +m[3],
        newCount: m[4] === undefined ? 1 : +m[4],
        section: m[5] ?? "",
        lines: []
      };
      out.hunks.push(cur);
      oldNo = cur.oldStart;
      newNo = cur.newStart;
      continue;
    }
    if (!cur) {
      if (line.startsWith("diff --git ") && out.header.length) break; // second file: ignore
      if (line.startsWith("Binary files ") || line.startsWith("GIT binary patch")) out.binary = true;
      out.header.push(line);
      continue;
    }
    if (line.startsWith("\\")) {
      const prev = cur.lines[cur.lines.length - 1];
      if (prev) prev.noEol = true;
      continue;
    }
    const k = line[0];
    if (k === " " || k === "+" || k === "-") {
      const dl: DiffLine = {
        kind: k,
        text: line.slice(1),
        oldNo: k === "+" ? null : oldNo,
        newNo: k === "-" ? null : newNo,
        noEol: false
      };
      if (k !== "+") oldNo++;
      if (k !== "-") newNo++;
      cur.lines.push(dl);
    } else if (line.startsWith("diff --git ")) {
      break;
    }
  }
  return out;
}

/** stage = apply to index; unstage = reverse-apply to index;
 *  discard = reverse-apply to the worktree. */
export type PatchMode = "stage" | "unstage" | "discard";

/** Hunk index → selected line indices, or null for the whole hunk. */
export type Picks = Map<number, Set<number> | null>;

export interface BuiltPatch {
  patch: string;
  /** false = every change in the file is included (callers may use the
   *  file-level command instead, which also handles new/deleted files) */
  partial: boolean;
}

function headerPaths(header: string[]): { a: string | null; b: string | null } {
  let a: string | null = null;
  let b: string | null = null;
  for (const l of header) {
    if (l.startsWith("--- a/")) a = l.slice(6);
    else if (l.startsWith("+++ b/")) b = l.slice(6);
  }
  if (!a || !b) {
    const m = /^diff --git a\/(.+) b\/(.+)$/.exec(header.find((l) => l.startsWith("diff --git ")) ?? "");
    if (m) {
      a = a ?? m[1];
      b = b ?? m[2];
    }
  }
  return { a: a ?? b, b: b ?? a };
}

/** Build the patch for the picked hunks / lines (the approach Fork and
 *  git-cola use). Forward (stage): unpicked "-" lines become context and
 *  unpicked "+" lines are dropped. Reverse (unstage, discard): the mirror,
 *  unpicked "+" become context and unpicked "-" are dropped, so the
 *  patch's post-image still matches the index / worktree it is reversed
 *  against. Returns null when nothing would change. */
export function buildPatch(fd: FileDiff, picks: Picks, mode: PatchMode): BuiltPatch | null {
  if (fd.binary) return null;
  const reverse = mode !== "stage";
  let partial = false;
  let delta = 0; // (+) − (−) of the hunks emitted so far
  const body: string[] = [];
  fd.hunks.forEach((h, hi) => {
    const included = picks.has(hi);
    const sel = included ? picks.get(hi) : undefined;
    const kept: { kind: LineKind; text: string; noEol: boolean }[] = [];
    let changes = 0;
    h.lines.forEach((l, li) => {
      if (l.kind === " ") {
        kept.push(l);
        return;
      }
      const picked = included && (sel === null || sel === undefined || sel.has(li));
      if (picked) {
        kept.push(l);
        changes++;
        return;
      }
      partial = true;
      if (!reverse) {
        if (l.kind === "-") kept.push({ kind: " ", text: l.text, noEol: l.noEol });
      } else if (l.kind === "+") {
        kept.push({ kind: " ", text: l.text, noEol: l.noEol });
      }
    });
    if (!changes) return;
    const oldCount = kept.filter((l) => l.kind !== "+").length;
    const newCount = kept.filter((l) => l.kind !== "-").length;
    // a zero-length side's start is "the line before", hence the ±1 dance
    let oldStart: number;
    let newStart: number;
    if (!reverse) {
      oldStart = h.oldStart;
      const pos = (h.oldCount === 0 ? h.oldStart + 1 : h.oldStart) + delta;
      newStart = newCount === 0 ? pos - 1 : pos;
      if (oldCount === 0 && h.oldCount !== 0) oldStart = h.oldStart - 1;
    } else {
      newStart = h.newStart;
      const pos = (h.newCount === 0 ? h.newStart + 1 : h.newStart) - delta;
      oldStart = oldCount === 0 ? pos - 1 : pos;
      if (newCount === 0 && h.newCount !== 0) newStart = h.newStart - 1;
    }
    delta += newCount - oldCount;
    body.push(`@@ -${oldStart},${oldCount} +${newStart},${newCount} @@${h.section ? " " + h.section : ""}`);
    for (const l of kept) {
      body.push(l.kind + l.text);
      if (l.noEol) body.push("\\ No newline at end of file");
    }
  });
  if (!body.length) return null;
  const { a, b } = headerPaths(fd.header);
  const header: string[] = [];
  for (const l of fd.header) {
    if (l.startsWith("index ")) continue; // blob ids no longer match a reduced patch
    if (partial && /^(new file mode|deleted file mode|old mode|new mode) /.test(l)) continue;
    if (partial && l === "--- /dev/null" && b) {
      header.push(`--- a/${b}`);
      continue;
    }
    if (partial && l === "+++ /dev/null" && a) {
      header.push(`+++ b/${a}`);
      continue;
    }
    header.push(l);
  }
  return { patch: [...header, ...body].join("\n") + "\n", partial };
}

// ---- status → sections ----

export interface StatusEntry {
  path: string;
  origPath?: string | null;
  index: string;
  worktree: string;
  kind: "changed" | "untracked" | "conflict" | string;
}

export interface SectionItem {
  path: string;
  origPath: string | null;
  /** git status letter for this side (A, M, D, R, C, T, U) */
  letter: string;
  /** not in HEAD/index yet: untracked or intent-to-add (worktree side) */
  newFile: boolean;
  untracked: boolean;
}

/** Split porcelain entries into Fork-style Unstaged / Staged lists. */
export function splitStatus(files: StatusEntry[]): { unstaged: SectionItem[]; staged: SectionItem[] } {
  const unstaged: SectionItem[] = [];
  const staged: SectionItem[] = [];
  for (const f of files) {
    const orig = f.origPath ?? null;
    if (f.kind === "conflict") {
      unstaged.push({ path: f.path, origPath: null, letter: "U", newFile: false, untracked: false });
      continue;
    }
    if (f.kind === "untracked") {
      unstaged.push({ path: f.path, origPath: null, letter: "A", newFile: true, untracked: true });
      continue;
    }
    if (f.index && f.index !== ".") staged.push({ path: f.path, origPath: orig, letter: f.index, newFile: f.index === "A", untracked: false });
    if (f.worktree && f.worktree !== ".") {
      const ita = f.worktree === "A"; // intent-to-add entry
      unstaged.push({ path: f.path, origPath: null, letter: f.worktree, newFile: ita, untracked: false });
    }
  }
  const byPath = (x: SectionItem, y: SectionItem) => (x.path < y.path ? -1 : x.path > y.path ? 1 : 0);
  return { unstaged: unstaged.sort(byPath), staged: staged.sort(byPath) };
}

/** Badge text + tone for a status letter. */
export function badgeFor(letter: string): { text: string; tone: "add" | "mod" | "del" | "ren" | "conflict" } {
  switch (letter) {
    case "A":
    case "?":
      return { text: "+", tone: "add" };
    case "D":
      return { text: "−", tone: "del" };
    case "R":
    case "C":
      return { text: letter, tone: "ren" };
    case "U":
      return { text: "!", tone: "conflict" };
    default:
      return { text: "M", tone: "mod" };
  }
}

// ---- folder tree ----

export interface TreeRow {
  /** unique per section: "d:<dir>" or "f:<path>" */
  key: string;
  depth: number;
  name: string;
  dir: boolean;
  /** dir path ("src/lib") or file path */
  path: string;
  item?: SectionItem;
}

/** Flatten items into a depth-first folder tree, dirs before files at each
 *  level, hiding the contents of collapsed dirs. */
export function buildTree(items: SectionItem[], collapsed: ReadonlySet<string>): TreeRow[] {
  interface Node {
    dirs: Map<string, Node>;
    files: SectionItem[];
  }
  const root: Node = { dirs: new Map(), files: [] };
  for (const it of items) {
    const parts = it.path.split("/");
    let n = root;
    for (const seg of parts.slice(0, -1)) {
      let next = n.dirs.get(seg);
      if (!next) {
        next = { dirs: new Map(), files: [] };
        n.dirs.set(seg, next);
      }
      n = next;
    }
    n.files.push(it);
  }
  const rows: TreeRow[] = [];
  const walk = (n: Node, prefix: string, depth: number) => {
    for (const name of [...n.dirs.keys()].sort()) {
      const path = prefix ? `${prefix}/${name}` : name;
      rows.push({ key: `d:${path}`, depth, name, dir: true, path });
      if (!collapsed.has(path)) walk(n.dirs.get(name)!, path, depth + 1);
    }
    for (const f of [...n.files].sort((x, y) => (x.path < y.path ? -1 : 1))) {
      rows.push({ key: `f:${f.path}`, depth, name: f.path.split("/").pop() ?? f.path, dir: false, path: f.path, item: f });
    }
  };
  walk(root, "", 0);
  return rows;
}

/** Items under a dir (for section-level and folder actions). */
export function itemsUnder(items: SectionItem[], dir: string): SectionItem[] {
  return items.filter((i) => i.path === dir || i.path.startsWith(dir + "/"));
}

/** Visible file paths of a section's tree rows in display order. */
export function visibleFilePaths(rows: TreeRow[]): string[] {
  return rows.flatMap((r) => (r.item ? [r.item.path] : []));
}

/** Pick the next (+1) or previous (-1) visible file in a section's tree rows.
 *  Starts from the first (+1) or last (-1) file when `currentPath` is null or
 *  hidden, and clamps at the ends of the section. */
export function stepTreeFile(rows: TreeRow[], currentPath: string | null, dir: 1 | -1): SectionItem | null {
  const files = rows.flatMap((r) => (r.item ? [r.item] : []));
  if (!files.length) return null;
  if (currentPath === null) return dir > 0 ? files[0] : files[files.length - 1];
  const idx = files.findIndex((f) => f.path === currentPath);
  if (idx < 0) return dir > 0 ? files[0] : files[files.length - 1];
  return files[Math.max(0, Math.min(files.length - 1, idx + dir))];
}

// ---- multi-selection ----

export interface FileSelection {
  staged: boolean;
  /** Selected file paths within `staged` (always contains `primary` when non-empty). */
  paths: ReadonlySet<string>;
  /** Focused/active file (diff preview target + keyboard cursor). */
  primary: string | null;
  /** Range anchor for Shift-click / Shift+Arrow. */
  anchor: string | null;
}

export const EMPTY_SELECTION: FileSelection = {
  staged: false,
  paths: new Set(),
  primary: null,
  anchor: null
};

export function singleSelection(staged: boolean, path: string): FileSelection {
  return { staged, paths: new Set([path]), primary: path, anchor: path };
}

/** Update selection on a file click (plain, Shift range, or Cmd/Ctrl toggle). */
export function clickTreeFile(
  sel: FileSelection,
  rows: TreeRow[],
  staged: boolean,
  path: string,
  opts: { shift?: boolean; toggle?: boolean } = {}
): FileSelection {
  const shift = Boolean(opts.shift);
  const toggle = Boolean(opts.toggle);
  const order = visibleFilePaths(rows);
  const sameSection = sel.paths.size > 0 && sel.staged === staged;

  if (shift) {
    const anchorPath =
      sameSection && sel.anchor && order.includes(sel.anchor)
        ? sel.anchor
        : sameSection && sel.primary && order.includes(sel.primary)
          ? sel.primary
          : null;
    const b = order.indexOf(path);
    if (anchorPath !== null && b >= 0) {
      const a = order.indexOf(anchorPath);
      const lo = Math.min(a, b);
      const hi = Math.max(a, b);
      const next = toggle ? new Set(sel.paths) : new Set<string>();
      for (let i = lo; i <= hi; i++) next.add(order[i]);
      return { staged, paths: next, primary: path, anchor: anchorPath };
    }
    return singleSelection(staged, path);
  }

  if (toggle && sameSection) {
    const next = new Set(sel.paths);
    if (next.has(path)) {
      next.delete(path);
      if (!next.size) return EMPTY_SELECTION;
      let nextPrimary = sel.primary;
      if (!nextPrimary || !next.has(nextPrimary)) {
        const idx = order.indexOf(path);
        let picked: string | null = null;
        if (idx >= 0) {
          for (let i = idx + 1; i < order.length; i++) {
            if (next.has(order[i])) { picked = order[i]; break; }
          }
          if (!picked) {
            for (let i = idx - 1; i >= 0; i--) {
              if (next.has(order[i])) { picked = order[i]; break; }
            }
          }
        }
        nextPrimary = picked ?? [...next][next.size - 1];
      }
      const nextAnchor = sel.anchor && next.has(sel.anchor) ? sel.anchor : nextPrimary;
      return { staged, paths: next, primary: nextPrimary, anchor: nextAnchor };
    }
    next.add(path);
    return { staged, paths: next, primary: path, anchor: path };
  }

  return singleSelection(staged, path);
}

/** Right-click preserves an existing multi-selection when the clicked file is
 *  already part of it; otherwise selects just the clicked file. */
export function contextMenuSelection(sel: FileSelection, staged: boolean, path: string): FileSelection {
  if (sel.staged === staged && sel.paths.has(path)) return sel;
  return singleSelection(staged, path);
}

/** ArrowUp / ArrowDown (optionally with Shift to extend range). */
export function stepSelection(
  sel: FileSelection,
  rows: TreeRow[],
  staged: boolean,
  dir: 1 | -1,
  extend = false
): FileSelection | null {
  const currentPath = sel.paths.size > 0 && sel.staged === staged ? sel.primary : null;
  const next = stepTreeFile(rows, currentPath, dir);
  if (!next) return null;
  return extend ? clickTreeFile(sel, rows, staged, next.path, { shift: true }) : singleSelection(staged, next.path);
}

/** Select all visible files in a section (Cmd/Ctrl+A). */
export function selectAllInSection(rows: TreeRow[], staged: boolean, currentPrimary: string | null = null): FileSelection {
  const order = visibleFilePaths(rows);
  if (!order.length) return EMPTY_SELECTION;
  const primary = currentPrimary && order.includes(currentPrimary) ? currentPrimary : order[0];
  return { staged, paths: new Set(order), primary, anchor: order[0] };
}

/** Selected items in visible tree order (falling back to section order for any
 *  item inside a collapsed folder). */
export function selectedItemsInOrder(
  sel: FileSelection,
  staged: boolean,
  rows: TreeRow[],
  items: SectionItem[]
): SectionItem[] {
  if (sel.staged !== staged || !sel.paths.size) return [];
  const seen = new Set<string>();
  const ordered: SectionItem[] = [];
  for (const r of rows) {
    if (r.item && sel.paths.has(r.item.path)) {
      ordered.push(r.item);
      seen.add(r.item.path);
    }
  }
  for (const it of items) {
    if (sel.paths.has(it.path) && !seen.has(it.path)) ordered.push(it);
  }
  return ordered;
}

/** Reconcile selection after a status refresh: keep surviving items; when all
 *  selected items left the section, move to the next (else previous) visible
 *  neighbour in the same section, and only follow across once the section has
 *  no visible neighbour left. */
export function reconcileSelection(
  sel: FileSelection,
  prevOrder: string[],
  unstaged: SectionItem[],
  stagedItems: SectionItem[]
): FileSelection {
  if (!sel.paths.size || !sel.primary) return EMPTY_SELECTION;
  const curList = sel.staged ? stagedItems : unstaged;
  const otherList = sel.staged ? unstaged : stagedItems;
  const live = new Set(curList.map((i) => i.path));
  const surviving = new Set([...sel.paths].filter((p) => live.has(p)));

  if (surviving.size > 0) {
    const primary = live.has(sel.primary)
      ? sel.primary
      : (prevOrder.find((p) => surviving.has(p)) ?? [...surviving][0]);
    const anchor = sel.anchor && surviving.has(sel.anchor) ? sel.anchor : primary;
    return { staged: sel.staged, paths: surviving, primary, anchor };
  }

  let at = prevOrder.indexOf(sel.primary);
  if (at < 0) at = prevOrder.findIndex((p) => sel.paths.has(p));
  if (at >= 0) {
    for (let i = at + 1; i < prevOrder.length; i++) {
      if (live.has(prevOrder[i])) return singleSelection(sel.staged, prevOrder[i]);
    }
    for (let i = at - 1; i >= 0; i--) {
      if (live.has(prevOrder[i])) return singleSelection(sel.staged, prevOrder[i]);
    }
  }

  const other = otherList.find((i) => i.path === sel.primary) ?? otherList.find((i) => sel.paths.has(i.path));
  return other ? singleSelection(!sel.staged, other.path) : EMPTY_SELECTION;
}

// ---- copy helpers ----

/** File or folder basename from a slash-separated relative path. */
export function gitFileName(relPath: string): string {
  const clean = relPath.replace(/\/+$/, "");
  return clean.split("/").pop() ?? clean;
}

/** Join a workspace root with a workspace-relative slash path, preserving
 *  Windows separators and stripping Rust's `\\?\` verbatim prefix. */
export function gitFullPath(root: string, relPath: string): string {
  let base = root.trim();
  if (base.startsWith("\\\\?\\UNC\\")) base = "\\" + base.slice("\\\\?\\UNC\\".length);
  else if (base.startsWith("\\\\?\\")) base = base.slice("\\\\?\\".length);
  const rel = relPath.replace(/^\/+/, "");
  if (!base) return rel;
  const win = base.includes("\\") && !base.includes("/");
  if (win) {
    const cleanBase = base.replace(/\\+$/, "");
    const winRel = rel.replace(/\//g, "\\");
    return winRel ? `${cleanBase}\\${winRel}` : cleanBase;
  }
  const cleanBase = base.replace(/\/+$/, "");
  return rel ? `${cleanBase}/${rel}` : cleanBase;
}

export type CopyKind = "name" | "path" | "fullPath";

/** Format clipboard text for one or more workspace-relative paths. */
export function formatCopyText(root: string, relPaths: string[], kind: CopyKind): string {
  return relPaths
    .map((p) => (kind === "name" ? gitFileName(p) : kind === "path" ? p : gitFullPath(root, p)))
    .join("\n");
}

/** highlight.js language id from a file name (null = plain). */
export function hljsLanguageFor(path: string): string | null {
  const base = path.split("/").pop() ?? path;
  const ext = base.includes(".") ? base.split(".").pop()!.toLowerCase() : base.toLowerCase();
  const map: Record<string, string> = {
    ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript",
    js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
    svelte: "xml", vue: "xml", html: "xml", htm: "xml", xml: "xml", svg: "xml",
    rs: "rust", py: "python", rb: "ruby", go: "go", java: "java", kt: "kotlin", swift: "swift",
    c: "c", h: "c", cc: "cpp", cpp: "cpp", hpp: "cpp", cs: "csharp", php: "php",
    css: "css", scss: "scss", less: "less", json: "json", toml: "ini", ini: "ini",
    yml: "yaml", yaml: "yaml", md: "markdown", sh: "bash", bash: "bash", zsh: "bash",
    sql: "sql", lua: "lua", makefile: "makefile", dockerfile: "dockerfile"
  };
  return map[ext] ?? null;
}
