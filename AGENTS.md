# AGENTS.md — Vicuna contributor notes

Vicuna is a minimal Ollama desktop chat client: SvelteKit + Svelte 5 + Tauri 2.
There is no test suite; verification is `npm run check`, `npm run build`
(and `cargo check` for the shell), plus live-browser driving the dev server.

## Layout

- `src/lib/ChatApp.svelte` — the entire app UI + interaction logic (large file, edit surgically with small `oldString` context).
- `src/lib/store.ts` — storage backends + prefs. `localStorage` on web, sqlite via `tauri-plugin-sql` in Tauri. Prefs shape: `theme | accent | fontSize | sidebarOpen | stopWords | ollamaBase` (`""` = default `http://localhost:11434`). sqlite keeps chats plus `meta` keys `pref_theme, pref_accent, pref_font_size, pref_sidebar, pref_stop_words, pref_ollama_base`.
- `src/lib/ollama.ts` — display-layer adapter (NOT inference logic): in Tauri it
  invokes the backend commands and renders streamed `Channel` events; in a
  plain browser it falls back to direct `fetch` so `npm run dev` works. `chatToTranscript`,
  `listModels/listRunning`, load/unload via `keep_alive`. `isTauriRuntime()` picks the path.
- `src/lib/markdown.ts` — marked + highlight.js + KaTeX rendering with copy-button injection.
- `src/routes/+layout.svelte` hosts the single `ChatApp` instance (`sessionId` from `page.params.id`); the `+page`/`s/[id]` pages render nothing so navigations only change the prop instead of remounting. Unknown ids redirect to `/` from inside ChatApp.
- `src/app.html` — title + pre-hydration theme flash (reads `vicuna-prefs` with `ollama-min-prefs` fallback).
- `src-tauri/` — shell + inference owner: `tauri.conf.json` (`productName: Vicuna`, `identifier: com.vicuna.app`), `Cargo.toml` package `vicuna` / lib `vicuna_lib` (+ `reqwest`, `futures-util`), `src/main.rs` calls `vicuna_lib::run()`, `capabilities/default.json` includes `sql:default` + `sql:allow-execute` (the default set lacks execute — without it sqlite init fails, see console `sql.execute not allowed`). Backend commands in `src/lib.rs`: `ollama_list_models`, `ollama_list_running`, `ollama_warm_model`, `ollama_chat_stream`, `ollama_generate_stream`, `ollama_server_version`, `ollama_start_server`, `ollama_stop_server`.

## Storage keys (post-rebrand)

- New: `vicuna-chats`, `vicuna-model`, `vicuna-prefs`, sqlite `vicuna.db`.
- Legacy (read-only fallback, do not write): `ollama-min-chats`, `ollama-min-model`, `ollama-min-prefs`, sqlite `ollama-desktop.db`.
- `lsGet(key, legacyKey)` in `store.ts` implements the fallback. Do not remove it.

## Load-bearing decisions (do not regress)

0. **Inference lives in Rust; JS is display-only.** All Ollama traffic goes
   through `src-tauri/src/lib.rs` commands (reqwest). `src/lib/ollama.ts`
   only invokes + renders `Channel` events. Direct `fetch` to Ollama exists
   solely as the plain-browser dev fallback. Stop-button abort surfaces
   `AbortError` on the frontend; the backend request runs detached.
   Backend send failures (dead `Channel` after a refresh) break the drain
   loop instead of streaming nowhere.

8. **Refresh mid-stream resumes.** `streamInto` writes a `vicuna-pending-stream`
   marker (`chatId/index/mode/model/think/temperature/stop`) synchronously at
   start; `finally` clears it — except on page unload (`unloading` flag via
   `beforeunload`), where an abort means refresh/close and the marker must
   survive. `initApp` auto-resumes validated markers (fresh mode truncates the
   stale partial first, otherwise the from-scratch regeneration duplicates on
   append; continue mode keeps it and appends). User-Stopped streams clear the
   marker and never resume. Live-verified: reload mid-list → exact 1..100,
   no duplication, marker cleared.

