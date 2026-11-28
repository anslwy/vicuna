#!/usr/bin/env node
// Version for a rolling dev build (the `latest` prerelease):
//   <major>.<minor>.<patch+1>-dev.<YYYYMMDDHHMMSS>
// Semver sorts `0.1.0-dev.N` BELOW `0.1.0`, so the patch is bumped first: the
// dev build is then above the stable it was built after, below the next
// stable (which supersedes it), and the numeric timestamp keeps successive
// dev builds increasing. The timestamp is the commit's committer time (UTC),
// so every build job of one push computes the same version.
//
// Usage: node scripts/dev-version.mjs [base-version] [unix-seconds]
//   (defaults: package.json version, HEAD's committer time)
import { readFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

export function devVersion(base, unixSeconds) {
  const m = /^v?(\d+)\.(\d+)\.(\d+)/.exec(String(base).trim());
  if (!m) throw new Error(`not a semver version: ${base}`);
  const d = new Date(Number(unixSeconds) * 1000);
  if (Number.isNaN(d.getTime())) throw new Error(`bad timestamp: ${unixSeconds}`);
  const p = (n) => String(n).padStart(2, '0');
  const stamp = `${d.getUTCFullYear()}${p(d.getUTCMonth() + 1)}${p(d.getUTCDate())}${p(d.getUTCHours())}${p(d.getUTCMinutes())}${p(d.getUTCSeconds())}`;
  return `${m[1]}.${m[2]}.${Number(m[3]) + 1}-dev.${stamp}`;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const root = new URL('..', import.meta.url);
  const base = process.argv[2] || JSON.parse(readFileSync(new URL('package.json', root), 'utf8')).version;
  const unix = process.argv[3] || execFileSync('git', ['log', '-1', '--format=%ct'], { encoding: 'utf8' }).trim();
  process.stdout.write(devVersion(base, unix) + '\n');
}
