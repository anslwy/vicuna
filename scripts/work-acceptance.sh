#!/usr/bin/env bash
# Headless acceptance for Work mode (opencode over ACP, Claude Code via
# `claude -p` stream-json, Codex via `codex app-server`, Antigravity via
# `agy acp`),
# through the real backend: the loopback bridge, no Tauri window needed.
#
#   scripts/work-acceptance.sh missing [engine]          # engine forced missing
#   scripts/work-acceptance.sh present <model> [engine]  # models listed, turn 1 + resumed turn 2
#   scripts/work-acceptance.sh mcp <model> [engine]      # a Vicuna MCP server's tool actually runs
#   scripts/work-acceptance.sh toolcap <model> [engine]  # MCP servers reach the engine, under 128 tools
#   scripts/work-acceptance.sh approve <model> [engine]  # 'ask' mode: a shell command waits for Allow, then runs
#   scripts/work-acceptance.sh combo <model> [engine]    # ONE turn: MCP tool with real args + 'ask' approval of a shell command
#   scripts/work-acceptance.sh stopq <model> [engine]    # a message queued mid-turn runs next; Stop interrupts a long reply
#   scripts/work-acceptance.sh ask <model> [engine]      # agent question (vicuna-mcp ask_user): card, answer continues, Stop cancels
#   scripts/work-acceptance.sh always <model> [engine]   # 'ask' mode: "Always allow" on the first shell command approves the second
#   scripts/work-acceptance.sh stoptool <model> [engine] # Stop during a long shell command ends the turn AND the command; missing workspace errors
#   scripts/work-acceptance.sh plan <model> codex       # Codex goal/plan updates show as a transcript note (1 turn)
#   scripts/work-acceptance.sh finder                    # Finder-style launch must not pick a stale opencode
#   scripts/work-acceptance.sh finder-codex              # Finder-style launch finds the newest codex (no model turn)
#
#   engine = opencode (default) | claude | codex | agy
#   e.g.  present opencode/beam-free          present haiku claude
#         present gpt-6-luna codex            combo gpt-6-luna codex
#         present gemini-3.8-flash-medium agy
#
# Build the bridge first:  (cd bridge && cargo build)
# Env: BRIDGE (bridge binary; default bridge/target/debug/vicuna-bridge or
#      src-tauri/target/debug/vicuna-bridge), PORT (default 1427),
#      VICUNA_OPENCODE_BIN / VICUNA_CLAUDE_BIN / VICUNA_CODEX_BIN / VICUNA_AGY_BIN,
#      VICUNA_CODEX_EFFORT (codex reasoning effort; default here: low),
#      VICUNA_CODEX_FAKE=1: run the real `codex app-server` against
#      scripts/fake-codex-provider.py (a scripted Responses API in a temp
#      CODEX_HOME): threads, tools, sandbox, approvals, MCP and resume are
#      real, no Codex quota is used; use model fake-codex.
#      (default: found on PATH).
# Billed turns (by the provider; opencode/beam-free is free): present = 2,
# mcp = 1, approve = 1, toolcap = 1, combo = 1, ask = 2 (Codex: present + combo = 3). Every mode runs the bridge with a temporary HOME so
# the user's real ~/vicuna/config is never touched; opencode keeps its real
# XDG config/auth, Claude its real ~/.claude login (symlinked in) and Codex
# its real ~/.codex (CODEX_HOME) and agy its real ~/.gemini (symlinked in,
# never copied).
set -euo pipefail

mode="${1:-}"
model="${2:-}"
engine="${3:-opencode}"
[ "$mode" = "missing" ] && engine="${2:-opencode}"
case "$engine" in opencode|claude|codex|agy) ;; *) echo "engine must be opencode, claude, codex or agy"; exit 2 ;; esac
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
work="${KEEP_WORK:-$(mktemp -d "${TMPDIR:-/tmp}/vicuna-accept.XXXXXX")}"; mkdir -p "$work"
real_home="$HOME"
pid=""
fake_pid=""
cleanup() { [ -n "$pid" ] && kill "$pid" 2>/dev/null; [ -n "$fake_pid" ] && kill "$fake_pid" 2>/dev/null; true; }
trap cleanup EXIT

# temp HOME with the engines' real logins linked in
mkdir -p "$work/home/vicuna/config" "$work/ws"
for f in .claude .claude.json .gemini; do
  [ -e "$real_home/$f" ] && ln -s "$real_home/$f" "$work/home/$f"
done
oc_bin="${VICUNA_OPENCODE_BIN:-$(command -v opencode || true)}"
cl_bin="${VICUNA_CLAUDE_BIN:-$(command -v claude || true)}"
cx_bin="${VICUNA_CODEX_BIN:-$(command -v codex || true)}"
agy_bin="${VICUNA_AGY_BIN:-$(command -v agy || true)}"
[ -z "$agy_bin" ] && [ -x "$real_home/.local/bin/agy" ] && agy_bin="$real_home/.local/bin/agy"
codex_home="${CODEX_HOME:-$real_home/.codex}"
if [ "$engine" = "codex" ] && [ -n "${VICUNA_CODEX_FAKE:-}" ]; then
  fake_port=$((port + 1000))
  codex_home="$work/codex-home"
  mkdir -p "$codex_home"
  printf 'model = "fake-codex"\nmodel_provider = "fake"\n[model_providers.fake]\nname = "fake"\nbase_url = "http://127.0.0.1:%s/v1"\nwire_api = "responses"\n' "$fake_port" >"$codex_home/config.toml"
  python3 "$root/scripts/fake-codex-provider.py" "$fake_port" "$work/fake-provider.log" &
  fake_pid=$!
  echo "codex: scripted provider on :$fake_port (CODEX_HOME=$codex_home)"
