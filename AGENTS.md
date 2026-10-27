# AGENTS.md — Vicuna contributor notes

Vicuna is a minimal Ollama desktop chat client: SvelteKit + Svelte 5 + Tauri 2.
There is no test suite; verification is `npm run check`, `npm run build`
(and `cargo check` for the shell), plus live-browser driving the dev server.

## Layout

- `src/lib/ChatApp.svelte` — the entire app UI + interaction logic (large file, edit surgically with small `oldString` context).
- `src/lib/store.ts` — storage backends + prefs. `localStorage` on web, sqlite via `tauri-plugin-sql` in Tauri. Prefs shape: `theme | accent | fontSize | sidebarOpen | stopWords`. sqlite keeps chats plus `meta` keys `pref_theme, pref_accent, pref_font_size, pref_sidebar, pref_stop_words`.
- `src/lib/ollama.ts` — display-layer adapter (NOT inference logic): in Tauri it
  invokes the backend commands and renders streamed `Channel` events; in a
  plain browser it falls back to direct `fetch` so `npm run dev` works. `chatToTranscript`,
  `listModels/listRunning`, load/unload via `keep_alive`. `isTauriRuntime()` picks the path.
- `src/lib/markdown.ts` — marked + highlight.js + KaTeX rendering with copy-button injection.
- `src/routes/+page.svelte` — home (null session). `src/routes/s/[id]/+page.svelte` — per-session URL; unknown ids redirect to `/`.
- `src/app.html` — title + pre-hydration theme flash (reads `vicuna-prefs` with `ollama-min-prefs` fallback).
- `src-tauri/` — shell + inference owner: `tauri.conf.json` (`productName: Vicuna`, `identifier: com.vicuna.app`), `Cargo.toml` package `vicuna` / lib `vicuna_lib` (+ `reqwest`, `futures-util`), `src/main.rs` calls `vicuna_lib::run()`, `capabilities/default.json` includes `sql:default`. Backend commands in `src/lib.rs`: `ollama_list_models`, `ollama_list_running`, `ollama_warm_model`, `ollama_chat_stream`, `ollama_generate_stream`.

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
4. **Model tag** (`m.model`) is recorded per assistant message, revealed on row hover only.
5. **Delete cascades**: deleting a user message deletes the following assistant message and vice versa, both behind confirm dialogs.
6. **Empty `done_reason:"load"`** responses mean the model was reloading; `chatStream`/`generateStream` retry up to 3x. Don't "fix" this by removing the retry.
7. **No fake macOS traffic lights, no center llama art** — removed per user request, don't re-add.

## Workflow

- Dev server: `npm run dev` → `http://127.0.0.1:1420`. Live-browser drives tab 2; if `click` hangs, call `activate_tab({tab: 2})` first, prefer CSS-selector clicks, fall back to keyboard Enter.
- Always run `npm run check` (expect 0 errors; 2 pre-existing a11y/autofocus warnings are fine) and `npm run build` after touching UI/store/ollama code.
- Prefer `edit` with minimal boundary over rewriting files. Never use `shell`/comments to talk to the user. Verify by executing, don't assert from memory.
- No git history rewriting; working tree has had parallel edits before — read files before editing, don't assume contents.
