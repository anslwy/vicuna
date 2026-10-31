// Headless checks for src/lib/update.ts (pure logic). Run: node scripts/check-update.mjs
import {
  isNewerRelease,
  pickAssetForPlatform,
  releaseTimestamp,
  formatAssetSize,
  formatBuildDate
} from "../src/lib/update.ts";

let failures = 0;
function eq(actual, expected, label) {
  const a = JSON.stringify(actual);
  const e = JSON.stringify(expected);
  if (a !== e) {
    failures++;
    console.error(`FAIL ${label}: got ${a}, want ${e}`);
  } else {
    console.log(`ok ${label}`);
  }
}

const assets = [
  { name: "Vicuna_0.1.0_universal.dmg", url: "u-dmg", size: 100 * 1024 * 1024 },
  { name: "Vicuna_0.1.0_setup.exe", url: "u-exe", size: 90 * 1024 * 1024 },
  { name: "Vicuna_0.1.0_amd64.AppImage", url: "u-app", size: 95 * 1024 * 1024 },
  { name: "Vicuna_0.1.0_amd64.deb", url: "u-deb", size: 80 * 1024 * 1024 },
  { name: "README.txt", url: "u-txt", size: 10 }
];

// newer-release gating on dates
eq(isNewerRelease("2026-10-31T12:00:00Z", 1793000000), true, "newer published wins");
eq(isNewerRelease("2026-10-31T12:00:00Z", 1793500000), false, "older published loses");
eq(isNewerRelease("not-a-date", 100), false, "bad published never claims");
eq(isNewerRelease("2026-10-31T12:00:00Z", 0), true, "unknown local build defers to release");

// platform picking
eq(pickAssetForPlatform(assets, "macos")?.url, "u-dmg", "macos takes universal dmg");
eq(pickAssetForPlatform(assets, "windows")?.url, "u-exe", "windows takes setup exe");
eq(pickAssetForPlatform(assets, "linux")?.url, "u-app", "linux takes appimage");
eq(
  pickAssetForPlatform(assets.filter((a) => !a.name.endsWith(".AppImage")), "linux")?.url,
  "u-deb",
  "linux falls back to deb"
);
eq(pickAssetForPlatform([], "macos"), null, "empty assets -> null");
eq(pickAssetForPlatform(assets, "other"), null, "unknown platform -> null");
// universal preferred over arch-specific dmgs
eq(
  pickAssetForPlatform(
    [{ name: "Vicuna_arm64.dmg", url: "u-arm" }, { name: "Vicuna_universal.dmg", url: "u-uni" }],
    "macos"
  )?.url,
  "u-uni",
  "universal dmg preferred"
);

// formatting helpers
eq(formatAssetSize(84.2 * 1024 * 1024).endsWith("MB"), true, "asset size unit");
eq(formatAssetSize(null), "", "null size empty");
eq(formatBuildDate(0), "", "zero timestamp empty");
eq(formatBuildDate(1793500000).length > 0, true, "valid timestamp formats");

// filtering robustness: junk entries never picked
eq(pickAssetForPlatform([{ name: "notes.txt", url: "u" }], "linux"), null, "junk ignored");

// freshness stamp prefers the platform asset's upload time over the frozen
// release published_at (the rolling `latest` release never re-publishes)
const rolling = {
  tag: "latest",
  publishedAt: "2026-10-28T18:29:59Z",
  updatedAt: "2026-10-31T15:53:28Z",
  htmlUrl: "https://github.com/anslwy/vicuna/releases/tag/latest",
  assets: [
    { name: "Vicuna_0.1.0_universal.dmg", url: "u-dmg", uploadedAt: "2026-10-31T15:53:28Z" },
    { name: "Vicuna_0.1.0_setup.exe", url: "u-exe", uploadedAt: "2026-10-31T15:52:16Z" }
  ]
};
eq(releaseTimestamp(rolling, "macos"), "2026-10-31T15:53:28Z", "asset stamp wins over published_at");
eq(
  releaseTimestamp(rolling, "windows"),
  "2026-10-31T15:52:16Z",
  "stamp is per-platform, not one global date"
);
// a build compiled after the frozen publish date but BEFORE the fresh asset
// is still behind: this is the bug the asset stamp fixes
eq(
  isNewerRelease(releaseTimestamp(rolling, "macos"), Date.parse("2026-10-29T00:00:00Z") / 1000),
  true,
  "fresh installer beats a build newer than published_at"
);
eq(
  isNewerRelease(releaseTimestamp(rolling, "macos"), Date.parse("2026-11-01T00:00:00Z") / 1000),
  false,
  "build newer than installer -> up to date"
);
eq(
  releaseTimestamp({ tag: "latest", publishedAt: "2026-10-28T18:29:59Z", htmlUrl: "u", assets: [] }, "macos"),
  "2026-10-28T18:29:59Z",
  "no assets falls back to published_at"
);
eq(
  releaseTimestamp(
    { tag: "latest", publishedAt: "2026-10-28T18:29:59Z", updatedAt: "2026-10-29T00:00:00Z", htmlUrl: "u", assets: [] },
    "linux"
  ),
  "2026-10-29T00:00:00Z",
  "release updated_at before published_at when no asset matches"
);

if (failures) {
  console.error(`${failures} failure(s)`);
  process.exit(1);
}
console.log("all update checks passed");