fi

start_bridge() { # extra env assignments...
  env HOME="$work/home" \
    XDG_CONFIG_HOME="${XDG_CONFIG_HOME:-$real_home/.config}" \
    XDG_DATA_HOME="${XDG_DATA_HOME:-$real_home/.local/share}" \
    XDG_CACHE_HOME="${XDG_CACHE_HOME:-$real_home/.cache}" \
    XDG_STATE_HOME="${XDG_STATE_HOME:-$real_home/.local/state}" \
    VICUNA_OPENCODE_BIN="${oc_bin:-/nonexistent/opencode}" \
    VICUNA_CLAUDE_BIN="${cl_bin:-/nonexistent/claude}" \
    VICUNA_CODEX_BIN="${cx_bin:-/nonexistent/codex}" \
    VICUNA_AGY_BIN="${agy_bin:-/nonexistent/agy}" \
    CODEX_HOME="$codex_home" VICUNA_CODEX_EFFORT="${VICUNA_CODEX_EFFORT:-low}" \
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

turn_args() { # channel-id fresh(true|false) prompt
  python3 - "$1" "$2" "$3" "$model" "$engine" "$work/ws" "accept-$$" <<'PY'
import json, sys
ch, fresh, prompt, model, engine, ws, sid = sys.argv[1:]
print(json.dumps({"engine": engine, "model": model, "workspace": ws, "sessionId": sid,
                  "freshSession": fresh == "true",
                  "messages": [{"role": "user", "content": prompt}],
                  "onEvent": {"__bridge_channel": ch}}))
PY
}

poll_turn() { # channel-id -> $work/events.jsonl
  : >"$work/events.jsonl"
  for _ in $(seq 1 600); do
    curl -sS "$base/poll/$1" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
    if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
    sleep 0.5
  done
}

run_turn() { # channel-id fresh prompt  (events -> $work/events.jsonl)
  local start
  start="$(invoke work_agent_start "$(turn_args "$1" "$2" "$3")")"
  echo "work_agent_start: $start"
  python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r' "$start"
  poll_turn "$1"
}

saved_session() { # the engine session id Vicuna saved for this chat
  python3 - "$work/home/vicuna/config/opencode-sessions.json" "$engine" "accept-$$|$work/ws" <<'PY'
import json, sys
path, engine, key = sys.argv[1:]
if engine in ("claude", "codex", "agy"):
    key = engine + "|" + key
try:
    print(json.load(open(path)).get(key, ""))
except FileNotFoundError:
    print("")
PY
}

want_label() { case "$engine" in claude|codex|agy) echo "$engine/$model" ;; *) echo "$model" ;; esac; }

check_turn() { # expect-substring label-check(yes|no)
  python3 - "$work/events.jsonl" "$1" "$2" "$(want_label)" <<'PY'
import json, sys
path, expect, check_label, want = sys.argv[1:]
evs = [json.loads(l) for l in open(path) if l.strip()]
types = [e.get("type") for e in evs]
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
print("  event types:", sorted(set(types)))
print("  assistant text:", repr(text[:200]))
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
assert "done" in types and "roundstart" in types and "roundend" in types, types
done = [e for e in evs if e.get("type") == "done"][-1]
assert done.get("stopped") is False, done
assert expect.upper() in text.upper(), f"expected {expect!r} in reply"
if check_label == "yes":
    models = [e.get("model") for e in evs if e.get("type") == "model"]
    assert models and models[-1] == want, f"model event {models} != {want}"
    print(f"  reply label = {models[-1]}")
PY
}

case "$mode" in
  missing)
    # exclusive overrides that do not exist: detection must report missing
    start_bridge VICUNA_OPENCODE_BIN=/nonexistent/opencode VICUNA_CLAUDE_BIN=/nonexistent/claude VICUNA_CODEX_BIN=/nonexistent/codex VICUNA_AGY_BIN=/nonexistent/agy
    [ "$engine" = "opencode" ] || invoke work_engine_select "{\"engine\":\"$engine\"}" >/dev/null
    info="$(invoke work_engine_info '{}')"
    echo "work_engine_info: $info"
    start="$(invoke work_agent_start "$(turn_args "miss-$$" true "hi")")"
    echo "work_agent_start: $start"
    python3 - "$info" "$start" "$engine" <<'PY'
import json, sys
info = json.loads(sys.argv[1]); start = json.loads(sys.argv[2]); engine = sys.argv[3]
d = info["data"]
assert info["ok"] and d["id"] == engine and d["installed"] is False, d
assert all(not e["installed"] for e in d["engines"]), d["engines"]
if engine == "claude":
    assert "Claude Code CLI not found, install it:" in d["missing"], d
    assert "Claude Code CLI not found, install it:" in start.get("error", ""), start
