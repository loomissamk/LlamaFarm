#!/usr/bin/env bash
# LlamaFarm Model Benchmark
#
# Two test tiers:
#   1. CAPABILITY  — direct Ollama API: does the model emit the right tool call JSON?
#   2. INTEGRATION — full LlamaFarm agent loop via CLI inside container:
#                    does the model actually complete a multi-turn agentic task?
#
# Usage:
#   ./scripts/bench.sh [OPTIONS]
#
# Options:
#   --host    <url>            LlamaFarm host        (default: http://192.168.1.154:42617)
#   --ollama  <url>            Ollama API base        (default: derived from --host)
#   --ssh     <user@host>      SSH target for integration tests (default: bat@192.168.1.154)
#   --models  <m1,m2,...>      Comma-separated model list (default: all from /api/tags)
#   --tests   <t1,t2,...>      Capability: shell,websearch,taskplan,dbquery
#                              Integration: int_shell,int_file,int_plan,int_calc
#                              (default: all capability tests + int_shell,int_file)
#   --timeout <secs>           Per-test timeout (default: 180)
#   --out     <file>           Write JSON results to file
#   --no-integration           Skip integration tests entirely
#
# Examples:
#   ./scripts/bench.sh
#   ./scripts/bench.sh --models qwen3.6:35b,devstral-small-2:latest
#   ./scripts/bench.sh --tests shell,websearch,int_shell --timeout 60

set -euo pipefail

LLAMAFARM_HOST="${LLAMAFARM_HOST:-http://192.168.1.154:42617}"
OLLAMA_HOST=""
SSH_TARGET="${LF_SSH:-bat@192.168.1.154}"
MODELS=""
TESTS="shell,websearch,taskplan,dbquery,int_shell,int_file"
TIMEOUT=300
OUT_FILE=""
NO_INTEGRATION=0

while [[ $# -gt 0 ]]; do
  case $1 in
    --host)    LLAMAFARM_HOST="$2"; shift 2 ;;
    --ollama)  OLLAMA_HOST="$2";    shift 2 ;;
    --ssh)     SSH_TARGET="$2";     shift 2 ;;
    --models)  MODELS="$2";         shift 2 ;;
    --tests)   TESTS="$2";          shift 2 ;;
    --timeout) TIMEOUT="$2";        shift 2 ;;
    --out)     OUT_FILE="$2";       shift 2 ;;
    --no-integration) NO_INTEGRATION=1; shift ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

# Derive Ollama host from LlamaFarm host if not set explicitly
if [[ -z "$OLLAMA_HOST" ]]; then
  LF_BASE="${LLAMAFARM_HOST%:*}"
  OLLAMA_HOST="${LF_BASE}:11434"
fi

python3 - \
  "$LLAMAFARM_HOST" "$OLLAMA_HOST" "$SSH_TARGET" \
  "$MODELS" "$TESTS" "$TIMEOUT" "${OUT_FILE:-}" "$NO_INTEGRATION" \
<< 'PYEOF'
import sys, json, time, urllib.request, urllib.error, datetime, subprocess, textwrap, shlex

lf_host, ollama_host, ssh_target, models_arg, tests_arg, timeout_str, out_file, no_int = sys.argv[1:]
TIMEOUT = int(timeout_str)
NO_INT  = no_int == "1"

# ─────────────────────────────────────────────────────────────────────────────
# Tool schemas (capability tier — single-turn Ollama API tests)
# ─────────────────────────────────────────────────────────────────────────────

def tool(name, desc, props, required=None):
    return {"type": "function", "function": {
        "name": name, "description": desc,
        "parameters": {"type": "object", "properties": props, "required": required or list(props)},
    }}

TOOLS = {
    "shell":          tool("shell",          "Run a shell command. Returns stdout/stderr.",
                           {"command": {"type": "string"}}),
    "file_write":     tool("file_write",     "Write content to a file.",
                           {"path": {"type": "string"}, "content": {"type": "string"}}),
    "web_search_tool":tool("web_search_tool","Search the web via DuckDuckGo.",
                           {"query": {"type": "string"}}, ["query"]),
    "web_fetch":      tool("web_fetch",      "Fetch and read a URL.",
                           {"url": {"type": "string"}}),
    "task_plan":      tool("task_plan",
                           "Create a structured task plan. hint=create to start. "
                           "IMMEDIATELY call the tools to execute each step after planning.",
                           {"hint": {"type": "string"}, "tasks": {"type": "array", "items": {"type": "string"}}},
                           ["hint"]),
    "db_query":       tool("db_query",
                           "Query a database connection (MongoDB or SQL).",
                           {"database": {"type": "string"}, "query": {}, "limit": {"type": "integer"}},
                           ["database", "query"]),
}

