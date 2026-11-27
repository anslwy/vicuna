// Settings → Ollama: server status/start/stop, loaded models (runners),
// pull with progress + cancel, two-step delete, and the Discover catalog
// (search, category chips, variant picks, Fetch refresh). Created once by ChatApp.
import { type RunningModel, serverVersion, startServer as startOllamaServer, stopServer as stopOllamaServer, listRunning, unloadModelFromMemory, loadModelIntoMemory, pullModel, deleteModel } from "./ollama";
import { type CatalogCat, type CatalogEntry, MODEL_CATALOG, CATALOG_UPDATED } from "./catalog";
import { type CatalogOverride, refreshCatalog } from "./library";
import { saveCatalog } from "./store";
import { isOpenRouterSelection } from "./openrouter";
import { isMlxVariant } from "./chatFormat";

export interface OllamaModelsDeps {
  readonly refreshModels: () => Promise<void>;
  readonly models: string[];
  selectedModel: string;
  readonly save: () => void;
}

export function createOllamaModelsState(deps: OllamaModelsDeps) {
  let runners = $state<RunningModel[]>([]);
  let runnersError = $state<string | null>(null);
  let modelOp = $state<string | null>(null);
  let pullName = $state("");
  let pulling = $state<string | null>(null);
  let pullStatus = $state("");
  let pullDone = $state(0);
  let pullTotal = $state(0);
  let pullRate = $state(0);
  // bytes/sec, EMA of completed deltas
  let pullLastAt = 0;
  let pullLastDone = 0;
  let pullLastDigest: string | null = null;
  let pullAborter: AbortController | null = null;
  let confirmDeleteModel = $state<string | null>(null);
  let browseView = $state<"installed" | "discover">("installed");
  let browseQuery = $state("");
  let browseCat = $state<"all" | CatalogCat | "small" | "mlx">("all");
  let variantPick = $state<Record<string, string>>({});
  // discover catalog: bundled list, replaced by a fetched override when the
  // user hits Fetch (persisted via store so it survives restarts)
  let catalog = $state<CatalogEntry[]>([...MODEL_CATALOG]);
  let catalogUpdated = $state(CATALOG_UPDATED);
  let fetching = $state<{ done: number; total: number } | null>(null);
  let catalogError = $state<string | null>(null);
  async function fetchCatalog() {
    if (fetching) return;
    catalogError = null;
    fetching = { done: 0, total: catalog.length };
    try {
      const over: CatalogOverride = await refreshCatalog(catalog, (done, total) => {
        fetching = { done, total };
      });
      catalog = over.entries;
      catalogUpdated = over.updated;
      saveCatalog(over);
    } catch (e: any) {
      catalogError = e?.message ?? String(e);
    } finally {
      fetching = null;
    }
  }
  let ollamaVersion: string | null = $state(null);
  let serverChecking = $state(false);
  let serverOp: null | "start" | "stop" = $state(null);
  let serverError: string | null = $state(null);
  async function refreshServerStatus() {
    if (serverChecking) return;
    serverChecking = true;
    serverError = null;
    try {
      ollamaVersion = await serverVersion();
    } catch (e: any) {
      ollamaVersion = null;
      serverError = e?.message ?? String(e);
    } finally {
      serverChecking = false;
    }
  }
  async function startServerFlow() {
    if (serverOp) return;
    serverOp = "start";
    serverError = null;
    try {
      ollamaVersion = await startOllamaServer();
      await deps.refreshModels();
      await refreshRunners();
    } catch (e: any) {
      ollamaVersion = null;
      serverError = e?.message ?? String(e);
    } finally {
      serverOp = null;
    }
  }
  async function stopServerFlow() {
    if (serverOp) return;
    serverOp = "stop";
    serverError = null;
    try {
      await stopOllamaServer();
      ollamaVersion = null;
      await deps.refreshModels();
      await refreshRunners();
    } catch (e: any) {
      serverError = e?.message ?? String(e);
    } finally {
      serverOp = null;
    }
  }
  async function refreshRunners() {
    runnersError = null;
    try {
      runners = await listRunning();
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
      runners = [];
    }
  }
  async function toggleModelLoaded(name: string, loaded: boolean) {
    if (modelOp || pulling) return;
    modelOp = name;
    runnersError = null;
    try {
      if (loaded) await unloadModelFromMemory(name);
      else await loadModelIntoMemory(name);
      await refreshRunners();
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
    } finally {
      modelOp = null;
    }
  }
  async function submitPull() {
    const name = pullName.trim();
    if (!name || pulling || modelOp) return;
    await submitPullName(name);
    pullName = "";
  }
  async function submitPullName(name: string) {
    if (!name || pulling || modelOp) return;
    pulling = name;
    pullStatus = "starting…";
    pullDone = 0;
    pullTotal = 0;
    pullRate = 0;
    pullLastAt = 0;
    pullLastDone = 0;
    pullLastDigest = null;
    runnersError = null;
    pullAborter = new AbortController();
    try {
      await pullModel(
        name,
        (p) => {
          pullStatus = p.status;
          if (typeof p.completed === "number") pullDone = p.completed;
          if (typeof p.total === "number") pullTotal = p.total;
          // Ollama reports per-layer completed/total with no rate, so derive
          // download speed from completed deltas (EMA over ~second windows).
          if (typeof p.completed === "number" && p.status.startsWith("pulling")) {
            const digest = p.digest ?? p.status;
            const now = performance.now();
            if (pullLastAt === 0 || digest !== pullLastDigest) {
              pullLastAt = now;
              pullLastDone = p.completed;
              pullLastDigest = digest;
            } else {
              const dt = (now - pullLastAt) / 1000;
              if (dt >= 0.2) {
                const delta = p.completed - pullLastDone;
                if (delta < 0) {
                  // new layer / restart: re-baseline without spiking
                  pullLastAt = now;
                  pullLastDone = p.completed;
                } else {
                  const inst = delta / dt;
                  pullRate = pullRate > 0 ? pullRate * 0.7 + inst * 0.3 : inst;
                  pullLastAt = now;
                  pullLastDone = p.completed;
                }
                pullLastDigest = digest;
              }
            }
          } else if (!p.status.startsWith("pulling")) {
            pullRate = 0;
          }
        },
        pullAborter.signal
      );
      pullName = "";
      await deps.refreshModels();
      if (deps.models.includes(name)) {
        deps.selectedModel = name;
        deps.save();
      } else if (deps.models.length > 0 && !deps.models.includes(deps.selectedModel) && !isOpenRouterSelection(deps.selectedModel)) {
        deps.selectedModel = deps.models[0];
        deps.save();
      }
      await refreshRunners();
    } catch (e: any) {
      if (e?.name !== "AbortError") runnersError = e?.message ?? String(e);
    } finally {
      pulling = null;
      pullRate = 0;
      pullAborter = null;
    }
  }
  function cancelPull() {
    pullAborter?.abort();
  }
  async function requestDeleteModel(name: string) {
    if (modelOp || pulling) return;
    if (confirmDeleteModel !== name) {
      confirmDeleteModel = name;
      return;
    }
    confirmDeleteModel = null;
    modelOp = name;
    runnersError = null;
    try {
      // a loaded model may refuse deletion: unload first, then delete
      if (runners.some((r) => r.name === name)) {
        try {
          await unloadModelFromMemory(name);
        } catch {
          // fall through: the delete below surfaces the real error
        }
      }
      await deleteModel(name);
      await deps.refreshModels();
      await refreshRunners();
      if (!deps.models.includes(deps.selectedModel) && deps.models.length > 0 && !isOpenRouterSelection(deps.selectedModel)) {
        deps.selectedModel = deps.models[0];
        deps.save();
      }
    } catch (e: any) {
      runnersError = e?.message ?? String(e);
    } finally {
      modelOp = null;
    }
  }
  const runningNames = $derived(new Set(runners.map((r) => r.name)));
  // manage tab: current chat's model first, then loaded models, then the rest
  const sortedManageModels = $derived.by(() => {
    const running = new Set(runners.map((r) => r.name));
    const rank = (m: string) => (m === deps.selectedModel ? 0 : running.has(m) ? 1 : 2);
    return deps.models.map((m, i) => ({ m, i })).sort((a, b) => rank(a.m) - rank(b.m) || a.i - b.i).map((x) => x.m);
  });
  /** variant tags ending in -mlx are Apple-silicon builds (see refresh-catalog.py) */
  const filteredCatalog = $derived.by(() => {
    const q = browseQuery.trim().toLowerCase();
    return catalog.filter((e) => {
      if (browseCat === "small") {
        if (Math.max(...e.variants.map((v) => v.sizeMB)) > 2560) return false;
      } else if (browseCat === "mlx") {
        if (!e.variants.some((v) => isMlxVariant(v.tag))) return false;
      } else if (browseCat !== "all" && !e.cats.includes(browseCat)) return false;
      if (!q) return true;
      return (
        e.name.toLowerCase().includes(q) ||
        e.blurb.toLowerCase().includes(q) ||
        e.cats.some((c) => c.includes(q)) ||
        e.variants.some((v) => v.tag.toLowerCase().includes(q))
      );
    });
  });
  function pickedVariant(entryName: string, fallback: string): string {
    return variantPick[entryName] ?? fallback;
  }

  return {
    get runners() { return runners; },
    set runners(v) { runners = v; },
    get runnersError() { return runnersError; },
    set runnersError(v) { runnersError = v; },
    get modelOp() { return modelOp; },
    set modelOp(v) { modelOp = v; },
    get pullName() { return pullName; },
    set pullName(v) { pullName = v; },
    get pulling() { return pulling; },
    set pulling(v) { pulling = v; },
    get pullStatus() { return pullStatus; },
    set pullStatus(v) { pullStatus = v; },
    get pullDone() { return pullDone; },
    set pullDone(v) { pullDone = v; },
    get pullTotal() { return pullTotal; },
    set pullTotal(v) { pullTotal = v; },
    get pullRate() { return pullRate; },
    set pullRate(v) { pullRate = v; },
    get pullLastAt() { return pullLastAt; },
    set pullLastAt(v) { pullLastAt = v; },
    get pullLastDone() { return pullLastDone; },
    set pullLastDone(v) { pullLastDone = v; },
    get pullLastDigest() { return pullLastDigest; },
    set pullLastDigest(v) { pullLastDigest = v; },
    get pullAborter() { return pullAborter; },
    set pullAborter(v) { pullAborter = v; },
    get confirmDeleteModel() { return confirmDeleteModel; },
    set confirmDeleteModel(v) { confirmDeleteModel = v; },
    get browseView() { return browseView; },
    set browseView(v) { browseView = v; },
    get browseQuery() { return browseQuery; },
    set browseQuery(v) { browseQuery = v; },
    get browseCat() { return browseCat; },
    set browseCat(v) { browseCat = v; },
    get variantPick() { return variantPick; },
    set variantPick(v) { variantPick = v; },
    get catalog() { return catalog; },
    set catalog(v) { catalog = v; },
    get catalogUpdated() { return catalogUpdated; },
    set catalogUpdated(v) { catalogUpdated = v; },
    get fetching() { return fetching; },
    set fetching(v) { fetching = v; },
    get catalogError() { return catalogError; },
    set catalogError(v) { catalogError = v; },
    fetchCatalog,
    get ollamaVersion() { return ollamaVersion; },
    set ollamaVersion(v) { ollamaVersion = v; },
    get serverChecking() { return serverChecking; },
    set serverChecking(v) { serverChecking = v; },
    get serverOp() { return serverOp; },
    set serverOp(v) { serverOp = v; },
    get serverError() { return serverError; },
    set serverError(v) { serverError = v; },
    refreshServerStatus,
    startServerFlow,
    stopServerFlow,
    refreshRunners,
    toggleModelLoaded,
    submitPull,
    submitPullName,
    cancelPull,
    requestDeleteModel,
    get runningNames() { return runningNames; },
    get sortedManageModels() { return sortedManageModels; },
    get filteredCatalog() { return filteredCatalog; },
    pickedVariant
  };
}

export type OllamaModelsState = ReturnType<typeof createOllamaModelsState>;
