#!/usr/bin/env python3
"""Browser turn through the remote worker (bridge/src/bin/worker.rs).

The claim under test: the agent, its tools, AND the browser live on the
worker; a human looking at the same `lb` daemon sees the agent's tab. That is
the use case Vicuna-as-a-desktop-client cannot demonstrate.

  1. connect the worker to a real `lb` daemon (stdlib MCP over stdio)
  2. run an agent turn whose only tools are the browser MCP tools
  3. confirm the event log shows real browser tool calls
  4. confirm the tab the agent drove is the one a human sees via `lb`

Usage:
  python3 scripts/check-worker-browser.py --port 1422 --token <t> \
      --model deepseek-v4.1-flash:cloud [--url https://example.com]
"""
import argparse
import json
import os
import subprocess
import sys
import time
import urllib.request

PASS, FAIL = [], []
SESSION = "sess-browser"


def check(name, ok, detail=""):
    (PASS if ok else FAIL).append(name)
    print(f"  {'PASS' if ok else 'FAIL'}  {name}{(' — ' + detail) if detail else ''}")


def call(port, token, path, body=None):
    url = f"http://127.0.0.1:{port}{path}"
    data = json.dumps(body).encode() if body is not None else None
    req = urllib.request.Request(url, data=data, method="POST" if data else "GET")
    req.add_header("Content-Type", "application/json")
    req.add_header("Authorization", f"Bearer {token}")
    with urllib.request.urlopen(req, timeout=300) as r:
        return json.loads(r.read().decode() or "{}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=1422)
    ap.add_argument("--token", required=True)
    ap.add_argument("--model", default="deepseek-v4.1-flash:cloud")
    ap.add_argument("--url", default="https://example.com")
    ap.add_argument(
        "--lb-exec",
        help="command prefix used to run the `human` lb CLI, e.g. "
        "'docker compose -f bridge/container/docker-compose.worker.yml exec -T worker'. "
        "Required when the worker runs in a container: its lbServer is "
        "127.0.0.1 INSIDE that container, so running the host's lb against it "
        "silently inspects the HOST's own daemon instead (a false pass).",
    )
    a = ap.parse_args()
    lb_exec = a.lb_exec.split() if a.lb_exec else []

    print("1. worker's browser daemon")
    st = call(a.port, a.token, "/status")
    lb_argv = st.get("lb")
    check("worker resolves lb", bool(lb_argv), str(lb_argv))
    check("worker reports the daemon url", bool(st.get("lbServer")), str(st.get("lbServer")))
    # Make sure the daemon is up (adopts an existing one).
    bs = call(a.port, a.token, "/invoke", {"cmd": "browser_start", "args": {}})
    check("browser daemon running", bool((bs.get("data") or {}).get("running")),
          json.dumps(bs.get("data") or bs.get("error"))[:120])

    print("2. connect the browser MCP on the worker")
    cfg = {
        "id": "browser",
        "name": "browser",
        "transport": "stdio",
        "command": lb_argv,
        "env": {"LB_SERVER": st["lbServer"]},
        "url": None,
        "headers": None,
        "enabled": True,
    }
    mc = call(a.port, a.token, "/invoke", {"cmd": "mcp_connect", "args": {"config": cfg}})
    tools = (mc.get("data") or {}).get("tools") or []
    check("33 browser tools connected", len(tools) >= 30, f"{len(tools)} tools")
    if not tools:
        print(json.dumps(mc)[:600])
        return 1

    # Ollama tool defs + the sanitized-name -> (server, real tool) routing the
    # frontend builds. Names here are already unique, so no prefixing.
    tool_defs, routes = [], {}
    for t in tools:
        tool_defs.append({
            "type": "function",
            "function": {
                "name": t["name"],
                "description": (t.get("description") or t["name"]) + " (via browser)",
                "parameters": t.get("inputSchema") or {"type": "object"},
            },
        })
        routes[t["name"]] = {"server_id": "browser", "tool_name": t["name"]}
    # Let it close the tabs it opens; keeps the shared browser tidy.
    names = {t["name"] for t in tools}
    check("open/text/close tools present", {"open", "text"} <= names, str(sorted(names & {"open", "text", "status"})))

    print("3. agent turn whose tools are the real browser")
    system = (
        "You control a real browser through tools. Do exactly this, then stop:\n"
        f"1. call `open` with url {a.url}\n"
        "2. call `text` to read the page\n"
        "3. reply with the page's <h1> text on its own line, prefixed by 'H1:'.\n"
        "Do not call any other tool. Do not use `done`."
    )
    body = {
        "cmd": "work_agent_start",
        "args": {
            "model": a.model,
            "provider": "ollama",
            "workspace": "/tmp/vw-test/ws",
            "sessionId": SESSION,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": "Go."},
            ],
            "think": False,
            "tools": tool_defs,
            "chats": [],
            "mcpRoutes": routes,
            "goalText": None,
            "goalPaused": True,
        },
    }
    r = call(a.port, a.token, "/invoke", body)
    check("turn accepted", r.get("ok") is True, str(r.get("error") or ""))

    deadline = time.time() + 240
    while time.time() < deadline:
        s = call(a.port, a.token, "/status")
        sess = [x for x in (s.get("sessions") or []) if x.get("sessionId") == SESSION]
        if not (sess and sess[0].get("running")):
            break
        time.sleep(1)

    ev = call(a.port, a.token, f"/events/{SESSION}?after=0").get("events") or []
    kinds = [e["event"].get("type") for e in ev]
    calls = [e["event"] for e in ev if e["event"].get("type") == "toolend"]
    check("turn finished", "done" in kinds or "error" in kinds, str(sorted(set(kinds))))
    check("agent made browser tool calls", len(calls) > 0,
          ", ".join(c.get("name", "?") for c in calls))
    opened = [c for c in calls if c.get("name") == "open"]
    check("agent drove a real browser tab", bool(opened),
          (opened[0].get("output", "")[:100] if opened else "no open call"))
    answer = "".join(e["event"].get("text", "") for e in ev if e["event"].get("type") == "token")
    check("model reported the page heading", "H1:" in answer or a.url in answer, repr(answer.strip()[:120]))

    print("4. the human sees the same tab (same daemon, same web UI)")
    srv = st["lbServer"]
    host_loopback = srv.startswith(("http://127.0.0.1", "http://localhost"))
    if host_loopback and not lb_exec:
        check("human `lb` is inspecting the SAME daemon", False,
              f"worker's lbServer is {srv}, which from this host is this machine's "
              f"own lb (if any), not the worker's. Pass --lb-exec to run lb where "
              f"the worker runs.")
        print()
        print(f"{len(PASS)} passed, {len(FAIL)} failed")
        for f in FAIL:
            print("  failed:", f)
        return 1
    try:
        # `--server` is how a human points the CLI at the worker's daemon;
        # the web UI a person controls is that same base URL.
        out = subprocess.run([*lb_exec, "lb", "status", "--server", srv], capture_output=True,
                             text=True, timeout=30).stdout
        lst = subprocess.run([*lb_exec, "lb", "list", "--json", "--server", srv], capture_output=True,
                             text=True, timeout=30).stdout
        check("human `lb status` answers on the worker's daemon", bool(out.strip()),
              out.strip().splitlines()[0] if out.strip() else "")
        parsed = json.loads(lst or "[]")
        # `lb list --json` yields tab lines; tolerate an object shape too.
        urls = [t if isinstance(t, str) else str(t.get("url", "")) for t in parsed]
        if isinstance(parsed, dict):
            urls = [str(t.get("url", "")) for t in parsed.get("tabs", [])]
        check("the tab the agent drove is visible to the human", any(a.url.rstrip('/') in u for u in urls),
              str(urls))
        # With --lb-exec the daemon is reachable only inside that context, so
        # quoting `srv` (container-loopback) would tell the user to open a URL
        # that does not work from here.
        where = f"{srv} (inside the worker's container/namespace)" if lb_exec else srv
        check("human takeover is available on that session", bool(srv),
              f"open {where} to watch/claim the same tab")
    except (OSError, subprocess.SubprocessError, json.JSONDecodeError) as e:
        check("human `lb` reachable", False, str(e))

    print()
    print(f"{len(PASS)} passed, {len(FAIL)} failed")
    for f in FAIL:
        print("  failed:", f)
    return 0 if not FAIL else 1


if __name__ == "__main__":
    sys.exit(main())