# ─────────────────────────────────────────────────────────────────────────────
# Capability test suite  (single-turn: checks model emits correct tool call)
# ─────────────────────────────────────────────────────────────────────────────

def calls(r, *names):
    return any(tc.get("function", {}).get("name") in names for tc in r.get("tool_calls", []))

CAP_SUITE = {
    "shell": {
        "label": "Shell",
        "tools": ["shell", "file_write"],
        "prompt": "Use the shell tool to run: echo 'bench_ok'. Call the tool immediately — no description.",
        "check": lambda r: calls(r, "shell", "file_write"),
    },
    "websearch": {
        "label": "Web search",
        "tools": ["web_search_tool", "web_fetch"],
        "prompt": "Use web_search_tool to search 'Streamlit python docs'. Call it now.",
        "check": lambda r: calls(r, "web_search_tool", "web_fetch"),
    },
    "taskplan": {
        "label": "Task plan",
        "tools": ["task_plan", "shell"],
        "prompt": (
            "Use task_plan (hint=create) to plan: (1) echo hello > /tmp/t.txt  (2) cat /tmp/t.txt. "
            "Then immediately call shell to execute step 1."
        ),
        "check": lambda r: calls(r, "task_plan", "shell"),
    },
    "dbquery": {
        "label": "DB query",
        "tools": ["db_query"],
        "prompt": "Use db_query on database='arxiv', query={'title': {'$exists': true}}, limit=3. Call it now.",
        "check": lambda r: calls(r, "db_query"),
    },
}

# ─────────────────────────────────────────────────────────────────────────────
# Integration test suite  (multi-turn: runs full LlamaFarm agent loop via CLI)
# Each test sends a prompt to LlamaFarm, waits, then verifies a side-effect.
# ─────────────────────────────────────────────────────────────────────────────

INT_SUITE = {
    "int_shell": {
        "label": "INT: shell exec",
        "prompt": "Use the shell tool to run this exact command: echo bench_shell_ok > /tmp/bench_int_shell.txt",
        "verify_cmd": "docker exec LlamaFarm grep -q bench_shell_ok /tmp/bench_int_shell.txt && echo PASS || echo FAIL",
        "cleanup_cmd": "docker exec LlamaFarm rm -f /tmp/bench_int_shell.txt",
    },
    "int_file": {
        "label": "INT: file write",
        "prompt": "Use file_write to write the text 'bench_file_ok' to the file bench_int_file.txt",
        "verify_cmd": "docker exec LlamaFarm grep -q bench_file_ok /llamafarm-data/workspace/bench_int_file.txt && echo PASS || echo FAIL",
        "cleanup_cmd": "docker exec LlamaFarm rm -f /llamafarm-data/workspace/bench_int_file.txt",
    },
    "int_plan": {
        "label": "INT: plan+execute",
        "prompt": (
            "Make a task plan to: (1) write 'plan_done' to /tmp/bench_int_plan.txt using shell, "
            "(2) cat that file. Execute every step with tools — do not stop after planning."
        ),
        "verify_cmd": "docker exec LlamaFarm grep -q plan_done /tmp/bench_int_plan.txt && echo PASS || echo FAIL",
        "cleanup_cmd": "docker exec LlamaFarm rm -f /tmp/bench_int_plan.txt",
    },
    "int_calc": {
        "label": "INT: calc app",
        "prompt": (
            "Write a minimal Streamlit calculator to /tmp/calc_bench.py (basic +,-,*,/ only) "
            "then run it with: nohup streamlit run /tmp/calc_bench.py --server.port 8501 "
            "--server.headless true &>/tmp/calc_bench.log & "
            "Wait 5 seconds then verify port 8501 is listening."
        ),
        "verify_cmd": "ss -tlnp | grep -q 8501 && echo PASS || docker exec LlamaFarm ss -tlnp 2>/dev/null | grep -q 8501 && echo PASS || echo FAIL",
        "cleanup_cmd": "pkill -f 'streamlit run /tmp/calc_bench.py' 2>/dev/null; rm -f /tmp/calc_bench.py /tmp/calc_bench.log",
    },
}

