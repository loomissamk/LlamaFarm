#!/usr/bin/env python3
"""End-to-end dashboard and REST surface audit for a running LlamaFarm node.

Run this inside the bundled container so ChromeDriver, the dashboard, and the
workspace paths are the same ones an operator uses:

    docker exec -i LlamaFarm python3 - \
      --node laptop < scripts/tests/full_surface_e2e.py

The audit uses unique, reversible fixtures. It never clears operator memory,
history, cron jobs, databases, chats, credentials, or workspace directories.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import queue
import re
import socket
import sys
import threading
import time
import traceback
import urllib.error
import urllib.parse
import urllib.request
import uuid
from pathlib import Path
from typing import Any, Callable, Mapping


APP_URL = os.environ.get("LLAMAFARM_UI_URL", "http://127.0.0.1:42617").rstrip("/")
DRIVER_URL = os.environ.get(
    "LLAMAFARM_CHROMEDRIVER_URL", "http://127.0.0.1:9515"
).rstrip("/")
ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"
ROUTES: tuple[tuple[str, str | None], ...] = (
    ("/", "Dashboard"),
    ("/agent", None),
    ("/federation", "Federation Runtime"),
    ("/tools", "Local Tooling"),
    ("/cron", "Scheduled Jobs"),
    ("/integrations", "Ollama Runtime"),
    ("/memory", "Memory"),
    ("/workspace", "Workspace IDE"),
    ("/workspace/files", "Workspace Files"),
    ("/workspace/prompts", "Prompt Files"),
    ("/database", "Agent Memory"),
    ("/config", "Configuration"),
    ("/runs", "Run Inspector"),
    ("/logs", "Runtime Logs"),
    ("/doctor", "Diagnostics"),
)
REDIRECT_ROUTES: tuple[tuple[str, str], ...] = (
    ("/models", "/integrations"),
    ("/cost", "/integrations"),
)


class SurfaceError(RuntimeError):
    """A required UI/API behavior was not observed."""


def wait_for(
    label: str, predicate: Callable[[], Any], timeout: float = 60.0
) -> Any:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:  # route/component may still be mounting
            last_error = error
        time.sleep(0.3)
    suffix = f" (last error: {last_error})" if last_error else ""
    raise SurfaceError(f"timed out waiting for {label}{suffix}")


def wait_for_unbounded(label: str, predicate: Callable[[], Any]) -> Any:
    """Wait for model-owned work until completion or explicit operator cancel."""

    last_error: Exception | None = None
    while True:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:
            last_error = error
        if last_error is not None:
            # Retain the exception for a debugger without turning transient
            # 404s during session creation into a model-run deadline.
            _ = last_error
        time.sleep(0.5)


class Api:
    def __init__(
        self,
        base_url: str,
        token: str | None,
        federation_token: str | None,
        timeout: float,
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.federation_token = federation_token
        self.timeout = timeout

    def request(
        self,
        method: str,
        path: str,
        *,
        json_body: Any | None = None,
        body: bytes | None = None,
        content_type: str | None = None,
        timeout: float | None = None,
        unlimited: bool = False,
    ) -> tuple[int, Mapping[str, str], bytes]:
        if json_body is not None and body is not None:
            raise ValueError("request accepts json_body or body, not both")
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if self.federation_token and path.startswith("/federation/"):
            headers["X-LlamaFarm-Federation-Token"] = self.federation_token
        if json_body is not None:
            body = json.dumps(json_body).encode("utf-8")
            content_type = "application/json"
        if content_type:
            headers["Content-Type"] = content_type
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=body,
            headers=headers,
            method=method,
        )
        try:
            request_timeout = (
                None
                if unlimited
                else self.timeout if timeout is None else timeout
            )
            with urllib.request.urlopen(
                request, timeout=request_timeout
            ) as response:
                return response.status, response.headers, response.read()
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")[-1200:]
            raise SurfaceError(
                f"{method} {path} returned HTTP {error.code}: {detail}"
            ) from error
        except (urllib.error.URLError, TimeoutError, socket.timeout) as error:
            raise SurfaceError(f"{method} {path} failed: {error}") from error

    def json(
        self,
        method: str,
        path: str,
        *,
        body: Any | None = None,
        timeout: float | None = None,
        unlimited: bool = False,
    ) -> Any:
        _, _, raw = self.request(
            method, path, json_body=body, timeout=timeout, unlimited=unlimited
        )
        try:
            return json.loads(raw)
        except json.JSONDecodeError as error:
            raise SurfaceError(
                f"{method} {path} did not return JSON: "
                f"{raw.decode('utf-8', errors='replace')[-800:]}"
            ) from error

    def text(self, method: str, path: str) -> str:
        _, _, raw = self.request(method, path)
        return raw.decode("utf-8", errors="replace")

    def status(self, method: str, path: str) -> int:
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                return int(response.status)
        except urllib.error.HTTPError as error:
            return int(error.code)
        except (urllib.error.URLError, TimeoutError, socket.timeout) as error:
            raise SurfaceError(f"{method} {path} failed: {error}") from error


class WebDriver:
    def __init__(self, base_url: str) -> None:
        self.base_url = base_url.rstrip("/")
        self.session_id: str | None = None

    def request(self, method: str, path: str, payload: Any | None = None) -> Any:
        body = None
        headers = {"Accept": "application/json"}
        if payload is not None:
            body = json.dumps(payload).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(
            f"{self.base_url}{path}",
            data=body,
            headers=headers,
            method=method,
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")[-1200:]
            raise SurfaceError(
                f"ChromeDriver {method} {path} returned HTTP {error.code}: {detail}"
            ) from error
        except urllib.error.URLError as error:
            raise SurfaceError(
                f"ChromeDriver unavailable at {self.base_url}: {error}"
            ) from error
        try:
            decoded = json.loads(raw) if raw else {}
        except json.JSONDecodeError as error:
            raise SurfaceError(
                f"ChromeDriver {method} {path} returned invalid JSON"
            ) from error
        value = decoded.get("value") if isinstance(decoded, dict) else decoded
        if isinstance(value, dict) and value.get("error"):
            raise SurfaceError(
                f"ChromeDriver {method} {path}: "
                f"{value.get('message', value['error'])}"
            )
        return value

    def start(self) -> None:
        value = self.request(
            "POST",
            "/session",
            {
                "capabilities": {
                    "alwaysMatch": {
                        "browserName": "chrome",
                        "goog:chromeOptions": {
                            "args": [
                                "--headless=new",
                                "--no-sandbox",
                                "--disable-dev-shm-usage",
                                "--window-size=1440,1100",
                            ]
                        },
                        "goog:loggingPrefs": {
                            "browser": "ALL",
                            "performance": "ALL",
                        },
                    }
                }
            },
        )
        if not isinstance(value, dict) or not value.get("sessionId"):
            raise SurfaceError(f"invalid ChromeDriver session response: {value}")
        self.session_id = str(value["sessionId"])

    def close(self) -> None:
        if self.session_id:
            try:
                self.request("DELETE", f"/session/{self.session_id}")
            except SurfaceError:
                pass
            self.session_id = None

    def _path(self, suffix: str) -> str:
        if not self.session_id:
            raise SurfaceError("ChromeDriver session is not started")
        return f"/session/{self.session_id}{suffix}"

    def navigate(self, url: str) -> None:
        self.request("POST", self._path("/url"), {"url": url})

    def current_url(self) -> str:
        value = self.request("GET", self._path("/url"))
        return value if isinstance(value, str) else str(value)

    def execute(self, script: str, args: list[Any] | None = None) -> Any:
        return self.request(
            "POST",
            self._path("/execute/sync"),
            {"script": script, "args": args or []},
        )

    def body_text(self) -> str:
        value = self.execute(
            "return document.body ? document.body.innerText : '';"
        )
        return value if isinstance(value, str) else str(value)

    def find(self, selector: str) -> str:
        value = self.request(
            "POST",
            self._path("/element"),
            {"using": "css selector", "value": selector},
        )
        if not isinstance(value, dict):
            raise SurfaceError(f"no element returned for {selector!r}")
        element_id = value.get(ELEMENT_KEY) or value.get("ELEMENT")
        if not element_id:
            raise SurfaceError(f"no element id returned for {selector!r}")
        return str(element_id)

    def click(self, element_id: str) -> None:
        self.request("POST", self._path(f"/element/{element_id}/click"), {})

    def send_keys(self, element_id: str, text: str) -> None:
        self.request(
            "POST",
            self._path(f"/element/{element_id}/value"),
            {"text": text, "value": list(text)},
        )

    def log(self, kind: str) -> list[dict[str, Any]]:
        value = self.request("POST", self._path("/log"), {"type": kind})
        return value if isinstance(value, list) else []

    def screenshot(self) -> bytes:
        value = self.request("GET", self._path("/screenshot"))
        if not isinstance(value, str):
            raise SurfaceError("invalid screenshot response")
        return base64.b64decode(value)


class FullSurfaceAudit:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.api = Api(
            args.base_url,
            args.token,
            args.federation_token,
            args.http_timeout,
        )
        self.driver = WebDriver(args.driver_url)
        self.run_id = uuid.uuid4().hex
        self.prefix = f"surface-e2e-{self.run_id[:12]}"
        self.workspace_dir = f".llamafarm-surface-audit/{self.run_id}"
        self.monaco_path = f"{self.prefix}-monaco.txt"
        self.monaco_initial = f"MONACO_INITIAL_{self.run_id}"
        self.monaco_saved = f"MONACO_SAVED_{self.run_id}"
        self.results: list[dict[str, Any]] = []
        self.browser_errors: list[str] = []
        self.cleanup_callbacks: list[tuple[str, Callable[[], None]]] = []
        self.status: dict[str, Any] = {}

    def record(self, name: str, evidence: str) -> None:
        self.results.append({"name": name, "ok": True, "evidence": evidence})
        print(f"PASS {name}: {evidence}", flush=True)

    def fail(self, name: str, error: BaseException) -> None:
        detail = f"{type(error).__name__}: {error}"
        self.results.append({"name": name, "ok": False, "error": detail})
        print(f"FAIL {name}: {detail}", file=sys.stderr, flush=True)

    def check(self, name: str, action: Callable[[], str]) -> None:
        try:
            self.record(name, action())
        except Exception as error:
            self.fail(name, error)

    def add_cleanup(self, name: str, callback: Callable[[], None]) -> None:
        self.cleanup_callbacks.append((name, callback))

    @staticmethod
    def require(condition: Any, message: str) -> None:
        if not condition:
            raise SurfaceError(message)

    def readonly_api_surface(self) -> str:
        health = self.api.json("GET", "/health")
        self.require(health.get("status") == "ok", "/health is not ok")
        self.status = self.api.json("GET", "/api/status")
        self.require(self.status.get("model"), "/api/status has no model")

        checks: tuple[tuple[str, str], ...] = (
            ("/api/config/presets", "safe"),
            ("/api/cron", "jobs"),
            ("/api/integrations", "integrations"),
            ("/api/integrations/settings", "integrations"),
            ("/api/memory?query=surface_e2e_no_match", "entries"),
            ("/api/cost", "cost"),
            ("/api/cli-tools", "cli_tools"),
            ("/api/health", "health"),
            ("/api/connections", "ollama"),
            ("/api/context", "max"),
            ("/api/runs", "runs"),
            ("/api/federation/peers", "peers"),
            ("/api/federation/delegation", "enabled"),
            ("/api/logs?limit=5", "entries"),
            ("/api/db/connections", "connections"),
            ("/api/chat-sessions", "sessions"),
            ("/federation/health", "status"),
            ("/federation/capabilities", "node_id"),
            ("/federation/models", "installed_models"),
            ("/federation/tools", "tools"),
        )
        for path, key in checks:
            payload = self.api.json("GET", path)
            self.require(isinstance(payload, dict) and key in payload, f"{path} lacks {key}")

        tools = self.api.json("GET", "/api/tools").get("tools", [])
        self.require(
            len(tools) == self.args.expected_tools,
            f"expected {self.args.expected_tools} tools, got {len(tools)}",
        )
        self.require(
            len({tool.get("name") for tool in tools}) == len(tools),
            "/api/tools contains duplicate names",
        )

        models = self.api.json("GET", "/v1/models")
        self.require(isinstance(models.get("data"), list), "/v1/models lacks data")
        metrics = self.api.text("GET", "/metrics")
        self.require(metrics.strip(), "/metrics returned an empty body")

        doctor_get = self.api.json("GET", "/api/doctor")
        doctor_post = self.api.json("POST", "/api/doctor", body={})
        self.require("results" in doctor_get, "GET /api/doctor lacks results")
        self.require("results" in doctor_post, "POST /api/doctor lacks results")

        runs = self.api.json("GET", "/api/runs").get("runs", [])
        if runs:
            run_id = runs[0].get("run_id")
            self.require(run_id, "run entry lacks run_id")
            detail = self.api.json(
                "GET", f"/api/runs/{urllib.parse.quote(str(run_id), safe='')}"
            )
            self.require("run" in detail, "run detail lacks run")

        sessions = self.api.json("GET", "/api/chat-sessions").get("sessions", [])
        if sessions:
            session_id = sessions[0].get("session_id")
            self.require(session_id, "chat session lacks session_id")
            detail = self.api.json(
                "GET",
                f"/api/chat-sessions/{urllib.parse.quote(str(session_id), safe='')}",
            )
            self.require(
                detail.get("session_id") == session_id
                and isinstance(detail.get("messages"), list),
                "chat session detail lacks the requested session/messages",
            )

        return f"{len(checks) + 10} read-only endpoints; {len(tools)} tools"

    def config_noop(self) -> str:
        payload = self.api.json("GET", "/api/config")
        original = (
            payload.get("content") if isinstance(payload, dict) else payload
        )
        self.require(isinstance(original, str), "config response lacks TOML content")
        self.require(original.strip(), "config is empty")
        self.api.request(
            "PUT",
            "/api/config",
            body=original.encode("utf-8"),
            content_type="application/toml",
        )
        current_payload = self.api.json("GET", "/api/config")
        current = (
            current_payload.get("content")
            if isinstance(current_payload, dict)
            else current_payload
        )
        self.require(isinstance(current, str), "config disappeared after no-op PUT")
        self.require(current.strip(), "config disappeared after no-op PUT")
        return "GET + exact no-op PUT + GET"

    def integration_noop(self) -> str:
        settings = self.api.json("GET", "/api/integrations/settings")
        ollama = next(
            (
                integration
                for integration in settings.get("integrations", [])
                if integration.get("id") == "ollama"
            ),
            None,
        )
        self.require(isinstance(ollama, dict), "Ollama settings are absent")
        fields: dict[str, str] = {}
        for field in ollama.get("fields", []):
            if field.get("key") in {"default_model", "default_temperature"}:
                value = field.get("current_value")
                if isinstance(value, str) and value:
                    fields[str(field["key"])] = value
        self.require(fields, "Ollama settings expose no no-op fields")
        result = self.api.json(
            "PUT",
            "/api/integrations/ollama/credentials",
            body={"revision": settings.get("revision"), "fields": fields},
        )
        self.require(result.get("status") == "ok", "Ollama no-op update failed")
        return "Ollama model/temperature exact no-op update"

    def context_noop(self) -> str:
        before = self.api.json("GET", "/api/context")
        result = self.api.json(
            "PUT",
            "/api/context",
            body={
                "num_ctx": before.get("num_ctx"),
                "gpu_layers": before.get("gpu_layers"),
                "set_gpu_layers": True,
            },
        )
        self.require(result.get("status") == "ok", "context PUT failed")
        after = self.api.json("GET", "/api/context")
        self.require(after.get("num_ctx") == before.get("num_ctx"), "num_ctx changed")
        self.require(
            after.get("gpu_layers") == before.get("gpu_layers"),
            "gpu_layers changed",
        )
        self.require(after.get("max") == 262_144, "context maximum is not 256K")
        return "adaptive/manual context and GPU values round-tripped unchanged"

    def federation_noop(self) -> str:
        before = self.api.json("GET", "/api/federation/delegation")
        enabled = before.get("enabled")
        self.require(isinstance(enabled, bool), "delegation enabled is not boolean")
        result = self.api.json(
            "PUT", "/api/federation/delegation", body={"enabled": enabled}
        )
        self.require(result.get("enabled") == enabled, "delegation no-op changed state")
        peers = self.api.json("GET", "/api/federation/peers")
        self.require(peers.get("enabled") is True, "federation is not live")
        peer_entries = peers.get("peers", [])
        self.require(
            len(peer_entries) >= self.args.expected_peers,
            f"expected at least {self.args.expected_peers} federation peers, "
            f"got {len(peer_entries)}",
        )
        self.require(
            all(peer.get("online") for peer in peer_entries),
            "one or more configured federation peers is offline",
        )
        return f"delegation no-op; {len(peer_entries)} peers online"

    def workspace_crud(self) -> str:
        root = self.workspace_dir
        file_path = f"{root}/api.txt"
        marker = f"WORKSPACE_API_{self.run_id}"
        self.api.json(
            "PUT",
            f"/api/workspace/directory?path={urllib.parse.quote(root, safe='')}",
        )
        self.add_cleanup(
            "workspace-directory",
            lambda: self.api.request(
                "DELETE",
                f"/api/workspace/path?path={urllib.parse.quote(root, safe='')}",
            ),
        )
        self.api.request(
            "PUT",
            f"/api/workspace/blob?path={urllib.parse.quote(file_path, safe='')}",
            body=(marker + "\n").encode(),
            content_type="text/plain",
        )
        browser = self.api.json(
            "GET",
            f"/api/workspace/browser?path={urllib.parse.quote(root, safe='')}",
        )
        self.require(
            any(entry.get("name") == "api.txt" for entry in browser.get("entries", [])),
            "workspace browser did not list created file",
        )
        downloaded = self.api.text(
            "GET",
            f"/api/workspace/download?path={urllib.parse.quote(file_path, safe='')}",
        )
        self.require(downloaded.strip() == marker, "workspace download changed bytes")
        _, _, archive = self.api.request(
            "GET",
            f"/api/workspace/download?path={urllib.parse.quote(root, safe='')}",
        )
        self.require(len(archive) > len(marker), "directory archive is empty")
        executed = self.api.json(
            "POST",
            "/api/workspace/exec",
            body={"command": f"printf '{marker}\\n'"},
        )
        self.require(executed.get("exit_code") == 0, "workspace exec failed")
        self.require(marker in executed.get("stdout", ""), "workspace exec marker absent")

        self.api.request(
            "PUT",
            f"/api/workspace/blob?path={urllib.parse.quote(self.monaco_path, safe='')}",
            body=(self.monaco_initial + "\n").encode(),
            content_type="text/plain",
        )
        self.add_cleanup(
            "monaco-file",
            lambda: self.api.request(
                "DELETE",
                f"/api/workspace/path?path={urllib.parse.quote(self.monaco_path, safe='')}",
            ),
        )
        return "mkdir/upload/browse/download/archive/exec fixtures created"

    def workspace_prompt_noop(self) -> str:
        payload = self.api.json("GET", "/api/workspace-files/AGENTS.md")
        content = payload.get("content")
        self.require(isinstance(content, str), "AGENTS.md response lacks content")
        result = self.api.json(
            "PUT", "/api/workspace-files/AGENTS.md", body={"content": content}
        )
        self.require(result.get("status") == "ok", "AGENTS.md no-op PUT failed")
        persisted = self.api.json("GET", "/api/workspace-files/AGENTS.md")
        self.require(
            persisted.get("content") == content,
            "AGENTS.md no-op did not preserve exact persisted content",
        )
        return "AGENTS.md exact no-op PUT + GET"

    def memory_crud(self) -> str:
        key = f"{self.prefix}-memory"
        marker = f"MEMORY_API_{self.run_id}"
        self.add_cleanup(
            "memory",
            lambda: self.api.request(
                "DELETE", f"/api/memory/{urllib.parse.quote(key, safe='')}"
            ),
        )
        stored = self.api.json(
            "POST",
            "/api/memory",
            body={"key": key, "content": marker, "category": "conversation"},
        )
        self.require(stored.get("status") == "ok", "memory store failed")
        recalled = self.api.json(
            "GET", f"/api/memory?query={urllib.parse.quote(marker, safe='')}"
        )
        self.require(
            any(entry.get("key") == key for entry in recalled.get("entries", [])),
            "stored memory was not recalled",
        )
        deleted = self.api.json(
            "DELETE", f"/api/memory/{urllib.parse.quote(key, safe='')}"
        )
        self.require(deleted.get("deleted"), "memory delete reported no deletion")
        self.cleanup_callbacks = [
            item for item in self.cleanup_callbacks if item[0] != "memory"
        ]
        return "store/search/delete"

    def cron_crud(self) -> str:
        marker = f"CRON_API_{self.run_id}"
        created = self.api.json(
            "POST",
            "/api/cron",
            body={
                "name": f"{self.prefix}-cron",
                "schedule_kind": "cron",
                "schedule": "0 0 1 1 *",
                "command": f"printf '{marker}\\n'",
                "enabled": False,
            },
        )
        job = created.get("job", {})
        job_id = job.get("id")
        self.require(job_id, "cron create returned no id")
        encoded = urllib.parse.quote(str(job_id), safe="")
        self.add_cleanup(
            "cron",
            lambda: self.api.request("DELETE", f"/api/cron/{encoded}"),
        )
        updated = self.api.json(
            "PUT",
            f"/api/cron/{encoded}",
            body={"name": f"{self.prefix}-cron-updated", "enabled": False},
        )
        self.require(
            updated.get("job", {}).get("name") == f"{self.prefix}-cron-updated",
            "cron update did not persist",
        )
        run = self.api.json("POST", f"/api/cron/{encoded}/run", body={})
        self.require(run.get("status") == "ok", "cron manual run failed")
        self.require(marker in run.get("output", ""), "cron output marker absent")
        runs = self.api.json("GET", f"/api/cron/{encoded}/runs?limit=5")
        self.require(runs.get("runs"), "cron run history is empty")
        self.api.request("DELETE", f"/api/cron/{encoded}")
        self.cleanup_callbacks = [
            item for item in self.cleanup_callbacks if item[0] != "cron"
        ]
        return "create/update/run/history/delete"

    def db_sqlite_crud(self) -> str:
        discovery = self.api.json(
            "POST",
            "/api/db/discover",
            body={"hosts": ["203.0.113.1"]},
        )
        self.require(
            isinstance(discovery.get("discovered"), list),
            "database discovery response lacks discovered entries",
        )
        name = f"{self.prefix}-sqlite"
        relative = f"{self.workspace_dir}/audit.sqlite"
        absolute = f"/llamafarm-data/workspace/{relative}"
        marker = f"DB_API_{self.run_id}"
        command = (
            "python3 -c \"import sqlite3; "
            f"c=sqlite3.connect('{absolute}'); "
            "c.execute('CREATE TABLE audit (marker TEXT NOT NULL)'); "
            f"c.execute(\\\"INSERT INTO audit VALUES ('{marker}')\\\"); "
            "c.commit(); c.close()\""
        )
        created = self.api.json(
            "POST", "/api/workspace/exec", body={"command": command}
        )
        self.require(created.get("exit_code") == 0, "SQLite fixture creation failed")
        body = {
            "name": name,
            "driver": "sqlite",
            "uri": absolute,
            "database": None,
            "label": "Catalogue SQLite",
            "read_only": True,
            "max_rows": 10,
        }
        tested = self.api.json("POST", "/api/db/connections/test", body=body)
        self.require(tested.get("ok") is True, "SQLite connection test failed")
        added = self.api.json("POST", "/api/db/connections", body=body)
        self.require(added.get("status") == "ok", "SQLite connection add failed")
        encoded = urllib.parse.quote(name, safe="")
        self.add_cleanup(
            "database",
            lambda: self.api.request("DELETE", f"/api/db/connections/{encoded}"),
        )
        body["uri"] = "***MASKED***"
        body["label"] = "Catalogue SQLite Updated"
        body["max_rows"] = 20
        updated = self.api.json(
            "PUT", f"/api/db/connections/{encoded}", body=body
        )
        self.require(updated.get("status") == "ok", "SQLite connection update failed")
        listed = self.api.json("GET", "/api/db/connections")
        self.require(
            any(item.get("name") == name for item in listed.get("connections", [])),
            "SQLite connection absent from list",
        )
        schema = self.api.json("GET", f"/api/db/{encoded}/schema")
        self.require(
            any(table.get("name") == "audit" for table in schema.get("tables", [])),
            "SQLite schema lacks audit table",
        )
        query = self.api.json(
            "POST",
            f"/api/db/{encoded}/query",
            body={"query": "SELECT marker FROM audit", "max_rows": 5},
        )
        self.require(
            any(marker in json.dumps(row) for row in query.get("rows", [])),
            "SQLite query marker absent",
        )
        self.api.request("DELETE", f"/api/db/connections/{encoded}")
        self.cleanup_callbacks = [
            item for item in self.cleanup_callbacks if item[0] != "database"
        ]
        return "discover/test/add/update/list/schema/query/delete"

    def _start_sse_reader(
        self, path: str
    ) -> tuple[threading.Event, queue.Queue[str], threading.Thread]:
        ready = threading.Event()
        events: queue.Queue[str] = queue.Queue()

        def reader() -> None:
            headers = {"Accept": "text/event-stream"}
            if self.args.token:
                headers["Authorization"] = f"Bearer {self.args.token}"
            request = urllib.request.Request(
                f"{self.args.base_url.rstrip('/')}{path}", headers=headers
            )
            try:
                with urllib.request.urlopen(request, timeout=None) as response:
                    content_type = response.headers.get("Content-Type", "")
                    if "text/event-stream" not in content_type:
                        events.put(f"ERROR content-type={content_type}")
                        ready.set()
                        return
                    ready.set()
                    while True:
                        line = response.readline().decode("utf-8", errors="replace")
                        if line.startswith("data:"):
                            events.put(line[5:].strip())
                            return
            except Exception as error:
                events.put(f"ERROR {type(error).__name__}: {error}")
                ready.set()

        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        return ready, events, thread

    def model_and_sse(self) -> str:
        event_ready, event_queue, _ = self._start_sse_reader("/api/events")
        log_ready, log_queue, _ = self._start_sse_reader("/api/logs/stream")
        self.require(event_ready.wait(15), "/api/events did not open")
        self.require(log_ready.wait(15), "/api/logs/stream did not open")

        marker = f"OPENAI_API_{self.run_id}"
        model = str(self.status.get("model") or "")
        self.require(model, "status has no model for OpenAI-compatible call")
        completion = self.api.json(
            "POST",
            "/v1/chat/completions",
            body={
                "model": model,
                "messages": [
                    {
                        "role": "user",
                        "content": f"Reply with exactly {marker} and nothing else.",
                    }
                ],
                "temperature": 0,
                "stream": False,
            },
            unlimited=True,
        )
        choices = completion.get("choices", [])
        content = (
            choices[0].get("message", {}).get("content", "") if choices else ""
        )
        self.require(marker in content, "OpenAI-compatible response lacks marker")
        event = event_queue.get()
        log = log_queue.get()
        self.require(not event.startswith("ERROR"), f"event SSE failed: {event}")
        self.require(not log.startswith("ERROR"), f"log SSE failed: {log}")
        return "non-stream chat completion plus event/log SSE delivery"

    def _delete_chat_session(self, session_id: str) -> None:
        self.driver.navigate(f"{self.args.base_url.rstrip('/')}/")
        sent = self.driver.execute(
            """
            const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
            const socket = new WebSocket(`${scheme}//${location.host}/ws/chat`);
            socket.addEventListener('open', () => {
              socket.send(JSON.stringify({
                type: 'session_delete',
                session_id: arguments[0]
              }));
              window.setTimeout(() => socket.close(), 300);
            });
            window.__surfaceDeleteSocket = socket;
            return true;
            """,
            [session_id],
        )
        self.require(sent, "could not send chat session deletion")
        encoded = urllib.parse.quote(session_id, safe="")
        wait_for(
            "background chat session deletion",
            lambda: self.api.status("GET", f"/api/chat-sessions/{encoded}") == 404,
            30,
        )

    def background_chat_survives_disconnect(self) -> str:
        session_id = f"{self.prefix}-background"
        marker = f"BACKGROUND_CHAT_{self.run_id}"
        encoded = urllib.parse.quote(session_id, safe="")
        self.add_cleanup(
            "background-chat",
            lambda: self._delete_chat_session(session_id),
        )

        self.driver.navigate(f"{self.args.base_url.rstrip('/')}/agent")
        started = self.driver.execute(
            """
            window.__surfaceBackground = {
              opened: false,
              firstEvent: false,
              error: null
            };
            const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
            const socket = new WebSocket(`${scheme}//${location.host}/ws/chat`);
            window.__surfaceBackground.socket = socket;
            socket.addEventListener('open', () => {
              window.__surfaceBackground.opened = true;
              socket.send(JSON.stringify({
                type: 'message',
                content: arguments[1],
                session_id: arguments[0],
                temporary: false
              }));
            });
            socket.addEventListener('message', (event) => {
              window.__surfaceBackground.firstEvent = true;
              window.__surfaceBackground.lastEvent = event.data;
            });
            socket.addEventListener('error', () => {
              window.__surfaceBackground.error = 'websocket error';
            });
            return true;
            """,
            [
                session_id,
                f"Reply with exactly {marker} and nothing else. Do not call a tool.",
            ],
        )
        self.require(started, "background WebSocket setup failed")
        wait_for(
            "background chat acceptance event",
            lambda: self.driver.execute(
                """
                const state = window.__surfaceBackground;
                if (state?.error) throw new Error(state.error);
                return Boolean(state?.opened && state?.firstEvent);
                """
            ),
            60,
        )

        # Remove the LlamaFarm document entirely. The server-owned run must
        # continue with no dashboard page and no live WebSocket viewer.
        self.driver.navigate("about:blank")

        def completed_session() -> dict[str, Any] | None:
            try:
                detail = self.api.json(
                    "GET", f"/api/chat-sessions/{encoded}"
                )
            except SurfaceError:
                return None
            messages = detail.get("messages", [])
            if any(
                message.get("role") == "assistant"
                and marker in str(message.get("content", ""))
                for message in messages
                if isinstance(message, dict)
            ):
                return detail
            return None

        wait_for_unbounded("background chat completion after disconnect", completed_session)
        runs = self.api.json("GET", "/api/runs").get("runs", [])
        matched_run = next(
            (
                run
                for run in runs
                if run.get("session_id") == session_id
                and run.get("status") == "completed"
            ),
            None,
        )
        self.require(matched_run, "background chat has no completed run ledger")
        run_id = urllib.parse.quote(str(matched_run["run_id"]), safe="")
        run_detail = self.api.json("GET", f"/api/runs/{run_id}")
        self.require("run" in run_detail, "background run detail is unavailable")

        self._delete_chat_session(session_id)
        self.cleanup_callbacks = [
            item for item in self.cleanup_callbacks if item[0] != "background-chat"
        ]
        return "browser detached; server completed/persisted chat and run ledger"

    def _browser_diagnostics(self, route: str) -> None:
        errors: list[str] = []
        for entry in self.driver.log("browser"):
            if str(entry.get("level", "")).upper() == "SEVERE":
                message = str(entry.get("message", ""))
                # Chromium labels this parser warning SEVERE even though it is
                # neither a JavaScript exception nor an application failure.
                # frame-ancestors is intentionally ignored in HTML meta tags;
                # retain strict failure for every other SEVERE console entry.
                if (
                    "The Content Security Policy directive 'frame-ancestors' "
                    "is ignored when delivered via a <meta> element."
                ) in message:
                    continue
                errors.append(f"{route} console: {message}")
        for entry in self.driver.log("performance"):
            try:
                envelope = json.loads(str(entry.get("message", "")))
                message = envelope.get("message", {})
                if message.get("method") != "Network.responseReceived":
                    continue
                response = message.get("params", {}).get("response", {})
                status = int(response.get("status", 0))
                url = str(response.get("url", ""))
                if url.startswith(self.args.base_url.rstrip("/")) and status >= 400:
                    errors.append(f"{route} network HTTP {status}: {url}")
            except (ValueError, TypeError, json.JSONDecodeError):
                continue
        self.browser_errors.extend(errors)
        if errors:
            raise SurfaceError(" | ".join(errors[-5:]))

    def browser_routes(self) -> str:
        for route, expected in ROUTES:
            self.driver.navigate(f"{self.args.base_url.rstrip('/')}{route}")
            if route == "/agent":
                wait_for(
                    "Agent chat composer",
                    lambda: bool(
                        self.driver.execute(
                            "return document.querySelector('[data-testid=\"agent-chat-input\"]');"
                        )
                    ),
                    60,
                )
            else:
                wait_for(
                    f"{route} content",
                    lambda expected=expected: (
                        expected in self.driver.body_text()
                        if expected
                        else bool(self.driver.body_text().strip())
                    ),
                    60,
                )
            body = self.driver.body_text()
            self.require("Page not found" not in body, f"{route} rendered NotFound")
            self.require("Failed to load" not in body, f"{route} rendered a load failure")
            self._browser_diagnostics(route)

        for route, target in REDIRECT_ROUTES:
            self.driver.navigate(f"{self.args.base_url.rstrip('/')}{route}")
            wait_for(
                f"{route} redirect",
                lambda target=target: urllib.parse.urlsplit(
                    self.driver.current_url()
                ).path
                == target,
                30,
            )
            self._browser_diagnostics(route)
        return f"{len(ROUTES)} canonical routes + {len(REDIRECT_ROUTES)} redirects"

    def monaco_roundtrip(self) -> str:
        self.driver.navigate(f"{self.args.base_url.rstrip('/')}/workspace")
        wait_for(
            "Monaco fixture in file tree",
            lambda: bool(
                self.driver.execute(
                    """
                    return Array.from(document.querySelectorAll('button')).some(
                      (button) => (button.textContent || '').trim() === arguments[0]
                    );
                    """,
                    [self.monaco_path],
                )
            ),
            60,
        )
        clicked = self.driver.execute(
            """
            const button = Array.from(document.querySelectorAll('button')).find(
              (candidate) => (candidate.textContent || '').trim() === arguments[0]
            );
            if (!button) return false;
            button.click();
            return true;
            """,
            [self.monaco_path],
        )
        self.require(clicked, "could not click Monaco fixture")
        wait_for(
            "Monaco editor",
            lambda: bool(
                self.driver.execute(
                    "return document.querySelector('.monaco-editor');"
                )
            ),
            90,
        )
        input_id = wait_for(
            "Monaco keyboard input",
            lambda: self.driver.find(
                ".monaco-editor .native-edit-context, "
                ".monaco-editor textarea.inputarea"
            ),
            90,
        )
        self.driver.execute(
            """
            document.querySelector(
              '.monaco-editor .native-edit-context, .monaco-editor textarea.inputarea'
            )?.focus();
            """
        )
        # Native WebDriver keystrokes exercise Monaco's real keyboard/editing
        # path. U+E009 is Control and U+E000 releases held modifier keys.
        self.driver.send_keys(input_id, "\ue009a\ue000")
        self.driver.send_keys(input_id, self.monaco_saved + "\n")
        wait_for(
            "Monaco dirty Save state",
            lambda: bool(
                self.driver.execute(
                    """
                    const button = Array.from(document.querySelectorAll('button')).find(
                      (candidate) => (candidate.textContent || '').trim().startsWith('Save')
                    );
                    return Boolean(button && !button.disabled);
                    """
                )
            ),
            30,
        )
        saved = self.driver.execute(
            """
            const button = Array.from(document.querySelectorAll('button')).find(
              (candidate) => (candidate.textContent || '').trim().startsWith('Save')
            );
            if (!button || button.disabled) return false;
            button.click();
            return true;
            """
        )
        self.require(saved, "Monaco Save button was unavailable")

        def persisted() -> bool:
            content = self.api.text(
                "GET",
                f"/api/workspace/download?path="
                f"{urllib.parse.quote(self.monaco_path, safe='')}",
            )
            return content.strip() == self.monaco_saved

        wait_for("Monaco save persisted through API", persisted, 30)
        self._browser_diagnostics("/workspace#monaco")
        return "editor loaded, model changed, Save persisted exact bytes"

    def cleanup(self) -> None:
        for name, callback in reversed(self.cleanup_callbacks):
            try:
                callback()
                print(f"CLEANUP {name}: ok", flush=True)
            except Exception as error:
                self.fail(f"cleanup-{name}", error)

    def save_artifact(self) -> None:
        path = Path(self.args.artifact)
        path.parent.mkdir(parents=True, exist_ok=True)
        payload = {
            "node": self.args.node,
            "run_id": self.run_id,
            "base_url": self.args.base_url,
            "results": self.results,
            "browser_errors": self.browser_errors,
            "finished_at_unix": int(time.time()),
        }
        path.write_text(
            json.dumps(payload, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        if self.driver.session_id:
            path.with_suffix(".png").write_bytes(self.driver.screenshot())

    def run(self) -> int:
        try:
            self.check("api-readonly-surface", self.readonly_api_surface)
            self.check("api-config-noop", self.config_noop)
            self.check("api-integration-noop", self.integration_noop)
            self.check("api-context-noop", self.context_noop)
            self.check("api-federation-noop", self.federation_noop)
            self.check("api-workspace-crud", self.workspace_crud)
            self.check("api-workspace-prompt-noop", self.workspace_prompt_noop)
            self.check("api-memory-crud", self.memory_crud)
            self.check("api-cron-crud", self.cron_crud)
            self.check("api-db-sqlite-crud", self.db_sqlite_crud)
            if not self.args.skip_model_call:
                self.check("api-model-and-sse", self.model_and_sse)

            try:
                self.driver.start()
                self.check("ui-all-routes", self.browser_routes)
                self.check("ui-monaco-roundtrip", self.monaco_roundtrip)
                if not self.args.skip_model_call:
                    self.check(
                        "ui-background-chat-disconnect",
                        self.background_chat_survives_disconnect,
                    )
            except Exception as error:
                self.fail("ui-browser-session", error)
        finally:
            self.cleanup()
            try:
                self.save_artifact()
            except Exception as error:
                self.fail("artifact", error)
            self.driver.close()

        failed = [result for result in self.results if not result["ok"]]
        print(
            json.dumps(
                {
                    "node": self.args.node,
                    "passed": len(self.results) - len(failed),
                    "failed": len(failed),
                    "artifact": self.args.artifact,
                },
                sort_keys=True,
            )
        )
        return 1 if failed else 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--node", default=os.environ.get("LLAMAFARM_NODE_NAME", "node"))
    parser.add_argument("--base-url", default=APP_URL)
    parser.add_argument("--driver-url", default=DRIVER_URL)
    parser.add_argument(
        "--token", default=os.environ.get("LLAMAFARM_GATEWAY_TOKEN")
    )
    parser.add_argument(
        "--federation-token",
        default=os.environ.get("LLAMAFARM_FEDERATION_TOKEN"),
    )
    parser.add_argument("--expected-tools", type=int, default=57)
    parser.add_argument(
        "--expected-peers",
        type=int,
        default=2,
        help="minimum distinct online federation peers required on this node",
    )
    parser.add_argument("--http-timeout", type=float, default=30)
    parser.add_argument(
        "--skip-model-call",
        action="store_true",
        help="diagnostic-only: skip OpenAI-compatible completion and SSE event assertion",
    )
    parser.add_argument(
        "--artifact",
        default=(
            "/llamafarm-data/workspace/acceptance-artifacts/"
            f"full-surface-{int(time.time())}.json"
        ),
    )
    return parser


def main() -> int:
    try:
        return FullSurfaceAudit(build_parser().parse_args()).run()
    except KeyboardInterrupt:
        print("Interrupted by operator.", file=sys.stderr)
        return 130
    except Exception as error:
        print(f"FATAL {type(error).__name__}: {error}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
