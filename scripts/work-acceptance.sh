#!/usr/bin/env bash
# Headless acceptance for Work mode = opencode over ACP, through the real
# backend (the loopback bridge, no Tauri window needed).
#
#   scripts/work-acceptance.sh missing            # opencode forced missing
#   scripts/work-acceptance.sh present <model>    # models listed + ONE Work turn
#   scripts/work-acceptance.sh toolcap <model>    # Vicuna MCP servers reach opencode, under 128 tools
#   scripts/work-acceptance.sh finder             # Finder-style launch must not pick a stale opencode
#
# Build the bridge first:  (cd bridge && cargo build)
# Env: BRIDGE (path to the bridge binary, default bridge/target/debug/vicuna-bridge
#      or src-tauri/target/debug/vicuna-bridge), PORT (default 1427).
# `present` and `toolcap` each run one real model turn (may be billed by the
# provider; opencode/beam-free is free). `toolcap` checks that the MCP
# dialog's enabled servers are handed to opencode and that a server which
# would push the request past 128 tools is skipped with a note.
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
    # Vicuna's MCP dialog (~/vicuna/config/vicuna.json + live connections)
    # applies to Work: enabled, connected servers are handed to opencode in
    # session/new, but never past the provider's 128-tool limit.
    #   small_mcp: 5 tools   -> must be started by opencode
    #   big_mcp:   140 tools -> must be skipped, with a note naming it
    # HOME is a temp dir so the user's real vicuna.json is never touched;
    # XDG dirs keep pointing at the real opencode config/auth.
    real_home="$HOME"
    mkdir -p "$work/home/vicuna/config"
    srv="$root/scripts/many-tools-mcp.py"
    python3 - "$work" "$srv" <<'PY'
import json, sys
work, srv = sys.argv[1], sys.argv[2]
def server(n, log):
    return {"type": "local", "command": ["python3", srv, str(n)],
            "env": {"MANY_TOOLS_LOG": f"{work}/{log}"}, "enabled": True}
json.dump({"mcp": {"small_mcp": server(5, "small.log"), "big_mcp": server(140, "big.log")}},
          open(f"{work}/home/vicuna/config/vicuna.json", "w"))
PY
    start_bridge env HOME="$work/home" \
      XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$real_home/.config}" \
      XDG_DATA_HOME="${XDG_DATA_HOME:-$real_home/.local/share}" \
      XDG_CACHE_HOME="${XDG_CACHE_HOME:-$real_home/.cache}" \
      XDG_STATE_HOME="${XDG_STATE_HOME:-$real_home/.local/state}" \
      VICUNA_OPENCODE_BIN="${VICUNA_OPENCODE_BIN:-$(command -v opencode)}"
    # connect both the way the MCP dialog does at launch
    for spec in "small_mcp 5 small.log" "big_mcp 140 big.log"; do
      set -- $spec
      r="$(invoke mcp_connect "{\"config\":{\"id\":\"$1\",\"name\":\"$1\",\"transport\":\"stdio\",\"command\":[\"python3\",\"$srv\",\"$2\"],\"env\":{\"MANY_TOOLS_LOG\":\"$work/$3\"},\"enabled\":true}}")"
      python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"] and len(r["data"]["tools"])==int(sys.argv[2]), r; print("mcp_connect", sys.argv[3] + ":", len(r["data"]["tools"]), "tools")' "$r" "$2" "$1"
    done
    ch="cap-$$"
    start="$(invoke work_agent_start "$(turn_args "$ch")")"
    echo "work_agent_start: $start"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r' "$start"
    poll_turn "$ch"
    python3 - "$work" <<'PY'
import json, sys
work = sys.argv[1]
evs = [json.loads(l) for l in open(f"{work}/events.jsonl") if l.strip()]
def inits(log):
    try:
        return sum(1 for l in open(f"{work}/{log}") if l.strip() == "initialize")
    except FileNotFoundError:
        return 0