elif engine == "codex":
    assert d["label"] == "Codex" and d["installUrl"].startswith("https://developers.openai.com/codex"), d
    assert "Codex CLI not found, install it:" in d["missing"], d
    assert "Codex CLI not found, install it:" in start.get("error", ""), start
elif engine == "agy":
    assert d["label"] == "Antigravity" and d["installUrl"].startswith("https://antigravity.google"), d
    assert "Antigravity CLI (agy) not found, install it:" in d["missing"], d
    assert "Antigravity CLI (agy) not found, install it:" in start.get("error", ""), start
else:
    assert d["path"] is None and d["installUrl"].startswith("https://opencode.ai/"), d
    assert "OpenCode CLI not found, install it:" in start.get("error", ""), start
assert not start["ok"]
print(f"PASS missing ({engine}): installed=false, install hint shown, work_agent_start refused")
print("     (UI: workSendGate(info) => send-btn disabled; covered by `npm test`)")
PY
    ;;
  present)
    [ -n "$model" ] || { echo "usage: $0 present <model> [engine]"; exit 2; }
    start_bridge
    sel="$(invoke work_engine_select "{\"engine\":\"$engine\"}")"
    models="$(invoke work_models "{\"engine\":\"$engine\"}")"
    python3 - "$sel" "$models" "$model" "$engine" <<'PY'
import json, sys
sel, models, want, engine = json.loads(sys.argv[1]), json.loads(sys.argv[2]), sys.argv[3], sys.argv[4]
assert sel["ok"] and sel["data"]["id"] == engine and sel["data"]["installed"], sel
ids = [m["id"] for m in models["data"]["models"]] if models.get("ok") else []
assert models["ok"], models
print(f"work_models ({engine}): {len(ids)} models, e.g. {ids[:6]}")
import os
if os.environ.get("VICUNA_CODEX_FAKE") and engine == "codex":
    print(f"  (scripted provider: {want} is not in Codex's catalog; that is expected)")
else:
    assert ids and want in ids, f"{want} not in the engine's list"
print("PASS engine selected and models listed from the engine")
PY
    # the pick persists (backend file) and comes back as `current`
    invoke work_set_model "{\"engine\":\"$engine\",\"model\":\"$model\"}" >/dev/null
    cur="$(invoke work_models "{\"engine\":\"$engine\"}")"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["data"]["current"]==sys.argv[2], r["data"]["current"]; print("PASS model pick persisted:", sys.argv[2])' "$cur" "$model"
    echo "turn 1:"
    run_turn "t1-$$" true "Reply with exactly the word PONG and nothing else. Do not use any tools."
    check_turn PONG yes
    s1="$(saved_session)"
    echo "turn 2 (same chat; only the new message is sent, so the answer must come from the resumed session):"
    run_turn "t2-$$" false "What exact word did you reply with in your previous message? Answer with only that word. Do not use any tools."
    check_turn PONG yes
    s2="$(saved_session)"
    [ -n "$s1" ] && [ "$s1" = "$s2" ] || { echo "FAIL session not resumed: '$s1' -> '$s2'"; exit 1; }
    echo "PASS two turns via $engine; turn 2 resumed session $s1 and remembered turn 1"
    ;;
  mcp)
    [ -n "$model" ] || { echo "usage: $0 mcp <model> [engine]"; exit 2; }
    srv="$root/scripts/many-tools-mcp.py"
    python3 - "$work" "$srv" <<'PY'
import json, sys
work, srv = sys.argv[1], sys.argv[2]
json.dump({"mcp": {"vicuna_probe": {"type": "local", "command": ["python3", srv, "3"],
           "env": {"MANY_TOOLS_LOG": f"{work}/probe.log"}, "enabled": True}}},
          open(f"{work}/home/vicuna/config/vicuna.json", "w"))
PY
    start_bridge
    r="$(invoke mcp_connect "{\"config\":{\"id\":\"vicuna_probe\",\"name\":\"vicuna_probe\",\"transport\":\"stdio\",\"command\":[\"python3\",\"$srv\",\"3\"],\"env\":{\"MANY_TOOLS_LOG\":\"$work/probe.log\"},\"enabled\":true}}")"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print("mcp_connect vicuna_probe:", len(r["data"]["tools"]), "tools")' "$r"
    run_turn "mcp-$$" true "Call the MCP tool named noop_001 (from the vicuna_probe server) exactly once with the argument note set to \"vicuna-args-check\", then reply with the single word DONE."
    python3 - "$work" <<'PY'
