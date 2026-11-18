#!/usr/bin/env python3
"""Measure GPU1 coding requests and tool calls through the live Tailscale endpoint."""

import concurrent.futures
import json
import os
import pathlib
import re
import subprocess
import sys
import threading
import time
import urllib.request

HOST = "http://100.107.226.49:11436"
SSH = ["ssh", "-i", str(pathlib.Path.home() / ".ssh/id_ed25519_popos1"),
       "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "bat@100.107.226.49"]
MODEL = os.environ.get("GPU1_BENCH_MODEL", "qwen3.5-rana:9b-32k")
CONTEXT = int(os.environ.get("GPU1_BENCH_CONTEXT", "32768"))
HELPERS = int(os.environ.get("GPU1_BENCH_HELPERS", "550"))


def gpu_sample():
    out = subprocess.check_output(SSH + ["nvidia-smi", "--query-gpu=index,memory.used,utilization.gpu",
                                         "--format=csv,noheader,nounits"], text=True, timeout=15)
    return [[int(x.strip()) for x in row.split(",")] for row in out.splitlines()]


def request(task):
    # The source listing and project notes create a coding-agent sized prompt.
    source = "\n".join(f"def normalize_{task}_{i}(value):\n    return value.strip().lower()\n" for i in range(HELPERS))
    prompt = (f"You are auditing a Python utility module. The codebase has {HELPERS} small helpers. "
              "Find the bug in the exact function named below, then call report_patch with "
              "the function name and a concise fix. Do not merely answer in text.\n"
              f"TARGET: divide_{task}\n\n" + source +
              f"\ndef divide_{task}(total, count):\n    return total / count\n" +
              "\nRequirement: empty input (count == 0) must return None.\n")
    payload = {
        "model": MODEL, "stream": False, "think": False,
        "messages": [{"role": "user", "content": prompt}],
        "tools": [{"type": "function", "function": {"name": "report_patch",
                   "description": "Report the identified coding fix",
                   "parameters": {"type": "object", "properties": {
                       "function": {"type": "string"}, "fix": {"type": "string"}},
                       "required": ["function", "fix"]}}}],
        "options": {"num_ctx": CONTEXT, "num_predict": 256, "temperature": 0}
    }
    started = time.monotonic()
    req = urllib.request.Request(HOST + "/api/chat", data=json.dumps(payload).encode(),
                                 headers={"Content-Type": "application/json"})
    try:
        result = json.load(urllib.request.urlopen(req, timeout=240))
        elapsed = time.monotonic() - started
        calls = result.get("message", {}).get("tool_calls") or []
        valid = any(c.get("function", {}).get("name") == "report_patch" and
                    c.get("function", {}).get("arguments", {}).get("function") == f"divide_{task}" and
                    ("zero" in str(c.get("function", {}).get("arguments", {}).get("fix", "")).lower() or
                     "count" in str(c.get("function", {}).get("arguments", {}).get("fix", "")).lower())
                    for c in calls)
        return {"task": task, "ok": valid, "latency_s": elapsed,
                "prompt_tokens": result.get("prompt_eval_count", 0),
                "output_tokens": result.get("eval_count", 0),
                "prompt_eval_s": result.get("prompt_eval_duration", 0) / 1e9,
                "generation_s": result.get("eval_duration", 0) / 1e9,
                "total_s": result.get("total_duration", 0) / 1e9,
                "tool_calls": len(calls)}
    except Exception as exc:
        return {"task": task, "ok": False, "latency_s": time.monotonic() - started,
                "error": f"{type(exc).__name__}: {exc}"}


def benchmark(n):
    samples = []
    stop = threading.Event()
    def sample_loop():
        while not stop.is_set():
            try:
                samples.append(gpu_sample())
            except Exception:
                pass
            stop.wait(0.5)
    thread = threading.Thread(target=sample_loop, daemon=True)
    thread.start()
    start = time.monotonic()
    offset = int(os.environ.get("GPU1_BENCH_OFFSET", "0"))
    with concurrent.futures.ThreadPoolExecutor(max_workers=n) as pool:
        results = list(pool.map(request, range(offset + 1, offset + n + 1)))
    wall = time.monotonic() - start
    stop.set()
    thread.join(timeout=3)
    ps = json.load(urllib.request.urlopen(HOST + "/api/ps", timeout=10))
    loaded = [{"name": m.get("name"), "size_vram": m.get("size_vram"),
               "context_length": m.get("context_length")} for m in ps.get("models", [])]
    gpu0 = [s[0] for s in samples if len(s) > 1]
    gpu1 = [s[1] for s in samples if len(s) > 1]
    return {"concurrency": n, "wall_s": wall, "results": results,
            "completed_tasks_per_min": 60 * sum(r["ok"] for r in results) / wall,
            "output_tokens_per_s": sum(r.get("output_tokens", 0) for r in results) / wall,
            "prompt_tokens_per_s": sum(r.get("prompt_tokens", 0) for r in results) / wall,
            "gpu0_max_mib": max((s[1] for s in gpu0), default=None),
            "gpu1_max_mib": max((s[1] for s in gpu1), default=None),
            "gpu0_max_util": max((s[2] for s in gpu0), default=None),
            "gpu1_max_util": max((s[2] for s in gpu1), default=None),
            "loaded": loaded}


if __name__ == "__main__":
    n = int(sys.argv[1])
    if n not in (1, 2, 3, 4):
        raise SystemExit("concurrency must be 1, 2, 3, or 4")
    print(json.dumps(benchmark(n), indent=2))
