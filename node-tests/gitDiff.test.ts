// Git panel pure logic (src/lib/gitDiff.ts). Run: `npm test`.
// The patch-builder tests feed the generated patches to real `git apply`
// in a temp repo, so they check what git accepts, not just string shapes.
import { test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { badgeFor, buildPatch, buildTree, hljsLanguageFor, parseDiff, splitStatus, type Picks } from "../src/lib/gitDiff.ts";

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
