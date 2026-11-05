# fx-sidecar

Bundled [fx](https://github.com/vercel-labs/fx) binary used by Work mode:

```
fx acp
```

is an ACP (Agent Client Protocol) agent server over stdio. Vicuna registers
an additive `vicuna-local` connection (OpenAI-compatible Chat Completions)
in `~/.fx/settings.json` for the user's Ollama endpoint, then spawns
`fx acp` with process-local `FX_PROVIDER=vicuna-local` / `FX_MODEL=<model>`.
The user's own fx provider/model selections are left untouched and no fx
login is required.

Populate this directory with:

```sh
scripts/fetch-fx-sidecar.sh
```

Layout:

- `fx` — Linux binary
- `fx-arm64` / `fx-x64` — macOS universal build (both archs; backend picks at runtime)
- `FX_VERSION` — pin marker

The binaries and marker are gitignored; this README keeps the directory.
fx has no Windows release; Windows builds fall back to the built-in agent.
