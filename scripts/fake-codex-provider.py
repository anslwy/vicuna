#!/usr/bin/env python3
"""A scripted OpenAI Responses-API endpoint for testing Vicuna's Codex engine
without spending Codex quota. The real `codex app-server` runs against it
(custom model_provider in a temporary CODEX_HOME), so threads, tools, the
sandbox, approvals, MCP and resume are all real; only the model's replies are
scripted:

  "count slowly"                -> 60 numbers over ~15 s (Stop / queue tests)
  "exactly the word W"          -> W
  prompt mentions noop_001      -> call the MCP tool (note=vicuna-args-check)
  prompt says "run exactly: X"  -> call the shell tool with X
  prompt says "ask me which fruit" -> call vicuna-mcp ask_user (Mango/Kiwi/Papaya),
                                   then reply FRUIT=<the answer>
  prompt says "make a plan"     -> call update_plan (3 steps) if offered, else
                                   create_goal (codex 0.16x), then PLANNED
  "previous message" question   -> answer with the word the model said
                                   earlier in the SAME thread history (proves
                                   resume: a fresh thread has no PONG)
  otherwise                     -> PONG
Each reply streams a reasoning summary and the text in small deltas.

  fake-codex-provider.py <port> [log.jsonl]
"""
import json, re, sys, time, uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = sys.argv[2] if len(sys.argv) > 2 else None


def texts(item):
    c = item.get("content")
    if isinstance(c, str):
        return c
    return "".join(p.get("text", "") for p in (c or []) if isinstance(p, dict))


CMD = re.compile(r"run exactly[^:]*: (.+?)\s*\.\s*(?:Then|$)")
NAMESPACE = {}  # tool name -> its namespace (MCP servers come as groups)


def tool_names(body):
    names = []
    for t in body.get("tools", []):
        n = t.get("name") or (t.get("function") or {}).get("name")
        if n:
            names.append(n)
        for sub in t.get("tools", []) or []:  # namespaced tool groups
            if sub.get("name"):
                names.append(sub["name"])
                NAMESPACE[sub["name"]] = n
    return names


def decide(body):
    items = body.get("input", [])
    last = items[-1] if items else {}
    names = tool_names(body)
    if last.get("type") == "function_call_output":
        out = last.get("output")
        out = out if isinstance(out, str) else json.dumps(out)
        # after the MCP call, go on to the shell step if the prompt has one
        user = [texts(i) for i in items if i.get("role") == "user"][-1]
        calls = [i for i in items if i.get("type") == "function_call"]
        if calls and (calls[-1].get("name") or "").endswith("ask_user"):
            a = re.search(r"The user answered: ([^\"\n]+)", out)
            return [("text", f"FRUIT={a.group(1).strip()}" if a else f"NO ANSWER: {out[:120]}")]
        if calls and calls[-1].get("name") in ("update_plan", "create_goal"):
            return [("text", "PLANNED")]
        done_calls = [i.get("name") for i in items if i.get("type") == "function_call"]
        m = CMD.search(user)
        if m and not any(n and ("shell" in n or "exec" in n) for n in done_calls):
            return shell_call(names, m.group(1))
        out_text = re.sub(r"\s+", " ", out)[-200:]
        return [("text", f"Output: {out_text}")]
    user_msgs = [texts(i) for i in items if i.get("role") == "user"]
    user = user_msgs[-1] if user_msgs else ""
    if "noop_001" in user:
        mcp = next((n for n in names if n.endswith("noop_001")), None)
        if mcp:
            return [("call", mcp, {"note": "vicuna-args-check"})]
        return [("text", f"no noop_001 tool offered (tools: {names})")]
    if "ask me which fruit" in user:
        ask = next((n for n in names if n.endswith("ask_user")), None)
        if ask:
            return [("call", ask, {"question": "Which fruit should I use?", "choices": ["Mango", "Kiwi", "Papaya"]})]
        return [("text", f"no ask_user tool offered (tools: {names})")]
    if "make a plan" in user and "update_plan" not in names and "create_goal" in names:
        return [("call", "create_goal", {"objective": "fake goal: make the change and run the tests"})]
    if "make a plan" in user and "update_plan" in names:
        return [("call", "update_plan", {"explanation": "fake plan", "plan": [
            {"step": "read the code", "status": "completed"},
            {"step": "make the change", "status": "in_progress"},
            {"step": "run the tests", "status": "pending"}]})]
    m = CMD.search(user)
    if m:
        return shell_call(names, m.group(1))
    if "count slowly" in user:
        return [("slow", " ".join(str(i) for i in range(1, 61)))]
    w = re.search(r"exactly the word (\w+)", user)
    if w and "previous message" not in user:
        return [("text", w.group(1))]
    if "previous message" in user:
        earlier = [texts(i) for i in items if i.get("role") == "assistant"]
        word = next((w for t in earlier for w in re.findall(r"[A-Z]{4,}", t)), "NOTHING")
        return [("text", word)]
    return [("text", "PONG")]


