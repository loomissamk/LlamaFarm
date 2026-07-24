from __future__ import annotations

import importlib.util
import io
import json
import sys
import threading
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from unittest import mock


SCRIPT_PATH = Path(__file__).parents[1] / "node-acceptance.py"
SPEC = importlib.util.spec_from_file_location("node_acceptance", SCRIPT_PATH)
NODE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = NODE
SPEC.loader.exec_module(NODE)


CONFIG_TOML = """
default_provider = "ollama"
default_model = "model-a"

[[model_routes]]
hint = "devsecops"
provider = "ollama"
model = "model-a"

[agents.devsecops]
provider = "ollama"
model = "model-a"
allowed_tools = [
  "cron_list",
  "cron_runs",
  "db_schema",
  "docker",
  "shell",
]
"""


class FixtureState:
    def __init__(
        self,
        *,
        cold_model: bool = False,
        component_status: str = "ok",
        leak_db: bool = False,
    ) -> None:
        self.model = "model-a"
        self.cold_model = cold_model
        self.component_status = component_status
        self.leak_db = leak_db
        self.requests: list[tuple[str, str]] = []

    def status(self) -> dict[str, object]:
        loaded_models = [] if self.cold_model else ["model-a"]
        return {
            "provider": "ollama",
            "model": self.model,
            "gateway_port": 42617,
            "health": {
                "pid": 123,
                "updated_at": "2026-01-01T00:00:00Z",
                "uptime_seconds": 10,
                "components": {
                    "gateway": {
                        "status": self.component_status,
                        "updated_at": "2026-01-01T00:00:00Z",
                        "last_ok": "2026-01-01T00:00:00Z",
                        "last_error": None,
                        "restart_count": 0,
                    }
                },
            },
            "ollama": {
                "endpoint": "http://127.0.0.1:11434",
                "reachable": True,
                "configured_model": self.model,
                "installed_models": ["model-a", "model-b"],
                "loaded_models": loaded_models,
                "active_model_loaded": self.model in loaded_models,
                "revision": "a" * 64,
                "model_environment_override": None,
            },
        }

    def settings(self) -> dict[str, object]:
        return {
            "revision": "a" * 64,
            "active_default_provider_integration_id": "ollama",
            "integrations": [
                {
                    "id": "ollama",
                    "configured": True,
                    "fields": [
                        {
                            "key": "default_model",
                            "options": ["model-a", "model-b"],
                            "current_value": self.model,
                        },
                        {
                            "key": "api_key",
                            "has_value": False,
                            "current_value": None,
                            "masked_value": None,
                        },
                    ],
                }
            ],
        }


