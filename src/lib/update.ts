// In-app update helpers (Settings → App updates).
//
// The CI release workflow attaches installers to the rolling `latest`
// prerelease on every main push, so the release *tag* never changes — and
// neither does its `published_at` (`gh release upload --clobber` only swaps
// assets). Staleness is therefore decided by date: the installed build's
// baked timestamp (`VICUNA_BUILD_UNIX`, see src-tauri/build.rs) vs the
// *platform installer asset's* upload time (see `releaseTimestamp`). Pure
// functions only (unit-tested by scripts/check-update.mjs); the ChatApp
// settings section owns the check/download/open flow.

export interface ReleaseAsset {
  name: string;
  url: string;
  size?: number | null;
  /** when GitHub received this upload (clobber uploads refresh it) */
  uploadedAt?: string | null;
}

export interface ReleaseInfo {
  tag: string;
  publishedAt: string;
  updatedAt?: string | null;
  htmlUrl: string;
  assets: ReleaseAsset[];
}

export interface BuildInfo {
  version: string;
  /** seconds since epoch, 0 when the build script couldn't read the clock */
  builtAtUnix: number;
}

export type DesktopPlatform = "macos" | "windows" | "linux" | "other";

/** Best-effort desktop OS detection for installer picking. */
export function currentPlatform(): DesktopPlatform {
  const ua = typeof navigator !== "undefined" ? navigator.userAgent ?? "" : "";
  const plat =
    typeof navigator !== "undefined" &&
    (navigator as Navigator & { userAgentData?: { platform?: string } }).userAgentData?.platform;
  const s = `${plat ?? ""} ${typeof navigator !== "undefined" ? (navigator.platform ?? "") : ""} ${ua}`;
  if (/mac/i.test(s)) return "macos";
  if (/win/i.test(s)) return "windows";
  if (/linux/i.test(s)) return "linux";
  return "other";
}

/** Newer release available? Strictly-greater published date beats the
 *  baked build timestamp. An unparseable date never claims an update; an
 *  unknown local build date (0) defers to the release date so the user can
 *  compare them in the UI. */
export function isNewerRelease(publishedAt: string, builtAtUnix: number): boolean {
  const pubMs = Date.parse(publishedAt);
  if (!Number.isFinite(pubMs)) return false;
  if (!(builtAtUnix > 0)) return true;
  return pubMs > builtAtUnix * 1000;
}

/** The timestamp that says when *this platform's* installer was built.
 *  The rolling `latest` release keeps its original `published_at` forever
 *  (`gh release upload --clobber` only replaces assets), so the per-asset
 *  upload time is the only honest signal. Falls back to the release's
 *  `updated_at` then `published_at` when GitHub omits asset timestamps. */
export function releaseTimestamp(release: ReleaseInfo, platform: DesktopPlatform): string {
  const asset = pickAssetForPlatform(release.assets, platform);
  return asset?.uploadedAt || release.updatedAt || release.publishedAt;
}

/** Pick the installer asset for this platform out of a release's assets.
 *  macOS takes the universal DMG (both arches, per release.yml),
 *  Windows the nsis `-setup.exe`, Linux the AppImage (deb fallback). */
export function pickAssetForPlatform(
  assets: ReleaseAsset[],
  platform: DesktopPlatform
): ReleaseAsset | null {
  const byName = (fn: (n: string) => boolean) => assets.find((a) => fn(a.name.toLowerCase())) ?? null;
  switch (platform) {
    case "macos":
      return (
        byName((n) => n.endsWith(".dmg") && n.includes("universal")) ??
        byName((n) => n.endsWith(".dmg"))
      );
    case "windows":
      return (
        byName((n) => n.endsWith(".exe") && n.includes("setup")) ??
        byName((n) => n.endsWith(".exe"))
      );
    case "linux":
      return byName((n) => n.endsWith(".appimage")) ?? byName((n) => n.endsWith(".deb"));
    default:
      return null;
  }
}

/** Human size for the download button ("84.2 MB"). */
export function formatAssetSize(bytes: number | null | undefined): string {
  if (bytes == null || !Number.isFinite(bytes) || bytes < 0) return "";
  if (bytes < 1024) return `${bytes}B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10240 ? 1 : 0)}KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)}MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)}GB`;
}

/** "Oct 31, 2026, 12:00 PM" or "" when unknown. */
export function formatBuildDate(builtAtUnix: number): string {
  if (!(builtAtUnix > 0)) return "";
  try {
    return new Date(builtAtUnix * 1000).toLocaleString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "numeric",
      minute: "2-digit"
    });
  } catch {
    return "";
  }
}
