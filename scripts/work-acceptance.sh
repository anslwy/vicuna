#!/usr/bin/env bash
# Headless acceptance for Work mode = opencode over ACP, through the real
# backend (the loopback bridge, no Tauri window needed).
#
#   scripts/work-acceptance.sh missing            # opencode forced missing
#   scripts/work-acceptance.sh present <model>    # models listed + ONE Work turn
#   scripts/work-acceptance.sh toolcap <model>    # opencode config with >128 MCP tools
#
# Build the bridge first:  (cd bridge && cargo build)
# Env: BRIDGE (path to the bridge binary, default bridge/target/debug/vicuna-bridge
#      or src-tauri/target/debug/vicuna-bridge), PORT (default 1427).
# `present` and `toolcap` each run one real model turn (may be billed by the
# provider; opencode/beam-free is free). `toolcap` reproduces a user whose own
# opencode MCP servers exceed a provider's 128-tool limit: the turn must fail
# with Vicuna's opencode-config hint, not a bare upstream error.
set -euo pipefail

mode="${1:-}"
model="${2:-}"
root="$(cd "$(dirname "$0")/.." && pwd)"
port="${PORT:-1427}"
bridge="${BRIDGE:-}"
if [ -z "$bridge" ]; then
  for c in "$root/bridge/target/debug/vicuna-bridge" "$root/src-tauri/target/debug/vicuna-bridge"; do
    [ -x "$c" ] && bridge="$c" && break
  done
fi
[ -x "${bridge:-}" ] || { echo "bridge binary not found; build it: (cd bridge && cargo build)"; exit 2; }

base="http://127.0.0.1:$port"
work="$(mktemp -d "${TMPDIR:-/tmp}/vicuna-accept.XXXXXX")"
pid=""
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null || true; }
trap cleanup EXIT

start_bridge() {
  "$@" "$bridge" "$port" >"$work/bridge.log" 2>&1 &
  pid=$!
  for _ in $(seq 1 50); do
    curl -fsS "$base/health" >/dev/null 2>&1 && return 0
    sleep 0.2
  done
  echo "bridge did not start:"; cat "$work/bridge.log"; exit 1
}

invoke() { # cmd json-args
  curl -sS -X POST "$base/invoke" -H 'content-type: application/json' \
    -d "{\"cmd\":\"$1\",\"args\":$2}"
}

turn_args() { # channel-id
  cat <<JSON
{"model":"$model","workspace":"$work","sessionId":"accept-$$","freshSession":true,
 "messages":[{"role":"user","content":"Reply with exactly the word PONG and nothing else. Do not use any tools."}],
 "onEvent":{"__bridge_channel":"$1"}}
JSON
}

poll_turn() { # channel-id -> $work/events.jsonl
  : >"$work/events.jsonl"
  for _ in $(seq 1 600); do
    curl -sS "$base/poll/$1" | python3 -c '
import json, os, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
    if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
    sleep 0.5
  done
}

case "$mode" in
  missing)
    # an exclusive override that does not exist: detection must report missing
    start_bridge env VICUNA_OPENCODE_BIN=/nonexistent/opencode
    info="$(invoke work_engine_info '{}')"
    echo "work_engine_info: $info"
    start="$(invoke work_agent_start "$(turn_args miss-$$)")"
    echo "work_agent_start: $start"
    python3 - "$info" "$start" <<'PY'
import json, os, sys
info = json.loads(sys.argv[1]); start = json.loads(sys.argv[2])
d = info["data"]
assert info["ok"] and d["installed"] is False and d["path"] is None, info
assert d["installUrl"].startswith("https://opencode.ai/"), d
assert not start["ok"] and "opencode CLI not found, install it:" in start.get("error", ""), start
print("PASS missing: installed=false, installUrl present, work_agent_start refused with the install hint")
print("     (UI: workSendGate(info) => send-btn disabled; covered by `npm test`)")
PY
    ;;
  present)
    [ -n "$model" ] || { echo "usage: $0 present <provider/model>"; exit 2; }
    start_bridge env
    info="$(invoke work_engine_info '{}')"
    echo "work_engine_info: $info"
    models="$(invoke opencode_models '{}')"
    python3 - "$info" "$models" "$model" <<'PY'
import json, os, sys
info, models, want = json.loads(sys.argv[1]), json.loads(sys.argv[2]), sys.argv[3]
assert info["ok"] and info["data"]["installed"], info
assert models["ok"], models
ids = [m["id"] for m in models["data"]["models"]]
print(f"opencode_models: {len(ids)} models, e.g. {ids[:5]}")
assert ids and all("/" in i for i in ids), ids
assert want in ids, f"{want} not in opencode's list"
print("PASS models listed from opencode")
PY
    ch="turn-$$"
    start="$(invoke work_agent_start "$(turn_args "$ch")")"
    echo "work_agent_start: $start"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r' "$start"
    poll_turn "$ch"
    python3 - "$work/events.jsonl" "$model" <<'PY'
