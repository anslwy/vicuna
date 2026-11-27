import { isTauriRuntime } from "./ollama";
import { type Terminal as XTerm } from "xterm";
import { invokeOrBridge } from "./bridge";

export const EXPLORER_MIN_W = 220;

export const EXPLORER_DEFAULT_W = 300;

export function clampExplorerWidth(w: number): number {
  const maxW = Math.max(240, window.innerWidth - 360);
  return Math.min(Math.max(EXPLORER_MIN_W, Math.round(w)), maxW);
}

/** Open a URL outside the app: system browser on desktop (Tauri opener),
 *  new tab in plain browsers. Never navigates the app webview itself. */
export async function openExternal(url: string): Promise<void> {
  if (isTauriRuntime()) {
    try {
      const { openUrl } = await import("@tauri-apps/plugin-opener");
      await openUrl(url);
      return;
    } catch {
      /* fall through to window.open */
    }
  }
  window.open(url, "_blank", "noopener");
}

/** Open the token generation page (github-only PAT prompt). */
export async function openTokenPage(e: MouseEvent) {
  e.preventDefault();
  await openExternal("https://github.com/settings/tokens/new");
}

/** Keep the sign-in card visible when it opens under an account row in the scrolling engine list. */
export function revealCard(node: HTMLElement) {
  requestAnimationFrame(() => node.scrollIntoView({ block: "nearest", behavior: "smooth" }));
}

// ⌘N on macOS, Ctrl+N elsewhere (shown as the New Chat hover hint)
export const isMac = typeof navigator !== "undefined" && /mac/i.test(navigator.userAgent ?? "");

// The streaming reply re-renders `{@html renderMarkdown(...)}` on every
// token, which replaces the answer DOM and collapses any selection inside
// it. Capture the selection as character offsets before the update and
// restore it after Svelte applies the new HTML, so selecting mid-stream
// no longer "goes off on its own".
export type StreamSel = { cls: string; a: number; f: number };

export function textOffsetIn(root: Element, node: Node | null, nodeOffset: number): number | null {
  if (!node) return null;
  try {
    // length of the scope's text from its start up to the caret: correct
    // for text anchors, element anchors, and markup-nested carets alike
    const range = document.createRange();
    range.selectNodeContents(root);
    range.setEnd(node, nodeOffset);
    return range.toString().length;
  } catch {
    return null;
  }
}

export function nodeForOffset(root: Element, offset: number): { node: Node; off: number } {
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  let cur: Node | null = walker.nextNode();
  let acc = 0;
  let last: Node = root;
  while (cur) {
    const len = cur.textContent?.length ?? 0;
    if (acc + len >= offset) return { node: cur, off: offset - acc };
    acc += len;
    last = cur;
    cur = walker.nextNode();
  }
  return { node: last, off: last.textContent?.length ?? 0 };
}

// delegated handler for code-block copy buttons rendered via {@html}
export const CODE_CHECK_SVG = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>';

export async function copyText(t: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(t);
    return true;
  } catch {
    try {
      const ta = document.createElement("textarea");
      ta.value = t;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      const ok = document.execCommand("copy");
      ta.remove();
      return ok;
    } catch {
      return false;
    }
  }
}

/** Copy xterm's model selection. Prefer the synchronous copy command:
 *  xterm's own `copy` listener fills clipboardData, and WebView2 may deny
 *  the async Clipboard API on the custom scheme. */
export function copyTermSelection(term: XTerm) {
  const text = term.getSelection();
  if (!text) return;
  let ok = false;
  try {
    ok = document.execCommand("copy");
  } catch {
    ok = false;
  }
  if (!ok) void copyText(text);
}

/** Reject `p` after `ms` so a blocked backend call can never leave the tab
 *  spinning forever (Windows antivirus/SmartScreen can stall a first launch
 *  far past every backend timeout). The backend attempt keeps running and a
 *  later poll adopts the daemon if it did come up. */
export function withBrowserTimeout<T>(p: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(message)), ms);
    p.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e) => {
        clearTimeout(timer);
        reject(e);
      }
    );
  });
}

export const BROWSER_START_TIMEOUT_MS = 4 * 60 * 1000;

export const BROWSER_START_TIMEOUT_MESSAGE =
  "The browser engine did not respond within 4 minutes. On Windows this is usually antivirus blocking the engine binary — click Retry, or set VICUNA_BROWSER_CHROME to a Chrome/Edge path and relaunch Vicuna.";

/** whether a drag carries OS files (vs. text selection etc.) */
export function hasDroppedFiles(e: DragEvent): boolean {
  const types = e.dataTransfer?.types;
  return !!types && Array.from(types).includes("Files");
}

/** a choice on an agent question: copy it into the answer box to edit,
 *  caret at the end — sending stays a separate, deliberate step */
export function prefillAsk(card: { text: string }, opt: string, from: HTMLElement) {
  card.text = opt;
  // the box of the card that was clicked (live and put-off cards coexist)
  const box = from.closest(".ask-card");
  requestAnimationFrame(() => {
    const el = box?.querySelector<HTMLTextAreaElement>("textarea.ask-input");
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
  });
}

export const isWindows = typeof navigator !== "undefined" && /win/i.test(navigator.userAgent ?? "");

export async function validateWorkspace(path: string): Promise<string | null> {
  const p = path.trim();
  if (!p) return null;
  try {
    await invokeOrBridge<string>("work_validate_root", { path: p });
    return null;
  } catch (e: any) {
    return typeof e === "string" ? e : (e?.message ?? String(e));
  }
}

export async function canonicalWorkspace(path: string): Promise<string> {
  return await invokeOrBridge<string>("work_validate_root", { path: path.trim() });
}

export async function ensureScratchWorkspace(sessionId: string): Promise<string> {
  return await invokeOrBridge<string>("work_ensure_scratch_workspace", { sessionId });
}

// downscale pasted images: full screenshots blow up localStorage quotas
// and model context windows, and vision models don't need more than this
export async function imageFileToDataUrl(f: File | Blob): Promise<string> {
  const bitmap = await createImageBitmap(f);
  const MAX = 1568;
  const scale = Math.min(1, MAX / Math.max(bitmap.width, bitmap.height));
  const w = Math.max(1, Math.round(bitmap.width * scale));
  const h = Math.max(1, Math.round(bitmap.height * scale));
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d");
  if (!ctx) {
    bitmap.close();
    throw new Error("canvas unavailable");
  }
  ctx.drawImage(bitmap, 0, 0, w, h);
  bitmap.close();
  return canvas.toDataURL("image/jpeg", 0.85);
}

export function pastedImageFiles(e: ClipboardEvent): File[] {
  const items = e.clipboardData?.items;
  if (!items) return [];
  const files: File[] = [];
  for (const it of items) {
    if (it.type.startsWith("image/")) {
      const f = it.getAsFile();
      if (f) files.push(f);
    }
  }
  return files;
}
