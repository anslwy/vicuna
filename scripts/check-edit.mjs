#!/usr/bin/env node
// Headless regression check for scripts/edit.mjs (the exact-anchor patcher).
// Covers the fail-closed contract and the two silent-partial bugs the tool
// exists to prevent: under-replacement with count > 1, and --diff lying.
//
//   node scripts/check-edit.mjs

import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { join } from "node:path";

const dir = mkdtempSync(join(tmpdir(), "vicuna-edit-check-"));
const tool = join(import.meta.dirname, "edit.mjs");
let failed = 0;

const run = (file, edits, flags = []) => {
  const spec = join(dir, `spec-${Math.random().toString(36).slice(2)}.json`);
  writeFileSync(spec, JSON.stringify({ file, edits }));
  const res = spawnSync(process.execPath, [tool, spec, ...flags], { encoding: "utf8" });
  return { code: res.status, out: `${res.stdout ?? ""}${res.stderr ?? ""}` };
};

const base = join(dir, "f.txt");
const reset = (text = "alpha\nbeta\ngamma\ndelta\n") => writeFileSync(base, text);
const body = () => readFileSync(base, "utf8");

const check = (name, cond, detail = "") => {
  if (cond) {
    console.log(`ok  ${name}`);
  } else {
    failed += 1;
    console.error(`FAIL ${name}${detail ? `\n${detail}` : ""}`);
  }
};

reset();
let r = run(base, [{ find: "beta", replace: "BETA" }]);
check("single anchor writes", r.code === 0 && body().includes("BETA"), r.out);

reset();
r = run(base, [{ find: "gamma", replace: "G" }, { find: "NOPE", replace: "z" }]);
check("missing anchor fails closed", r.code === 1 && body() === "alpha\nbeta\ngamma\ndelta\n", r.out);

reset("dup x\ndup x\n");
r = run(base, [{ find: "dup x", replace: "y" }]);
check("ambiguous anchor fails closed", r.code === 1 && body() === "dup x\ndup x\n", r.out);

reset();
r = run(base, [{ find: "beta", replace: "BETA" }], ["--check"]);
check("--check never writes", r.code === 0 && body().includes("beta"), r.out);

r = run(base, [{ find: "beta", replace: "BETA" }], ["--diff"]);
check("--diff shows a real diff", r.code === 0 && r.out.includes("-beta") && r.out.includes("+BETA"), r.out);
check("--diff is read-only", body().includes("beta") && !body().includes("BETA"), `file changed:\n${body()}`);

// spec.file may live next to the spec even when invoked from another cwd.
const nested = join(dir, "nested");
mkdirSync(nested, { recursive: true });
writeFileSync(join(nested, "t.txt"), "alpha\nbeta\n");
const nestedSpec = join(nested, "spec.json");
writeFileSync(nestedSpec, JSON.stringify({ file: "t.txt", edits: [{ find: "beta", replace: "BETA" }] }));
const nestedRes = spawnSync(process.execPath, [tool, nestedSpec], { cwd: dir, encoding: "utf8" });
check(
  "spec.file resolves beside the spec",
  nestedRes.status === 0 && readFileSync(join(nested, "t.txt"), "utf8").includes("BETA"),
  `${nestedRes.stdout ?? ""}${nestedRes.stderr ?? ""}`,
);

reset();
const inline = spawnSync(
  process.execPath,
  [tool, "--edits-json", JSON.stringify({ file: base, edits: [{ find: "beta", replace: "BETA" }] })],
  { encoding: "utf8" },
);
check("--edits-json works without a spec file", inline.status === 0 && body().includes("BETA"), `${inline.stdout ?? ""}${inline.stderr ?? ""}`);

reset();
const viaStdin = spawnSync(process.execPath, [tool, "-"], {
  input: JSON.stringify({ file: base, edits: [{ find: "beta", replace: "BETA" }] }),
  encoding: "utf8",
});
check("stdin spec works", viaStdin.status === 0 && body().includes("BETA"), `${viaStdin.stdout ?? ""}${viaStdin.stderr ?? ""}`);

reset("a\nx\nx\nx\nz\n");
r = run(base, [{ find: "x", replace: "y", count: 3 }]);
check("count>1 without --allow-multi fails", r.code === 1 && body() === "a\nx\nx\nx\nz\n", r.out);

r = run(base, [{ find: "x", replace: "y", count: 3 }], ["--allow-multi"]);
check("count>1 with --allow-multi replaces all", r.code === 0 && body() === "a\ny\ny\ny\nz\n", r.out);

r = run(base, [{ find: "y", replace: "z", count: 1 }], ["--allow-multi"]);
check("explicit count is authoritative", r.code === 1 && body() === "a\ny\ny\ny\nz\n", r.out);

rmSync(dir, { recursive: true, force: true });
if (failed) {
  console.error(`\n${failed} check(s) failed`);
  process.exit(1);
}
console.log("\nall edit.mjs checks passed");
