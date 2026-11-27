// Side-panel Browser tab: the managed lb daemon (start/adopt/restart/
// takeover), one-time engine download, framed UI URL, and the poll that keeps
// a visible tab converging to running. Created once by ChatApp.
import { type BrowserStatus, type EngineDownloadProgress, LB_URL, formatDownloadProgress, browserEnsureEngine, browserStatus, browserStart, browserRestart, browserTakeover } from "./browser";
import { ACCENTS } from "./chatFormat";
import { browserThemeParams } from "./pluginTheme";
import { withBrowserTimeout, BROWSER_START_TIMEOUT_MS, BROWSER_START_TIMEOUT_MESSAGE } from "./chatDom";
import { activeServerId, serverLbProxyUrl } from "./bridge";
import { isTauriRuntime } from "./ollama";
import { browserButtonAction, browserPollAction, browserPollRevived } from "./browserPoll";
import type { ActivePluginTheme } from "./pluginTheme";
import type { SideTab } from "./SidePanel.svelte";

export interface BrowserStateDeps {
  readonly onServer: boolean;
  readonly accent: string;
  readonly pluginAccent: { bg: string; hover: string; fg: string } | null;
  readonly pluginAccentMuted: boolean;
  readonly themeBase: "light" | "dark";
  readonly pluginTheme: ActivePluginTheme | null;
  readonly now: number;
  readonly flashExpNotice: (msg: string, ttl?: number) => void;
  readonly sideTabs: SideTab[];
  readonly activeSideTab: string;
}