import json, sys
work = sys.argv[1]
evs = [json.loads(l) for l in open(f"{work}/events.jsonl") if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
calls = [l.strip() for l in open(f"{work}/probe.log")] if __import__("os").path.exists(f"{work}/probe.log") else []
print("  probe server saw:", {m: calls.count(m) for m in sorted(set(calls))})
starts = [e for e in evs if e.get("type") == "toolstart"]
print("  tool cards:", [(e.get("name"), str(e.get("args"))[:60]) for e in starts])
assert calls.count("initialize") >= 2, "the engine never started the MCP server"
assert "tools/call" in calls, "the MCP tool was not called"
assert starts, "no tool card was shown"
print("PASS a Vicuna MCP server's tool ran inside the engine's turn")
probe = [e for e in starts if "noop_001" in str(e.get("name")) or "noop_001" in json.dumps(e.get("args"))]
assert probe and "vicuna-args-check" in json.dumps(probe[-1].get("args")), f"tool card args missing: {starts}"
print("PASS the tool card shows the real arguments:", json.dumps(probe[-1].get("args"))[:100])
PY
    ;;
  approve)
    [ -n "$model" ] || { echo "usage: $0 approve <model> [engine]"; exit 2; }
    # "Ask before running tools": the engine's permission request becomes an
    # ask card; the turn waits until the user answers. Answer Allow, then the
    # command must run and the transcript must say "Approved".
    start_bridge
    r="$(invoke work_set_approval '{"ask":true}')"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"] and r["data"]["askPermission"] is True, r; print("approval mode: ask")' "$r"
    start="$(invoke work_agent_start "$(turn_args "ap-$$" true "Use your shell/bash tool to run exactly this one command (it writes a file, so it needs approval): mkdir -p approve-test && echo vicuna-approve-ok > approve-test/out.txt && cat approve-test/out.txt. Then reply with its output.")")"
    echo "work_agent_start: $start"
    turn_id="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    : >"$work/events.jsonl"
    answered=""
    for _ in $(seq 1 600); do
      curl -sS "$base/poll/ap-$$" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
      if [ -z "$answered" ]; then
        ask="$(python3 - "$work/events.jsonl" <<'PY'
import json, sys
for l in open(sys.argv[1]):
    e = json.loads(l)
    if e.get("type") == "askrequest":
        opts = e["options"]
        pick = next((o for o in opts if o.lower() in ("allow", "allow once", "yes")), None) or next((o for o in opts if "allow" in o.lower() and "always" not in o.lower()), opts[0])
        print(json.dumps({"askId": e["ask_id"], "answer": pick, "question": e["question"], "options": opts}))
        break
PY
)"
        if [ -n "$ask" ]; then
          echo "ask card: $ask"
          aid="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["askId"])' "$ask")"
          ans="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["answer"])' "$ask")"
          invoke work_agent_answer "{\"turnId\":\"$turn_id\",\"askId\":\"$aid\",\"answer\":\"$ans\"}" >/dev/null
          answered=1
        fi
      fi
      if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
      sleep 0.5
    done
    invoke work_set_approval '{"ask":false}' >/dev/null
    python3 - "$work/events.jsonl" "$answered" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
assert sys.argv[2] == "1", "no ask card appeared: the command was not held for approval"
notes = "".join(e.get("text", "") for e in evs if e.get("type") == "displaynote")
assert "Approved:" in notes and "Auto-approved" not in notes, notes
outs = " ".join(e.get("output", "") for e in evs if e.get("type") == "toolend")
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
assert "vicuna-approve-ok" in outs + text, (outs[:300], text[:300])
import os
print("PASS ask mode: the command waited for the ask card, ran after Allow, transcript says Approved")
PY
    ;;
  stoptool)
    [ -n "$model" ] || { echo "usage: $0 stoptool <model> [engine]"; exit 2; }
    # Stop while a shell command runs: the turn ends (done stopped=true) and
    # the command's process is gone too (agy leaves it running on cancel).
    # Then a turn in a folder that does not exist fails with a readable error.
    start_bridge
    marker="vicuna-stoptool-$$"
    start="$(invoke work_agent_start "$(turn_args "st-$$" true "Use your shell tool to run exactly this one command and wait for it to finish: sleep 45 && echo $marker. Do not run it in the background.")")"
    turn_id="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    : >"$work/events.jsonl"
    stopped_at=""
    for _ in $(seq 1 240); do
      curl -sS "$base/poll/st-$$" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
      if [ -z "$stopped_at" ] && grep -q '"type": "toolstart"' "$work/events.jsonl" && pgrep -f "sleep 45" >/dev/null; then
        sleep 2
        echo "sleep is running; pressing Stop"
        invoke work_agent_stop "{\"turnId\":\"$turn_id\"}" >/dev/null
        stopped_at=1
      fi
      if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
      sleep 0.5
    done
    sleep 2
    left="$(pgrep -fa "sleep 45" || true)"
    python3 - "$work/events.jsonl" "$stopped_at" "$left" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
assert sys.argv[2] == "1", "the shell command never started"
assert not [e for e in evs if e.get("type") == "error"], evs[-3:]
done = [e for e in evs if e.get("type") == "done"]
assert done and done[-1].get("stopped") is True, done
assert not sys.argv[3].strip(), f"the command outlived Stop: {sys.argv[3]}"
print("PASS Stop mid-tool: turn stopped and the running command was ended")
PY
    start="$(invoke work_agent_start "$(python3 - "$model" "$engine" <<'PY'
import json, sys
print(json.dumps({"engine": sys.argv[2], "model": sys.argv[1], "workspace": "/nonexistent/vicuna-ws", "sessionId": "gone-1",
                  "freshSession": True, "messages": [{"role": "user", "content": "hi"}], "onEvent": {"__bridge_channel": "gone-1"}}))
PY
)")"
    poll_turn "gone-1"
    python3 - "$work/events.jsonl" "$start" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