def handler_for(state: FixtureState) -> type[BaseHTTPRequestHandler]:
    class FixtureHandler(BaseHTTPRequestHandler):
        def log_message(self, format: str, *args: object) -> None:
            del format, args

        def send_json(
            self,
            status: int,
            payload: object,
            *,
            headers: dict[str, str] | None = None,
        ) -> None:
            body = json.dumps(payload).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            for key, value in (headers or {}).items():
                self.send_header(key, value)
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self) -> None:
            path = self.path.split("?", 1)[0]
            state.requests.append(("GET", path))
            if path == "/health":
                self.send_json(
                    200,
                    {
                        "status": "ok",
                        "runtime": {
                            "pid": 123,
                            "updated_at": "2026-01-01T00:00:00Z",
                            "uptime_seconds": 10,
                            "components": {
                                "gateway": {
                                    "status": state.component_status,
                                    "updated_at": "2026-01-01T00:00:00Z",
                                    "last_ok": "2026-01-01T00:00:00Z",
                                    "last_error": None,
                                    "restart_count": 0,
                                }
                            },
                        },
                    },
                )
            elif path == "/api/health":
                self.send_json(
                    200,
                    {
                        "health": {
                            "pid": 123,
                            "updated_at": "2026-01-01T00:00:00Z",
                            "uptime_seconds": 10,
                            "components": {
                                "gateway": {
                                    "status": state.component_status,
                                    "updated_at": "2026-01-01T00:00:00Z",
                                    "last_ok": "2026-01-01T00:00:00Z",
                                    "last_error": None,
                                    "restart_count": 0,
                                }
                            },
                        }
                    },
                )
            elif path == "/api/status":
                self.send_json(200, state.status())
            elif path == "/api/integrations/settings":
                self.send_json(200, state.settings())
            elif path == "/api/integrations/ollama/credentials":
                self.send_json(405, {"error": "method not allowed"}, headers={"Allow": "PUT"})
            elif path == "/api/tags":
                self.send_json(
                    200,
                    {"models": [{"name": "model-a"}, {"name": "model-b"}]},
                )
            elif path == "/api/ps":
                models = [] if state.cold_model else [{"name": "model-a"}]
                self.send_json(200, {"models": models})
            elif path == "/api/connections":
                self.send_json(
                    200,
                    {
                        "github": {"status": "not_connected"},
                        "ollama": {
                            "status": "configured",
                            "model": state.model,
                            "provider": "ollama",
                        },
                        "memory": {"status": "configured", "backend": "sqlite"},
                        "discord": {"status": "not_connected"},
                        "tailscale": {"status": "unavailable"},
                    },
                )
            elif path == "/api/db/connections":
                uri = (
                    "mongodb://reader:fixture-secret@db.internal:27017/research"
                    if state.leak_db
                    else NODE.MASKED_SECRET
                )
                self.send_json(
                    200,
                    {
                        "connections": [
                            {
                                "name": "research",
                                "driver": "mongodb",
                                "uri": uri,
                                "database": "research",
                                "read_only": True,
                                "max_rows": 100,
                                "label": "Research",
                            }
                        ]
                    },
                )
            elif path == "/api/db/discover":
                self.send_json(
                    405,
                    {"error": "method not allowed"},
                    headers={"Allow": "POST"},
                )
            elif path == "/api/db/research/schema":
                self.send_json(
                    200,
                    {
                        "driver": "mongodb",
                        "database": "research",
                        "tables": [
                            {
                                "name": "papers",
                                "kind": "collection",
                                "columns": [
                                    {"name": "title", "data_type": "mixed"}
                                ],
                            }
                        ],
                    },
                )
            elif path == "/api/config":
                self.send_json(200, {"format": "toml", "content": CONFIG_TOML})
            elif path == "/api/config/presets":
                self.send_json(
                    200,
                    {
                        "safe": {
                            "content": "default_provider = 'ollama'",
                            "workspace_files": [{"name": "AGENTS.md", "content": "Safe"}],
                        }
                    },
                )
            elif path == "/api/workspace-files/AGENTS.md":
                self.send_json(
                    200,
                    {"name": "AGENTS.md", "content": "Project rules", "exists": True},
                )
            elif path == "/api/workspace-files/SOUL.md":
                self.send_json(
                    404,
                    {
                        "error": "unsupported workspace file",
                        "allowed_files": ["AGENTS.md"],
                    },
                )
            elif path == "/api/tools":
                self.send_json(
                    200,
                    {
                        "tools": [
                            {"name": "shell", "description": "Run commands"},
                            {"name": "cron_list", "description": "List schedules"},
                            {"name": "cron_runs", "description": "List run history"},
                        ]
                    },
                )
            elif path == "/api/workspace/browser":
                self.send_json(
                    200,
                    {
                        "root_path": "/workspace",
                        "current_path": "",
                        "entries": [
                            {"name": "AGENTS.md", "path": "AGENTS.md", "kind": "file"}
                        ],
                    },
                )
            elif path == "/api/cron":
                self.send_json(200, {"jobs": []})
            elif path == "/":
                body = b'<html><script src="/_app/app.js"></script></html>'
                self.send_response(200)
                self.send_header("Content-Type", "text/html")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            elif path == "/_app/app.js":
                body = b'console.log("LlamaFarm");'
                self.send_response(200)
                self.send_header("Content-Type", "text/javascript")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
            else:
                self.send_json(404, {"error": "not found"})

        def do_PUT(self) -> None:
            path = self.path.split("?", 1)[0]
            state.requests.append(("PUT", path))
            length = int(self.headers.get("Content-Length", "0"))
            body = json.loads(self.rfile.read(length))
            if path != "/api/integrations/ollama/credentials":
                self.send_json(404, {"error": "not found"})
                return
            state.model = body["fields"]["default_model"]
            self.send_json(
                200,
                {
                    "status": "ok",
                    "revision": "b" * 64,
                    "resident_models_changed": False,
                },
            )

    return FixtureHandler


class FixtureServer:
    def __init__(self, state: FixtureState) -> None:
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler_for(state))
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    @property
    def port(self) -> int:
        return self.server.server_address[1]

    def __enter__(self) -> "FixtureServer":
        self.thread.start()
        return self

    def __exit__(self, *args: object) -> None:
        del args
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


