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
- Work mode runs on the user's installed [opencode](https://opencode.ai) CLI
  over ACP: `src-tauri/src/acp.rs` keeps one warm `opencode acp` process,
  opens a session per chat (workspace cwd + the MCP dialog's enabled servers and the built-in browser, kept under the 128-tool limit), applies
  the model picked in Vicuna (listed from `opencode models`) and streams ACP
  updates into the same UI event channel. Nothing is bundled and there is no
  fallback: without opencode, Work mode shows an install link and Send is
  disabled. Install: https://opencode.ai/docs/#install
- Claude Code is the second Work engine: Vicuna drives your installed
  `claude` CLI headless (`claude -p` with stream-json input/output, your own
  login; no SDK or adapter, nothing else to install,
  https://docs.claude.com/en/docs/claude-code/setup). When both engines are
  installed, the Work model menu and Settings → Work engine let you switch;
  models come from the engine itself, and replies are labelled
  `claude/<model>` or opencode's `provider/model`.
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
