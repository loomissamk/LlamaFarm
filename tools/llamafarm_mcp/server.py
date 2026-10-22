#!/usr/bin/env python3
"""LlamaFarm MCP server — exposes a remote LlamaFarm node (Qwen local model) to Claude Code.

Transport: stdio (Claude Code spawns this process).
Backend:   LlamaFarm's OpenAI-compatible API at LLAMAFARM_BASE_URL
           (default http://127.0.0.1:42617; set LLAMAFARM_BASE_URL for a remote node).

Tools:
  - llamafarm_chat          single-turn chat with the local model
  - llamafarm_conversation  multi-turn chat (Claude supplies full message list)
  - llamafarm_agent         tool-enabled task through LlamaFarm's /agent endpoint
  - llamafarm_agent_batch   1-4 independent tool-enabled agent tasks
  - llamafarm_tools_catalog see what LlamaFarm can do internally
  - llamafarm_health        LlamaFarm uptime + component status
  - llamafarm_runs          list recent and live agent runs
  - llamafarm_run_status    inspect one run's ledger and evidence
  - llamafarm_cancel_run    cancel a live or stale run by ID

Resource:
  - llamafarm://model       current model id + base URL
"""

from __future__ import annotations

import asyncio
import json
import os
import re
import time
from typing import Any

import requests
from mcp.server.mcpserver import MCPServer

BASE_URL = os.environ.get("LLAMAFARM_BASE_URL", "http://127.0.0.1:42617").rstrip("/")
MODEL = os.environ.get("LLAMAFARM_MODEL", "qwen3.8:27b")
DEFAULT_TIMEOUT = float(os.environ.get("LLAMAFARM_TIMEOUT", "120"))
DELEGATE_SLOTS = asyncio.Semaphore(4)
AGENT_SLOTS = asyncio.Semaphore(4)

mcp = MCPServer(
    "llamafarm",
    instructions=(
        "LlamaFarm is a local agent runtime (GPU-backed, model "
        f"{MODEL}) running on a machine on the user's Tailscale network. Use "
        "these tools to delegate sub-tasks to it as a sub-agent: ask "
        "questions, get a second opinion, or run reasoning workloads on "
        "that machine instead of locally. Responses come from the local "
        "model, not from Claude."
    ),
)


def _post_chat(messages: list[dict[str, str]], max_tokens: int | None, timeout: float) -> dict[str, Any]:
    payload: dict[str, Any] = {
        "model": MODEL,
        "messages": messages,
        "stream": False,
    }
    if max_tokens:
        payload["max_tokens"] = max_tokens
    try:
        resp = requests.post(f"{BASE_URL}/v1/chat/completions", json=payload, timeout=timeout)
    except requests.ConnectionError as exc:
        raise RuntimeError(f"LlamaFarm not reachable at {BASE_URL}: {exc}") from exc
    except requests.Timeout as exc:
        raise RuntimeError(f"LlamaFarm timed out after {timeout}s at {BASE_URL}") from exc
    if resp.status_code != 200:
        raise RuntimeError(f"LlamaFarm returned HTTP {resp.status_code}: {resp.text[:500]}")
    return resp.json()


def _post_agent(prompt: str, timeout: float) -> str:
    try:
        resp = requests.post(f"{BASE_URL}/agent", json={"message": prompt}, timeout=timeout)
    except requests.ConnectionError as exc:
        raise RuntimeError(f"LlamaFarm not reachable at {BASE_URL}: {exc}") from exc
    except requests.Timeout as exc:
        raise RuntimeError(f"LlamaFarm agent timed out after {timeout}s at {BASE_URL}") from exc
    if resp.status_code != 200:
        raise RuntimeError(f"LlamaFarm agent returned HTTP {resp.status_code}: {resp.text[:500]}")
    data = resp.json()
    answer = data.get("response")
    if not isinstance(answer, str):
        raise RuntimeError("LlamaFarm agent response did not contain text")
    run_id = data.get("run_id")
    if isinstance(run_id, str) and run_id:
        return f"{answer}\n\n[llamafarm] run_id={run_id}"
    return answer