def shell_call(names, cmd):
    for n in ("exec_command", "shell_command", "shell", "local_shell"):
        if n in names:
            if n == "exec_command":
                return [("call", n, {"cmd": cmd})]
            if n == "shell":
                return [("call", n, {"command": ["bash", "-lc", cmd]})]
            return [("call", n, {"command": cmd})]
    return [("text", f"no shell tool offered (tools: {names})")]


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def do_GET(self):
        body = json.dumps({"object": "list", "data": [{"id": "fake-codex", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        n = int(self.headers.get("content-length", 0))
        raw = self.rfile.read(n)
        try:
            body = json.loads(raw)
        except Exception:
            body = {}
        plan = decide(body)
        if LOG:
            with open(LOG, "a") as f:
                f.write(json.dumps({"path": self.path, "tools": tool_names(body), "n_input": len(body.get("input", [])),
                                    "last": (body.get("input") or [{}])[-1], "plan": plan}) + "\n")
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        rid = "resp_" + uuid.uuid4().hex[:12]

        def ev(t, **kw):
            kw["type"] = t
            self.wfile.write(f"event: {t}\ndata: {json.dumps(kw)}\n\n".encode())
            self.wfile.flush()

        ev("response.created", response={"id": rid, "object": "response", "status": "in_progress", "output": []})
        idx = 0
        # reasoning summary, streamed
        rs_id = "rs_" + uuid.uuid4().hex[:8]
        ev("response.output_item.added", output_index=idx, item={"type": "reasoning", "id": rs_id, "summary": []})
        ev("response.reasoning_summary_part.added", item_id=rs_id, output_index=idx, summary_index=0, part={"type": "summary_text", "text": ""})
        thought = "Planning the reply"
        for w in [thought[:8], thought[8:]]:
            ev("response.reasoning_summary_text.delta", item_id=rs_id, output_index=idx, summary_index=0, delta=w)
            time.sleep(0.05)
        ev("response.output_item.done", output_index=idx, item={"type": "reasoning", "id": rs_id, "summary": [{"type": "summary_text", "text": thought}]})
        idx += 1
        for step in plan:
            if step[0] in ("text", "slow"):
                text = step[1]
                delay = 0.25 if step[0] == "slow" else 0.03
                mid = "msg_" + uuid.uuid4().hex[:8]
                ev("response.output_item.added", output_index=idx, item={"type": "message", "id": mid, "role": "assistant", "content": []})
                for i in range(0, len(text), 3):
                    ev("response.output_text.delta", item_id=mid, output_index=idx, content_index=0, delta=text[i:i + 3])
                    time.sleep(delay)
                ev("response.output_item.done", output_index=idx, item={"type": "message", "id": mid, "role": "assistant",
                                                                        "content": [{"type": "output_text", "text": text}]})
            else:
                _, name, args = step
                cid = "call_" + uuid.uuid4().hex[:8]
                item = {"type": "function_call", "id": "fc_" + cid, "call_id": cid, "name": name, "arguments": json.dumps(args)}
                if name in NAMESPACE:
                    item["namespace"] = NAMESPACE[name]
                ev("response.output_item.added", output_index=idx, item=item)
                ev("response.output_item.done", output_index=idx, item=item)
            idx += 1
        ev("response.completed", response={"id": rid, "object": "response", "status": "completed", "output": [],
                                           "usage": {"input_tokens": 100, "input_tokens_details": {"cached_tokens": 0},
                                                     "output_tokens": 12, "output_tokens_details": {"reasoning_tokens": 4},
                                                     "total_tokens": 112}})
        self.close_connection = True


if __name__ == "__main__":
    ThreadingHTTPServer(("127.0.0.1", int(sys.argv[1])), H).serve_forever()
