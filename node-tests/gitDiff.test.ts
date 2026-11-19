// Git panel pure logic (src/lib/gitDiff.ts). Run: `npm test`.
// The patch-builder tests feed the generated patches to real `git apply`
// in a temp repo, so they check what git accepts, not just string shapes.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
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
  parseDiff,
  reconcileSelection,
  selectAllInSection,
  selectedItemsInOrder,
  singleSelection,
  splitStatus,
  stepSelection,
  stepTreeFile,
  type Picks
} from "../src/lib/gitDiff.ts";

function repo() {
  const dir = mkdtempSync(join(tmpdir(), "vicuna-gitdiff-"));
  const git = (args: string[], input?: string) =>
    execFileSync("git", ["-c", "core.quotepath=false", ...args], { cwd: dir, input, encoding: "utf8" });
  git(["init", "-q"]);
  git(["config", "user.email", "t@t"]);
  git(["config", "user.name", "t"]);
  return {
    dir,
    git,
    write: (p: string, s: string) => writeFileSync(join(dir, p), s),
    read: (p: string) => readFileSync(join(dir, p), "utf8"),
    index: (p: string) => git(["show", `:${p}`]),
    diff: (p: string, cached = false) => git(["diff", "--no-ext-diff", "--src-prefix=a/", "--dst-prefix=b/", ...(cached ? ["--cached"] : []), "--", p]),
    apply: (patch: string, opts: string[]) => git(["apply", "--recount", ...opts, "-"], patch),
    done: () => rmSync(dir, { recursive: true, force: true })
  };
}

const lines = (n: number) => Array.from({ length: n }, (_, i) => `line${i + 1}`).join("\n") + "\n";

test("parseDiff numbers lines and reads hunk headers", () => {
  const fd = parseDiff(
    "diff --git a/f b/f\nindex 1..2 100644\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@ fn main\n a\n-b\n+B\n c\n\\ No newline at end of file\n"
  );
  assert.equal(fd.hunks.length, 1);
  const h = fd.hunks[0];
  assert.equal(h.section, "fn main");
  assert.deepEqual(
    h.lines.map((l) => [l.kind, l.text, l.oldNo, l.newNo]),
    [[" ", "a", 1, 1], ["-", "b", 2, null], ["+", "B", null, 2], [" ", "c", 3, 3]]
  );
  assert.equal(h.lines[3].noEol, true);
  assert.equal(parseDiff("diff --git a/x b/x\nBinary files a/x and b/x differ\n").binary, true);
});

test("stage single lines and whole hunks into the index", () => {
  const r = repo();
  try {
    r.write("f.txt", lines(30));
    r.git(["add", "."]);
    r.git(["commit", "-qm", "i"]);
    // two hunks: change line2, and add a line after line25
    const next = lines(30).replace("line2\n", "LINE2\nextra\n").replace("line25\n", "line25\nadded\n");
    r.write("f.txt", next);
    const fd = parseDiff(r.diff("f.txt"));
    assert.equal(fd.hunks.length, 2);

    // only the "+extra" line of hunk 0 (the "-line2" stays, "+LINE2" dropped)
    const h0 = fd.hunks[0];
    const extra = h0.lines.findIndex((l) => l.text === "extra");
    const p1 = buildPatch(fd, new Map([[0, new Set([extra])]]) as Picks, "stage")!;
    assert.equal(p1.partial, true);
    r.apply(p1.patch, ["--cached"]);
    assert.equal(r.index("f.txt"), lines(30).replace("line2\n", "line2\nextra\n"));

    // second hunk whole, built from the fresh worktree diff
    const fd2 = parseDiff(r.diff("f.txt"));
    const hi = fd2.hunks.findIndex((h) => h.lines.some((l) => l.text === "added"));
    r.apply(buildPatch(fd2, new Map([[hi, null]]), "stage")!.patch, ["--cached"]);
    assert.ok(r.index("f.txt").includes("line25\nadded\n"));
    assert.ok(!r.index("f.txt").includes("LINE2"));
  } finally {
    r.done();
  }
});