export function createBrowserState(deps: BrowserStateDeps) {
  // ---- Browser tab (shared `lb` daemon: iframe view + `lb mcp` agent tools) ----
  // The tab is a view onto the daemon, not its owner: closing the tab leaves
  // the daemon (and the agent's MCP connection) running. The only tab chrome
  // is a stale-engine strip (owned daemon predates the on-disk bundle):
  // in-place recover can't fix that, so it offers a full restart instead.
  let browserDaemon = $state<BrowserStatus | null>(null);
  let browserStarting = $state(false);
  let browserError = $state<string | null>(null);
  /** starting-view label: plain starts vs button restarts/takeovers. */
  let browserStartLabel = $state("Starting browser…");
  /** live CloakBrowser download progress while an engine fetch is in flight. */
  let browserDownload = $state<EngineDownloadProgress | null>(null);
  /** wall-clock start of the in-flight start drive: the starting view shows
   *  elapsed seconds so a slow Windows first launch never reads as frozen. */
  let browserStartedAt = $state<number | null>(null);
  /** iframe key: bumped whenever the daemon behind a live frame is (re)started,
   *  so the frame re-navigates instead of sitting on a failed navigation. */
  let browserFrameRev = $state(0);
  let browserProxyUrl = $state<string | null>(null);
  /** lb UI url + Vicuna theme/palette/accent: lb reads `?theme=`/`?accent=`/
   *  `?accentfg=` + palette vars (`bg`, `fg`, `card`, `border`, …) pre-paint
   *  (older lb builds ignore unknown params and fall back to their saved theme
   *  + built-in greens). Derived from `themeBase`/`pluginTheme`/`accent`, so
   *  appearance flips re-navigate the frame into the matching lb look. */
  const browserFrameSrc = $derived.by(() => {
    const base = deps.onServer ? browserProxyUrl : (browserDaemon?.url ?? LB_URL);
    if (!base) return "about:blank";
    const a = ACCENTS[deps.accent] ?? ACCENTS.orange;
    const v = deps.pluginAccent && !deps.pluginAccentMuted ? deps.pluginAccent : deps.themeBase === "dark" ? a.dark : a.light;
    return `${base}/?${browserThemeParams(deps.themeBase, deps.pluginTheme, v).toString()}`;
  });
  /** status → start when down. Throws on failure (callers surface it).
   *  Concurrent callers share one in-flight drive (tab-open + visible-tab
   *  ensure + poll can all fire together) instead of stacking spawns. */
  let browserDrive: Promise<BrowserStatus> | null = null;
  function ensureBrowserDaemon(): Promise<BrowserStatus> {
    browserDrive ??= driveBrowserDaemon().finally(() => {
      browserDrive = null;
    });
    return browserDrive;
  }
  /** one-time CloakBrowser fetch with live progress polling (non-blocking on
   *  new backends so remote servers never hit HTTP timeouts during a slow
   *  ~200 MB download; falls back gracefully on older backends that ignore
   *  `background: true`). Best-effort: a failed/blocked download must not
   *  abort the start, since the backend falls back to a system Chromium. */
  async function prewarmBrowserEngine(existing?: EngineDownloadProgress | null) {
    const active = (d?: EngineDownloadProgress | null) =>
      d != null && (d.phase === "downloading" || d.phase === "extracting");
    browserDownload = active(existing) ? existing! : null;
    browserStartLabel = browserDownload
      ? formatDownloadProgress(browserDownload)
      : "Downloading browser engine (one-time setup)…";
    try {
      let poll = active(existing);
      if (!poll) {
        const info = await browserEnsureEngine(true);
        poll = !!info.downloading;
      }
      if (poll) {
        const deadline = Date.now() + 15 * 60 * 1000;
        while (Date.now() < deadline) {
          await new Promise((r) => setTimeout(r, 1000));
          const st = await browserStatus();
          if (st.running) {
            browserDownload = null;
            return;
          }
          const d = st.download ?? null;
          if (!active(d)) {
            browserDownload = null;
            return;
          }
          browserDownload = d;
          browserStartLabel = formatDownloadProgress(d!);
        }
      }
    } catch {
      /* fall through: the start path falls back to a system Chromium */
    } finally {
      browserDownload = null;
    }
  }
  /** Seconds the start drive has been running (0 while idle). */
  const browserStartSeconds = $derived.by(() => {
    if (!browserStarting || browserStartedAt == null) return 0;
    return Math.max(0, Math.floor((deps.now - browserStartedAt) / 1000));
  });
  /** Starting label + elapsed tick ("Starting browser… (12s)"). */
  const browserStartDisplay = $derived.by(() => {
    return browserStartSeconds >= 3
      ? `${browserStartLabel} (${browserStartSeconds}s)`
      : browserStartLabel;
  });
  async function driveBrowserDaemon(): Promise<BrowserStatus> {
    browserStartLabel = "Starting browser…";
    browserStarting = true;
    browserStartedAt = Date.now();
    browserError = null;
    try {
      let st = await browserStatus();
      if (!st.running) {
        await prewarmBrowserEngine(st.download);
        browserStartedAt = Date.now();
        browserStartLabel = "Starting browser…";
        st = await withBrowserTimeout(
          browserStart(),
          BROWSER_START_TIMEOUT_MS,
          BROWSER_START_TIMEOUT_MESSAGE
        );
      }
      if (deps.onServer && !browserProxyUrl) {
        const sid = activeServerId();
        if (sid) browserProxyUrl = await serverLbProxyUrl(sid);
      }
      browserDaemon = st;
      return st;
    } catch (e: any) {
      // plain browser with no bridge: still allow viewing a manually-run
      // daemon (`lb web --port 18765` in a terminal) via a direct probe
      if (!isTauriRuntime()) {
        try {
          const r = await fetch(`${LB_URL}/api/health`);
          const h = await r.json();
          if (h?.ok === true && typeof h?.probe === "string") {
            browserDaemon = {
              running: true,
              owned: false,
              engine_stale: false,
              url: LB_URL,
              health: { browser_alive: !!h.browser_alive, probe: h.probe, tabs: h.tabs ?? 0 }
            };
            return browserDaemon;
          }
        } catch {
          /* fall through to the backend error below */
        }
      }
      browserError = e?.message ?? String(e);
      browserDaemon = null;
      throw e;
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }
  /** full stop + start of an owned daemon (stale engine / wedged
   *  browser). Refuses adopted daemons — the backend owns that refusal. */
  async function restartBrowserDaemon() {
    // swap the view instantly: the stop + start takes seconds, and the
    // stale frame meanwhile reads as "nothing happens"
    browserStartLabel = "Restarting browser…";
    browserDaemon = null;
    browserStarting = true;
    browserStartedAt = Date.now();
    browserError = null;
    try {
      await prewarmBrowserEngine();
      browserStartLabel = "Restarting browser…";
      browserDaemon = await withBrowserTimeout(
        browserRestart(),
        BROWSER_START_TIMEOUT_MS,
        BROWSER_START_TIMEOUT_MESSAGE
      );
      browserFrameRev += 1;
    } catch (e: any) {
      browserError = e?.message ?? String(e);
      browserDaemon = null;
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }
  /** two-step arm for adopted takeovers (first click explains + arms,
   *  second within 5s executes) — killing a foreign session must stay a
   *  deliberate act, never a stray click. */
  let browserKillArmed = $state(false);
  let browserKillTimer: ReturnType<typeof setTimeout> | null = null;
  /** tab-bar button: `lb start` when down, stop + start when owned,
   *  confirmed kill + start when adopted. Failures flash a transient
   *  notice instead of replacing a (possibly healthy) frame. */
  async function restartBrowserFromButton() {
    if (browserStarting) return;
    let st: BrowserStatus | null = null;
    try {
      st = await browserStatus();
    } catch {
      /* fall through to ensure on last-known state */
    }
    const action = browserButtonAction(st ?? browserDaemon);
    if (action === "start") {
      // instant view swap (a stale running frame would sit blank meanwhile)
      browserStartLabel = "Starting browser…";
      browserDaemon = null;
      const ok = await ensureBrowserDaemon().then(
        () => true,
        () => false
      );
      if (ok) browserFrameRev += 1;
      return;
    }
    if (action === "restart") {
      browserStartLabel = "Restarting browser…";
      browserDaemon = null;
      browserStarting = true;
      browserStartedAt = Date.now();
      try {
        await prewarmBrowserEngine();
        browserStartLabel = "Restarting browser…";
        browserDaemon = await withBrowserTimeout(
          browserRestart(),
          BROWSER_START_TIMEOUT_MS,
          BROWSER_START_TIMEOUT_MESSAGE
        );
        browserError = null;
        browserFrameRev += 1;
      } catch (e: any) {
        deps.flashExpNotice(e?.message ?? String(e));
        // restore truth instead of guessing: stop may have died mid-flight
        browserDaemon = await browserStatus().catch(() => null);
      } finally {
        browserStarting = false;
        browserStartedAt = null;
      }
      return;
    }
    if (!browserKillArmed) {
      browserKillArmed = true;
      deps.flashExpNotice(
        "Browser was started outside Vicuna — click again to force-stop it and start Vicuna's own (its tabs will be lost)."
      );
      if (browserKillTimer) clearTimeout(browserKillTimer);
      browserKillTimer = setTimeout(() => {
        browserKillArmed = false;
        browserKillTimer = null;
      }, 5000);
      return;
    }
    if (browserKillTimer) {
      clearTimeout(browserKillTimer);
      browserKillTimer = null;
    }
    browserKillArmed = false;
    browserStartLabel = "Taking over browser…";
    browserDaemon = null;
    browserStarting = true;
    browserStartedAt = Date.now();
    try {
      // pre-warm BEFORE the kill: a failed download must not cost their tabs
      // (backend takeover ensures pre-kill too — this is the honest label)
      await prewarmBrowserEngine();
      browserStartLabel = "Taking over browser…";
      browserDaemon = await withBrowserTimeout(
        browserTakeover(),
        BROWSER_START_TIMEOUT_MS,
        BROWSER_START_TIMEOUT_MESSAGE
      );
      browserError = null;
      browserFrameRev += 1;
    } catch (e: any) {
      deps.flashExpNotice(e?.message ?? String(e));
      browserDaemon = await browserStatus().catch(() => null);
    } finally {
      browserStarting = false;
      browserStartedAt = null;
    }
  }
  /** one poll tick for a visible running tab (see browserPollAction). */
  async function pollBrowserDaemon() {
    if (browserStarting) return;
    let st: BrowserStatus | null = null;
    try {
      st = await browserStatus();
    } catch {
      /* backend unreachable: keep the frame, retry next tick */
    }
    const action = browserPollAction(browserDaemon, st);
    if (action === "update" && st) browserDaemon = st;
    else if (action === "ensure") {
      const ok = await ensureBrowserDaemon().then(
        () => true,
        () => false
      );
      if (browserPollRevived(action, ok)) browserFrameRev += 1;
    }
  }
  // A visible Browser tab must converge to running: the daemon can die
  // under a live tab (crash, kill, sleep) leaving a dead iframe with no
  // recourse, so re-validate on every view and poll while up. Never
  // revives while hidden; errors stay user-driven (Retry owns them).
  let browserPoll: ReturnType<typeof setInterval> | null = null;
  const browserTabVisible = $derived(
    deps.sideTabs.find((t) => t.key === deps.activeSideTab)?.kind === "browser"
  );
  $effect(() => {
    const visible = browserTabVisible;
    const running = browserDaemon?.running === true;
    if (visible && !running && !browserStarting && !browserError) {
      void ensureBrowserDaemon().catch(() => {});
    }
    if (visible && running && !browserPoll) {
      browserPoll = setInterval(() => void pollBrowserDaemon(), 5000);
    } else if ((!visible || !running) && browserPoll) {
      clearInterval(browserPoll);
      browserPoll = null;
    }
  });

  return {
    get browserDaemon() { return browserDaemon; },
    set browserDaemon(v) { browserDaemon = v; },
    get browserStarting() { return browserStarting; },
    set browserStarting(v) { browserStarting = v; },
    get browserError() { return browserError; },
    set browserError(v) { browserError = v; },
    get browserStartLabel() { return browserStartLabel; },
    set browserStartLabel(v) { browserStartLabel = v; },
    get browserDownload() { return browserDownload; },
    set browserDownload(v) { browserDownload = v; },
    get browserStartedAt() { return browserStartedAt; },
    set browserStartedAt(v) { browserStartedAt = v; },
    get browserFrameRev() { return browserFrameRev; },
    set browserFrameRev(v) { browserFrameRev = v; },
    get browserProxyUrl() { return browserProxyUrl; },
    set browserProxyUrl(v) { browserProxyUrl = v; },
    get browserFrameSrc() { return browserFrameSrc; },
    get browserDrive() { return browserDrive; },
    set browserDrive(v) { browserDrive = v; },
    ensureBrowserDaemon,
    prewarmBrowserEngine,
    get browserStartSeconds() { return browserStartSeconds; },
    get browserStartDisplay() { return browserStartDisplay; },
    driveBrowserDaemon,
    restartBrowserDaemon,
    get browserKillArmed() { return browserKillArmed; },
    set browserKillArmed(v) { browserKillArmed = v; },
    get browserKillTimer() { return browserKillTimer; },
    set browserKillTimer(v) { browserKillTimer = v; },
    restartBrowserFromButton,
    pollBrowserDaemon,
    get browserPoll() { return browserPoll; },
    set browserPoll(v) { browserPoll = v; },
    get browserTabVisible() { return browserTabVisible; }
  };
}

export type BrowserState = ReturnType<typeof createBrowserState>;