def checker_for(
    server: FixtureServer,
    state: FixtureState,
    *,
    switch_model: str | None = None,
) -> tuple[object, object, io.StringIO]:
    del state
    stream = io.StringIO()
    reporter = NODE.Reporter(stream)
    target = NODE.Target(
        kind="local",
        scheme="http",
        host="127.0.0.1",
        gateway_port=server.port,
    )
    options = NODE.Options(
        stock_port=5000,
        ollama_port=server.port,
        managed_ports=(8501, 8502),
        timeout=1.0,
        db_connection="research",
        skip_docker_port_map=True,
        switch_model=switch_model,
    )
    checker = NODE.NodeAcceptance(target, options, reporter, None)
    return checker, reporter, stream


class NodeAcceptanceTests(unittest.TestCase):
    def test_default_run_uses_get_only(self) -> None:
        state = FixtureState()
        with FixtureServer(state) as server:
            checker, reporter, _ = checker_for(server, state)
            with mock.patch.object(
                NODE.NodeAcceptance,
                "check_network_ports",
                return_value="offline fixture",
            ):
                checker.run()

        self.assertEqual(reporter.failures, 0)
        self.assertTrue(state.requests)
        self.assertEqual({method for method, _ in state.requests}, {"GET"})
        self.assertIn("/api/db/discover", {path for _, path in state.requests})
        self.assertNotIn("/api/db/research/query", {path for _, path in state.requests})

    def test_switch_model_is_the_only_write_when_explicitly_enabled(self) -> None:
        state = FixtureState()
        with FixtureServer(state) as server:
            checker, reporter, _ = checker_for(
                server, state, switch_model="model-b"
            )
            with mock.patch.object(
                NODE.NodeAcceptance,
                "check_network_ports",
                return_value="offline fixture",
            ):
                checker.run()

        self.assertEqual(reporter.failures, 0)
        writes = [request for request in state.requests if request[0] != "GET"]
        self.assertEqual(
            writes,
            [("PUT", "/api/integrations/ollama/credentials")],
        )
        self.assertEqual(state.model, "model-b")

    def test_database_leak_fails_without_printing_secret(self) -> None:
        state = FixtureState(leak_db=True)
        with FixtureServer(state) as server:
            checker, reporter, stream = checker_for(server, state)
            checker.run_check("database-read-only-schema", checker.check_database)

        self.assertEqual(reporter.failures, 1)
        output = stream.getvalue()
        self.assertIn("credential-like data", output)
        self.assertNotIn("fixture-secret", output)
        self.assertNotIn("mongodb://", output)

    def test_cold_configured_model_fails_by_default(self) -> None:
        state = FixtureState(cold_model=True)
        with FixtureServer(state) as server:
            checker, _, _ = checker_for(server, state)
            with self.assertRaisesRegex(NODE.AcceptanceError, "loading is unproven"):
                checker.check_ollama_status()

            checker.options.allow_cold_model = True
            self.assertIn("not resident", checker.check_ollama_status())

    def test_starting_health_component_is_not_accepted(self) -> None:
        state = FixtureState(component_status="starting")
        with FixtureServer(state) as server:
            checker, _, _ = checker_for(server, state)
            with self.assertRaisesRegex(NODE.AcceptanceError, "not ready"):
                checker.check_health()

    def test_plural_and_camel_case_secret_fields_are_detected(self) -> None:
        payload = {
            "gateway": {"paired_tokens": ["fixture-opaque-value"]},
            "reliability": {
                "fallback_api_keys": {"provider_a": "fixture-opaque-value"}
            },
            "integration": {"apiKey": "fixture-opaque-value"},
        }
        classes = NODE.credential_classes(json.dumps(payload), payload)
        self.assertIn("unmasked sensitive field", classes)

        masked = {
            "gateway": {"paired_tokens": [NODE.MASKED_SECRET]},
            "reliability": {
                "fallback_api_keys": {"provider_a": NODE.MASKED_SECRET}
            },
            "integration": {"apiKey": "••••••••", "max_tokens": 8192},
        }
        self.assertEqual(NODE.credential_classes(json.dumps(masked), masked), set())

    def test_managed_port_parser_supports_ranges_and_rejects_bad_input(self) -> None:
        self.assertEqual(NODE.parse_ports("8501-8503,8599"), (8501, 8502, 8503, 8599))
        expected = NODE.parse_ports(NODE.DEFAULT_MANAGED_PORTS)
        self.assertEqual(expected, tuple(range(8501, 8600)))
        self.assertEqual(len(expected), 99)
        with self.assertRaises(NODE.AcceptanceError):
            NODE.parse_ports("8599-8501")
        with self.assertRaises(NODE.AcceptanceError):
            NODE.parse_ports("8501,,8502")

    def test_required_remote_managed_listener_must_be_open(self) -> None:
        target = NODE.Target("lan", "http", "192.168.1.20", 42617)
        options = NODE.Options(
            managed_ports=(8501, 8502),
            required_managed_ports=(8502,),
        )
        checker = NODE.NodeAcceptance(
            target, options, NODE.Reporter(io.StringIO()), None
        )

        def fake_tcp_open(host: str, port: int, timeout: float) -> bool:
            del host, timeout
            return port in {42617, 5000, 8501}

        with mock.patch.object(NODE, "tcp_open", side_effect=fake_tcp_open):
            with self.assertRaisesRegex(NODE.AcceptanceError, "required managed"):
                checker.check_network_ports()

    def test_target_parser_rejects_embedded_credentials(self) -> None:
        with self.assertRaises(NODE.AcceptanceError):
            NODE.parse_target(
                "http://reader:fixture-secret@127.0.0.1",
                "lan",
                42617,
            )
        with self.assertRaises(NODE.AcceptanceError):
            NODE.parse_target("http://127.0.0.1/private", "lan", 42617)

    def test_plaintext_token_is_refused_off_loopback_without_opt_in(self) -> None:
        local = NODE.Target("local", "http", "127.0.0.1", 42617)
        lan = NODE.Target("lan", "http", "192.168.1.20", 42617)
        secure = NODE.Target("lan", "https", "node.internal", 42617)
        NODE.validate_token_transport([local], "fixture-token", False)
        NODE.validate_token_transport([secure], "fixture-token", False)
        NODE.validate_token_transport([lan], "fixture-token", True)
        with self.assertRaisesRegex(NODE.AcceptanceError, "off-loopback HTTP"):
            NODE.validate_token_transport([lan], "fixture-token", False)

    def test_docker_port_map_parser_ignores_non_tcp_and_invalid_bindings(self) -> None:
        raw = json.dumps(
            {
                "42617/tcp": [{"HostIp": "127.0.0.1", "HostPort": "42617"}],
                "8501/tcp": [{"HostIp": "0.0.0.0", "HostPort": "8501"}],
                "8502/udp": [{"HostIp": "0.0.0.0", "HostPort": "8502"}],
                "8503/tcp": [{"HostIp": "0.0.0.0", "HostPort": "invalid"}],
            }
        )
        self.assertEqual(
            NODE.published_port_bindings(raw),
            {
                42617: [("127.0.0.1", 42617)],
                8501: [("0.0.0.0", 8501)],
                8503: [],
            },
        )

    def test_docker_check_accepts_all_99_default_managed_bindings(self) -> None:
        target = NODE.Target("local", "http", "127.0.0.1", 42617)
        options = NODE.Options()
        reporter = NODE.Reporter(io.StringIO())
        checker = NODE.NodeAcceptance(target, options, reporter, None)
        checker.status = {"gateway_port": 42617}

        ports: dict[str, list[dict[str, str]]] = {
            "42617/tcp": [{"HostIp": "127.0.0.1", "HostPort": "42617"}],
            "11434/tcp": [{"HostIp": "127.0.0.1", "HostPort": "11434"}],
            "5000/tcp": [{"HostIp": "127.0.0.1", "HostPort": "5000"}],
        }
        ports.update(
            {
                f"{port}/tcp": [
                    {"HostIp": "127.0.0.1", "HostPort": str(port)}
                ]
                for port in range(8501, 8600)
            }
        )
        completed = NODE.subprocess.CompletedProcess(
            args=["docker"], returncode=0, stdout=json.dumps(ports), stderr=""
        )
        with (
            mock.patch.object(NODE.shutil, "which", return_value="/usr/bin/docker"),
            mock.patch.object(NODE.subprocess, "run", return_value=completed),
        ):
            detail = checker.check_docker_ports()
        self.assertIn("all 102 expected bindings", detail)


if __name__ == "__main__":
    unittest.main()