test("unstage and discard selected lines (reverse patches)", () => {
  const r = repo();
  try {
    r.write("f.txt", "a\nb\nc\n");
    r.git(["add", "."]);
    r.git(["commit", "-qm", "i"]);
    r.write("f.txt", "a\nB\nc\nd\n");
    r.git(["add", "."]);
    // unstage only "+d" from the staged diff
    const st = parseDiff(r.diff("f.txt", true));
    const d = st.hunks[0].lines.findIndex((l) => l.text === "d");
    r.apply(buildPatch(st, new Map([[0, new Set([d])]]), "unstage")!.patch, ["--cached", "--reverse"]);
    assert.equal(r.index("f.txt"), "a\nB\nc\n");

    // worktree still has d; discard only the "-b/+B" pair from the unstaged diff (none: B is staged)
    // so make a fresh worktree change and discard one of two lines
    r.write("f.txt", "a\nB\nc\nd\ne\n");
    const un = parseDiff(r.diff("f.txt"));
    const e = un.hunks[0].lines.findIndex((l) => l.text === "e");
    r.apply(buildPatch(un, new Map([[0, new Set([e])]]), "discard")!.patch, ["--reverse"]);
    assert.equal(r.read("f.txt"), "a\nB\nc\nd\n");
  } finally {
    r.done();
  }
});

test("partial staging of an untracked file rewrites /dev/null headers", () => {
  const r = repo();
  try {
    r.write("seed", "s\n");
    r.git(["add", "."]);
    r.git(["commit", "-qm", "i"]);
    r.write("n.txt", "x\ny\nz\n");
    let text = "";
    try {
      r.git(["diff", "--no-index", "--src-prefix=a/", "--dst-prefix=b/", "--", "/dev/null", "n.txt"]);
    } catch (e: any) {
      text = e.stdout; // exit 1 = files differ
    }
    const fd = parseDiff(text);
    const y = fd.hunks[0].lines.findIndex((l) => l.text === "y");
    const built = buildPatch(fd, new Map([[0, new Set([y])]]), "stage")!;
    assert.ok(built.patch.includes("--- a/n.txt"));
    assert.ok(!built.patch.includes("new file mode"));
    r.git(["add", "-N", "--", "n.txt"]);
    r.apply(built.patch, ["--cached"]);
    assert.equal(r.index("n.txt"), "y\n");

    // whole-file selection keeps the new-file header and reports !partial
    const full = buildPatch(fd, new Map([[0, null]]), "stage")!;
    assert.equal(full.partial, false);
    assert.ok(full.patch.includes("--- /dev/null"));
  } finally {
    r.done();
  }
});

test("buildPatch returns null when nothing is picked", () => {
  const fd = parseDiff("--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n");
  assert.equal(buildPatch(fd, new Map(), "stage"), null);
  assert.equal(buildPatch(fd, new Map([[0, new Set<number>()]]), "stage"), null);
});

test("splitStatus sorts entries into Unstaged / Staged like Fork", () => {
  const { unstaged, staged } = splitStatus([
    { path: "src/a.ts", index: "M", worktree: "M", kind: "changed" },
    { path: "new.txt", index: ".", worktree: "?", kind: "untracked" },
    { path: "ita.txt", index: ".", worktree: "A", kind: "changed" },
    { path: "b.ts", origPath: "old.ts", index: "R", worktree: ".", kind: "changed" },
    { path: "c.ts", index: "U", worktree: "U", kind: "conflict" }
  ]);
  assert.deepEqual(unstaged.map((i) => [i.path, i.letter, i.newFile, i.untracked]), [
    ["c.ts", "U", false, false],
    ["ita.txt", "A", true, false],
    ["new.txt", "A", true, true],
    ["src/a.ts", "M", false, false]
  ]);
  assert.deepEqual(staged.map((i) => [i.path, i.letter, i.origPath]), [
    ["b.ts", "R", "old.ts"],
    ["src/a.ts", "M", null]
  ]);
  assert.equal(badgeFor("A").text, "+");
  assert.equal(badgeFor("D").tone, "del");
});