start = json.loads(sys.argv[2])
msgs = [e.get("message", "") for e in evs if e.get("type") == "error"] + [start.get("error") or ""]
assert any("workspace folder not found: /nonexistent/vicuna-ws" in m for m in msgs), (start, evs[-3:])
print("PASS missing workspace: readable error:", [m for m in msgs if m][0])
PY
    ;;
  always)
    [ -n "$model" ] || { echo "usage: $0 always <model> [engine]"; exit 2; }
    # "Ask before running tools" + "Always allow": the first shell command's
    # card is answered Always allow; the second shell command in the same
    # session runs without a card (transcript: "Always allowed", then
    # "Auto-approved").
    start_bridge
    invoke work_set_approval '{"ask":true}' >/dev/null
    start="$(invoke work_agent_start "$(turn_args "al-$$" true "Use your shell tool twice, as two separate tool calls, one after the other: first run exactly: echo vicuna-first-ok > first.txt ; then run exactly: echo vicuna-second-ok > second.txt && cat first.txt second.txt . Then reply with the final output.")")"
    echo "work_agent_start: $start"
    turn_id="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    : >"$work/events.jsonl"
    seen=""
    for _ in $(seq 1 600); do
      curl -sS "$base/poll/al-$$" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
      for aid in $(python3 - "$work/events.jsonl" <<'PY'
import json, sys
for l in open(sys.argv[1]):
    e = json.loads(l)
    if e.get("type") == "askrequest":
        print(e["ask_id"])
PY
); do
        case " $seen " in *" $aid "*) continue ;; esac
        seen="$seen $aid"
        echo "ask card $aid -> Always allow"
        invoke work_agent_answer "{\"turnId\":\"$turn_id\",\"askId\":\"$aid\",\"answer\":\"Always allow\"}" >/dev/null
      done
      if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
      sleep 0.5
    done
    invoke work_set_approval '{"ask":false}' >/dev/null
    python3 - "$work/events.jsonl" "$work/ws" <<'PY'
import json, os, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
asks = [e for e in evs if e.get("type") == "askrequest"]
assert asks, "no ask card appeared"
print("  card options:", asks[0]["options"])
assert any(o.lower() == "always allow" for o in asks[0]["options"]), asks[0]["options"]
execs = [e for e in evs if e.get("type") == "toolstart" and e.get("name") == "exec"]
assert len(execs) >= 2, f"expected two shell calls, got {len(execs)}"
assert len(asks) < len(execs), f"{len(asks)} cards for {len(execs)} shell calls: Always allow was not remembered"
notes = "".join(e.get("text", "") for e in evs if e.get("type") == "displaynote")
assert "Always allowed:" in notes and "Auto-approved:" in notes, notes
ws = sys.argv[2]
assert open(os.path.join(ws, "second.txt")).read().strip() == "vicuna-second-ok", "second command did not run in the workspace"
print(f"PASS always: {len(asks)} card(s) for {len(execs)} shell calls; transcript says Always allowed, then Auto-approved")
PY
    ;;
  ask)
    [ -n "$model" ] || { echo "usage: $0 ask <model> [engine]"; exit 2; }
    # The agent asks the user through vicuna-mcp's ask_user: the card shows
    # the choices, the answer comes back as the tool result and the turn
    # continues with it. Turn 2: Stop while the question is pending ends the
    # turn cleanly (done stopped=true, no error).
    start_bridge
    ask_prompt='Before doing anything else, ask me which fruit to use. Ask with your ask_user tool (vicuna-mcp server), choices exactly: Mango, Kiwi, Papaya. After I answer, reply with exactly: FRUIT=<my answer>. Do not use any other tool.'
    ask_turn() { # channel fresh answer|stop
      local start turn_id handled=""
      start="$(invoke work_agent_start "$(turn_args "$1" "$2" "$ask_prompt")")"
      echo "work_agent_start: $start"
      turn_id="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
      : >"$work/events.jsonl"
      for _ in $(seq 1 600); do
        curl -sS "$base/poll/$1" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
        if [ -z "$handled" ]; then
          ask="$(python3 - "$work/events.jsonl" <<'PY'
import json, sys
for l in open(sys.argv[1]):
    e = json.loads(l)
    if e.get("type") == "askrequest" and e["ask_id"].startswith("ask-"):
        print(json.dumps(e)); break
PY
)"
          if [ -n "$ask" ]; then
            echo "ask card: $ask"
            python3 - "$ask" <<'PY'