small, big = inits("small.log"), inits("big.log")
print(f"initialize calls: small_mcp={small} big_mcp={big} (1 = Vicuna's own connect, 2 = opencode too)")
errs = [e["message"] for e in evs if e.get("type") == "error"]
assert not errs, errs
assert any(e.get("type") == "done" for e in evs), "turn did not finish"
assert small >= 2, "Vicuna did not pass its enabled MCP server to opencode"
print("PASS enabled MCP server from Vicuna's dialog reached opencode")
assert big == 1, "Vicuna passed a 140-tool server: the request would exceed the 128-tool limit"
notes = "".join(e.get("text", "") for e in evs if e.get("type") == "displaynote")
assert "big_mcp (140 tools)" in notes, notes
print("PASS 140-tool server skipped with a note:", notes.strip()[:160])
PY
    ;;
  finder)
    # Reproduces the release-.dmg bug without a Mac. The bridge starts with
    # launchd's minimal environment (env -i, PATH=/usr/bin:/bin), like a
    # Finder/Dock launch. Three installs, as on the owner's Mac and worse:
    #   ~/.opencode/bin/opencode       1.3.17  stale fallback (owner's Mac), prints the
    #                                          outdated-config error
    #   <login PATH dir>/opencode       1.0.0  on the shell PATH (~/.profile)
    #   ~/.nvm/versions/node/v24.14.1/bin/opencode  the real, newest one; nvm
    #                                          is NOT on any PATH (it loads
    #                                          from ~/.zshrc on a Mac)
    # Detection must pick the nvm one (newest), Settings must name it, the
    # others are "also found", and the model menu must work. No model turn.
    real_oc="$(command -v opencode)" || { echo "opencode not on PATH"; exit 2; }
    fh="$work/finder-home"
    nvm_bin="$fh/.nvm/versions/node/v24.14.1/bin"
    mkdir -p "$fh/.opencode/bin" "$fh/login-bin" "$fh/.nvm/versions/node/v18.20.4/bin" "$nvm_bin"
    cat >"$fh/.opencode/bin/opencode" <<'SH'
#!/bin/sh
case "$1" in
  --version) echo "1.3.17" ;;
  *) printf '\033[91m\033[1mError: \033[0mConfiguration is invalid at https://opencode.ai/console/api/config\n  Invalid option: expected one of "alpha"|"beta"|"deprecated" provider.opencode.models.beam-free.status\n' >&2; exit 1 ;;
esac
SH
    printf '#!/bin/sh\necho 1.0.0\n' >"$fh/login-bin/opencode"
    chmod +x "$fh/.opencode/bin/opencode" "$fh/login-bin/opencode"
    ln -s "$real_oc" "$nvm_bin/opencode"
    printf 'export PATH="%s:$PATH"\n' "$fh/login-bin" >"$fh/.profile"
    env -i HOME="$fh" SHELL=/bin/sh PATH=/usr/bin:/bin USER="${USER:-user}" \
      XDG_CONFIG_HOME="$HOME/.config" XDG_DATA_HOME="$HOME/.local/share" \
      XDG_CACHE_HOME="$HOME/.cache" XDG_STATE_HOME="$HOME/.local/state" \
      "$bridge" "$port" >"$work/bridge.log" 2>&1 &
    pid=$!
    for _ in $(seq 1 50); do curl -fsS "$base/health" >/dev/null 2>&1 && break; sleep 0.2; done
    info="$(invoke work_engine_info '{}')"
    models="$(invoke opencode_models '{}')"
    python3 - "$info" "$models" "$nvm_bin/opencode" <<'PY'
import json, sys
info, models, want = json.loads(sys.argv[1]), json.loads(sys.argv[2]), sys.argv[3]
d = info["data"]
print("work_engine_info:", json.dumps({k: d.get(k) for k in ("installed", "path", "version", "others")}, indent=1))
assert d["installed"], d
assert d["path"] == want, f"picked {d['path']}, want the nvm install {want}"
assert d["version"] and d["version"] not in ("1.3.17", "1.0.0"), d
assert not d.get("warning"), d.get("warning")
others = " ".join(d.get("others", []))
assert ".opencode/bin/opencode (1.3.17)" in others and "login-bin/opencode (1.0.0)" in others, others
print("PASS Finder-style launch finds the nvm-only opencode (newest), others listed as 'also found'")
if not models["ok"]:
    raise SystemExit(f"opencode_models failed: {models}")
print(f"PASS model menu lists {len(models['data']['models'])} models (no 'Configuration is invalid')")
PY
    # forcing the stale build: warned below the tested minimum, clean error
    kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null || true
    env -i HOME="$fh" SHELL=/bin/sh PATH=/usr/bin:/bin VICUNA_OPENCODE_BIN="$fh/.opencode/bin/opencode" \
      "$bridge" "$port" >"$work/bridge2.log" 2>&1 &
    pid=$!
    for _ in $(seq 1 50); do curl -fsS "$base/health" >/dev/null 2>&1 && break; sleep 0.2; done
    info="$(invoke work_engine_info '{}')"
    models="$(invoke opencode_models '{}')"
    python3 - "$info" "$models" <<'PY'
import json, sys
info, models = json.loads(sys.argv[1]), json.loads(sys.argv[2])
w = info["data"].get("warning") or ""
assert "1.3.17" in w and "opencode upgrade" in w, info["data"]
print("PASS stale 1.3.17 forced: warning:", w)
err = models.get("error", "")
assert not models["ok"] and "\x1b" not in err and err.startswith("This opencode") and "opencode upgrade" in err, models
print("PASS its model-menu error is clean and starts with the upgrade hint:", err.splitlines()[0][:120])
PY
    ;;
  *)
    echo "usage: $0 missing | present <provider/model> | toolcap <provider/model> | finder"; exit 2 ;;
esac
