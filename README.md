# Vicuna

Vicuna is a minimal desktop chat client for [Ollama](https://ollama.com).
SvelteKit + Svelte 5 frontend in a Tauri 2 shell, talking to Ollama's local
HTTP API (`http://localhost:11434`).

Features:

- Chat with any installed Ollama model, streaming via `/api/chat`
- Model picker with search, load/unload from memory (`/api/ps`)
- Sidebar chat history, per-session URLs (`/s/{uuid}`), fork, rename, copy link, delete (with cascades)
- Continue a stopped reply (appends via `/api/chat` with `think:false`, low temp, custom stop words)
- Thinking blocks, markdown + code highlighting + KaTeX math
- Message actions: copy, edit, continue, fork, delete
- Settings: light/dark theme, accent color, font size, editable stop words, model management
- Persistence: browser `localStorage` on web, sqlite via `tauri-plugin-sql` in Tauri (`vicuna.db`)
- Pre-rebrand installs (`ollama-min-*` keys) are read as fallback and migrated forward on write

## Prerequisites

- [Ollama](https://ollama.com) installed and serving: `ollama serve`
- At least one model pulled, e.g. `ollama pull gemma3:4b`
- Node 18+ and npm
- Rust toolchain (only for the Tauri desktop shell)

## Run it

```sh
npm install
```

Web-only dev (fastest, uses the same UI against local Ollama):

```sh
npm run dev
# open http://127.0.0.1:1420
```

Desktop app dev:

```sh
npm run tauri dev
```

Other useful commands:

```sh
npm run check   # svelte-check (must be 0 errors)
npm run build   # static build to ./build
```

`cargo check` / `cargo build` also pass for the Tauri shell in `src-tauri/`.

## Architecture

- `src-tauri/src/lib.rs` owns all Ollama traffic: `ollama_list_models`,
  `ollama_list_running`, `ollama_warm_model`, `ollama_chat_stream`,
  `ollama_generate_stream` (reqwest → `http://localhost:11434`).
- `src/lib/ollama.ts` is display-only: in the Tauri webview it invokes those
  commands and renders streamed `Channel` events (`token` / `think` / `done`);
  in a plain browser (`npm run dev`) it falls back to direct `fetch` so
  web-only dev works without the shell.
- Chat streaming returns the full text; `done_reason: "load"` retries up to
  3x after a 2s wait (model was reloading). Stop-button abort raises
  `AbortError` on the frontend; the backend request runs detached (no cancel
  propagation yet).
- Refreshing mid-stream resumes automatically: a pending-stream marker
  survives page unload and `initApp` picks the turn back up (live-verified).

## Notes

- The app expects Ollama at `http://localhost:11434` (`OLLAMA_BASE` in `src/lib/ollama.ts`, `src-tauri/src/lib.rs`).
- Continue uses `/api/chat`, not `/api/generate`: the raw generate path was tested and emits harmony channel tokens, ignores stops, and roleplays new turns on gpt-oss-style models. Keep it that way.
- Web dev server and the Tauri webview share the same frontend; storage backend is chosen at runtime (`__TAURI_INTERNALS__` / `__TAURI__` present = sqlite).
