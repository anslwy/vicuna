// Side-panel terminal tabs: xterm.js views (lazy-loaded), theming from the
// app palette, fit/resize, pty I/O, and the terminal right-click menu.
// Created once by ChatApp; tab bookkeeping (sideTabs) stays in ChatApp.
import { type Terminal as XTerm, type ITheme as XTermTheme } from "xterm";
import { type FitAddon as XFitAddon } from "@xterm/addon-fit";
import { copyText, isMac, copyTermSelection } from "./chatDom";
import { resolveTerminalColors } from "./pluginTheme";
import { resizeTerm, spawnTerm, writeTerm } from "./work";
import { tick } from "svelte";
import { isTauriRuntime } from "./ollama";
import { bridgeAvailable } from "./bridge";
import { type SideTab } from "./SidePanel.svelte";
import type { ActivePluginTheme } from "./pluginTheme";

export interface TerminalsDeps {
  readonly flashExpNotice: (msg: string, ttl?: number) => void;
  readonly themeBase: "light" | "dark";
  readonly pluginTheme: ActivePluginTheme | null;
  sideTabs: SideTab[];
  newTabMenu: boolean;
  sideTabSeq: number;
  readonly explorerRootBase: string;
  activeSideTab: string;
  readonly explorerRoot: string;
}

