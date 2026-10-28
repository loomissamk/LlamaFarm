#!/usr/bin/env python3
"""Record direct Ollama timing and NVIDIA telemetry without changing server settings."""

from __future__ import annotations

import argparse
import json
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime, timezone
from pathlib import Path
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


MODELS = [
    "qwen3.8:27b",
    "qwen3.5:9b",
    "qwen3.6:35b-a3b-coding-mtp-q4_K_M",
    "qwen3-next:80b-a3b-instruct-q4_K_M",
]


def gpu_sample() -> dict[str, int] | None:
    result = subprocess.run(
        ["nvidia-smi", "--query-gpu=memory.used,utilization.gpu", "--format=csv,noheader,nounits"],
        capture_output=True, text=True, timeout=5, check=False,
    )
    if result.returncode:
        return None
    first = result.stdout.strip().splitlines()[0].split(",")
    return {"memory_mib": int(first[0].strip()), "utilization_pct": int(first[1].strip())}


def request(base_url: str, payload: dict, timeout: int) -> tuple[int, dict]:
    body = json.dumps(payload).encode()
    req = Request(f"{base_url}/api/chat", body, {"Content-Type": "application/json"})
    try:
        with urlopen(req, timeout=timeout) as response:
            return response.status, json.load(response)
    except HTTPError as error:
        return error.code, {"error": error.read().decode(errors="replace")[:1000]}