def _post_agent_batch_task(prompt: str, timeout: float) -> dict[str, Any]:
    """Call the same authenticated gateway route as llamafarm_agent.

    Keep the gateway's HTTP status and durable run ID on individual failures,
    so one failed task does not discard the other agents' results.
    """
    try:
        resp = requests.post(f"{BASE_URL}/agent", json={"message": prompt}, timeout=timeout)
    except requests.RequestException as exc:
        return {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
    try:
        data = resp.json()
    except ValueError:
        data = None
    run_id = data.get("run_id") if isinstance(data, dict) else None
    result: dict[str, Any] = {"ok": False, "http_status": resp.status_code}
    if isinstance(run_id, str) and run_id:
        result["run_id"] = run_id
    if resp.status_code != 200:
        error = data.get("error") if isinstance(data, dict) else None
        result["error"] = str(error)[:500] if error else resp.text[:500]
        return result
    answer = data.get("response") if isinstance(data, dict) else None
    if not isinstance(answer, str):
        result["error"] = "LlamaFarm agent response did not contain text"
        return result
    result.update({"ok": True, "response": answer})
    return result


def _get(path: str, timeout: float = 10) -> Any:
    try:
        resp = requests.get(f"{BASE_URL}{path}", timeout=timeout)
    except requests.ConnectionError as exc:
        raise RuntimeError(f"LlamaFarm not reachable at {BASE_URL}: {exc}") from exc
    if resp.status_code != 200:
        raise RuntimeError(f"LlamaFarm {path} returned HTTP {resp.status_code}: {resp.text[:300]}")
    return resp.json()


def _run_id(value: str) -> str:
    if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", value):
        raise ValueError("run_id must contain 1-128 letters, digits, hyphens, or underscores")
    return value


@mcp.tool()
def llamafarm_chat(prompt: str, system: str = "", max_tokens: int = 1024) -> str:
    """Ask the remote LlamaFarm node's local model a single question.

    Args:
        prompt: The user message to send.
        system: Optional system prompt to prepend.
        max_tokens: Max tokens in the reply (default 1024).

    Returns the model's text reply plus a short usage footer.
    """
    messages: list[dict[str, str]] = []
    if system:
        messages.append({"role": "system", "content": system})
    messages.append({"role": "user", "content": prompt})
    data = _post_chat(messages, max_tokens, DEFAULT_TIMEOUT)
    content = data["choices"][0]["message"]["content"]
    usage = data.get("usage", {})
    footer = (
        f"\n\n---\n[llamafarm] model={data.get('model', MODEL)} "
        f"prompt_tokens={usage.get('prompt_tokens')} completion_tokens={usage.get('completion_tokens')}"
    )
    return content + footer


@mcp.tool()
def llamafarm_agent(prompt: str, timeout_seconds: int = 300) -> str:
    """Run a tool-capable LlamaFarm task on the remote node.

    Unlike llamafarm_chat, this calls POST /agent and may execute LlamaFarm's
    permitted tools. Use it for tasks that require real actions or file access.
    The selected dashboard model and the node's tool policy apply.

    Args:
        prompt: Complete task instruction for the remote agent.
        timeout_seconds: Maximum wait for the synchronous agent response (1-3600).
    """
    if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > 64000:
        raise ValueError("prompt must be nonempty and at most 64000 characters")
    if type(timeout_seconds) is not int or not 1 <= timeout_seconds <= 3600:
        raise ValueError("timeout_seconds must be an integer from 1 to 3600")
    return _post_agent(prompt, timeout_seconds)


@mcp.tool()
def llamafarm_conversation(messages: list[dict[str, str]], max_tokens: int = 1024) -> str:
    """Multi-turn conversation with the remote LlamaFarm node's local model.

    Args:
        messages: Full message list, each {"role": "system"|"user"|"assistant", "content": str}.
        max_tokens: Max tokens in the reply (default 1024).

    Returns the model's text reply plus a short usage footer.
    """
    if not messages:
        raise ValueError("messages must be a non-empty list of {role, content}")
    for m in messages:
        if m.get("role") not in ("system", "user", "assistant"):
            raise ValueError(f"invalid role: {m.get('role')!r}")
    data = _post_chat(messages, max_tokens, DEFAULT_TIMEOUT)
    content = data["choices"][0]["message"]["content"]
    usage = data.get("usage", {})
    footer = (
        f"\n\n---\n[llamafarm] model={data.get('model', MODEL)} "
        f"prompt_tokens={usage.get('prompt_tokens')} completion_tokens={usage.get('completion_tokens')}"
    )
    return content + footer


@mcp.tool()
async def llamafarm_delegate_batch(tasks: list[dict[str, str]], max_tokens: int = 512) -> str:
    """Run 1–4 independent text delegates concurrently on the configured model.

    Each task requires unique id and prompt; optional system sets its role.
    Conversations are isolated. Delegates return text, NOT file edits or tool
    execution. The inference backend may queue requests rather than generate
    simultaneously. All delegates share this profile's model, avoiding reloads.
    A failed delegate is reported individually; inspect every result's ok flag.
    """
    if not isinstance(tasks, list) or not 1 <= len(tasks) <= 4:
        raise ValueError("provide 1–4 tasks")
    if type(max_tokens) is not int or not 1 <= max_tokens <= 2048:
        raise ValueError("max_tokens must be an integer from 1 to 2048")
    seen = set()
    for task in tasks:
        if not isinstance(task, dict) or set(task) - {"id", "prompt", "system"}:
            raise ValueError("task fields: id, prompt, optional system")
        for key in ("id", "prompt"):
            if not isinstance(task.get(key), str) or not task[key].strip():
                raise ValueError(f"task {key} must be a nonempty string")
        if not isinstance(task.get("system", ""), str):
            raise ValueError("task system must be a string")
        if len(task["prompt"]) > 64000 or len(task.get("system", "")) > 16000:
            raise ValueError("task exceeds prompt/system size limit")
        if task["id"] in seen:
            raise ValueError("task ids must be unique")
        seen.add(task["id"])

    started = time.monotonic()

    async def delegate(task):
        messages = []
        if task.get("system"):
            messages.append({"role": "system", "content": task["system"]})
        messages.append({"role": "user", "content": task["prompt"]})
        async with DELEGATE_SLOTS:
            dispatched = time.monotonic()
            try:
                data = await asyncio.to_thread(_post_chat, messages, max_tokens, DEFAULT_TIMEOUT)
                content = data["choices"][0]["message"]["content"]
                if not isinstance(content, str) or not content.strip():
                    raise ValueError("backend returned no text")
                result = {"id": task["id"], "ok": True,
                          "model_reported": data.get("model"), "text": content}
            except Exception as exc:
                result = {"id": task["id"], "ok": False,
                          "error": f"{type(exc).__name__}: {exc}"}
            result["dispatched_after_seconds"] = round(dispatched - started, 3)
            result["elapsed_seconds"] = round(time.monotonic() - dispatched, 3)
            return result

    results = await asyncio.gather(*(delegate(task) for task in tasks))
    return json.dumps({"mode": "text_delegates", "model_requested": MODEL,
                       "all_ok": all(item["ok"] for item in results),
                       "elapsed_seconds": round(time.monotonic() - started, 3),
                       "results": results}, indent=2)


@mcp.tool()
async def llamafarm_agent_batch(tasks: list[dict[str, str]], timeout_seconds: int = 300) -> str:
    """Run 1-4 independent tool-capable LlamaFarm agents via POST /agent.

    Each task has a unique id and a complete prompt. Tasks have separate agent
    turns; the node's tool policy applies to each. Results keep
    their input order and include per-task success, response or error, HTTP
    status where available, a durable run ID if the gateway returns one, and timing. The gateway or
    model backend may queue work even when requests are concurrent. Tool actions
    can change files or external state, so give each agent a bounded task.

    Args:
        tasks: One to four objects containing only id and prompt strings.
        timeout_seconds: Per-agent synchronous request timeout (1-3600).
    """
    if not isinstance(tasks, list) or not 1 <= len(tasks) <= 4:
        raise ValueError("provide 1-4 tasks")
    if type(timeout_seconds) is not int or not 1 <= timeout_seconds <= 3600:
        raise ValueError("timeout_seconds must be an integer from 1 to 3600")
    seen: set[str] = set()
    for task in tasks:
        if not isinstance(task, dict) or set(task) != {"id", "prompt"}:
            raise ValueError("task fields: id and prompt")
        for key in ("id", "prompt"):
            if not isinstance(task[key], str) or not task[key].strip():
                raise ValueError(f"task {key} must be a nonempty string")
        if len(task["id"]) > 128 or len(task["prompt"]) > 64000:
            raise ValueError("task exceeds id/prompt size limit")
        if task["id"] in seen:
            raise ValueError("task ids must be unique")
        seen.add(task["id"])

    started = time.monotonic()

    async def run_one(task: dict[str, str]) -> dict[str, Any]:
        async with AGENT_SLOTS:
            dispatched = time.monotonic()
            try:
                result = await asyncio.to_thread(
                    _post_agent_batch_task, task["prompt"], timeout_seconds
                )
            except Exception as exc:
                result = {"ok": False, "error": f"{type(exc).__name__}: {exc}"}
            result["id"] = task["id"]
            result["dispatched_after_seconds"] = round(dispatched - started, 3)
            result["elapsed_seconds"] = round(time.monotonic() - dispatched, 3)
            return result

    results = await asyncio.gather(*(run_one(task) for task in tasks))
    return json.dumps({
        "mode": "agent_runs", "all_ok": all(item["ok"] for item in results),
        "elapsed_seconds": round(time.monotonic() - started, 3),
        "results": results,
    }, indent=2)


@mcp.tool()
def llamafarm_tools_catalog() -> str:
    """List the internal tools LlamaFarm can use in its own agent loop.

    Returns tool names with one-line descriptions (from GET /api/tools).
    """
    data = _get("/api/tools")
    tools = data if isinstance(data, list) else data.get("tools", [])
    lines = []
    for t in tools:
        if isinstance(t, dict):
            name = t.get("name") or t.get("id") or "?"
            desc = (t.get("description") or "").strip().splitlines()
            lines.append(f"- {name}: {desc[0] if desc else ''}")
        else:
            lines.append(f"- {t}")
    if not lines:
        return f"Raw catalog: {json.dumps(data)[:2000]}"
    return f"LlamaFarm internal tools ({len(lines)}):\n" + "\n".join(lines)


@mcp.tool()
def llamafarm_health() -> str:
    """Check the remote LlamaFarm node's runtime health (uptime + component statuses)."""
    data = _get("/api/health")
    return json.dumps(data, indent=2)


@mcp.tool()
def llamafarm_runs() -> str:
    """List recent LlamaFarm run ledgers and currently live run IDs."""
    return json.dumps(_get("/api/runs"), indent=2)


@mcp.tool()
def llamafarm_run_status(run_id: str) -> str:
    """Read a LlamaFarm run's recorded status, plan, tool timeline, and evidence."""
    return json.dumps(_get(f"/api/runs/{_run_id(run_id)}"), indent=2)


@mcp.tool()
def llamafarm_cancel_run(run_id: str) -> str:
    """Cancel a LlamaFarm run by ID, including a run detached from its chat tab."""
    run_id = _run_id(run_id)
    try:
        resp = requests.post(f"{BASE_URL}/api/runs/{run_id}/cancel", timeout=15)
    except requests.RequestException as exc:
        raise RuntimeError(f"LlamaFarm cancel request failed: {exc}") from exc
    if resp.status_code != 200:
        raise RuntimeError(f"LlamaFarm cancel returned HTTP {resp.status_code}: {resp.text[:300]}")
    return json.dumps(resp.json(), indent=2)


@mcp.resource("llamafarm://model")
def llamafarm_model_info() -> str:
    """Current LlamaFarm model id and base URL."""
    data = _get("/v1/models")
    models = data.get("data", [])
    names = [m.get("id") or m.get("name") for m in models]
    return json.dumps({"base_url": BASE_URL, "default_model": MODEL, "available_models": names}, indent=2)


if __name__ == "__main__":
    mcp.run()  # stdio transport by default
