#!/usr/bin/env python3
"""End-to-end check for the Vicuna remote worker (bridge/src/bin/worker.rs).

Proves the things the desktop bridge cannot do:
  1. /health is open; every other route needs the bearer token
  2. a real agent turn runs on the worker (ollama cloud model, real tools)
  3. the event log is cursor-addressed and non-destructive
  4. a client that "disconnects" mid-turn and reconnects with ?after= receives
     exactly the events it missed, and the turn finished while nobody polled
  5. events survive the turn ending (readable after done)

Usage:
  python3 scripts/check-worker.py --port 1422 --token <t> \
      --workspace /tmp/vw-test/ws --model deepseek-v4.1-flash:cloud

Exit code 0 = all checks passed.
"""
import argparse
import json
import os
import sys
import time
import urllib.error
import urllib.request

PASS, FAIL = [], []


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print(f"  {'PASS' if ok else 'FAIL'}  {name}{(' — ' + detail) if detail else ''}")


def call(port, token, path, body=None, method=None):
    url = f"http://127.0.0.1:{port}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method=method or ("POST" if data else "GET"))
    req.add_header("Content-Type", "application/json")
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=300) as r:
            return r.status, json.loads(r.read().decode() or "{}")
    except urllib.error.HTTPError as e:
        raw = e.read().decode()
        try:
            return e.code, json.loads(raw or "{}")
        except json.JSONDecodeError:
            return e.code, {"raw": raw}


TOOL_DEF = {
    "type": "function",
    "function": {
        "name": "write",
        "description": "Write a file under the workspace root.",
        "parameters": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "content": {"type": "string"},
            },
            "required": ["path", "content"],
        },
    },
}

SYSTEM = (
    "You are a work agent with workspace tools. Call `write` exactly once with "
    "path `proof.txt` and content `written by the remote worker`. Then reply "
    "with the single word DONE. Do not call any other tool."
)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=1422)
    ap.add_argument("--token", required=True)
    ap.add_argument("--workspace", required=True)
    ap.add_argument("--model", default="deepseek-v4.1-flash:cloud")
    ap.add_argument("--session", default="sess-e2e")
    ap.add_argument("--no-turn", action="store_true", help="skip the live model turn")
    a = ap.parse_args()

    os.makedirs(a.workspace, exist_ok=True)
    proof = os.path.join(a.workspace, "proof.txt")
    if os.path.exists(proof):
        os.remove(proof)

    print("1. auth")
    st, j = call(a.port, None, "/health")
    check("health is open without a token", st == 200 and j.get("ok") is True, f"status={st}")
    st, j = call(a.port, None, "/status")
    check("unauthorized /status rejected", st == 401, f"status={st}")
    st, j = call(a.port, None, "/events/x")
    check("unauthorized /events rejected", st == 401, f"status={st}")
    st, j = call(a.port, "wrong-token-aaaaaaaaaaaaaa", "/status")
    check("wrong token rejected", st == 401, f"status={st}")

    st, j = call(a.port, a.token, "/status")
    check("authorized /status works", st == 200 and j.get("ok") is True, f"status={st}")
    check("worker reports an lb command", bool(j.get("lb")), str(j.get("lb")))

    print("2. live agent turn on the worker")
    unseen = "sess-never-" + str(int(time.time()))
    st, j = call(a.port, a.token, "/events/" + unseen)
    check("unknown session reads as an empty log", st == 200 and j.get("events") == [],
          f"status={st} events={len(j.get('events') or [])}")

    if not a.no_turn:
        body = {
            "cmd": "work_agent_start",
            "args": {
                "model": a.model,
                "provider": "ollama",
                "workspace": a.workspace,
                "sessionId": a.session,
                "messages": [
                    {"role": "system", "content": SYSTEM},
                    {"role": "user", "content": "Write proof.txt now."},
                ],
                "think": False,
                "tools": [TOOL_DEF],
                "chats": [],
                "mcpRoutes": {},
                "goalText": None,
                "goalPaused": True,
            },
        }
        t0 = time.time()
        st, j = call(a.port, a.token, "/invoke", body)
        check("work_agent_start accepted", st == 200 and j.get("ok") is True, str(j.get("error") or ""))
        check("turn id returned", bool(j.get("data")), str(j.get("data")))

        # The client now goes silent — it never polls /events. This is the
        # laptop-closed case: everything below only reads /status, which is
        # what a reconnecting client would ask.
        deadline = time.time() + 240
        running = True
        while time.time() < deadline:
            st, s_ = call(a.port, a.token, "/status")
            sess = [x for x in (s_.get("sessions") or []) if x.get("sessionId") == a.session]
            running = bool(sess and sess[0].get("running"))
            if not running:
                break
            time.sleep(1)
        check("turn completed with no client polling /events", not running, f"{time.time() - t0:.0f}s")

        st, j1 = call(a.port, a.token, f"/events/{a.session}?after=0")
        full = j1.get("events") or []
        check("log retained every event while nobody was connected", len(full) > 0,
              f"{len(full)} events, cursor={j1.get('cursor')}")
        cursors = [e["cursor"] for e in full]
        check("cursors are strictly increasing", cursors == sorted(set(cursors)), str(cursors))

        kinds = [e["event"].get("type") for e in full]
        check("log includes the terminal done event", "done" in kinds, str(sorted(set(kinds))))
        check("log includes the tool call", "toolstart" in kinds and "toolend" in kinds,
              str(sorted(set(kinds))))

        # Reconnect from a mid-turn cursor: exactly the tail, nothing replayed.
        mid = cursors[len(cursors) // 2]
        st, j2 = call(a.port, a.token, f"/events/{a.session}?after={mid}")
        tail = j2.get("events") or []
        check("?after= resumes at the cursor (no duplicates)",
              [e["cursor"] for e in tail] == [c for c in cursors if c > mid],
              f"after={mid} -> {len(tail)} events")

        st, j3 = call(a.port, a.token, f"/events/{a.session}?after={cursors[-1]}")
        check("reading at the head is empty (non-destructive)", (j3.get("events") or []) == [])

        text = "".join(e["event"].get("text", "") for e in full if e["event"].get("type") == "token")
        check("model streamed an answer", len(text.strip()) > 0, repr(text.strip()[:60]))

    print("3. the work actually happened on the worker's disk")
    ok = os.path.exists(proof)
    content = open(proof).read() if ok else ""
    check("proof.txt written by the agent", ok and "remote worker" in content, repr(content[:60]))

    print()
    print(f"{len(PASS)} passed, {len(FAIL)} failed")
    for f in FAIL:
        print("  failed:", f)
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