import json, os, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
want = sys.argv[2]
types = [e.get("type") for e in evs]
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
print("event types:", sorted(set(types)))
print("assistant text:", repr(text[:200]))
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
assert "done" in types and "roundstart" in types and "roundend" in types, types
done = [e for e in evs if e.get("type") == "done"][-1]
assert done.get("stopped") is False, done
assert "PONG" in text.upper(), text
# reply label: the backend must report the opencode model the turn ran on
# (the UI labels the bubble with it, never with the Chat model)
models = [e.get("model") for e in evs if e.get("type") == "model"]
assert models and models[-1] == want, f"model event {models} != {want}"
print(f"PASS one Work turn completed via opencode ACP, reply model = {models[-1]}")
PY
    ;;
  toolcap)
    [ -n "$model" ] || { echo "usage: $0 toolcap <provider/model>"; exit 2; }
    # 1) Vicuna's own MCP config (~/vicuna/config/vicuna.json, used by the
    #    MCP popup) holds a server: Work turns must NOT hand it to opencode
    #    (this injection is what pushed real users past 128 tools).
    # 2) The user's own opencode config (here: the workspace's opencode.json)
    #    adds TOOLCAP_N (default 140) tools. If the provider enforces a 128
    #    limit, the turn must fail with Vicuna's opencode-config hint.
    # HOME is a temp dir so the user's real vicuna.json is never touched;
    # XDG dirs keep pointing at the real opencode config/auth.
    real_home="$HOME"
    mkdir -p "$work/home/vicuna/config"
    python3 - "$work" "$root/scripts/many-tools-mcp.py" <<'PY'
import json, os, sys
work, srv = sys.argv[1], sys.argv[2]
n = os.environ.get("TOOLCAP_N", "140")
json.dump({"mcp": {"vicuna_side": {"type": "local", "command": ["python3", srv, "5"],
           "env": {"MANY_TOOLS_LOG": f"{work}/vicuna-mcp.log"}, "enabled": True}}},
          open(f"{work}/home/vicuna/config/vicuna.json", "w"))
json.dump({"mcp": {"servers": {"many_tools": {"type": "local", "command": ["python3", srv, n],
           "environment": {"MANY_TOOLS_LOG": f"{work}/mcp.log"}}}}},
          open(f"{work}/opencode.json", "w"))
PY
    start_bridge env HOME="$work/home" \
      XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$real_home/.config}" \
      XDG_DATA_HOME="${XDG_DATA_HOME:-$real_home/.local/share}" \
      XDG_CACHE_HOME="${XDG_CACHE_HOME:-$real_home/.cache}" \
      XDG_STATE_HOME="${XDG_STATE_HOME:-$real_home/.local/state}" \
      VICUNA_OPENCODE_BIN="${VICUNA_OPENCODE_BIN:-$(command -v opencode)}"
    ch="cap-$$"
    start="$(invoke work_agent_start "$(turn_args "$ch")")"
    echo "work_agent_start: $start"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r' "$start"
    poll_turn "$ch"
    echo "opencode-config MCP server saw: $(sort "$work/mcp.log" 2>/dev/null | uniq -c | tr '\n' ' ')"
    echo "vicuna.json MCP server saw:     $(sort "$work/vicuna-mcp.log" 2>/dev/null | uniq -c | tr '\n' ' ')"
    python3 - "$work" <<'PY'
import json, os, sys
work = sys.argv[1]
evs = [json.loads(l) for l in open(f"{work}/events.jsonl") if l.strip()]
assert any(e.get("type") in ("done", "error") for e in evs), "turn did not finish"
assert not os.path.exists(f"{work}/vicuna-mcp.log"), "Vicuna injected its own MCP server into the opencode session"
print("PASS Vicuna injected no MCP servers/tools into the opencode session")
assert os.path.exists(f"{work}/mcp.log"), "opencode did not load the workspace opencode.json MCP server"
print("PASS opencode loaded the user's own MCP server")
errs = [e["message"] for e in evs if e.get("type") == "error"]
print("errors:", errs)
if not errs:
    print("NOTE: this opencode/provider accepted all tools (no 128 limit here); the hint path is covered by")
    print("      cargo test acp::tests::tool_limit_error_points_at_opencode_config")
    sys.exit(0)
msg = errs[-1]
assert "Vicuna adds no tools" in msg and "opencode mcp list" in msg, msg
print("PASS provider tool limit -> clear opencode-config message")
PY
    ;;
  *)
    echo "usage: $0 missing | present <provider/model> | toolcap <provider/model>"; exit 2 ;;
esac
