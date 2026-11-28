// Remote servers (sidebar server menu): saved server list, connect dialog,
// rename/forget, the server's version + one-click upgrade, and copy helpers.
// Created once by ChatApp; each server opens in its own window.
import { type ServerInfo, invokeOrBridge, listServers, touchServer, switchServer, connectServer, renameServer, removeServer } from "./bridge";
import { cmpVersion, defaultServerName } from "./chatFormat";
import { SERVER_SETUP_CMD } from "./chatAppData";

export interface ServerSwitcherDeps {
  readonly activeServer: string | null;
  readonly onServer: boolean;
  readonly clientVersion: string;
  /** the app's Settings → Update channel; server upgrades follow it */
  readonly updateChannel: "stable" | "dev";
  readonly uiReady: boolean;
  readonly flashNotice: (text: string, ttl?: number) => void;
  error: string | null;
}

export function createServerSwitcherState(deps: ServerSwitcherDeps) {
  let srvList = $state<ServerInfo[]>([]);
  let srvMenuOpen = $state(false);
  let srvDialogOpen = $state(false);
  let srvUrlDraft = $state("");
  let srvTokenDraft = $state("");
  let srvNameDraft = $state("");
  let srvNameTouched = $state(false);
  let srvBusy = $state(false);
  let srvErr = $state<string | null>(null);
  /** chats could not be loaded from the active server */
  let srvLoadError = $state<string | null>(null);
  let srvCopied = $state<string | null>(null);
  const srvCurrent = $derived(srvList.find((x) => x.id === deps.activeServer) ?? null);
  const srvLabel = $derived(deps.onServer ? (srvCurrent?.name ?? "Remote server") : "This computer");
  let srvVersion = $state<string | null>(null);
  let srvLatest = $state<string | null>(null);
  let srvCanUpgrade = $state(false);
  let srvUpgrading = $state(false);
  let srvVersionChecked = false;
  const srvVersionSkew = $derived(!!srvVersion && srvVersion !== deps.clientVersion);
  const srvUpgradeAvailable = $derived(!!srvLatest && !!srvVersion && cmpVersion(srvLatest, srvVersion) > 0);
  async function loadServerVersion() {
    const info = await invokeOrBridge<{ version?: string; canUpgrade?: boolean }>("server_info", {}).catch(() => null);
    srvVersion = info?.version ?? null;
    srvCanUpgrade = !!info?.canUpgrade;
    if (!srvCanUpgrade) return;
    await checkServerLatest();
  }
  /** newest server build on the app's update channel (older servers ignore
   *  `channel` and answer for stable) */
  let latestReq = 0;
  async function checkServerLatest() {
    const req = ++latestReq;
    const channel = deps.updateChannel;
    const chk = await invokeOrBridge<{ latest?: string }>("server_upgrade_check", { channel }).catch(() => null);
    if (req === latestReq) srvLatest = chk?.latest ?? null;
  }
  $effect(() => {
    if (deps.onServer && deps.uiReady && !srvVersionChecked) {
      srvVersionChecked = true;
      void loadServerVersion();
    }
  });
  // switching Stable/Dev in Settings re-checks at once (no reload)
  let lastChannel: string | null = null;
  $effect(() => {
    const ch = deps.updateChannel;
    if (lastChannel !== null && lastChannel !== ch && srvVersionChecked && srvCanUpgrade && !srvUpgrading) {
      srvLatest = null;
      void checkServerLatest();
    }
    lastChannel = ch;
  });
  /** live progress of a server upgrade (`server_upgrade_status`) */
  let srvUpgradeStage = $state<string | null>(null);
  let srvUpgradePct = $state<number | null>(null);
  let srvUpgradeDetail = $state<string | null>(null);
  async function upgradeServer() {
    if (srvUpgrading || !srvLatest) return;
    const from = srvVersion;
    srvUpgrading = true;
    srvUpgradeStage = "Starting upgrade…";
    srvUpgradePct = null;
    srvUpgradeDetail = null;
    type St = { running?: boolean; failed?: boolean; error?: string; stage?: string | null; step?: number; steps?: number; percent?: number | null; detail?: string | null };
    try {
      await invokeOrBridge("server_upgrade", { channel: deps.updateChannel });
      // ~3 minutes: progress from the upgrade log, then the restart
      for (let i = 0; i < 180; i++) {
        await new Promise((r) => setTimeout(r, 1000));
        const st = await invokeOrBridge<St>("server_upgrade_status", {}).catch(() => null);
        if (st?.failed) throw new Error(st.error || "upgrade failed");
        if (st?.stage && st.running !== false) {
          srvUpgradeStage = st.steps && st.step ? `${st.stage} (${st.step}/${st.steps})` : st.stage;
          srvUpgradePct = st.percent ?? null;
          srvUpgradeDetail = st.detail ?? null;
          continue;
        }
        // upgrader finished (or the server is down / restarted): wait for the new version
        srvUpgradeStage = "Restarting the server…";
        srvUpgradePct = null;
        srvUpgradeDetail = null;
        const info = await invokeOrBridge<{ version?: string }>("server_info", {}).catch(() => null);
        if (info?.version && info.version !== from) {
          srvVersion = info.version;
          deps.flashNotice(`${srvLabel} is now on v${info.version}.`);
          return;
        }
      }
      deps.error = `${srvLabel} did not come back on a new version. Check ~/vicuna/server/upgrade.log there.`;
    } catch (e) {
      deps.error = `Server upgrade failed: ${e instanceof Error ? e.message : String(e)}`;
    } finally {
      srvUpgrading = false;
      srvUpgradeStage = null;
      srvUpgradePct = null;
      srvUpgradeDetail = null;
    }
  }
  async function refreshServers() {
    try {
      srvList = await listServers();
    } catch {
      srvList = [];
    }
  }
  function toggleServerMenu() {
    srvMenuOpen = !srvMenuOpen;
    srvMenuRenaming = false;
    if (srvMenuOpen) void refreshServers();
  }
  function openServerDialog() {
    srvMenuOpen = false;
    srvMenuRenaming = false;
    srvEditingId = null;
    srvEditDraft = "";
    srvErr = null;
    srvUrlDraft = "";
    srvTokenDraft = "";
    srvNameDraft = "";
    srvNameTouched = false;
    srvDialogOpen = true;
    void refreshServers();
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-url-input]")?.focus());
  }
  function fillRecentServer(sv: ServerInfo) {
    srvUrlDraft = sv.url;
    srvTokenDraft = "";
    srvNameDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
    srvNameTouched = true;
  }
  /** Open the server's own window (or focus it), or focus the main window
   *  for This computer. This window keeps running whatever it shows. */
  async function requestServerSwitch(id: string | null) {
    srvMenuOpen = false;
    if (id === deps.activeServer) return;
    try {
      if (id) await touchServer(id).catch(() => {});
      await switchServer(id, srvList.find((x) => x.id === id)?.name);
    } catch (e) {
      deps.error = e instanceof Error ? e.message : String(e);
    }
  }
  async function submitServer() {
    if (srvBusy || !srvUrlDraft.trim()) return;
    srvBusy = true;
    srvErr = null;
    try {
      const sv = await connectServer(
        srvUrlDraft.trim(),
        srvTokenDraft.trim(),
        srvNameTouched ? srvNameDraft.trim() : null
      );
      srvDialogOpen = false;
      await refreshServers();
      await switchServer(sv.id, sv.name);
    } catch (e) {
      srvErr = e instanceof Error ? e.message : String(e);
    } finally {
      srvBusy = false;
    }
  }
  let srvEditingId = $state<string | null>(null);
  let srvEditDraft = $state("");
  let srvRenaming = $state(false);
  let srvMenuRenaming = $state(false);
  function startRenameServer(sv: ServerInfo) {
    srvEditingId = sv.id;
    srvEditDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-rename-input]")?.focus());
  }
  function cancelRenameServer() {
    srvEditingId = null;
    srvEditDraft = "";
  }
  async function saveRenameServer() {
    if (srvRenaming || !srvEditingId) return;
    srvRenaming = true;
    srvErr = null;
    try {
      const sv = await renameServer(srvEditingId, srvEditDraft.trim());
      srvEditingId = null;
      srvEditDraft = "";
      if (srvUrlDraft.trim() === sv.url) {
        srvNameDraft = sv.name === defaultServerName(sv.url) ? "" : sv.name;
      }
      await refreshServers();
    } catch (e) {
      srvErr = e instanceof Error ? e.message : String(e);
    } finally {
      srvRenaming = false;
    }
  }
  function startMenuRenameServer() {
    if (!srvCurrent) return;
    srvEditDraft = srvCurrent.name === defaultServerName(srvCurrent.url) ? "" : srvCurrent.name;
    srvMenuRenaming = true;
    requestAnimationFrame(() => document.querySelector<HTMLInputElement>("[data-testid=server-menu-rename-input]")?.focus());
  }
  async function saveMenuRenameServer() {
    if (srvRenaming || !srvCurrent) return;
    srvRenaming = true;
    try {
      await renameServer(srvCurrent.id, srvEditDraft.trim());
      srvMenuRenaming = false;
      srvMenuOpen = false;
      await refreshServers();
    } catch (e) {
      deps.error = e instanceof Error ? e.message : String(e);
    } finally {
      srvRenaming = false;
    }
  }
  async function forgetServer(id: string) {
    if (srvEditingId === id) cancelRenameServer();
    await removeServer(id).catch(() => {});
    await refreshServers();
    // removed the server this window shows: back to the main window, close this one
    if (id === deps.activeServer) {
      await switchServer(null).catch(() => {});
      const { getCurrentWindow } = await import("@tauri-apps/api/window");
      void getCurrentWindow().close();
    }
  }
  let srvCmdCopied = $state(false);
  async function copyServerSetup() {
    try {
      await navigator.clipboard.writeText(SERVER_SETUP_CMD);
      srvCmdCopied = true;
      setTimeout(() => (srvCmdCopied = false), 1500);
    } catch {
      /* clipboard unavailable */
    }
  }
  async function copyServerUrl(url: string) {
    try {
      await navigator.clipboard.writeText(url);
      srvCopied = url;
      setTimeout(() => {
        if (srvCopied === url) srvCopied = null;
      }, 1500);
    } catch {
      /* clipboard unavailable */
    }
  }

  return {
    get srvList() { return srvList; },
    set srvList(v) { srvList = v; },
    get srvMenuOpen() { return srvMenuOpen; },
    set srvMenuOpen(v) { srvMenuOpen = v; },
    get srvDialogOpen() { return srvDialogOpen; },
    set srvDialogOpen(v) { srvDialogOpen = v; },
    get srvUrlDraft() { return srvUrlDraft; },
    set srvUrlDraft(v) { srvUrlDraft = v; },
    get srvTokenDraft() { return srvTokenDraft; },
    set srvTokenDraft(v) { srvTokenDraft = v; },
    get srvNameDraft() { return srvNameDraft; },
    set srvNameDraft(v) { srvNameDraft = v; },
    get srvNameTouched() { return srvNameTouched; },
    set srvNameTouched(v) { srvNameTouched = v; },
    get srvBusy() { return srvBusy; },
    set srvBusy(v) { srvBusy = v; },
    get srvErr() { return srvErr; },
    set srvErr(v) { srvErr = v; },
    get srvLoadError() { return srvLoadError; },
    set srvLoadError(v) { srvLoadError = v; },
    get srvCopied() { return srvCopied; },
    set srvCopied(v) { srvCopied = v; },
    get srvCurrent() { return srvCurrent; },
    get srvLabel() { return srvLabel; },
    get srvVersion() { return srvVersion; },
    set srvVersion(v) { srvVersion = v; },
    get srvLatest() { return srvLatest; },
    set srvLatest(v) { srvLatest = v; },
    get srvCanUpgrade() { return srvCanUpgrade; },
    set srvCanUpgrade(v) { srvCanUpgrade = v; },
    get srvUpgrading() { return srvUpgrading; },
    set srvUpgrading(v) { srvUpgrading = v; },
    get srvVersionChecked() { return srvVersionChecked; },
    set srvVersionChecked(v) { srvVersionChecked = v; },
    get srvVersionSkew() { return srvVersionSkew; },
    get srvUpgradeAvailable() { return srvUpgradeAvailable; },
    loadServerVersion,
    get srvUpgradeStage() { return srvUpgradeStage; },
    set srvUpgradeStage(v) { srvUpgradeStage = v; },
    get srvUpgradePct() { return srvUpgradePct; },
    set srvUpgradePct(v) { srvUpgradePct = v; },
    get srvUpgradeDetail() { return srvUpgradeDetail; },
    set srvUpgradeDetail(v) { srvUpgradeDetail = v; },
    upgradeServer,
    refreshServers,
    toggleServerMenu,
    openServerDialog,
    fillRecentServer,
    requestServerSwitch,
    submitServer,
    get srvEditingId() { return srvEditingId; },
    set srvEditingId(v) { srvEditingId = v; },
    get srvEditDraft() { return srvEditDraft; },
    set srvEditDraft(v) { srvEditDraft = v; },
    get srvRenaming() { return srvRenaming; },
    set srvRenaming(v) { srvRenaming = v; },
    get srvMenuRenaming() { return srvMenuRenaming; },
    set srvMenuRenaming(v) { srvMenuRenaming = v; },
    startRenameServer,
    cancelRenameServer,
    saveRenameServer,
    startMenuRenameServer,
    saveMenuRenameServer,
    forgetServer,
    get srvCmdCopied() { return srvCmdCopied; },
    set srvCmdCopied(v) { srvCmdCopied = v; },
    copyServerSetup,
    copyServerUrl
  };
}

export type ServerSwitcherState = ReturnType<typeof createServerSwitcherState>;
