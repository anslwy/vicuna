// Native OS notification when a Chat/Work turn finishes while the window is
// not in front. Decision logic is pure in ./notifyGate (unit-tested).
import { isTauriRuntime } from "$lib/ollama";
import { shouldNotify, notifyTitle, notifyBody, parseNotifyPref, NOTIFY_PREF_KEY, NOTIFY_SOUND_PREF_KEY, NOTIFY_SOUND_CHOICE_KEY, type TurnEnd, type TurnMode } from "$lib/notifyGate";
import { playChime, normalizeChime, type ChimeId } from "$lib/chime";

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

export function getNotifySound(): boolean {
  try {
    return parseNotifyPref(localStorage.getItem(NOTIFY_SOUND_PREF_KEY));
  } catch {
    return true;
  }
}

export function setNotifySound(on: boolean): void {
  try {
    localStorage.setItem(NOTIFY_SOUND_PREF_KEY, on ? "1" : "0");
  } catch {}
}

export function getNotifySoundChoice(): ChimeId {
  try {
    return normalizeChime(localStorage.getItem(NOTIFY_SOUND_CHOICE_KEY));
  } catch {
    return normalizeChime(null);
  }
}

export function setNotifySoundChoice(id: string): void {
  try {
    localStorage.setItem(NOTIFY_SOUND_CHOICE_KEY, normalizeChime(id));
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
    // Chime first (plays even if notification permission is denied). If Web
    // Audio is unavailable/still locked, ask macOS for its notification sound.
    const chimed = getNotifySound() ? await playChime(getNotifySoundChoice()) : true;
    const osSound = !chimed && /Mac/i.test(navigator.userAgent) ? "default" : undefined;
    const n = await import("@tauri-apps/plugin-notification");
    let granted = await n.isPermissionGranted();
    // macOS asks on first use
    if (!granted) granted = (await n.requestPermission()) === "granted";
    if (!granted) return;
    n.sendNotification({ title: notifyTitle(opts.title), body: notifyBody(opts.mode, opts.end, opts.reply), ...(osSound ? { sound: osSound } : {}) });
  } catch {}
}