import json, sys
e = json.loads(sys.argv[1])
assert [o.lower() for o in e["options"]] == ["mango", "kiwi", "papaya"], e["options"]
assert "fruit" in e["question"].lower(), e["question"]
PY
            aid="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["ask_id"])' "$ask")"
            if [ "$3" = "stop" ]; then
              sleep 2
              invoke work_agent_stop "{\"turnId\":\"$turn_id\"}" >/dev/null
            else
              invoke work_agent_answer "{\"turnId\":\"$turn_id\",\"askId\":\"$aid\",\"answer\":\"Kiwi\"}" >/dev/null
            fi
            handled=1
          fi
        fi
        if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
        sleep 0.5
      done
      [ -n "$handled" ] || { echo "FAIL no ask_user card appeared"; cat "$work/events.jsonl" | tail -20; exit 1; }
    }
    echo "turn 1 (answer Kiwi):"
    ask_turn "ask1-$$" true answer
    python3 - "$work/events.jsonl" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
assert not [e for e in evs if e.get("type") == "error"], evs[-3:]
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
print("  assistant text:", repr(text[-200:]))
assert "FRUIT=KIWI" in text.upper().replace(" ", ""), "the answer did not reach the agent"
done = [e for e in evs if e.get("type") == "done"][-1]
assert done["stopped"] is False, done
print("PASS the agent asked via ask_user, the card had the choices, and the turn continued with the answer")
PY
    echo "turn 2 (Stop while the question is pending):"
    ask_turn "ask2-$$" true stop
    python3 - "$work/events.jsonl" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
done = [e for e in evs if e.get("type") == "done"]
assert done and done[-1]["stopped"] is True, done
print("PASS Stop while a question was pending ended the turn cleanly (done stopped=true, no error)")
PY
    ;;
  plan)
    [ -n "$model" ] || { echo "usage: $0 plan <model> codex"; exit 2; }
    # Codex 0.16x offers goal tools (create_goal/update_goal) instead of
    # update_plan; either must reach the transcript as a note.
    start_bridge
    run_turn "plan-$$" true "First make a plan: set a goal with your goal tool (create_goal) or, if you have update_plan, a 3-step plan. Objective: add a README line. Then reply PLANNED. Do not run commands."
    python3 - "$work/events.jsonl" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
assert not [e for e in evs if e.get("type") == "error"], evs[-3:]
notes = [e.get("text", "") for e in evs if e.get("type") == "displaynote"]
hit = [n for n in notes if "**Goal" in n or "**Plan**" in n]
print("  notes:", [n.strip()[:120] for n in hit] or notes[-3:])
assert hit, "no Goal/Plan note"
print("PASS the agent's goal/plan reached the transcript")
PY
    ;;
  toolcap)
    [ -n "$model" ] || { echo "usage: $0 toolcap <model> [engine]"; exit 2; }
    # Vicuna's MCP dialog (~/vicuna/config/vicuna.json + live connections)
    # applies to Work: enabled, connected servers are handed to the engine in
    # session/new, but never past the provider's 128-tool limit.
    #   small_mcp: 5 tools   -> must be started by the engine
    #   big_mcp:   140 tools -> must be skipped, with a note naming it
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
    start_bridge
    # connect both the way the MCP dialog does at launch
    for spec in "small_mcp 5 small.log" "big_mcp 140 big.log"; do
      set -- $spec
      r="$(invoke mcp_connect "{\"config\":{\"id\":\"$1\",\"name\":\"$1\",\"transport\":\"stdio\",\"command\":[\"python3\",\"$srv\",\"$2\"],\"env\":{\"MANY_TOOLS_LOG\":\"$work/$3\"},\"enabled\":true}}")"
      python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"] and len(r["data"]["tools"])==int(sys.argv[2]), r; print("mcp_connect", sys.argv[3] + ":", len(r["data"]["tools"]), "tools")' "$r" "$2" "$1"
    done
    run_turn "cap-$$" true "Reply with exactly the word PONG and nothing else. Do not use any tools."
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
print(f"initialize calls: small_mcp={small} big_mcp={big} (1 = Vicuna's own connect, 2 = the engine too)")
errs = [e["message"] for e in evs if e.get("type") == "error"]
assert not errs, errs
assert any(e.get("type") == "done" for e in evs), "turn did not finish"
assert small >= 2, "Vicuna did not pass its enabled MCP server to the engine"
print("PASS enabled MCP server from Vicuna's dialog reached the engine")
assert big == 1, "Vicuna passed a 140-tool server: the request would exceed the 128-tool limit"
notes = "".join(e.get("text", "") for e in evs if e.get("type") == "displaynote")
assert "big_mcp (140 tools)" in notes, notes
print("PASS 140-tool server skipped with a note:", notes.strip()[:160])
PY
    ;;
  combo)
    [ -n "$model" ] || { echo "usage: $0 combo <model> [engine]"; exit 2; }
    # One billed turn for tight quotas (Codex): 'ask' mode on, a Vicuna MCP
    # server connected. The agent must call the MCP tool with real args and
    # run a shell command that waits for Allow. Every ask card is answered
    # Allow (an engine may also ask before the MCP call).
    srv="$root/scripts/many-tools-mcp.py"
    python3 - "$work" "$srv" <<'PY'
import json, sys
work, srv = sys.argv[1], sys.argv[2]
json.dump({"mcp": {"vicuna_probe": {"type": "local", "command": ["python3", srv, "3"],
           "env": {"MANY_TOOLS_LOG": f"{work}/probe.log"}, "enabled": True}}},
          open(f"{work}/home/vicuna/config/vicuna.json", "w"))