# ─────────────────────────────────────────────────────────────────────────────
# Helpers
# ─────────────────────────────────────────────────────────────────────────────

def fetch(url, data=None, timeout=30):
    req = urllib.request.Request(
        url, data=json.dumps(data).encode() if data else None,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read())

def get_models(ollama_host):
    try:
        return [m["name"] for m in fetch(f"{ollama_host}/api/tags", timeout=10).get("models", [])]
    except Exception as e:
        print(f"  WARN: could not list models: {e}")
        return []

def set_model(lf_host, model):
    """Hot-switch LlamaFarm default model for integration tests."""
    try:
        cfg = fetch(f"{lf_host}/api/config", timeout=10)
        toml = cfg["content"]
        import re
        toml = re.sub(r'^default_model\s*=\s*"[^"]*"', f'default_model = "{model}"', toml, flags=re.MULTILINE)
        req = urllib.request.Request(
            f"{lf_host}/api/config",
            data=toml.encode(),
            headers={"Content-Type": "text/plain"},
            method="PUT",
        )
        with urllib.request.urlopen(req, timeout=10) as resp:
            return json.loads(resp.read()).get("status") == "ok"
    except Exception as e:
        print(f"  WARN: could not switch model: {e}")
        return False

def _is_local(ssh_target):
    """Return True if the target is the local machine (no SSH needed)."""
    host = ssh_target.split("@")[-1] if "@" in ssh_target else ssh_target
    return host in ("localhost", "127.0.0.1", "")

def run_remote(ssh_target, cmd, timeout=120):
    if _is_local(ssh_target):
        result = subprocess.run(
            ["bash", "-c", cmd], capture_output=True, text=True, timeout=timeout
        )
    else:
        result = subprocess.run(
            ["ssh", "-o", "StrictHostKeyChecking=no", "-o", "ConnectTimeout=5", ssh_target, cmd],
            capture_output=True, text=True, timeout=timeout,
        )
    return result.stdout.strip(), result.returncode

def run_lf_agent(ssh_target, model, prompt, timeout):
    """Run a prompt through LlamaFarm CLI inside the container and return output."""
    escaped = prompt.replace("'", "'\\''")
    cmd = f"docker exec LlamaFarm llamafarm agent --model {shlex.quote(model)} -m '{escaped}' 2>&1"
    try:
        out, rc = run_remote(ssh_target, cmd, timeout=timeout)
        return out, rc == 0
    except subprocess.TimeoutExpired:
        return "TIMEOUT", False
    except Exception as e:
        return str(e), False

# ─────────────────────────────────────────────────────────────────────────────
# Test runners
# ─────────────────────────────────────────────────────────────────────────────

def run_cap_test(ollama_host, model, tk, td, timeout):
    tools = [TOOLS[t] for t in td["tools"] if t in TOOLS]
    payload = {
        "model": model,
        "messages": [{"role": "user", "content": td["prompt"]}],
        "tools": tools,
        "think": False,
        "stream": False,
        "options": {"num_predict": 1024, "temperature": 0.0},
    }
    t0 = time.time()
    try:
        resp = fetch(f"{ollama_host}/api/chat", payload, timeout=timeout)
    except Exception as e:
        return {"status": "ERROR", "elapsed": time.time() - t0, "error": str(e), "tool_calls": []}
    elapsed = time.time() - t0
    msg = resp.get("message", {})
    passed = td["check"](msg)
    return {
        "status": "PASS" if passed else "FAIL",
        "elapsed": elapsed,
        "tool_calls": [tc.get("function", {}).get("name") for tc in msg.get("tool_calls", [])],
        "tokens": resp.get("eval_count", 0),
    }

def run_int_test(ssh_target, lf_host, model, tk, td, timeout):
    # Switch model, run agent via CLI, verify side-effect
    set_model(lf_host, model)
    # Clean up any leftover from previous run
    if td.get("cleanup_cmd"):
        run_remote(ssh_target, td["cleanup_cmd"], timeout=15)

    t0 = time.time()
    out, _ = run_lf_agent(ssh_target, model, td["prompt"], timeout)
    elapsed = time.time() - t0

    if out == "TIMEOUT":
        return {"status": "TIMEOUT", "elapsed": elapsed, "output": ""}

    # Verify side-effect
    verify_out, _ = run_remote(ssh_target, td["verify_cmd"], timeout=15)
    passed = "PASS" in verify_out

    return {
        "status": "PASS" if passed else "FAIL",
        "elapsed": elapsed,
        "verify": verify_out,
        "output_excerpt": out[:300],
    }

