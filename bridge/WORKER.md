# Vicuna remote worker

The worker runs the **agent itself** — inference loop, workspace tools, MCP
servers, and the `lb` (live-browser) daemon — on a machine that is not the
user's laptop. The desktop app stays a client. Closing the laptop does not
stop the work.

This is different from "remote Ollama", which only moves token generation: the
agent loop, tools, and browser would still die with the app.

## Install (prebuilt)

The worker is published as a rolling GitHub release by
`.github/workflows/worker.yml` on every push that touches `core/` or `bridge/`.
No Rust toolchain is needed on the server:

```sh
curl -fsSL -o vicuna-worker \
  https://github.com/anslwy/vicuna/releases/download/worker-latest/vicuna-worker-linux-x86_64
curl -fsSL -O https://github.com/anslwy/vicuna/releases/download/worker-latest/SHA256SUMS
sha256sum -c --ignore-missing SHA256SUMS
chmod +x vicuna-worker
```

The binary is built with **rustls**, not OpenSSL, so `ldd` shows only
`libc`/`libgcc`/`libm` — it runs on a bare `debian:bookworm-slim` with no
`libssl` installed. That matters for an install path that drops a binary onto
an arbitrary server. If you ever see `libssl.so.3: cannot open shared object
file`, the rustls feature set in `core/Cargo.toml` has regressed.

## Build from source

```sh
cd bridge
cargo build --release --bin worker
# → bridge/target/release/worker   (no desktop app, no GUI, no app bundle)
```

The worker is a headless binary. It needs:

| Component | Why |
|---|---|
| `worker` | the agent runtime + HTTP API |
| `lb` (live-browser) | resolved from `PATH`, or put next to the worker |
| a Chromium engine | downloaded by `lb` on first use — never system Chrome |
| inference | local Ollama, Ollama cloud, or OpenRouter (whatever the turn asks for) |

Installing the Vicuna desktop package on a server is **not** required and not
supported.

## Run

```sh
./worker --bind 127.0.0.1:1421 --state-dir ~/.vicuna-worker
```

Prints the bearer token on first start and persists it (`0600`) so restarts
don't invalidate a paired client. Override with `--token` or
`VICUNA_WORKER_TOKEN`.

> The token is authentication, not transport security. Do not expose the port.
> Put it behind a private network or an SSH tunnel (`ssh -L 1421:127.0.0.1:1421 gpu-box`),
> which is also how the browser UI should be reached.

## Run in a container

`bridge/container/` has a Dockerfile + compose file for a worker that is not
the laptop at all. Useful for a GPU/server box, and for testing the "agent is
somewhere else" topology on a development machine.

The image **downloads the released worker binary** (with checksum
verification) instead of compiling Rust. That keeps the build to seconds and,
more importantly, means the container exercises the same artifact a `curl`
install on a real server would get.

```sh
docker compose -f bridge/container/docker-compose.worker.yml up -d --build

# the token lives on the worker, not the client
TOKEN=$(docker compose -f bridge/container/docker-compose.worker.yml \
          exec worker cat /var/lib/vicuna-worker/worker-token)

# --workspace is the path AS THE CONTAINER SEES IT (/workspace is the bind
# mount); --workspace-host is the same directory on this machine, which is where
# proof.txt is verified. A relative path is rejected by the worker.
python3 scripts/check-worker.py --port 1422 --token "$TOKEN" \
    --workspace /workspace \
    --workspace-host "$PWD/bridge/container/scratch/container"
# --lb-exec: the worker's lbServer is 127.0.0.1 INSIDE the container, so running
# the host's own `lb` against it would inspect this machine's daemon instead
# (a silent false pass).
python3 scripts/check-worker-browser.py --port 1422 --token "$TOKEN" \
    --lb-exec "docker compose -f bridge/container/docker-compose.worker.yml \
                exec -T worker"

# The browser UI a human watches/takes over on the agent's tab:
#   http://127.0.0.1:18766   (forwarded from the container's 127.0.0.1:18765)
```