test("buildTree nests folders first and hides collapsed dirs", () => {
  const items = ["src/lib/a.ts", "src/lib/b.ts", "src/x.ts", "README.md", "node-tests/t.ts"].map((path) => ({
    path, origPath: null, letter: "M", newFile: false, untracked: false
  }));
  const rows = buildTree(items, new Set());
  assert.deepEqual(rows.map((r) => `${"  ".repeat(r.depth)}${r.name}${r.dir ? "/" : ""}`), [
    "node-tests/", "  t.ts", "src/", "  lib/", "    a.ts", "    b.ts", "  x.ts", "README.md"
  ]);
  const collapsed = buildTree(items, new Set(["src/lib"]));
  assert.ok(!collapsed.some((r) => r.name === "a.ts"));
  assert.ok(collapsed.some((r) => r.key === "d:src/lib"));
  assert.equal(hljsLanguageFor("src/lib/ChatApp.svelte"), "xml");
  assert.equal(hljsLanguageFor("x.unknown"), null);
});

test("stepTreeFile navigates visible files and clamps at section ends", () => {
  assert.equal(stepTreeFile([], null, 1), null);
  const items = ["src/a.ts", "src/b.ts", "README.md"].map((path) => ({
    path, origPath: null, letter: "M", newFile: false, untracked: false
  }));
  const rows = buildTree(items, new Set());
  // visible order: src/a.ts, src/b.ts, README.md (dir row skipped)
  assert.equal(stepTreeFile(rows, null, 1)?.path, "src/a.ts");
  assert.equal(stepTreeFile(rows, null, -1)?.path, "README.md");
  assert.equal(stepTreeFile(rows, "src/a.ts", 1)?.path, "src/b.ts");
  assert.equal(stepTreeFile(rows, "src/b.ts", 1)?.path, "README.md");
  assert.equal(stepTreeFile(rows, "README.md", 1)?.path, "README.md");
  assert.equal(stepTreeFile(rows, "README.md", -1)?.path, "src/b.ts");
  assert.equal(stepTreeFile(rows, "src/a.ts", -1)?.path, "src/a.ts");

  const collapsed = buildTree(items, new Set(["src"]));
  assert.equal(stepTreeFile(collapsed, "src/a.ts", 1)?.path, "README.md");
});

const mkItems = (paths: string[]) =>
  paths.map((path) => ({ path, origPath: null, letter: "M", newFile: false, untracked: false }));

