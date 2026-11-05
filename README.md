# Vicuna

<img width="1189" height="785" alt="image" src="https://github.com/user-attachments/assets/9aad0db5-8163-4362-9875-11677bf317b1" />

> Cross Platform ChatGPT-style Client built for [Ollama](https://ollama.com). Your private assistant with chat and work mode.

Features:

- Offline-first: Chat / work with your assistant without an internet connection. All sessions are also stored in your device offline.
- Remote Ollama Host: Dynamically change the Ollama host to your own GPU server for a more powerful model.
- Chat Mode: Suitable for general query with features like forking sessions, continuing from mid-sentence, editing user / assistant responses with/without triggering regeneration and more.
- Work Mode: Designed to work autonomously to finish a task with features like built-in sidebar file explorer & terminal, MCP servers management and more.

## Download (Windows / MacOS / Linux)
https://github.com/anslwy/vicuna/releases

## Prerequisites

- [Ollama](https://ollama.com) installed and serving: `ollama serve`
- At least one model pulled, e.g. `ollama pull gemma3:4b`
- Node 18+ and npm (for development)
- Rust toolchain (for development)

## Development

```sh
npm install
sh scripts/fetch-fx-sidecar.sh   # Work mode's fx ACP engine (macOS/Linux)
sh scripts/fetch-lb-sidecar.sh   # Browser tab's lb sidecar
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
- Work mode runs through the bundled [fx](https://github.com/vercel-labs/fx)
  agent over ACP: `src-tauri/src/acp.rs` spawns `fx acp`, registers an
  additive `vicuna-local` Ollama connection in `~/.fx/settings.json`, and
  streams its ACP updates into the same UI event channel. A missing sidecar
  (or Windows) falls back to the built-in agent loop.
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