PY
    start_bridge
    r="$(invoke mcp_connect "{\"config\":{\"id\":\"vicuna_probe\",\"name\":\"vicuna_probe\",\"transport\":\"stdio\",\"command\":[\"python3\",\"$srv\",\"3\"],\"env\":{\"MANY_TOOLS_LOG\":\"$work/probe.log\"},\"enabled\":true}}")"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print("mcp_connect vicuna_probe:", len(r["data"]["tools"]), "tools")' "$r"
    r="$(invoke work_set_approval '{"ask":true}')"
    python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"] and r["data"]["askPermission"] is True, r; print("approval mode: ask")' "$r"
    start="$(invoke work_agent_start "$(turn_args "co-$$" true "Do these two steps in order. 1) Call the MCP tool noop_001 (vicuna_probe server) exactly once with the argument note set to \"vicuna-args-check\". 2) Use your shell tool to run exactly: mkdir -p approve-test && echo vicuna-approve-ok > approve-test/out.txt && cat approve-test/out.txt . Then reply with the command's output.")")"
    echo "work_agent_start: $start"
    turn_id="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    : >"$work/events.jsonl"
    : >"$work/answered"
    for _ in $(seq 1 600); do
      curl -sS "$base/poll/co-$$" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
      python3 - "$work/events.jsonl" "$work/answered" <<'PY' | while read -r aid; do
import json, sys
done_ids = set(open(sys.argv[2]).read().split())
for l in open(sys.argv[1]):
    e = json.loads(l)
    if e.get("type") == "askrequest" and e["ask_id"] not in done_ids:
        print(e["ask_id"])
PY
        echo "ask card $aid -> Allow"
        invoke work_agent_answer "{\"turnId\":\"$turn_id\",\"askId\":\"$aid\",\"answer\":\"Allow\"}" >/dev/null
        echo "$aid" >>"$work/answered"
      done
      if grep -qE '"type": "(done|error)"' "$work/events.jsonl"; then break; fi
      sleep 0.5
    done
    invoke work_set_approval '{"ask":false}' >/dev/null
    python3 - "$work" <<'PY'
import json, os, sys
work = sys.argv[1]
evs = [json.loads(l) for l in open(f"{work}/events.jsonl") if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
asks = [e for e in evs if e.get("type") == "askrequest"]
print("  ask cards:", [e["question"][:90] for e in asks])
starts = [e for e in evs if e.get("type") == "toolstart"]
print("  tool cards:", [(e.get("name"), json.dumps(e.get("args"))[:70]) for e in starts])
calls = [l.strip() for l in open(f"{work}/probe.log")] if os.path.exists(f"{work}/probe.log") else []
assert "tools/call" in calls, "the MCP tool was not called"
probe = [e for e in starts if "noop_001" in str(e.get("name"))]
assert probe and "vicuna-args-check" in json.dumps(probe[-1].get("args")), f"MCP card args missing: {starts}"
print("PASS MCP: the engine called the Vicuna MCP tool; card args:", json.dumps(probe[-1].get("args")))
assert any(any(w in e["question"] for w in ("exec", "Bash", "shell", "mkdir")) for e in asks), "the shell command was not held for approval"
notes = "".join(e.get("text", "") for e in evs if e.get("type") == "displaynote")
assert "Approved:" in notes and "Auto-approved" not in notes, notes
outs = " ".join(e.get("output", "") for e in evs if e.get("type") == "toolend")
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
assert "vicuna-approve-ok" in outs + text, (outs[:300], text[:300])
assert os.path.exists(f"{work}/ws/approve-test/out.txt"), "the approved command did not write its file"
print("PASS ask mode: the shell command waited for its ask card, ran after Allow; transcript says Approved")
PY
    ;;
  finder-codex)
    # Finder/Dock-style launch (env -i, PATH=/usr/bin:/bin): the real codex
    # only under nvm (npm i -g @openai/codex), a stale one in ~/.codex/bin.
    # Detection must pick the newest, list the other, and model/list must work
    # (no model turn).
    real_cx="$(command -v codex)" || { echo "codex not on PATH"; exit 2; }
    fh="$work/finder-home"
    nvm_bin="$fh/.nvm/versions/node/v24.14.1/bin"
    mkdir -p "$fh/.codex/bin" "$nvm_bin"
    printf '#!/bin/sh\necho "codex-cli 0.1.0"\n' >"$fh/.codex/bin/codex"
    chmod +x "$fh/.codex/bin/codex"
    ln -s "$real_cx" "$nvm_bin/codex"
    printf '{"engine":"codex"}' >"$fh/work-engine.json"
    mkdir -p "$fh/vicuna/config" && mv "$fh/work-engine.json" "$fh/vicuna/config/"
    env -i HOME="$fh" SHELL=/bin/sh PATH=/usr/bin:/bin USER="${USER:-user}" CODEX_HOME="$codex_home" \
      "$bridge" "$port" >"$work/bridge.log" 2>&1 &
    pid=$!
    for _ in $(seq 1 50); do curl -fsS "$base/health" >/dev/null 2>&1 && break; sleep 0.2; done
    info="$(invoke work_engine_info '{}')"
    models="$(invoke work_models '{"engine":"codex"}')"
    python3 - "$info" "$models" "$nvm_bin/codex" <<'PY'
