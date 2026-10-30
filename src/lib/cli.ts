// Vicuna CLI launcher (Settings → General, desktop only).
// Display-layer adapters over the `cli_*` backend commands: the installer
// writes a tiny `vicuna` launcher script that opens `vicuna://open?path=…`
// links, so `vicuna [path]` in a terminal raises a work session rooted at
// that folder (default: the caller's current directory).

import { isTauriRuntime } from "./ollama";

async function invokeCmd<T>(cmd: string, args: Record<string, any>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return await invoke<T>(cmd, args);
}

export interface CliStatus {
  exe_name: string;
  /** resolved launcher on PATH, if any (managed or foreign) */
  found: string | null;
  /** whether `found` was installed by this app */
  managed: boolean;
  /** where Install would write */
  install_dir: string;
  /** whether `install_dir` is reachable from fresh terminals */
  on_path: boolean;
}

export interface CliInstallResult {
  location: string;
  on_path: boolean;
  note: string;
}

/** Parse a `vicuna://open?path=…` deep link: percent-decoded absolute path,
 *  or null when the URL is not an open-path link (or is malformed). Pure —
 *  safe to unit-test without Tauri. */
export function extractOpenPath(urls: string[] | null): string | null {
  if (!urls?.length) return null;
  for (const u of urls) {
    const rest = u.replace(/^[a-z][a-z0-9+.-]*:\/\//i, "");
    const m = rest.match(/^\/?open\/?\?path=([^#]*)/);
    if (!m) continue;
    try {
      const p = decodeURIComponent(m[1]).trim();
      if (p) return p;
    } catch {
      // malformed escape: ignore this URL
    }
  }
  return null;
}

/** Launcher status (desktop only). */
export async function cliStatus(): Promise<CliStatus> {
  if (!isTauriRuntime()) throw new Error("the Vicuna CLI needs the desktop app.");
  return await invokeCmd<CliStatus>("cli_status", {});
}

/** Install (or reinstall) the launcher (desktop only). */
export async function cliInstall(): Promise<CliInstallResult> {
  if (!isTauriRuntime()) throw new Error("the Vicuna CLI needs the desktop app.");
  return await invokeCmd<CliInstallResult>("cli_install", {});
}

/** Remove a launcher this app installed (desktop only). */
export async function cliUninstall(): Promise<string> {
  if (!isTauriRuntime()) throw new Error("the Vicuna CLI needs the desktop app.");
  return await invokeCmd<string>("cli_uninstall", {});
}

/** Bring the main window forward (best-effort, desktop only). CLI links can
 *  arrive while the window hides in the tray. */
export async function focusAppWindow(): Promise<void> {
  if (!isTauriRuntime()) return;
  try {
    await invokeCmd<void>("show_main_window", {});
  } catch {
    // showing is best-effort (e.g. no window yet at launch)
  }
}
