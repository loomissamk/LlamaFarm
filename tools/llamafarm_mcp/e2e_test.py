#!/usr/bin/env python3
"""End-to-end MCP stdio test: initialize -> tools/list -> tools/call -> resource read."""
import json
import os
import subprocess
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
VENV_PYTHON = os.path.join(HERE, ".venv", "bin", "python")
SERVER = [VENV_PYTHON if os.path.isfile(VENV_PYTHON) else sys.executable,
          os.path.join(HERE, "server.py")]

proc = subprocess.Popen(SERVER, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True, bufsize=1)

def send(obj):
    line = json.dumps(obj)
    proc.stdin.write(line + "\n")
    proc.stdin.flush()
    return line

def recv():
    line = proc.stdout.readline()
    if not line:
        raise RuntimeError("server closed stdout; stderr: " + proc.stderr.read()[:2000])
    return json.loads(line)

def rpc(rid, method, params=None):
    msg = {"jsonrpc": "2.0", "id": rid, "method": method}
    if params is not None:
        msg["params"] = params
    send(msg)
    while True:
        resp = recv()
        if resp.get("id") == rid:
            return resp

results = {}

r = rpc(1, "initialize", {
    "protocolVersion": "2024-11-05",
    "capabilities": {},
    "clientInfo": {"name": "e2e-test", "version": "1.0"},
})
results["initialize"] = r.get("result", {}).get("serverInfo")
if "error" in r:
    print("INITIALIZE FAILED:", json.dumps(r["error"])); sys.exit(1)

send({"jsonrpc": "2.0", "method": "notifications/initialized"})

r = rpc(2, "tools/list")
tools = [t["name"] for t in r.get("result", {}).get("tools", [])]
results["tools"] = tools
print("TOOLS:", tools)

r = rpc(3, "tools/call", {"name": "llamafarm_health", "arguments": {}})
content = r.get("result", {}).get("content", [])
text = content[0].get("text", "") if content else ""
results["health"] = text[:300]
print("HEALTH:", text[:300])

r = rpc(6, "tools/call", {"name": "llamafarm_runs", "arguments": {}})
content = r.get("result", {}).get("content", [])
text = content[0].get("text", "") if content else ""
results["runs"] = text[:300]
print("RUNS:", text[:300])

r = rpc(4, "tools/call", {"name": "llamafarm_chat",
                          "arguments": {"prompt": "Reply with exactly: LAMBO-OK-42", "max_tokens": 50}})
content = r.get("result", {}).get("content", [])
text = content[0].get("text", "") if content else ""
results["chat"] = text
print("CHAT:", text)

r = rpc(5, "resources/read", {"uri": "llamafarm://model"})
content = r.get("result", {}).get("contents", [])
text = content[0].get("text", "") if content else ""
results["model_resource"] = text[:300]
print("MODEL RESOURCE:", text[:300])

if os.environ.get("LLAMAFARM_TEST_AGENT_BATCH") == "1":
    r = rpc(7, "tools/call", {
        "name": "llamafarm_agent_batch",
        "arguments": {
            "tasks": [
                {"id": "math_a", "prompt": "Read-only question: what is 2 + 3? Reply briefly."},
                {"id": "math_b", "prompt": "Read-only question: what is 4 + 5? Reply briefly."},
            ],
            "timeout_seconds": 120,
        },
    })
    content = r.get("result", {}).get("content", [])
    batch_text = content[0].get("text", "") if content else ""
    try:
        results["agent_batch"] = json.loads(batch_text)
    except json.JSONDecodeError:
        results["agent_batch"] = {"parse_error": batch_text[:500]}
    print("AGENT BATCH:", json.dumps(results["agent_batch"], indent=2)[:2000])

proc.stdin.close()
try:
    proc.wait(timeout=5)
except subprocess.TimeoutExpired:
    proc.kill()

print("\n=== SUMMARY ===")
print(json.dumps(results, indent=2)[:2000])
ok = (
    results.get("initialize")
    and "llamafarm_chat" in results.get("tools", [])
    and "llamafarm_agent" in results.get("tools", [])
    and "llamafarm_agent_batch" in results.get("tools", [])
    and "llamafarm_runs" in results.get("tools", [])
    and "llamafarm_run_status" in results.get("tools", [])
    and "llamafarm_cancel_run" in results.get("tools", [])
    and '"runs"' in results.get("runs", "")
    and "LAMBO-OK-42" in results.get("chat", "")
    and (os.environ.get("LLAMAFARM_TEST_AGENT_BATCH") != "1"
         or (results.get("agent_batch", {}).get("all_ok") is True
             and len(results["agent_batch"].get("results", [])) == 2
             and all(item.get("http_status") == 200
                     for item in results["agent_batch"]["results"])))
)
print("\nE2E RESULT:", "PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
