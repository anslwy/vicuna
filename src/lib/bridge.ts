// Vicuna headless bridge client (src-tauri/src/bridge.rs).
//
// When the app runs as a desktop shell, `isTauriRuntime()` is true and every
// invoke below goes through Tauri IPC. In a plain browser (`npm run dev`)
// the same calls route to the loopback bridge instead — `cargo run --bin
// bridge` in src-tauri/ — so live-browser/Chrome can drive the REAL backend
// (work turns, FS tools, MCP, registry fetch) with byte-identical frontend
// code. When neither is present the call fails with the usual desktop error.

import { isTauriRuntime } from "./ollama";

function bridgePort(): number {
  try {
    const q = new URLSearchParams(window.location.search).get("bridge");
    const n = q ? parseInt(q, 10) : NaN;
    if (Number.isFinite(n) && n > 0 && n < 65536) return n;
  } catch {
    /* ignore */
  }
  return 1421;
}

function bridgeBase(): string {
  return `http://127.0.0.1:${bridgePort()}`;
}

let healthCache: { ok: boolean; at: number } | null = null;

/** Is the loopback bridge up? Cached ~10s; a refused localhost port fails
 *  fast so the common no-bridge case costs one quick probe per burst. */
export async function bridgeAvailable(): Promise<boolean> {
  if (typeof window === "undefined") return false;
  if (healthCache && Date.now() - healthCache.at < 10000) return healthCache.ok;
  try {
    const ctrl = new AbortController();
    const t = setTimeout(() => ctrl.abort(), 1500);
    const res = await fetch(`${bridgeBase()}/health`, { signal: ctrl.signal });
    clearTimeout(t);
    const ok = res.ok;
    healthCache = { ok, at: Date.now() };
    return ok;
  } catch {
    healthCache = { ok: false, at: Date.now() };
    return false;
  }
}

/** Poll-based stand-in for a Tauri Channel. The bridge queues events per
 *  channel id; this drains them on a 200ms interval into onmessage. */
export class BridgeChannel {
  readonly id: string;
  onmessage: ((ev: any) => void) | null = null;
  private timer: ReturnType<typeof setInterval> | null = null;
  private failures = 0;

  constructor() {
    this.id = crypto.randomUUID();
    this.timer = setInterval(() => void this.poll(), 200);
  }

  /** Marker object to splice into invoke args where Tauri takes a Channel. */
  marker(): { __bridge_channel: string } {
    return { __bridge_channel: this.id };
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }

  private async poll(): Promise<void> {
    try {
      const res = await fetch(`${bridgeBase()}/poll/${this.id}`);
      if (!res.ok) throw new Error(`poll ${res.status}`);
      const j = await res.json();
      this.failures = 0;
      for (const ev of j.events ?? []) {
        try {
          this.onmessage?.(ev);
        } catch (e) {
          console.error("bridge event handler failed:", e);
        }
      }
    } catch {
      // localhost doesn't flake: repeated poll failures mean the bridge
      // died mid-turn — surface it as an error event instead of hanging.
      this.failures += 1;
      if (this.failures >= 10) {
        this.stop();
        this.onmessage?.({ type: "error", message: "Bridge unreachable — is `cargo run --bin bridge` still running?" });
      }
    }
  }
}

/** POST one command to the bridge. Throws the backend's error string. */
export async function bridgeInvoke(cmd: string, args: Record<string, any>): Promise<any> {
  const res = await fetch(`${bridgeBase()}/invoke`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ cmd, args })
  });
  if (!res.ok) throw new Error(`bridge ${cmd} failed: HTTP ${res.status}`);
  const j = await res.json();
  if (!j.ok) throw new Error(j.error ?? `${cmd} failed`);
  return j.data ?? null;
}

/** Tauri invoke when shelled, bridge POST when plain-browser-with-bridge,
 *  otherwise the classic desktop-needed error. Drop-in for non-streaming
 *  commands (same arg keys as Tauri — the bridge speaks camelCase too). */
export async function invokeOrBridge<T>(cmd: string, args: Record<string, any>): Promise<T> {
  if (isTauriRuntime()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<T>(cmd, args);
  }
  if (await bridgeAvailable()) return (await bridgeInvoke(cmd, args)) as T;
  throw new Error("This needs the Vicuna desktop app (or start `cargo run --bin bridge` for browser testing).");
}
