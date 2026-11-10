#!/usr/bin/env bash
# Headless acceptance for Work mode = opencode over ACP, through the real
# backend (the loopback bridge, no Tauri window needed).
#
#   scripts/work-acceptance.sh missing            # opencode forced missing
#   scripts/work-acceptance.sh present <model>    # models listed + ONE Work turn
#
# Build the bridge first:  (cd bridge && cargo build)
# Env: BRIDGE (path to the bridge binary, default bridge/target/debug/vicuna-bridge
#      or src-tauri/target/debug/vicuna-bridge), PORT (default 1427).
# `present` runs exactly one real model turn (may be billed by the provider).
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

case "$mode" in
  missing)
    # an exclusive override that does not exist: detection must report missing
    start_bridge env VICUNA_OPENCODE_BIN=/nonexistent/opencode
    info="$(invoke work_engine_info '{}')"
    echo "work_engine_info: $info"
    start="$(invoke work_agent_start "$(turn_args miss-$$)")"
    echo "work_agent_start: $start"
    python3 - "$info" "$start" <<'PY'
import json, sys
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
import json, sys
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
    : >"$work/events.jsonl"
    for _ in $(seq 1 600); do
      curl -sS "$base/poll/$ch" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
      if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
      sleep 0.5
    done
    python3 - "$work/events.jsonl" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
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
print("PASS one Work turn completed via opencode ACP")
PY
    ;;
  *)
    echo "usage: $0 missing | present <provider/model>"; exit 2 ;;
esac
