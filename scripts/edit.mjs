#!/usr/bin/env node
// Exact-anchor patcher for files too large to rewrite whole.
//
//   node scripts/edit.mjs <edits.json> [--check] [--diff] [--backup]
//
// edits.json:
//   { "file": "src/lib/ChatApp.svelte",
//     "edits": [ { "find": "exact old text", "replace": "new text" } ] }
//
// Contract (the whole point — a half-applied patch is the failure mode this
// exists to prevent):
//   * every `find` must occur EXACTLY `count` times (default 1) in the text as
//     it stands at that point, else nothing is written at all;
//   * `count: N` with N > 1 requires --allow-multi and replaces ALL N hits —
//     never one of them;
//   * all edits are applied in memory first; the file is written once, via a
//     temp file + rename, only after every edit validated;
//   * a missing or ambiguous anchor reports the occurrence count and line
//     numbers so the anchor can be widened instead of guessed.
//
// --check  validate + report, never write
// --diff   print a real unified diff (implies validate first, never writes)
// --backup write <file>.bak before replacing
// --allow-multi  permit replacing more than one occurrence (required when
//                count > 1; without an explicit count it replaces every hit)

import {
  readFileSync,
  writeFileSync,
  renameSync,
  copyFileSync,
  unlinkSync,
  existsSync,
  mkdtempSync,
  rmSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";

const argv = process.argv.slice(2);
const flags = new Set(argv.filter((a) => a.startsWith("--")));
const positional = argv.filter((a) => !a.startsWith("--"));

const die = (msg) => {
  console.error(`edit: ${msg}`);
  process.exit(1);
};

if (positional.length !== 1) {
  die("usage: node scripts/edit.mjs <edits.json> [--check] [--diff] [--backup]");
}

const specPath = resolve(positional[0]);
if (!existsSync(specPath)) die(`no such spec file: ${relative(process.cwd(), specPath)}`);

let spec;
try {
  spec = JSON.parse(readFileSync(specPath, "utf8"));
} catch (e) {
  die(`spec is not valid JSON: ${e.message}`);
}

if (!spec || typeof spec.file !== "string" || !spec.file.trim()) {
  die('spec needs { "file": "<path>", "edits": [...] }');
}
if (!Array.isArray(spec.edits) || spec.edits.length === 0) {
  die("spec.edits must be a non-empty array");
}

// `spec.file` is normally repo-relative (cwd); accept a path relative to the
// spec file too, so `cd fixtures && node ../../scripts/edit.mjs spec.json`
// does not silently break every spec.
const fromCwd = resolve(spec.file);
const fromSpecDir = resolve(dirname(specPath), spec.file);
const target = existsSync(fromCwd) ? fromCwd : existsSync(fromSpecDir) ? fromSpecDir : null;
if (!target) {
  die(
    `no such target file: spec.file = ${spec.file} (tried ${fromCwd} and ${fromSpecDir})`,
  );
}

const original = readFileSync(target, "utf8");
let text = original;

const occurrences = (haystack, needle) => {
  const at = [];
  let i = haystack.indexOf(needle);
  while (i !== -1) {
    at.push(i);
    i = haystack.indexOf(needle, i + needle.length);
  }
  return at;
};

const lineOf = (s, idx) => s.slice(0, idx).split("\n").length;

/** Print a real unified diff of the original vs. patched text. */
function printDiff(before, after, label) {
  const dir = mkdtempSync(join(tmpdir(), "vicuna-edit-"));
  const aPath = join(dir, "before");
  const bPath = join(dir, "after");
  try {
    writeFileSync(aPath, before, "utf8");
    writeFileSync(bPath, after, "utf8");
    let res = spawnSync("diff", ["-u", "--label", `a/${label}`, "--label", `b/${label}`, aPath, bPath], {
      encoding: "utf8",
    });
    if (res.status !== 0 && res.status !== 1) {
      // older/no-GNU diff: retry without labels, then fall back to plain text
      res = spawnSync("diff", ["-u", aPath, bPath], { encoding: "utf8" });
    }
    if (res.error || res.status === null || res.status > 1) {
      console.log(`\n(diff unavailable: ${res.error?.message ?? res.stderr?.trim() ?? "diff did not run"})`);
      return;
    }
    if (res.status === 0) {
      console.log("\n(no textual diff)");
      return;
    }
    const out = (res.stdout || "")
      .replaceAll(aPath, `a/${label}`)
      .replaceAll(bPath, `b/${label}`);
    if (out.trim()) console.log(out.trimEnd());
  } finally {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {}
  }
}

// Validate + apply every edit against the running text, collecting a report.
const applied = [];
const problems = [];

spec.edits.forEach((edit, n) => {
  const label = `edit #${n + 1}`;
  if (!edit || typeof edit.find !== "string" || edit.find.length === 0) {
    problems.push(`${label}: missing/empty "find"`);
    return;
  }
  if (typeof edit.replace !== "string") {
    problems.push(`${label}: missing "replace" (use "" to delete)`);
    return;
  }

  const hits = occurrences(text, edit.find);
  const explicitCount = Number.isInteger(edit.count);
  const allowMulti = flags.has("--allow-multi");
  let want = explicitCount ? edit.count : 1;

  if (explicitCount && want < 1) {
    problems.push(`${label}: "count" must be >= 1`);
    return;
  }
  if (hits.length === 0) {
    problems.push(
      `${label}: anchor not found. First line of anchor: ${JSON.stringify(
        edit.find.split("\n")[0],
      )}`,
    );
    return;
  }
  // Without an explicit count, --allow-multi means "replace every hit".
  if (!explicitCount && allowMulti && hits.length > 1) want = hits.length;
  if (hits.length !== want) {
    const lines = hits.slice(0, 12).map((h) => lineOf(text, h)).join(", ");
    problems.push(
      `${label}: anchor matched ${hits.length}x, expected ${want}. Lines: ${lines}` +
        (hits.length > 12 ? ` …(+${hits.length - 12} more)` : "") +
        " — widen the anchor with surrounding context.",
    );
    return;
  }
  // Replacing only the first of N hits is the silent-partial failure this
  // tool exists to prevent: multi-hit replacement must be explicit.
  if (want > 1 && !allowMulti) {
    const lines = hits.slice(0, 12).map((h) => lineOf(text, h)).join(", ");
    problems.push(
      `${label}: anchor matched ${hits.length}x but --allow-multi was not passed. ` +
        `Lines: ${lines}` +
        (hits.length > 12 ? ` …(+${hits.length - 12} more)` : "") +
        " — add --allow-multi to replace all of them atomically, or widen the anchor to one hit.",
    );
    return;
  }

  const line = lineOf(text, hits[0]);
  if (want > 1) {
    text = text.split(edit.find).join(edit.replace);
  } else {
    text = text.slice(0, hits[0]) + edit.replace + text.slice(hits[0] + edit.find.length);
  }
  applied.push({ n: n + 1, line, times: hits.length });
});

if (problems.length) {
  console.error("edit: nothing written — the spec did not validate:");
  for (const p of problems) console.error(`  - ${p}`);
  process.exit(1);
}

const changed = text !== original;
const fileLabel = relative(process.cwd(), target) || target;

for (const a of applied) {
  console.log(`ok edit #${a.n} at line ${a.line}${a.times > 1 ? ` (${a.times} matches)` : ""}`);
}

if (!changed) {
  console.log(`no change: ${fileLabel} already matches the spec`);
  process.exit(0);
}

if (flags.has("--diff")) {
  printDiff(original, text, fileLabel);
  process.exit(0);
}

if (flags.has("--check")) {
  console.log(`check: ${fileLabel} WOULD change (${applied.length} edit(s)); not written`);
  process.exit(0);
}

if (flags.has("--backup")) {
  const bak = `${target}.bak`;
  copyFileSync(target, bak);
  console.log(`backup: ${relative(process.cwd(), bak)}`);
}

const tmp = join(dirname(target), `.${target.split("/").pop()}.edit-${process.pid}`);
try {
  writeFileSync(tmp, text, "utf8");
  renameSync(tmp, target);
} catch (e) {
  try {
    if (existsSync(tmp)) unlinkSync(tmp);
  } catch {}
  die(`write failed, original left intact: ${e.message}`);
}

const delta = text.length - original.length;
console.log(
  `wrote ${fileLabel}: ${applied.length} edit(s), ${delta >= 0 ? "+" : ""}${delta} bytes`,
);
