// Native OS notification when a Chat/Work turn finishes while the window is
// not in front. Decision logic is pure in ./notifyGate (unit-tested).
import { isTauriRuntime } from "$lib/ollama";
import { shouldNotify, notifyTitle, notifyBody, parseNotifyPref, NOTIFY_PREF_KEY, type TurnEnd, type TurnMode } from "$lib/notifyGate";

export function getNotifyEnabled(): boolean {
  try {
    return parseNotifyPref(localStorage.getItem(NOTIFY_PREF_KEY));
  } catch {
    return true;
  }
}

export function setNotifyEnabled(on: boolean): void {
  try {
    localStorage.setItem(NOTIFY_PREF_KEY, on ? "1" : "0");
  } catch {}
}

async function windowState(): Promise<{ focused: boolean; visible: boolean }> {
  if (isTauriRuntime()) {
    try {
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      const w = getCurrentWindow();
      const [focused, visible, minimized] = await Promise.all([w.isFocused(), w.isVisible(), w.isMinimized()]);
      return { focused, visible: visible && !minimized };
    } catch {}
  }
  return { focused: document.hasFocus(), visible: document.visibilityState === "visible" };
}

/** Fire-and-forget; never throws. */
export async function notifyTurnEnd(opts: { title?: string | null; mode: TurnMode; end: TurnEnd; reply?: string; userStopped: boolean }): Promise<void> {
  try {
    if (!isTauriRuntime()) return;
    const { focused, visible } = await windowState();
    if (!shouldNotify({ enabled: getNotifyEnabled(), focused, visible, userStopped: opts.userStopped })) return;
    const n = await import("@tauri-apps/plugin-notification");
    let granted = await n.isPermissionGranted();
    // macOS asks on first use
    if (!granted) granted = (await n.requestPermission()) === "granted";
    if (!granted) return;
    n.sendNotification({ title: notifyTitle(opts.title), body: notifyBody(opts.mode, opts.end, opts.reply) });
  } catch {}
}