import json, sys
info, models, want = json.loads(sys.argv[1]), json.loads(sys.argv[2]), sys.argv[3]
d = info["data"]
print("work_engine_info:", json.dumps({k: d.get(k) for k in ("id", "installed", "path", "version", "others")}, indent=1))
assert d["id"] == "codex" and d["installed"], d
assert d["path"] == want, f"picked {d['path']}, want the nvm install {want}"
assert d["version"] and d["version"] != "0.1.0", d
assert any(".codex/bin/codex (0.1.0)" in o for o in d.get("others", [])), d.get("others")
print("PASS Finder-style launch finds the nvm-only codex (newest), the stale one listed as 'also found'")
assert models["ok"] and models["data"]["models"], models
print(f"PASS model menu lists {len(models['data']['models'])} Codex models from model/list:", [m["id"] for m in models["data"]["models"]][:5])
PY
    ;;
  stopq)
    [ -n "$model" ] || { echo "usage: $0 stopq <model> [engine]"; exit 2; }
    # Queue + Stop (billed: 2 turns; free with VICUNA_CODEX_FAKE=1).
    # 1) a long reply; a message queued mid-turn must run as the next prompt
    #    of the same turn (queuedrained, a second round, its answer).
    # 2) a long reply; Stop mid-stream must end the turn as stopped, no error.
    start_bridge
    pull() { # channel: append new events
      curl -sS "$base/poll/$1" | python3 -c '
import json, sys
for ev in json.load(sys.stdin).get("events", []):
    print(json.dumps(ev))' >>"$work/events.jsonl"
    }
    wait_token() { # channel: until the first token
      for _ in $(seq 1 240); do
        pull "$1"
        grep -q '"type": "token"' "$work/events.jsonl" && return 0
        grep -qE '"type": "(done|error)"' "$work/events.jsonl" && return 1
        sleep 0.25
      done
      return 1
    }
    finish() { # channel: until done/error
      for _ in $(seq 1 600); do
        pull "$1"
        grep -qE '"type": "(done|error)"' "$work/events.jsonl" && return 0
        sleep 0.25
      done
    }
    : >"$work/events.jsonl"
    start="$(invoke work_agent_start "$(turn_args "q-$$" true "Please count slowly from 1 to 60, numbers separated by spaces. Do not use any tools.")")"
    tid="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    wait_token "q-$$" || { echo "FAIL no token"; cat "$work/events.jsonl"; exit 1; }
    invoke work_agent_queue "{\"turnId\":\"$tid\",\"id\":\"qmsg-1\",\"message\":{\"role\":\"user\",\"content\":\"Now reply with exactly the word QUEUED and nothing else.\"}}" >/dev/null
    echo "queued a message mid-turn"
    finish "q-$$"
    python3 - "$work/events.jsonl" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
types = [e["type"] for e in evs]
assert "queuedrained" in types, types
qi = types.index("queuedrained")
after = "".join(e.get("text", "") for e in evs[qi:] if e.get("type") == "token")
assert types.count("roundstart") >= 2 and "QUEUED" in after.upper(), (types, after[:200])
assert [e for e in evs if e["type"] == "done"][-1]["stopped"] is False
print("PASS queue: the message queued mid-turn ran as the next prompt; reply:", after.strip()[:40])
PY
    : >"$work/events.jsonl"
    start="$(invoke work_agent_start "$(turn_args "s-$$" false "Please count slowly from 1 to 60 again, numbers separated by spaces. Do not use any tools.")")"
    tid="$(python3 -c 'import json,sys; r=json.loads(sys.argv[1]); assert r["ok"], r; print(r["data"])' "$start")"
    wait_token "s-$$" || { echo "FAIL no token"; cat "$work/events.jsonl"; exit 1; }
    t0=$(date +%s)
    invoke work_agent_stop "{\"turnId\":\"$tid\"}" >/dev/null
    finish "s-$$"
    t1=$(date +%s)
    python3 - "$work/events.jsonl" "$((t1 - t0))" <<'PY'
import json, sys
evs = [json.loads(l) for l in open(sys.argv[1]) if l.strip()]
errs = [e for e in evs if e.get("type") == "error"]
assert not errs, errs
done = [e for e in evs if e["type"] == "done"]
assert done and done[-1]["stopped"] is True, done
text = "".join(e.get("text", "") for e in evs if e.get("type") == "token")
assert "60" not in text.split(), "the reply ran to the end: Stop did not interrupt"
print(f"PASS stop: the turn ended as stopped {sys.argv[2]}s after Stop, reply cut at: ...{text.strip()[-20:]!r}")
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
    echo "usage: $0 missing [engine] | present <model> [engine] | mcp <model> [engine] | approve <model> [engine] | toolcap <model> [engine] | combo <model> [engine] | stopq <model> [engine] | finder | finder-codex"; exit 2 ;;
esac