# ─────────────────────────────────────────────────────────────────────────────
# Main
# ─────────────────────────────────────────────────────────────────────────────

all_test_keys = [t.strip() for t in tests_arg.split(",") if t.strip()]
cap_keys = [k for k in all_test_keys if k in CAP_SUITE]
int_keys = [] if NO_INT else [k for k in all_test_keys if k in INT_SUITE]

print(f"\n{'═'*74}")
print(f"  LlamaFarm Model Benchmark")
print(f"  Host   : {lf_host}")
print(f"  Ollama : {ollama_host}")
print(f"  Cap tests : {', '.join(cap_keys) or 'none'}")
print(f"  Int tests : {', '.join(int_keys) or 'none'}")
print(f"  Time   : {datetime.datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
print(f"{'═'*74}\n")

if models_arg.strip():
    all_models = [m.strip() for m in models_arg.split(",") if m.strip()]
else:
    all_models = get_models(ollama_host)
    if not all_models:
        print("No models found. Pass --models explicitly.")
        sys.exit(1)

print(f"  Models ({len(all_models)}): {', '.join(all_models)}\n")

# Build column list
all_cols = [(k, CAP_SUITE[k]["label"], "cap") for k in cap_keys] + \
           [(k, INT_SUITE[k]["label"], "int") for k in int_keys]

COL = 16
hdr = f"  {'Model':<26} │"
for _, lbl, tier in all_cols:
    prefix = "⚡" if tier == "cap" else "🔗"
    hdr += f" {(prefix+lbl)[:COL]:<{COL}} │"
hdr += " Score"
print(hdr)
print("  " + "─" * (len(hdr) - 2))

results = {}
for model in all_models:
    row = {}
    row_str = f"  {model:<26} │"
    passed = total = 0

    for tk, lbl, tier in all_cols:
        total += 1
        if tier == "cap":
            r = run_cap_test(ollama_host, model, tk, CAP_SUITE[tk], TIMEOUT)
        else:
            r = run_int_test(ssh_target, lf_host, model, tk, INT_SUITE[tk], TIMEOUT)
        row[tk] = r

        if r["status"] == "PASS":
            passed += 1
            cell = f"✓ {r['elapsed']:.0f}s"
        elif r["status"] == "TIMEOUT":
            cell = "⏱ TIMEOUT"
        elif r["status"] == "ERROR":
            cell = "! ERR"
        else:
            call_names = ",".join(r.get("tool_calls", [])) if r.get("tool_calls") else r.get("verify", "no calls")
            cell = f"✗ {call_names[:10]}"
        row_str += f" {cell:<{COL}} │"
        sys.stdout.flush()

    row_str += f" {passed}/{total}"
    print(row_str)
    results[model] = row

    # Unload model from VRAM before testing the next one to avoid contention.
    try:
        import urllib.request as _ur, json as _json
        _req = _ur.Request(
            f"{ollama_host}/api/generate",
            data=_json.dumps({"model": model, "keep_alive": 0}).encode(),
            method="POST",
        )
        _ur.urlopen(_req, timeout=10)
    except Exception:
        pass

print("  " + "─" * (len(hdr) - 2))
print()

# Failure details
any_fail = False
for model, model_results in results.items():
    for tk, r in model_results.items():
        if r["status"] != "PASS":
            if not any_fail:
                print("Failures:\n")
                any_fail = True
            tier = "cap" if tk in CAP_SUITE else "int"
            lbl = CAP_SUITE.get(tk, INT_SUITE.get(tk, {})).get("label", tk)
            print(f"  [{model}] {lbl} ({tier}): {r['status']}")
            for key in ("error", "verify", "output_excerpt"):
                if r.get(key):
                    print(f"    {key}: {str(r[key])[:200]}")
            print()

if out_file:
    with open(out_file, "w") as f:
        json.dump({
            "host": lf_host, "ollama": ollama_host,
            "timestamp": datetime.datetime.utcnow().isoformat(),
            "results": {m: {k: {kk: vv for kk, vv in v.items()} for k, v in mr.items()}
                        for m, mr in results.items()},
        }, f, indent=2)
    print(f"  Results → {out_file}")

print(f"\n{'═'*74}\n")
PYEOF
