// Per-session messaging bridges (Telegram + bridge plugins): connection
// status polling for sidebar badges/menu dots and which bridge modal is open.
// Created once by ChatApp.
import { type TelegramStatusSnapshot, type TelegramConnState, telegramConnState } from "./telegram";
import { invokeOrBridge } from "./bridge";
import { type BridgeStatusSnapshot } from "./bridgePlugin";
import type { Chat } from "./store";
import type { InstalledPlugin } from "./pluginTheme";

export interface MessagingBridgesDeps {
  readonly chats: Chat[];
  readonly plugins: InstalledPlugin[];
  readonly ready: boolean;
}

export function createMessagingBridgesState(deps: MessagingBridgesDeps) {
  /** the lead may manage other sessions; making one the lead also pins it */
  // Per-session Telegram bridge dialog; the session it was opened for
  let telegramFor = $state<string | null>(null);
  // Per-session Telegram statuses keyed by session ID (for sidebar badges & context menu dots)
  let telegramStatuses = $state<Record<string, TelegramStatusSnapshot>>({});
  function telegramStateFor(id: string | undefined): TelegramConnState {
    return telegramConnState(id ? telegramStatuses[id] : null);
  }
  async function refreshTelegramStatus() {
    try {
      const res = await invokeOrBridge<{ bridges?: Record<string, TelegramStatusSnapshot> }>("telegram_list", {});
      telegramStatuses = res?.bridges ?? {};
    } catch {
      // Fallback for older servers that only expose single-lead telegram_status
      try {
        const lead = deps.chats.find((c) => c.role === "lead");
        if (!lead) {
          telegramStatuses = {};
          return;
        }
        const s = await invokeOrBridge<TelegramStatusSnapshot>("telegram_status", {});
        telegramStatuses = s?.configured ? { [lead.id]: s } : {};
      } catch {
        telegramStatuses = {};
      }
    }
  }
  // Per-session bridge plugin dialog & statuses
  const bridgePlugins = $derived(deps.plugins.filter((p) => !!p.bridge));
  let bridgeModal = $state<{ pluginId: string; sessionId: string } | null>(null);
  let bridgeStatuses = $state<Record<string, Record<string, BridgeStatusSnapshot>>>({});
  async function refreshBridgeStatuses() {
    if (bridgePlugins.length === 0) {
      bridgeStatuses = {};
      return;
    }
    try {
      const res = await invokeOrBridge<{ plugins?: Record<string, Record<string, BridgeStatusSnapshot>> }>("bridge_list", {});
      bridgeStatuses = res?.plugins ?? {};
    } catch {
      bridgeStatuses = {};
    }
  }
  $effect(() => {
    if (!deps.ready) return;
    void refreshTelegramStatus();
    const t = setInterval(refreshTelegramStatus, 15000);
    return () => clearInterval(t);
  });
  $effect(() => {
    if (!deps.ready) return;
    void bridgePlugins.length;
    void refreshBridgeStatuses();
    if (bridgePlugins.length === 0) return;
    const t = setInterval(refreshBridgeStatuses, 15000);
    return () => clearInterval(t);
  });

  return {
    get telegramFor() { return telegramFor; },
    set telegramFor(v) { telegramFor = v; },
    get telegramStatuses() { return telegramStatuses; },
    set telegramStatuses(v) { telegramStatuses = v; },
    telegramStateFor,
    refreshTelegramStatus,
    get bridgePlugins() { return bridgePlugins; },
    get bridgeModal() { return bridgeModal; },
    set bridgeModal(v) { bridgeModal = v; },
    get bridgeStatuses() { return bridgeStatuses; },
    set bridgeStatuses(v) { bridgeStatuses = v; },
    refreshBridgeStatuses
  };
}

export type MessagingBridgesState = ReturnType<typeof createMessagingBridgesState>;