test("clickTreeFile: plain, Shift range (2nd -> 5th selects 2,3,4,5), reverse Shift, Cmd/Ctrl toggle", () => {
  // visible order: dir/a.ts, dir/b.ts, f3.ts, f4.ts, f5.ts, f6.ts (1..6)
  const items = mkItems(["dir/a.ts", "dir/b.ts", "f3.ts", "f4.ts", "f5.ts", "f6.ts"]);
  const rows = buildTree(items, new Set());

  // select 2nd item (dir/b.ts)
  let sel = clickTreeFile(EMPTY_SELECTION, rows, false, "dir/b.ts");
  assert.deepEqual([...sel.paths], ["dir/b.ts"]);
  assert.equal(sel.primary, "dir/b.ts");
  assert.equal(sel.anchor, "dir/b.ts");

  // shift-select 5th item (f5.ts) => selects 2nd, 3rd, 4th, 5th
  sel = clickTreeFile(sel, rows, false, "f5.ts", { shift: true });
  assert.deepEqual(
    selectedItemsInOrder(sel, false, rows, items).map((i) => i.path),
    ["dir/b.ts", "f3.ts", "f4.ts", "f5.ts"]
  );
  assert.equal(sel.primary, "f5.ts");
  assert.equal(sel.anchor, "dir/b.ts");

  // shifting again adjusts from the same anchor (2nd -> 3rd => 2,3)
  sel = clickTreeFile(sel, rows, false, "f3.ts", { shift: true });
  assert.deepEqual(
    selectedItemsInOrder(sel, false, rows, items).map((i) => i.path),
    ["dir/b.ts", "f3.ts"]
  );

  // reverse Shift (5th -> 2nd => 2,3,4,5)
  sel = clickTreeFile(EMPTY_SELECTION, rows, false, "f5.ts");
  sel = clickTreeFile(sel, rows, false, "dir/b.ts", { shift: true });
  assert.deepEqual(
    selectedItemsInOrder(sel, false, rows, items).map((i) => i.path),
    ["dir/b.ts", "f3.ts", "f4.ts", "f5.ts"]
  );
  assert.equal(sel.anchor, "f5.ts");
  assert.equal(sel.primary, "dir/b.ts");

  // Cmd/Ctrl-click removes an item from the selection without losing the rest
  sel = clickTreeFile(sel, rows, false, "f3.ts", { toggle: true });
  assert.deepEqual(
    selectedItemsInOrder(sel, false, rows, items).map((i) => i.path),
    ["dir/b.ts", "f4.ts", "f5.ts"]
  );

  // Cmd/Ctrl-click adds a non-contiguous item
  sel = clickTreeFile(sel, rows, false, "f6.ts", { toggle: true });
  assert.deepEqual(
    selectedItemsInOrder(sel, false, rows, items).map((i) => i.path),
    ["dir/b.ts", "f4.ts", "f5.ts", "f6.ts"]
  );

  // Cmd/Ctrl-click removing the last selected item clears selection
  const one = singleSelection(false, "f6.ts");
  assert.equal(clickTreeFile(one, rows, false, "f6.ts", { toggle: true }).paths.size, 0);

  // Shift-click in the other section starts a fresh selection there
  const cross = clickTreeFile(sel, rows, true, "f3.ts", { shift: true });
  assert.equal(cross.staged, true);
  assert.deepEqual([...cross.paths], ["f3.ts"]);

  // Shift-click skips files hidden inside collapsed folders
  const collapsed = buildTree(items, new Set(["dir"]));
  let csel = clickTreeFile(EMPTY_SELECTION, collapsed, false, "f3.ts");
  csel = clickTreeFile(csel, collapsed, false, "f5.ts", { shift: true });
  assert.deepEqual(
    selectedItemsInOrder(csel, false, collapsed, items).map((i) => i.path),
    ["f3.ts", "f4.ts", "f5.ts"]
  );
});

test("contextMenuSelection keeps multi-selection on right-clicking a selected item", () => {
  const items = mkItems(["a.ts", "b.ts", "c.ts", "d.ts"]);
  const rows = buildTree(items, new Set());
  let sel = clickTreeFile(EMPTY_SELECTION, rows, false, "a.ts");
  sel = clickTreeFile(sel, rows, false, "c.ts", { shift: true });

  // right-clicking b.ts (already selected) preserves a, b, c
  const kept = contextMenuSelection(sel, false, "b.ts");
  assert.equal(kept, sel);
  assert.deepEqual([...kept.paths].sort(), ["a.ts", "b.ts", "c.ts"]);

  // right-clicking d.ts (outside selection) switches to just d.ts
  const switched = contextMenuSelection(sel, false, "d.ts");
  assert.deepEqual([...switched.paths], ["d.ts"]);

  // right-clicking in the other section switches section
  const other = contextMenuSelection(sel, true, "b.ts");
  assert.equal(other.staged, true);
  assert.deepEqual([...other.paths], ["b.ts"]);
});

test("stepSelection (Shift+Arrow) and selectAllInSection", () => {
  const items = mkItems(["a.ts", "b.ts", "c.ts", "d.ts"]);
  const rows = buildTree(items, new Set());
  let sel = singleSelection(false, "b.ts");
  sel = stepSelection(sel, rows, false, 1, true)!;
  sel = stepSelection(sel, rows, false, 1, true)!;
  assert.deepEqual([...sel.paths].sort(), ["b.ts", "c.ts", "d.ts"]);
  assert.equal(sel.anchor, "b.ts");
  assert.equal(sel.primary, "d.ts");

  // Shift+ArrowUp shrinks range back toward anchor
  sel = stepSelection(sel, rows, false, -1, true)!;
  assert.deepEqual([...sel.paths].sort(), ["b.ts", "c.ts"]);

  // plain ArrowDown collapses to single next item
  sel = stepSelection(sel, rows, false, 1, false)!;
  assert.deepEqual([...sel.paths], ["d.ts"]);

  // select all
  const all = selectAllInSection(rows, false, "c.ts");
  assert.deepEqual([...all.paths].sort(), ["a.ts", "b.ts", "c.ts", "d.ts"]);
  assert.equal(all.primary, "c.ts");
});