export function createTerminalsState(deps: TerminalsDeps) {
  /** live xterm instances by tab key (non-reactive on purpose) */
  const termViews = new Map<string, { term: XTerm; fit: XFitAddon; ro: ResizeObserver; resizeTimer: ReturnType<typeof setTimeout> | null }>();
  /** terminal right-click menu (see the xterm `contextmenu` listener) */
  let termCtx = $state.raw<{ x: number; y: number; term: XTerm; sel: string } | null>(null);
  function termMenu(action: "copy" | "paste" | "selectAll" | "clear") {
    const c = termCtx;
    termCtx = null;
    if (!c) return;
    const { term } = c;
    if (action === "copy") {
      void copyText(c.sel).then((ok) => deps.flashExpNotice(ok ? "Copied" : "Copy failed (clipboard unavailable)", 1800));
    } else if (action === "paste") {
      const clip = navigator.clipboard;
      if (!clip?.readText) {
        deps.flashExpNotice("Paste isn't available here — use ⌘V / Ctrl+Shift+V.");
      } else {
        clip
          .readText()
          .then((text) => {
            if (text) term.paste(text);
          })
          .catch(() => deps.flashExpNotice("Paste isn't available here — use ⌘V / Ctrl+Shift+V."));
      }
    } else if (action === "selectAll") {
      term.selectAll();
    } else {
      term.clear();
    }
    term.focus();
  }
  function termTheme(): XTermTheme {
    // Full 16-color ANSI palettes: PowerShell / PSReadLine assume a dark
    // console (yellow prompt segments, white text), so the defaults that
    // ship with xterm go unreadable on a light background. Both palettes
    // follow the active app/plugin theme's bg/fg while remapping every ANSI
    // slot to stay legible.
    const chrome = resolveTerminalColors(deps.themeBase, deps.pluginTheme);
    if (deps.themeBase === "dark") {
      return {
        ...chrome,
        selectionBackground: "rgba(255,255,255,.25)",
        selectionInactiveBackground: "rgba(255,255,255,.16)",
        black: "#000000",
        red: "#cd3131",
        green: "#0dbc79",
        yellow: "#e5e510",
        blue: "#2472c8",
        magenta: "#bc3fbc",
        cyan: "#11a8cd",
        white: "#e5e5e5",
        brightBlack: "#666666",
        brightRed: "#f14c4c",
        brightGreen: "#23d18b",
        brightYellow: "#f5f543",
        brightBlue: "#3b8eea",
        brightMagenta: "#d670d6",
        brightCyan: "#29b8db",
        brightWhite: "#ffffff"
      };
    }
    return {
      ...chrome,
      selectionBackground: "rgba(0,0,0,.20)",
      selectionInactiveBackground: "rgba(0,0,0,.12)",
      black: "#000000",
      red: "#b3261e",
      green: "#0a7a42",
      yellow: "#795e00",
      blue: "#0a4fb3",
      magenta: "#9a3a9a",
      cyan: "#0a7a8a",
      white: "#767676",
      brightBlack: "#5a5a5a",
      brightRed: "#d32f2f",
      brightGreen: "#1a7f37",
      brightYellow: "#8a6d00",
      brightBlue: "#2563eb",
      brightMagenta: "#d63384",
      brightCyan: "#0a8a9a",
      brightWhite: "#333333"
    };
  }
  // re-theme live terminals when the app theme flips (terminals are created
  // once with the theme at open time, so existing tabs would go stale)
  $effect(() => {
    const t = termTheme();
    for (const [, v] of termViews) {
      try {
        v.term.options.theme = t;
      } catch {
        /* terminal mid-dispose */
      }
    }
  });
  /** fit the active terminal to its container and tell the pty the size */
  function fitSideTerm(key: string) {
    const el = document.querySelector(`[data-term="${key}"]`);
    if (!(el instanceof HTMLElement) || el.clientWidth === 0 || el.clientHeight === 0) return;
    const v = termViews.get(key);
    const t = deps.sideTabs.find((x) => x.key === key);
    if (!v || !t?.termId || t.dead) return;
    try {
      v.fit.fit();
      const id = t.termId;
      if (v.resizeTimer) clearTimeout(v.resizeTimer);
      v.resizeTimer = setTimeout(() => {
        void resizeTerm(id, v.term.cols, v.term.rows).catch(() => {});
      }, 150);
    } catch {
      /* container hidden mid-switch */
    }
  }
  /** open a Terminal tab rooted at the workspace (desktop or dev bridge) */
  async function openSideTerminal() {
    deps.newTabMenu = false;
    const key = `term-${deps.sideTabSeq++}`;
    deps.sideTabs = [...deps.sideTabs, { key, kind: "terminal", title: deps.explorerRootBase || "Terminal" }];
    deps.activeSideTab = key;
    await tick();
    if (!isTauriRuntime() && !(await bridgeAvailable())) {
      deps.sideTabs = deps.sideTabs.map((t) =>
        t.key === key ? { ...t, error: "Terminal needs the Vicuna desktop app." } : t
      );
      return;
    }
    const root = deps.explorerRoot.trim();
    if (!root) {
      deps.sideTabs = deps.sideTabs.map((t) =>
        t.key === key ? { ...t, error: "Pick a workspace folder first." } : t
      );
      return;
    }
    try {
      const [{ Terminal }, { FitAddon }] = await Promise.all([
        import("xterm"),
        import("@xterm/addon-fit")
      ]);
      await import("xterm/css/xterm.css");
      const el = document.querySelector(`[data-term="${key}"]`);
      if (!(el instanceof HTMLElement)) throw new Error("terminal view missing");
      const term = new Terminal({
        theme: termTheme(),
        fontSize: 13,
        fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
        cursorBlink: true,
        scrollback: 5000,
        windowsPty: /win/i.test(navigator.userAgent ?? "") ? { backend: "conpty" } : undefined
      });
      const fit = new FitAddon();
      term.loadAddon(fit);
      term.open(el);
      fit.fit();
      // xterm leaves clipboard chords to the embedder. macOS WKWebView fires
      // the browser copy event for ⌘C, but WebView2 does not, so a selection
      // there could not be copied and Ctrl+C interrupted the shell instead.
      term.attachCustomKeyEventHandler((ev) => {
        if (ev.type !== "keydown") return true;
        const key = ev.key.toLowerCase();
        // Windows Terminal's select-all chord: a no-drag escape hatch.
        if (!isMac && ev.ctrlKey && ev.shiftKey && key === "a") {
          term.selectAll();
          return false;
        }
        if (key !== "c" || !term.hasSelection()) return true;
        const copyChord = isMac ? ev.metaKey && !ev.ctrlKey : ev.ctrlKey && !ev.altKey;
        if (!copyChord) return true;
        // Returning false also keeps xterm from sending ETX, which sent SIGINT
        // to the shell and cleared the selection on Windows.
        copyTermSelection(term);
        return false;
      });
      // Tauri disables the native WebView context menu, so right-click opens
      // our own (Copy / Paste / Select All / Clear). It used to copy silently
      // on right-click, with no sign anything happened.
      el.addEventListener("contextmenu", (ev) => {
        ev.preventDefault();
        ev.stopPropagation();
        termCtx = {
          x: Math.max(8, Math.min(ev.clientX, window.innerWidth - 200)),
          y: Math.max(8, Math.min(ev.clientY, window.innerHeight - 180)),
          term,
          // read now: focus leaves the terminal once the menu is clicked
          sel: term.getSelection()
        };
      });
      const setTab = (patch: Partial<SideTab>) => {
        deps.sideTabs = deps.sideTabs.map((t) => (t.key === key ? { ...t, ...patch } : t));
      };
      const termId = await spawnTerm(
        root,
        term.cols,
        term.rows,
        (text) => term.write(text),
        (code) => {
          setTab({ dead: true });
          term.write(`\r\n[process exited${code == null ? "" : ` with code ${code}`} — close the tab or open a new one]`);
        }
      );
      term.onData((d) => {
        void writeTerm(termId, d).catch(() => {});
      });
      const ro = new ResizeObserver(() => fitSideTerm(key));
      ro.observe(el);
      termViews.set(key, { term, fit, ro, resizeTimer: null });
      setTab({ termId });
      term.focus();
    } catch (e: any) {
      deps.sideTabs = deps.sideTabs.map((t) =>
        t.key === key ? { ...t, error: typeof e === "string" ? e : (e?.message ?? String(e)) } : t
      );
    }
  }

  return {
    get termViews() { return termViews; },
    get termCtx() { return termCtx; },
    set termCtx(v) { termCtx = v; },
    termMenu,
    termTheme,
    fitSideTerm,
    openSideTerminal
  };
}

export type TerminalsState = ReturnType<typeof createTerminalsState>;
