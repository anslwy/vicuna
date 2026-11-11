#!/usr/bin/env python3
"""Minimal stdio MCP server exposing N no-op tools (default 140).

Used by scripts/work-acceptance.sh `toolcap` to reproduce a user's opencode
config whose MCP servers push the request past a provider's 128-tool limit.
"""
import json
import os
import sys

N = int(sys.argv[1]) if len(sys.argv) > 1 else 140
TOOLS = [
    {
        "name": f"noop_{i:03d}",
        "description": f"No-op test tool {i}.",
        "inputSchema": {"type": "object", "properties": {}},
    }
    for i in range(N)
]


def reply(msg_id, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id, "result": result}) + "\n")
    sys.stdout.flush()


for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        msg = json.loads(line)
    except ValueError:
        continue
    method, msg_id = msg.get("method"), msg.get("id")
    if os.environ.get("MANY_TOOLS_LOG"):
        with open(os.environ["MANY_TOOLS_LOG"], "a") as log:
            log.write(f"{method}\n")
    if msg_id is None:
        continue  # notifications
    if method == "initialize":
        reply(msg_id, {
            "protocolVersion": msg.get("params", {}).get("protocolVersion", "2024-11-05"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "many-tools", "version": "1"},
        })
    elif method == "tools/list":
        reply(msg_id, {"tools": TOOLS})
    elif method == "tools/call":
        reply(msg_id, {"content": [{"type": "text", "text": "ok"}]})
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": msg_id,
                                     "error": {"code": -32601, "message": "method not found"}}) + "\n")
        sys.stdout.flush()