test("reconcileSelection hands view to neighbour after bulk stage/discard, or follows across when empty", () => {
  const prevOrder = ["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"];
  const sel = { staged: false, paths: new Set(["b.ts", "c.ts", "d.ts"]), primary: "d.ts", anchor: "b.ts" };

  // bulk staged b, c, d => e.ts is next neighbour in unstaged
  const r1 = reconcileSelection(sel, prevOrder, mkItems(["a.ts", "e.ts"]), mkItems(["b.ts", "c.ts", "d.ts"]));
  assert.equal(r1.staged, false);
  assert.deepEqual([...r1.paths], ["e.ts"]);

  // bulk staged b, c, d, e => falls back to a.ts before them
  const selEnd = { staged: false, paths: new Set(["b.ts", "c.ts", "d.ts", "e.ts"]), primary: "e.ts", anchor: "b.ts" };
  const r2 = reconcileSelection(selEnd, prevOrder, mkItems(["a.ts"]), mkItems(["b.ts", "c.ts", "d.ts", "e.ts"]));
  assert.equal(r2.staged, false);
  assert.deepEqual([...r2.paths], ["a.ts"]);

  // section became empty after staging => follows primary across to staged
  const selAll = { staged: false, paths: new Set(["a.ts", "b.ts"]), primary: "b.ts", anchor: "a.ts" };
  const r3 = reconcileSelection(selAll, ["a.ts", "b.ts"], [], mkItems(["a.ts", "b.ts"]));
  assert.equal(r3.staged, true);
  assert.deepEqual([...r3.paths], ["b.ts"]);

  // section became empty after discarding => clears selection
  const r4 = reconcileSelection(selAll, ["a.ts", "b.ts"], [], []);
  assert.equal(r4.paths.size, 0);

  // partial external removal keeps surviving selected files
  const r5 = reconcileSelection(sel, prevOrder, mkItems(["a.ts", "b.ts", "d.ts", "e.ts"]), []);
  assert.equal(r5.staged, false);
  assert.deepEqual([...r5.paths].sort(), ["b.ts", "d.ts"]);
});

test("gitFileName, gitFullPath and formatCopyText handle POSIX, Windows and multi-file copies", () => {
  assert.equal(gitFileName("src/lib/GitPanel.svelte"), "GitPanel.svelte");
  assert.equal(gitFileName("README.md"), "README.md");
  assert.equal(gitFileName("src/lib/"), "lib");

  assert.equal(gitFullPath("/repo/vicuna/", "src/lib/GitPanel.svelte"), "/repo/vicuna/src/lib/GitPanel.svelte");
  assert.equal(gitFullPath("\\\\?\\C:\\dev\\vicuna\\", "src/lib/GitPanel.svelte"), "C:\\dev\\vicuna\\src\\lib\\GitPanel.svelte");
  assert.equal(gitFullPath("\\\\?\\UNC\\srv\\share\\repo", "a/b.ts"), "\\srv\\share\\repo\\a\\b.ts");

  const paths = ["src/a.ts", "src/lib/b.ts", "README.md"];
  assert.equal(formatCopyText("/repo", paths, "name"), "a.ts\nb.ts\nREADME.md");
  assert.equal(formatCopyText("/repo", paths, "path"), "src/a.ts\nsrc/lib/b.ts\nREADME.md");
  assert.equal(formatCopyText("/repo", paths, "fullPath"), "/repo/src/a.ts\n/repo/src/lib/b.ts\n/repo/README.md");
});
