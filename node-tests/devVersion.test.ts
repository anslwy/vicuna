import { test } from "node:test";
import assert from "node:assert/strict";
// @ts-ignore plain .mjs script
import { devVersion } from "../scripts/dev-version.mjs";

// minimal semver precedence for x.y.z[-pre] (numeric ids compare numerically)
function cmp(a: string, b: string): number {
  const [ca, pa] = a.split("-", 2) as [string, string | undefined];
  const [cb, pb] = b.split("-", 2) as [string, string | undefined];
  const na = ca.split(".").map(Number), nb = cb.split(".").map(Number);
  for (let i = 0; i < 3; i++) if (na[i] !== nb[i]) return na[i] - nb[i];
  if (pa === undefined || pb === undefined) return pa === pb ? 0 : pa === undefined ? 1 : -1;
  const ia = pa.split("."), ib = pb.split(".");
  for (let i = 0; i < Math.max(ia.length, ib.length); i++) {
    if (ia[i] === undefined) return -1;
    if (ib[i] === undefined) return 1;
    const x = /^\d+$/.test(ia[i]) ? Number(ia[i]) : NaN, y = /^\d+$/.test(ib[i]) ? Number(ib[i]) : NaN;
    if (!Number.isNaN(x) && !Number.isNaN(y)) { if (x !== y) return x - y; }
    else if (ia[i] !== ib[i]) return ia[i] < ib[i] ? -1 : 1;
  }
  return 0;
}

test("dev version bumps the patch and stamps UTC commit time", () => {
  assert.equal(devVersion("0.0.100", Date.UTC(2026, 10, 28, 9, 5, 7) / 1000), "0.0.101-dev.20261128090507");
  assert.equal(devVersion("v1.2.9", 0), "1.2.10-dev.19700101000000");
  assert.throws(() => devVersion("latest", 0));
});

test("dev sorts above its stable, below the next stable, and increases", () => {
  const a = devVersion("0.0.100", 1_800_000_000);
  const b = devVersion("0.0.100", 1_800_000_060);
  assert.ok(cmp(a, "0.0.100") > 0);
  assert.ok(cmp(a, "0.0.101") < 0);
  assert.ok(cmp(b, a) > 0);
  assert.ok(cmp("0.0.101-dev.1", "0.0.101") < 0, "why the patch is bumped");
});