def run_case(base_url: str, model: str, context: int, kind: str, timeout: int, keep_alive: str | int = "5m") -> dict:
    if kind == "tool":
        payload = {
            "model": model, "stream": False, "think": False, "keep_alive": keep_alive,
            "messages": [{"role": "user", "content": "Call record_probe with token V100-TOOL-28. Do not answer in text."}],
            "tools": [{"type": "function", "function": {
                "name": "record_probe", "description": "Record a benchmark token",
                "parameters": {"type": "object", "properties": {"token": {"type": "string"}}, "required": ["token"]},
            }}],
            "options": {"num_ctx": context, "num_predict": 128, "temperature": 0, "seed": 42},
        }
    elif kind == "prefill":
        payload = {
            "model": model, "stream": False, "think": False, "keep_alive": keep_alive,
            "messages": [{"role": "user", "content":
                ("Review this Rust service invariant: every request must either commit exactly once "
                 "or return an error without changing durable state. Check retries, timeouts, locks, "
                 "and cancellation. ") * 240 + "Give one concise finding."}],
            "options": {"num_ctx": context, "num_predict": 32, "temperature": 0, "seed": 42},
        }
    elif kind == "restore":
        payload = {
            "model": model, "stream": False, "think": False, "keep_alive": keep_alive,
            "messages": [{"role": "user", "content": "Reply OK."}],
            "options": {"num_ctx": context, "num_predict": 4, "temperature": 0, "seed": 42},
        }
    else:
        payload = {
            "model": model, "stream": False, "think": False, "keep_alive": keep_alive,
            "messages": [{"role": "user", "content":
                "Write a numbered list of 80 distinct practical checks for a Rust code review. "
                "Use one short sentence per item. Start immediately with item 1."}],
            "options": {"num_ctx": context, "num_predict": 160, "temperature": 0, "seed": 42},
        }
    samples: list[dict] = []
    stop = threading.Event()

    def monitor() -> None:
        while not stop.is_set():
            try:
                sample = gpu_sample()
                if sample:
                    samples.append({"elapsed_s": round(time.monotonic() - started, 2), **sample})
            except (OSError, ValueError, subprocess.TimeoutExpired):
                pass
            stop.wait(0.5)

    started = time.monotonic()
    thread = threading.Thread(target=monitor, daemon=True)
    thread.start()
    try:
        status, data = request(base_url, payload, timeout)
        error = data.get("error")
    except (URLError, TimeoutError, OSError, ValueError) as exc:
        status, data, error = 0, {}, f"{type(exc).__name__}: {exc}"
    finally:
        stop.set()
        thread.join(timeout=2)
    wall_s = time.monotonic() - started
    count = data.get("eval_count") or 0
    decode_ns = data.get("eval_duration") or 0
    prompt_count = data.get("prompt_eval_count") or 0
    prompt_ns = data.get("prompt_eval_duration") or 0
    calls = data.get("message", {}).get("tool_calls", [])
    def correct_tool_call(call: dict) -> bool:
        function = call.get("function", {})
        arguments = function.get("arguments", {})
        if isinstance(arguments, str):
            try:
                arguments = json.loads(arguments)
            except json.JSONDecodeError:
                return False
        return (function.get("name") == "record_probe" and isinstance(arguments, dict)
                and arguments.get("token") == "V100-TOOL-28")

    result = {
        "model": model, "context_requested": context, "kind": kind, "http_status": status,
        "wall_s": round(wall_s, 3), "error": error,
        "prompt_eval_count": data.get("prompt_eval_count"), "eval_count": data.get("eval_count"),
        "load_duration_ns": data.get("load_duration"),
        "prompt_eval_duration_ns": data.get("prompt_eval_duration"),
        "eval_duration_ns": data.get("eval_duration"),
        "total_duration_ns": data.get("total_duration"),
        "prefill_tps": round(prompt_count / (prompt_ns / 1e9), 2) if prompt_ns else None,
        "decode_tps": round(count / (decode_ns / 1e9), 2) if decode_ns else None,
        "ttft_backend_ms": round(((data.get("load_duration") or 0) + prompt_ns) / 1e6, 2) if prompt_ns else None,
        "tool_calls": calls,
        "tool_call_correct": any(correct_tool_call(call) for call in calls if isinstance(call, dict)),
        "response_excerpt": data.get("message", {}).get("content", "")[:300],
        "gpu_samples": samples,
        "peak_gpu_memory_mib": max((sample["memory_mib"] for sample in samples), default=None),
        "peak_gpu_utilization_pct": max((sample["utilization_pct"] for sample in samples), default=None),
    }
    return result


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:11434")
    parser.add_argument("--output", required=True)
    parser.add_argument("--timeout", type=int, default=180)
    parser.add_argument("--restore-model", default="qwen3.8:27b-128k")
    parser.add_argument("--restore-context", type=int, default=131072)
    args = parser.parse_args()
    output = Path(args.output)
    output.parent.mkdir(parents=True, exist_ok=True)
    results = {"started_utc": datetime.now(timezone.utc).isoformat(), "host": subprocess.check_output(["hostname"], text=True).strip(), "cases": []}

    def save(case: dict) -> None:
        results["cases"].append(case)
        output.write_text(json.dumps(results, indent=2) + "\n")
        print(json.dumps({key: case[key] for key in ("model", "context_requested", "kind", "http_status", "wall_s", "decode_tps", "peak_gpu_memory_mib", "tool_call_correct", "error")}), flush=True)

    try:
        for context in (8192, 16384, 32768, 65536):
            save(run_case(args.base_url, MODELS[0], context, "text", args.timeout))
        save(run_case(args.base_url, MODELS[0], 8192, "text", args.timeout))
        save(run_case(args.base_url, MODELS[0], 32768, "prefill", args.timeout))
        save(run_case(args.base_url, MODELS[0], 8192, "tool", args.timeout))
        for model in MODELS[1:]:
            save(run_case(args.base_url, model, 8192, "text", args.timeout))
            save(run_case(args.base_url, model, 8192, "text", args.timeout))
            save(run_case(args.base_url, model, 8192, "tool", args.timeout))
    finally:
        save(run_case(args.base_url, MODELS[0], 8192, "restore", args.timeout, keep_alive=-1))
        for _ in range(4):
            save(run_case(args.base_url, MODELS[0], 8192, "text", args.timeout, keep_alive=-1))
        started = time.monotonic()
        with ThreadPoolExecutor(max_workers=2) as pool:
            cases = list(pool.map(lambda _: run_case(args.base_url, MODELS[0], 8192, "text", args.timeout, keep_alive=-1), range(2)))
        wall_s = time.monotonic() - started
        count = sum(case["eval_count"] or 0 for case in cases)
        results["concurrency_cases"] = [{
            "model": MODELS[0], "parallel_requests": 2, "wall_s": round(wall_s, 3),
            "total_eval_count": count,
            "aggregate_output_tps": round(count / wall_s, 2), "cases": cases,
        }]
        output.write_text(json.dumps(results, indent=2) + "\n")
        # Restore the Kilo profile's resident model and context.
        save(run_case(args.base_url, args.restore_model, args.restore_context, "restore", args.timeout, keep_alive=-1))
        results["final_resident_context_requested"] = args.restore_context
        results["finished_utc"] = datetime.now(timezone.utc).isoformat()
        output.write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
