# lb-sidecar (build artifact, gitignored except this file)

`scripts/fetch-lb-sidecar.sh` downloads the pinned `lb` (live-browser)
release here before a desktop build:

- `lb` (linux) / `lb.exe` (windows) / `lb-arm64` + `lb-x64` (macOS universal)
- `web/dist/` — the lb web UI served by `lb web` (the backend spawns the
  daemon with cwd set here so `lb` finds its UI via its cwd fast-path)
- `LB_VERSION` — pin marker (makes the fetch idempotent)

Bundled via `bundle.resources: ["lb-sidecar/"]` so the backend can spawn it
with a plain `Command` (same pattern as self-started `ollama serve`) and
resolve an absolute path for `lb mcp` — the Tauri externalBin/sidecar API
only returns a Command handle, never a path, which `McpManager` needs.

Local dev without the fetch still works when `lb` is on PATH (the backend
falls back to PATH resolution); the Browser tab shows a setup note when
neither source has it.