1. **Continue uses `/api/chat`, never `/api/generate`.** Live-tested: `/api/generate` with `raw:true` emits raw harmony channel tokens on gpt-oss-style models, ignores `stop`, and roleplays new turns. `/api/chat` with the partial assistant message sent as-is, `think:false`, `temperature: 0.2`, `num_ctx: 32768`, and `options.stop = stopWords` appends natively. There is deliberately NO sentence-boundary rewind — it caused duplication (`yo!yo!...`) because a complete sentence reads as a finished turn and the model restarts instead of continuing mid-sentence.
2. **`/api/continue` does not exist** (verified 404 on Ollama 0.32.x). Don't try it.
3. **Custom stop words** live in prefs (`DEFAULT_STOP_WORDS` in `store.ts`), edited in Settings → General textarea (`\n` escape per line), sent only on Continue via `streamInto(..., { think:false, temperature:0.2, stop })` → `chatStream` → `options.stop`. Normal sends do not set `stop`/`temperature`.
4. **Assistant footer shows `model time tok/s` (space-separated).** `genMs` (wall-clock, accumulated across Continue) and `genTps` are recorded at settle, rendered always-visible in `.model-tag` (explicit `{" "}` separators — literal spaces get trimmed around block tags), and persisted to sqlite (`gen_ms`/`gen_tps`, ALTER-added). `genTps` prefers server eval stats; cloud APIs omit `eval_duration` (verified), so it falls back to eval tokens over wall time. Continue appends wall time and overwrites tok/s. `toHistory` strips stats (Ollama ignores them anyway); `onTok`/`onTh` carry them through so mid-stream renders don't drop them.
5. **Delete cascades**: deleting a user message deletes the following assistant message and vice versa, both behind confirm dialogs.
6. **Empty `done_reason:"load"`** responses mean the model was reloading; `chatStream`/`generateStream` retry up to 3x. Don't "fix" this by removing the retry.
7. **No fake macOS traffic lights, no center llama art** — removed per user request, don't re-add.
8. **Non-reasoning models reject `think:true`.** Plain models (verified: tinyllama) fail `/api/chat` with 400 `"does not support thinking"`; `chatStream` retries once with `think:false` instead of failing the turn. Don't "fix" this by defaulting think to false — reasoning models need the thinking stream.
9. **Pull/delete live in Settings → Models**, backed by `pullModel`/`deleteModel` in `ollama.ts` (`ollama_pull_model`/`ollama_delete_model` Tauri commands + direct-fetch fallback). Pull streams progress with Cancel; delete unloads first, then deletes behind a two-step confirm (`Delete` → `Confirm?`), then refreshes the list. `refreshModels` replaces the list on success (a reachable-but-empty library shows "No models installed", not the stale default).
10. **Ollama endpoint is configurable, not always localhost.** `ollama.ts` defaults to `http://localhost:11434`; Settings → General → "Ollama host" overrides it per browser (persisted in prefs; Tauri shell ignores it and always uses local Ollama). Remote use = tailscale: `tailscale serve --bg --https 11434 http://127.0.0.1:11435` + Host-rewrite proxy `/tmp/opencode/ollama-host-proxy.py` (Ollama 403s non-loopback Host headers, so plain proxying fails) + `OLLAMA_ORIGINS` including the app origin. Restarting ollama serve must re-export `OLLAMA_ORIGINS` or remote browsers get CORS failures.
11. **Ollama parallel-download sidecars are fragile.** Killing `ollama serve` mid-pull can leave 0-byte `*-partial-N` range trackers; on resume the server JSON-decodes one, gets EOF, and every pull of that model fails instantly with `{"error":"EOF"}`. Fix: delete the 0-byte sidecars (completed ranges are kept) and pull resumes. At finalize the server removes every range sidecar and aborts on the first missing one — if a resume created new gaps, recreate those sidecars as completed (`{"N":<n>,"Offset":<n*343339956>,"Size":343339956,"Completed":343339956}`) and pull again.
12. **`gemma4:e4b` default tag is broken upstream (2026-10-28).** Emits `<unusedN>`/`<tool_call|>` soup for every prompt — verified across think on/off, raw `/api/generate`, CPU-only (`OLLAMA_VULKAN=false`), and draft-disabled (`PARAMETER draft_num_predict 0`) builds. Weights hash-verify, so the published quant/packaging is at fault, not the download. Try `gemma4:e2b` or explicit `-it-q*` quants instead; `qwen3`/`llama3.1` are known-good fallbacks.
12. **Discover tab is a curated catalog, not a live registry search.** `ollama.com/library` sends no CORS headers so browsers can't scrape it; `src/lib/catalog.ts` bundles verified model tags + sizes (all tags HEAD-checked 200). Re-verify tags before adding entries. `Installed`/`Discover` sub-tabs live in Settings → Models; variant `<select>` + Pull reuse `submitPullName`.
13. **Settings modal is fixed 80vw × 80vh** (capped 900×820, centered, scrolls internally) so the library fits; narrow screens keep the full-width rule.
14. **Cloud models (`:cloud` / `*-cloud`) are second-class in two places.** They never appear in `/api/ps`, so the Models tab shows a green `Cloud · ready` badge instead of Load/Unload (verified: warm succeeds but ps stays empty). Continue is disabled when a cloud model is *selected* (it generates with the selected model): the native partial-append that local models do (verified qwen3:0.6b) restarts the turn on cloud gemma (verified `gemma4:31b-cloud` returns a fresh greeting for a partial history, and raw `/api/generate` returns empty). Switching to a local model re-enables Continue on any reply.
15. **Pasted images attach as data URLs; Ollama gets raw base64.** `ChatMsg.images` (data URLs) render in the composer strip + user bubbles and persist to sqlite (`messages.images` JSON column, ALTER-added for old DBs). Transport (`toWireMessages`) strips the `data:` prefix; backend `ChatMessage.images` is `Option` + `skip_serializing_if` so text requests stay byte-identical. Text-only models 400 (`does not support multimodal`, verified pre-load on gpt-oss:20b) → `chatStream` retries stripped + `onImagesStripped` note, same shape as the think-fallback in #8. Paste reads `clipboardData.items`, downscales to 1568px JPEG via canvas (localStorage quota + context window), never dumps image bytes as text. The `+` button is image-only (`accept="image/*"`, multi-select) through the same pipeline; the old text-file attachment path is gone. All history builders go through `toHistory` so regenerate/continue/resume keep the picture.
16. **Never mutate a `$state` object through a pre-prepend raw reference.** Svelte 5 caches proxied reads at first touch (verified in `proxy.js` `get` trap: sources snapshot `target[prop]`), and even `save()`'s `JSON.stringify` cements the snapshot — later raw writes stay invisible until something writes *through* the proxy (which is why the empty-view stall "fixed itself" on first token). `send()` therefore builds a fresh session COMPLETE (user + placeholder messages) *before* `chats = [chat, ...chats]`; the old `chats = [...chats]` re-notify hack is gone. Same rule applies anywhere: finish raw construction before the object enters reactive state.
17. **Non-last saves ask first; the edit box has Fork & Save.** The pencil goes straight to the edit box (Cancel | Fork & Save | Save). **Save** on the last message saves directly; anywhere earlier it raises a "Save here will remove N messages" confirm. **Fork & Save** forks first, then applies the edit + regeneration inside the fork, leaving the original untouched. The edit box supports pasting + removing images (`editImages` state, written back on save; image-only saves allowed); `saveOrConfirm`/`forkAndSave` own the flow.
18. **Settings → Models shows the Ollama server itself.** A status row (`ollama_server_version`, plain `/api/version` in browser) shows `Running · vX.Y.Z` or `Not reachable`. Start/Stop exist only in Tauri: `ollama_start_server` adopts an already-running server instead of double-binding the port, and probes `ollama` at `ollama`, `/opt/homebrew/bin/ollama`, `/usr/local/bin/ollama` (Finder-launched apps get a sparse PATH). `ollama_stop_server` only kills a server Vicuna itself launched — never a foreign one. Children are left running if the app quits.
19. **Copy Link is runtime-aware; desktop uses a `vicuna://` deep link.** `tauri://…` webview URLs are dead outside the app, so Tauri copies `vicuna://s/<id>` (registered via `tauri-plugin-deep-link`, schemes in `tauri.conf.json`) while browsers keep the `http…/s/<id>` link. The frontend listens (`onOpenUrl` live + `getCurrent` at launch, stashed until chats load) and navigates to the session. Scheme registration happens at install — `npm run desktop:dev` builds never claim `vicuna://`, so test links against the installed .app.
20. **One ChatApp instance for the whole app (mounted in `+layout.svelte`).** Per-route instances unmounted all state on every navigation: deleting the active chat blanked the entire sidebar until `initApp` reloaded, and `onDestroy` aborted any in-flight stream (as a fake user-stop, clearing the resume marker). Never go back to per-route instances; navigations must only change the `sessionId` prop.

## Workflow

- Dev server: `npm run dev` → `http://127.0.0.1:1420`. Live-browser drives tab 2; if `click` hangs, call `activate_tab({tab: 2})` first, prefer CSS-selector clicks, fall back to keyboard Enter.
- Desktop dev: `npm run desktop:dev` → `tauri dev`, whose `beforeDevCommand` is `scripts/desktop-dev.sh`: it reuses an already-running dev server on :1420 (verified ours by `<title>Vicuna</title>`) instead of crashing on the busy port, and fails loudly if the occupant isn't Vicuna. Never leave a stray `npm run dev` running — it blocks nothing now, but kill it when done anyway.
- Always run `npm run check` (expect 0 errors; 2 pre-existing a11y/autofocus warnings are fine) and `npm run build` after touching UI/store/ollama code.
- Prefer `edit` with minimal boundary over rewriting files. Never use `shell`/comments to talk to the user. Verify by executing, don't assert from memory.
- No git history rewriting; working tree has had parallel edits before — read files before editing, don't assume contents.
- Releases: `.github/workflows/release-dmg.yml` builds a universal (arm64+x86_64) DMG on every `main` push and attaches it to the rolling `latest` prerelease. Unsigned build — other Macs need right-click → Open on first launch.
