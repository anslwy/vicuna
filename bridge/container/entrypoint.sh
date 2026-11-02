#!/bin/sh
# Vicuna remote worker container entrypoint.
#
# The worker's /status reports which `lb` it resolved and which Ollama it will
# talk to, so both are settled before it starts.
set -eu

WORKSPACE="${VICUNA_WORKSPACE:-/workspace}"

mkdir -p "$WORKSPACE" "$VICUNA_WORKER_STATE" "${CLOAKBROWSER_CACHE_DIR:-/var/lib/vicuna-engine}"

echo "vicuna-worker container"
echo "  workspace: $WORKSPACE"
echo "  ollama:    ${VICUNA_OLLAMA_BASE:-http://localhost:11434}"
echo "  lb:        $(command -v lb || echo 'NOT FOUND')"

# 1. Browser engine. `lb` never uses system Chrome, so without this it has
#    nothing to drive and the browser tools fail at launch.
#    The version is PINNED on purpose: the newest free CloakBrowser release
#    publishes only checksums (no archives), while the pinned tag has
#    cloakbrowser-linux-x64.tar.gz. Downloading is idempotent (cache hit = no-op).
if [ "${VICUNA_SKIP_ENGINE:-0}" != "1" ]; then
    if ! lb browsers 2>/dev/null | grep -qi 'active'; then
        echo "  fetching browser engine ${LB_BROWSER_VERSION} (one-time, ~200MB)"
        lb browsers download --version "${LB_BROWSER_VERSION}" \
            || echo "  WARNING: engine download failed; browser tools will not start"
    fi
fi

# 2. Chromium in a container has no user namespace for its sandbox.
#    browser.rs passes --no-sandbox to `lb web` when this is set.
if [ "${VICUNA_LB_NO_SANDBOX:-1}" = "1" ]; then
    echo "  browsers: --no-sandbox (container, no userns)"
fi

# 3. `lb web` listens on 127.0.0.1 only and offers no --host/--bind flag, so
#    from outside the container neither its UI nor its API is reachable. Republish
#    it on 0.0.0.0 so a human can watch and take over the agent's tab.
#    The daemon is started lazily by the worker, so retry until it answers.
if [ "${VICUNA_LB_FORWARD_PORT:-0}" != "0" ]; then
    (
        port="${VICUNA_LB_PORT:-18765}"
        fwd="${VICUNA_LB_FORWARD_PORT}"
        for _ in $(seq 1 60); do
            if curl -fsS -m 2 -o /dev/null "http://127.0.0.1:${port}/" 2>/dev/null; then
                exec socat "TCP-LISTEN:${fwd},fork,reuseaddr,bind=0.0.0.0" \
                           "TCP:127.0.0.1:${port}"
            fi
            sleep 2
        done
        echo "  WARNING: browser daemon never came up on 127.0.0.1:${port}; \
no UI forward on ${fwd} (takeover will not work from outside)" >&2
    ) &
    echo "  browser UI: forwarding 0.0.0.0:${VICUNA_LB_FORWARD_PORT} -> 127.0.0.1:${VICUNA_LB_PORT:-18765}"
fi

cd "$WORKSPACE"
exec /usr/local/bin/vicuna-worker \
    --bind "${VICUNA_WORKER_BIND:-0.0.0.0:1421}" \
    --state-dir "$VICUNA_WORKER_STATE" \
    "$@"
