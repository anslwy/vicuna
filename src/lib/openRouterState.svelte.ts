// Settings → OpenRouter: API key (verify/save/clear), cached model catalog,
// search/filter chips and the per-model Added list. Created once by ChatApp.
import { type OpenRouterKeyInfo, type OpenRouterModel, type OpenRouterFilter, filterOpenRouterModels, setOpenRouterApiKey, testOpenRouterKey, fetchOpenRouterModels, writeOpenRouterCache } from "./openrouter";

export interface OpenRouterStateDeps {
  readonly initialPrefs: { openrouterApiKey?: string; openrouterAdded?: string[] };
  readonly persistPrefs: () => void;
  settingsTab: "general" | "work" | "models" | "openrouter" | "chat";
}

export function createOpenRouterState(deps: OpenRouterStateDeps) {
  // ── OpenRouter (Settings → OpenRouter) ─────────────────────────────
  // The key lives in prefs; the catalog is fetched at runtime (the public
  // endpoint is CORS-enabled) and cached in localStorage. Chat + Work both
  // run through OpenRouter selections (`openrouter:` ids).
  let openrouterKey = $state(deps.initialPrefs.openrouterApiKey ?? "");
  let openrouterKeyDraft = $state(deps.initialPrefs.openrouterApiKey ?? "");
  let openrouterKeyVisible = $state(false);
  /** Once a key is saved the full form collapses (frees room for the model
   *  list); "Change" reveals it again. Always open when no key is set. */
  let openrouterKeyEditing = $state(false);
  let openrouterKeyBusy = $state(false);
  let openrouterKeyInfo = $state<OpenRouterKeyInfo | null>(null);
  let openrouterKeyError = $state<string | null>(null);
  let openrouterModels = $state<OpenRouterModel[]>([]);
  let openrouterUpdated = $state("");
  let openrouterLoading = $state(false);
  let openrouterError = $state<string | null>(null);
  let openrouterQuery = $state("");
  let openrouterCat = $state<OpenRouterFilter>("added");
  const openrouterKeyDirty = $derived(openrouterKeyDraft.trim() !== openrouterKey);
  /** Curated ids: only these appear in the model picker and under the
   *  Added chip — the live catalog is 400+ entries. Any other chip or a
   *  search query browses the full catalog for discovery. */
  let openrouterAdded = $state<string[]>([...(deps.initialPrefs.openrouterAdded ?? [])]);
  const openrouterAddedSet = $derived(new Set(openrouterAdded));
  const openrouterBrowsing = $derived(openrouterCat !== "added" || openrouterQuery.trim() !== "");
  const filteredOpenRouterModels = $derived.by(() => {
    const base =
      openrouterCat === "added" && openrouterQuery.trim() === ""
        ? openrouterModels.filter((m) => openrouterAddedSet.has(m.id))
        : openrouterModels;
    return filterOpenRouterModels(base, openrouterQuery, openrouterCat);
  });
  function toggleOpenRouterAdded(id: string) {
    openrouterAdded = openrouterAddedSet.has(id)
      ? openrouterAdded.filter((x) => x !== id)
      : [...openrouterAdded, id];
    deps.persistPrefs();
  }
  function openOpenRouterTab() {
    deps.settingsTab = "openrouter";
    // Verify a saved key when the tab opens. Success sticks (no network per
    // click); a failed check retries next time, which helps after the account
    // or network is fixed.
    if (openrouterKey && !openrouterKeyInfo && !openrouterKeyBusy) void verifyOpenRouterKey();
  }
  /** Persist the trimmed key and verify it immediately. */
  async function saveOpenRouterKey() {
    const next = openrouterKeyDraft.trim();
    const changed = next !== openrouterKey;
    openrouterKey = next;
    openrouterKeyDraft = next;
    openrouterKeyInfo = null;
    openrouterKeyError = null;
    setOpenRouterApiKey(next);
    if (changed) deps.persistPrefs();
    if (!next) {
      openrouterKeyVisible = false;
      return;
    }
    await verifyOpenRouterKey();
    if (!openrouterKeyError) openrouterKeyEditing = false;
  }
  async function verifyOpenRouterKey() {
    if (openrouterKeyBusy) return;
    openrouterKeyError = null;
    if (!openrouterKey) {
      openrouterKeyError = "Enter an API key first.";
      return;
    }
    openrouterKeyBusy = true;
    try {
      openrouterKeyInfo = await testOpenRouterKey(openrouterKey);
    } catch (e: any) {
      openrouterKeyInfo = null;
      openrouterKeyError = e?.message ?? String(e);
    } finally {
      openrouterKeyBusy = false;
    }
  }
  function clearOpenRouterKey() {
    openrouterKey = "";
    openrouterKeyDraft = "";
    openrouterKeyVisible = false;
    openrouterKeyEditing = false;
    openrouterKeyInfo = null;
    openrouterKeyError = null;
    setOpenRouterApiKey("");
    deps.persistPrefs();
  }
  async function fetchOpenRouterCatalog() {
    if (openrouterLoading) return;
    openrouterLoading = true;
    openrouterError = null;
    try {
      const list = await fetchOpenRouterModels(openrouterKey);
      openrouterModels = list;
      openrouterUpdated = writeOpenRouterCache(list);
    } catch (e: any) {
      openrouterError = e?.message ?? String(e);
    } finally {
      openrouterLoading = false;
    }
  }

  return {
    get openrouterKey() { return openrouterKey; },
    set openrouterKey(v) { openrouterKey = v; },
    get openrouterKeyDraft() { return openrouterKeyDraft; },
    set openrouterKeyDraft(v) { openrouterKeyDraft = v; },
    get openrouterKeyVisible() { return openrouterKeyVisible; },
    set openrouterKeyVisible(v) { openrouterKeyVisible = v; },
    get openrouterKeyEditing() { return openrouterKeyEditing; },
    set openrouterKeyEditing(v) { openrouterKeyEditing = v; },
    get openrouterKeyBusy() { return openrouterKeyBusy; },
    set openrouterKeyBusy(v) { openrouterKeyBusy = v; },
    get openrouterKeyInfo() { return openrouterKeyInfo; },
    set openrouterKeyInfo(v) { openrouterKeyInfo = v; },
    get openrouterKeyError() { return openrouterKeyError; },
    set openrouterKeyError(v) { openrouterKeyError = v; },
    get openrouterModels() { return openrouterModels; },
    set openrouterModels(v) { openrouterModels = v; },
    get openrouterUpdated() { return openrouterUpdated; },
    set openrouterUpdated(v) { openrouterUpdated = v; },
    get openrouterLoading() { return openrouterLoading; },
    set openrouterLoading(v) { openrouterLoading = v; },
    get openrouterError() { return openrouterError; },
    set openrouterError(v) { openrouterError = v; },
    get openrouterQuery() { return openrouterQuery; },
    set openrouterQuery(v) { openrouterQuery = v; },
    get openrouterCat() { return openrouterCat; },
    set openrouterCat(v) { openrouterCat = v; },
    get openrouterKeyDirty() { return openrouterKeyDirty; },
    get openrouterAdded() { return openrouterAdded; },
    set openrouterAdded(v) { openrouterAdded = v; },
    get openrouterAddedSet() { return openrouterAddedSet; },
    get openrouterBrowsing() { return openrouterBrowsing; },
    get filteredOpenRouterModels() { return filteredOpenRouterModels; },
    toggleOpenRouterAdded,
    openOpenRouterTab,
    saveOpenRouterKey,
    verifyOpenRouterKey,
    clearOpenRouterKey,
    fetchOpenRouterCatalog
  };
}

export type OpenRouterState = ReturnType<typeof createOpenRouterState>;