**Why the forward exists:** `lb web` binds `127.0.0.1` and has no
`--host`/`--bind` flag, so inside a container neither its UI nor its API is
reachable from outside. The entrypoint runs `socat` to republish it
(`VICUNA_LB_FORWARD_PORT`, `0` to disable). On a real remote host you would
instead reach it over SSH: `ssh -L 18765:127.0.0.1:18765 server`.

Notes that matter:

- **amd64 only.** `live-browser` publishes just `linux-x86_64`; CloakBrowser
  has `cloakbrowser-linux-x64.tar.gz` on the pinned tag but **no arm64 Linux
  build at all**. On Apple silicon the image runs emulated — fine as a smoke
  test, not something to benchmark.
- **Pin the engine version.** `lb browsers download` with no `--version` asks
  for the newest *free* CloakBrowser release, and the current newest publishes
  only checksums (no archives). The entrypoint pins
  `LB_BROWSER_VERSION=145.0.7632.109.2`, the tag that actually has a Linux
  archive.
- **`--no-sandbox`.** A container has no user namespace for Chrome's sandbox.
  `VICUNA_LB_NO_SANDBOX=1` makes `browser.rs` pass it to `lb web`; it is
  env-gated because it weakens browser isolation and must stay off on a
  desktop.
- **The model host is not `localhost`.** Inside the container, the Mac's Ollama
  is `host.docker.internal:11434`. `OLLAMA_BASE` used to be a hardcoded
  `const`, so **`VICUNA_OLLAMA_BASE`** was added (see `ollama_base()` in
  `src-tauri/src/lib.rs`) — without it a container can reach the network but
  never the model.
- The port is published on `127.0.0.1` only: the agent runs shell commands,
  so this must never be a public port.

## API

Every route except `GET /health` requires `Authorization: Bearer <token>`.

| Route | Purpose |
|---|---|
| `GET /health` | liveness probe (no auth) |
| `GET /status` | worker info, `cwd`, resolved `lb`, daemon URL, per-session state |
| `POST /invoke` `{"cmd","args"}` | the same command surface as the desktop app |
| `GET /events/<sid>?after=<cursor>` | append-only session event log |
| `POST /sessions/<sid>/stop` | interrupt the running turn |

`work_agent_start` must carry a `sessionId`; the turn's events are keyed by it
rather than by a client-generated channel id.

### Reconnect

Events are appended with a monotonically increasing cursor per session and are
**not** drained by reading. A client stores the last cursor it saw; after a
disconnect it calls `/events/<sid>?after=<cursor>` and receives exactly the
events it missed, including the terminal `done`. Nothing about the running
turn depends on a client being connected.

## Verify

Both scripts drive a real worker. The browser one needs `lb` on `PATH` and the
model must support tools.

```sh
./worker --bind 127.0.0.1:1422 --state-dir /tmp/vw/state &
TOKEN=$(cat /tmp/vw/state/worker-token)

# auth, a live turn, and disconnect/reconnect replay
python3 scripts/check-worker.py --port 1422 --token "$TOKEN" \
    --workspace /tmp/vw/ws --model deepseek-v4.1-flash:cloud

# a real browser turn, then the same tab visible to a human
python3 scripts/check-worker-browser.py --port 1422 --token "$TOKEN"
```

## Not done yet (deliberately)

- **Durability is in memory.** A worker restart loses the event log and the
  turn. Persisting session history, pending questions, and status is the next
  step.
- **No pairing/installer flow.** Tokens are printed by the worker; there is no
  pairing code and no systemd/launchd unit. A prebuilt binary is published
  (`worker-latest`), but installing it is still a manual `curl` + `chmod`.
- **No transport security.** Bearer over loopback/SSH only. The container
  harness publishes to `127.0.0.1` for this reason.
- **No arm64 Linux browser.** Upstream publishes x86_64 only, so an
  Apple-silicon worker needs emulation (slow).
- **No OS-level sandbox.** `exec` runs real shell commands; run the worker as
  an unprivileged user in a container or VM you are willing to have modified.
- **One workspace per worker process** (`cwd`), and one turn per session at a time.
- **The desktop UI has no remote-worker setting yet** — clients speak this
  protocol directly.
