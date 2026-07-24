#!/usr/bin/env python3
"""Read-only deployment acceptance for one or more LlamaFarm nodes.

The default path sends only HTTP GET requests plus TCP connection probes.  It
never invokes database discovery/query operations, cron run operations, or any
other POST/PUT/DELETE route.  A GET-only method probe confirms that the
POST-only discovery route exists without starting its network sweep.  The
selected database schema endpoint performs read-only inspection; its MongoDB
adapter lists collections and samples one document per collection to infer
columns.  Run discovery/autoconnection before acceptance: this script verifies
its saved, masked result without persisting a connection.

Examples:

  scripts/docker/node-acceptance.py --db-connection research
  scripts/docker/node-acceptance.py \
    --lan-host 192.168.1.20 --tail-host node.tailnet.example.ts.net

Model switching is the sole opt-in mutation:

  scripts/docker/node-acceptance.py --switch-model qwen3.5:9b

Authentication is read from LLAMAFARM_GATEWAY_TOKEN by default, or from a file
selected with --token-file.  Tokens and response bodies are never printed.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import ipaddress
import json
import os
import re
import shutil
import socket
import ssl
import subprocess
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, BinaryIO, Callable, Iterable, TextIO


DEFAULT_GATEWAY_PORT = 42617
DEFAULT_STOCK_PORT = 5000
DEFAULT_OLLAMA_PORT = 11434
DEFAULT_MANAGED_PORTS = "8501-8599"
MASKED_SECRET = "***MASKED***"
MAX_RESPONSE_BYTES = 16 * 1024 * 1024
TAILSCALE_V4 = ipaddress.ip_network("100.64.0.0/10")
TAILSCALE_V6 = ipaddress.ip_network("fd7a:115c:a1e0::/48")
SOUL_PATTERN = re.compile(r"(?i)\bsoul(?:\.md)?\b")
URI_USERINFO_PATTERN = re.compile(
    r"(?i)\b(?:mongodb(?:\+srv)?|postgres(?:ql)?|mysql|mariadb|redis)"
    r"://[^/\s:@]+:[^@\s/]+@"
)
GENERIC_USERINFO_PATTERN = re.compile(
    r"(?i)\b[a-z][a-z0-9+.-]{1,20}://[^/\s:@]+(?::[^@\s/]*)?@"
)
PRIVATE_KEY_PATTERN = re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")
TOKEN_PATTERNS = (
    re.compile(r"\b(?:gh[oprsu]_[A-Za-z0-9]{16,}|github_pat_[A-Za-z0-9_]{20,})\b"),
    re.compile(r"\bsk-[A-Za-z0-9_-]{16,}\b"),
    re.compile(r"\bxox[baprs]-[A-Za-z0-9-]{16,}\b"),
    re.compile(r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"),
)
SENSITIVE_KEYS = {
    "api_key",
    "api_keys",
    "access_token",
    "refresh_token",
    "auth_token",
    "bearer_token",
    "bot_token",
    "client_secret",
    "db_url",
    "encrypt_key",
    "federation_token",
    "password",
    "paired_tokens",
    "passwd",
    "private_key",
    "pwd",
    "secret",
    "server_password",
    "signing_secret",
    "token",
    "uri",
    "verification_token",
    "webhook_secret",
}
EXPECTED_DEVSECOPS_TOOLS = {
    "cron_list",
    "cron_runs",
    "db_schema",
    "docker",
    "shell",
}


class AcceptanceError(RuntimeError):
    """A deployment condition required for acceptance was not observed."""


class NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Prevent credentials from following a redirect to another origin."""

    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: BinaryIO,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> None:
        del req, fp, code, msg, headers, newurl
        return None


@dataclass(frozen=True)
class Target:
    kind: str
    scheme: str
    host: str
    gateway_port: int

    @property
    def label(self) -> str:
        return f"{self.kind}:{format_host(self.host)}:{self.gateway_port}"

    @property
    def origin(self) -> str:
        return f"{self.scheme}://{format_host(self.host)}:{self.gateway_port}"


@dataclass(frozen=True)
class HttpResponse:
    status: int
    headers: dict[str, str]
    body: bytes


@dataclass(frozen=True)
class JsonDocument:
    value: Any
    raw: str


@dataclass
class Options:
    stock_port: int = DEFAULT_STOCK_PORT
    ollama_port: int = DEFAULT_OLLAMA_PORT
    managed_ports: tuple[int, ...] = field(
        default_factory=lambda: tuple(range(8501, 8600))
    )
    timeout: float = 4.0
    db_connection: str | None = None
    container: str = "LlamaFarm"
    skip_docker_port_map: bool = False
    allow_cold_model: bool = False
    required_managed_ports: tuple[int, ...] = ()
    workspace_directory_limit: int = 2000
    switch_model: str | None = None


@dataclass(frozen=True)
class Result:
    target: str
    name: str
    status: str
    detail: str


class Reporter:
    def __init__(self, stream: TextIO = sys.stdout) -> None:
        self.stream = stream
        self.results: list[Result] = []

    def _record(self, target: str, name: str, status: str, detail: str) -> None:
        result = Result(target=target, name=name, status=status, detail=detail)
        self.results.append(result)
        print(f"{status} [{target}] {name}: {detail}", file=self.stream, flush=True)

    def passed(self, target: str, name: str, detail: str) -> None:
        self._record(target, name, "PASS", detail)

    def failed(self, target: str, name: str, detail: str) -> None:
        self._record(target, name, "FAIL", detail)

    def warning(self, target: str, name: str, detail: str) -> None:
        self._record(target, name, "WARN", detail)

    @property
    def failures(self) -> int:
        return sum(result.status == "FAIL" for result in self.results)

    def summary(self) -> None:
        passed = sum(result.status == "PASS" for result in self.results)
        warned = sum(result.status == "WARN" for result in self.results)
        print(
            f"SUMMARY pass={passed} fail={self.failures} warn={warned}",
            file=self.stream,
            flush=True,
        )


class SafeHttpClient:
    def __init__(self, target: Target, token: str | None, timeout: float) -> None:
        self.target = target
        self.token = token
        self.timeout = timeout
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            NoRedirectHandler(),
            urllib.request.HTTPSHandler(context=ssl.create_default_context()),
        )

    def request(
        self,
        path: str,
        *,
        method: str = "GET",
        payload: Any | None = None,
        accepted: Iterable[int] = (200,),
    ) -> HttpResponse:
        if not path.startswith("/"):
            raise AcceptanceError("internal request path is not absolute")
        accepted_statuses = frozenset(accepted)
        body = None
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if payload is not None:
            body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"

        request = urllib.request.Request(
            f"{self.target.origin}{path}",
            data=body,
            headers=headers,
            method=method,
        )
        try:
            response = self.opener.open(request, timeout=self.timeout)
        except urllib.error.HTTPError as error:
            response = error
        except (urllib.error.URLError, TimeoutError, OSError):
            raise AcceptanceError(f"{method} {safe_path(path)} was unreachable") from None

        try:
            raw = read_bounded(response)
            status = int(response.status)
            response_headers = {key.lower(): value for key, value in response.headers.items()}
        finally:
            response.close()

        if status not in accepted_statuses:
            raise AcceptanceError(
                f"{method} {safe_path(path)} returned HTTP {status}"
            )
        return HttpResponse(status=status, headers=response_headers, body=raw)

    def json(
        self,
        path: str,
        *,
        method: str = "GET",
        payload: Any | None = None,
        accepted: Iterable[int] = (200,),
    ) -> JsonDocument:
        response = self.request(
            path, method=method, payload=payload, accepted=accepted
        )
        try:
            raw = response.body.decode("utf-8")
            parsed = json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise AcceptanceError(
                f"{method} {safe_path(path)} did not return valid JSON"
            ) from None
        return JsonDocument(value=parsed, raw=raw)


def read_bounded(response: BinaryIO) -> bytes:
    raw = response.read(MAX_RESPONSE_BYTES + 1)
    if len(raw) > MAX_RESPONSE_BYTES:
        raise AcceptanceError("response exceeded the safe size limit")
    return raw


def safe_path(path: str) -> str:
    parsed = urllib.parse.urlsplit(path)
    return re.sub(
        r"^/api/db/[^/]+/schema$",
        "/api/db/<connection>/schema",
        parsed.path,
    )


def format_host(host: str) -> str:
    return f"[{host}]" if ":" in host else host


def parse_target(raw: str, kind: str, default_port: int) -> Target:
    candidate = raw.strip()
    if not candidate:
        raise AcceptanceError("target host is empty")
    if "://" not in candidate:
        candidate = f"http://{candidate}"
    parsed = urllib.parse.urlsplit(candidate)
    if parsed.scheme not in {"http", "https"}:
        raise AcceptanceError("target scheme must be http or https")
    if parsed.username is not None or parsed.password is not None:
        raise AcceptanceError("credentials are not allowed in target URLs")
    if not parsed.hostname:
        raise AcceptanceError("target host is missing")
    if parsed.path not in {"", "/"} or parsed.query or parsed.fragment:
        raise AcceptanceError("target URLs may not include a path, query, or fragment")
    try:
        port = parsed.port or default_port
    except ValueError:
        raise AcceptanceError("target port is invalid") from None
    if not 1 <= port <= 65535:
        raise AcceptanceError("target port is outside 1-65535")
    return Target(
        kind=kind,
        scheme=parsed.scheme,
        host=parsed.hostname,
        gateway_port=port,
    )


def parse_ports(raw: str) -> tuple[int, ...]:
    ports: set[int] = set()
    for piece in raw.split(","):
        token = piece.strip()
        if not token:
            raise AcceptanceError("managed port range contains an empty segment")
        if "-" in token:
            parts = token.split("-")
            if len(parts) != 2 or not all(part.isdigit() for part in parts):
                raise AcceptanceError("managed ports must use N or N-M")
            start, end = (int(part) for part in parts)
            if start > end:
                raise AcceptanceError("managed port range start exceeds its end")
            ports.update(range(start, end + 1))
        elif token.isdigit():
            ports.add(int(token))
        else:
            raise AcceptanceError("managed ports must use N or N-M")
    if not ports or len(ports) > 2048:
        raise AcceptanceError("managed port set must contain 1-2048 ports")
    if min(ports) < 1 or max(ports) > 65535:
        raise AcceptanceError("managed ports are outside 1-65535")
    return tuple(sorted(ports))


def is_local_host(host: str) -> bool:
    lowered = host.rstrip(".").lower()
    if lowered == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def is_tailscale_address(address: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    return address in (TAILSCALE_V4 if address.version == 4 else TAILSCALE_V6)


def resolve_target_addresses(target: Target) -> list[ipaddress.IPv4Address | ipaddress.IPv6Address]:
    try:
        values = socket.getaddrinfo(
            target.host, target.gateway_port, type=socket.SOCK_STREAM
        )
    except socket.gaierror:
        raise AcceptanceError("target hostname did not resolve") from None
    addresses: list[ipaddress.IPv4Address | ipaddress.IPv6Address] = []
    for value in values:
        raw_address = value[4][0].split("%", 1)[0]
        address = ipaddress.ip_address(raw_address)
        if address not in addresses:
            addresses.append(address)
    if not addresses:
        raise AcceptanceError("target hostname resolved to no addresses")
    return addresses


def validate_target_scope(target: Target) -> None:
    addresses = resolve_target_addresses(target)
    if target.kind == "local":
        if not all(address.is_loopback for address in addresses):
            raise AcceptanceError("localhost target resolved outside loopback")
        return
    if target.kind == "tail":
        if not all(is_tailscale_address(address) for address in addresses):
            raise AcceptanceError("Tail target resolved outside Tailscale address space")
        return
    if target.kind == "lan":
        if not all(
            address.is_private or address.is_link_local or address.is_loopback
            for address in addresses
        ):
            raise AcceptanceError("LAN target resolved to a public address")
        return
    raise AcceptanceError("unknown target kind")


def tcp_open(host: str, port: int, timeout: float) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def canonical_model(model: str) -> str:
    normalized = model.strip()
    leaf = normalized.rsplit("/", 1)[-1]
    return normalized if ":" in leaf else f"{normalized}:latest"


def model_names(payload: Any) -> list[str]:
    if not isinstance(payload, dict) or not isinstance(payload.get("models"), list):
        raise AcceptanceError("Ollama response is missing models[]")
    names: list[str] = []
    for entry in payload["models"]:
        if not isinstance(entry, dict):
            raise AcceptanceError("Ollama models[] contains an invalid entry")
        name = entry.get("name") or entry.get("model")
        if not isinstance(name, str) or not name.strip():
            raise AcceptanceError("Ollama model entry is missing its name")
        names.append(name.strip())
    return sorted(set(names))


def is_masked_value(value: Any) -> bool:
    if value is None or value == "":
        return True
    if isinstance(value, list):
        return all(is_masked_value(item) for item in value)
    if isinstance(value, dict):
        return all(is_masked_value(item) for item in value.values())
    if not isinstance(value, str):
        return False
    stripped = value.strip()
    if stripped in {MASKED_SECRET, "<redacted>", "[redacted]"}:
        return True
    return bool(stripped) and all(char in {"*", "•", "x", "X"} for char in stripped)


def sensitive_key(key: str) -> bool:
    snake_case = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "_", key)
    normalized = snake_case.lower().replace("-", "_")
    if normalized.endswith(("_env", "_file", "_path")):
        return False
    token_collection = normalized.endswith("_tokens") and normalized.startswith(
        (
            "access_",
            "auth_",
            "bearer_",
            "bot_",
            "federation_",
            "paired_",
            "refresh_",
            "session_",
            "verification_",
            "webhook_",
        )
    )
    return normalized in SENSITIVE_KEYS or token_collection or normalized.endswith(
        (
            "_api_keys",
            "_password",
            "_passwords",
            "_private_key",
            "_private_keys",
            "_secret",
            "_secrets",
            "_token",
        )
    )


def credential_classes(raw: str, payload: Any | None = None) -> set[str]:
    classes: set[str] = set()
    if URI_USERINFO_PATTERN.search(raw) or GENERIC_USERINFO_PATTERN.search(raw):
        classes.add("credential-bearing URI")
    if PRIVATE_KEY_PATTERN.search(raw):
        classes.add("private key material")
    if any(pattern.search(raw) for pattern in TOKEN_PATTERNS):
        classes.add("token-shaped value")

    def walk(value: Any) -> None:
        if isinstance(value, dict):
            for key, child in value.items():
                if sensitive_key(str(key)) and not is_masked_value(child):
                    classes.add("unmasked sensitive field")
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)

    if payload is not None:
        walk(payload)
    return classes


def require_no_credentials(document: JsonDocument, surface: str) -> None:
    classes = credential_classes(document.raw, document.value)
    if classes:
        raise AcceptanceError(
            f"{surface} exposed credential-like data ({', '.join(sorted(classes))})"
        )


def require_dict(value: Any, label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise AcceptanceError(f"{label} is not an object")
    return value


def require_list(value: Any, label: str) -> list[Any]:
    if not isinstance(value, list):
        raise AcceptanceError(f"{label} is not an array")
    return value


def workspace_has_soul_name(name: Any) -> bool:
    return isinstance(name, str) and name.casefold() in {"soul", "soul.md"}


def asset_paths(index_body: str, origin: str) -> list[str]:
    found: set[str] = set()
    for match in re.finditer(r"""(?:src|href)=["']([^"'#]+)["']""", index_body):
        absolute = urllib.parse.urljoin(f"{origin}/", match.group(1))
        parsed = urllib.parse.urlsplit(absolute)
        if f"{parsed.scheme}://{parsed.netloc}" != origin:
            continue
        if parsed.path == "/":
            continue
        path = parsed.path
        if parsed.query:
            path += f"?{parsed.query}"
        found.add(path)
    if len(found) > 128:
        raise AcceptanceError("static index references too many assets")
    return sorted(found)


def published_port_bindings(raw: str) -> dict[int, list[tuple[str, int]]]:
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError:
        raise AcceptanceError("Docker returned an invalid port map") from None
    if not isinstance(parsed, dict):
        raise AcceptanceError("Docker port map is not an object")
    result: dict[int, list[tuple[str, int]]] = {}
    for key, bindings in parsed.items():
        if not isinstance(key, str) or not key.endswith("/tcp"):
            continue
        try:
            container_port = int(key.removesuffix("/tcp"))
        except ValueError:
            continue
        if not isinstance(bindings, list):
            result[container_port] = []
            continue
        normalized: list[tuple[str, int]] = []
        for binding in bindings:
            if not isinstance(binding, dict):
                continue
            host_ip = binding.get("HostIp")
            host_port = binding.get("HostPort")
            if not isinstance(host_ip, str) or not isinstance(host_port, str):
                continue
            try:
                normalized.append((host_ip, int(host_port)))
            except ValueError:
                continue
        result[container_port] = normalized
    return result


class NodeAcceptance:
    def __init__(
        self,
        target: Target,
        options: Options,
        reporter: Reporter,
        token: str | None,
    ) -> None:
        self.target = target
        self.options = options
        self.reporter = reporter
        self.client = SafeHttpClient(target, token, options.timeout)
        self.public_client = SafeHttpClient(target, None, options.timeout)
        self.status: dict[str, Any] | None = None
        self.settings: dict[str, Any] | None = None
        self.config: dict[str, Any] | None = None
        self.tools: list[Any] | None = None

    def run_check(self, name: str, check: Callable[[], str]) -> None:
        try:
            detail = check()
        except AcceptanceError as error:
            self.reporter.failed(self.target.label, name, str(error))
        except Exception as error:  # Keep unexpected errors secret-safe too.
            self.reporter.failed(
                self.target.label,
                name,
                f"unexpected internal {type(error).__name__}",
            )
        else:
            self.reporter.passed(self.target.label, name, detail)

    def run(self) -> None:
        self.run_check("health", self.check_health)
        self.run_check("network-ports", self.check_network_ports)
        if self.target.kind != "local":
            self.reporter.warning(
                self.target.label,
                "managed-port-publication",
                "remote TCP scans show active listeners only; Docker publication "
                "is verified on localhost",
            )
        self.run_check("ollama-status", self.check_ollama_status)
        if self.target.kind == "local":
            self.run_check("ollama-tags-and-ps", self.check_raw_ollama)
        self.run_check("connections-and-tailscale", self.check_connections)
        self.run_check("database-read-only-schema", self.check_database)
        self.run_check("devsecops-agent", self.check_devsecops)
        self.run_check("soul-removal", self.check_soul_absence)
        self.run_check("cron-list-and-latest-history", self.check_cron)
        if (
            self.target.kind == "local"
            and not self.options.skip_docker_port_map
        ):
            self.run_check("docker-published-ports", self.check_docker_ports)
        if self.options.switch_model is not None:
            self.run_check("model-switch", self.switch_model)

    def check_health(self) -> str:
        public = self.public_client.json("/health")
        public_body = require_dict(public.value, "/health")
        if public_body.get("status") != "ok":
            raise AcceptanceError("public health status is not ok")
        runtime = require_dict(public_body.get("runtime"), "/health runtime")

        api = self.client.json("/api/health")
        require_no_credentials(api, "health API")
        api_body = require_dict(api.value, "/api/health")
        health = require_dict(api_body.get("health"), "/api/health health")
        components = require_dict(health.get("components"), "health components")
        if "gateway" not in components:
            raise AcceptanceError("health snapshot is missing the gateway component")
        unhealthy: list[str] = []
        for name, component in components.items():
            component_body = require_dict(component, "health component")
            status = str(component_body.get("status", "")).lower()
            if status != "ok":
                unhealthy.append(str(name))
        if unhealthy:
            raise AcceptanceError(
                f"{len(unhealthy)} health component(s) are not ready"
            )
        if runtime.get("pid") != health.get("pid"):
            raise AcceptanceError("public and authenticated health snapshots disagree")
        return f"public/API snapshots agree; {len(components)} component(s) checked"

    def check_network_ports(self) -> str:
        if not tcp_open(
            self.target.host, self.target.gateway_port, self.options.timeout
        ):
            raise AcceptanceError(
                f"gateway TCP port {self.target.gateway_port} is closed"
            )
        if not tcp_open(self.target.host, self.options.stock_port, self.options.timeout):
            raise AcceptanceError(
                f"stock app TCP port {self.options.stock_port} is closed"
            )

        probe_timeout = min(self.options.timeout, 0.5)
        with concurrent.futures.ThreadPoolExecutor(max_workers=32) as executor:
            outcomes = list(
                executor.map(
                    lambda port: tcp_open(self.target.host, port, probe_timeout),
                    self.options.managed_ports,
                )
            )
        active = sum(outcomes)
        active_by_port = dict(zip(self.options.managed_ports, outcomes, strict=True))
        required_closed = sum(
            not active_by_port[port] for port in self.options.required_managed_ports
        )
        if required_closed:
            raise AcceptanceError(
                f"{required_closed} required managed listener(s) are closed"
            )
        first = self.options.managed_ports[0]
        last = self.options.managed_ports[-1]
        return (
            f"gateway {self.target.gateway_port} and stock {self.options.stock_port} "
            f"reachable; scanned managed {first}-{last} ({active} active)"
        )

    def _load_status(self) -> dict[str, Any]:
        if self.status is None:
            document = self.client.json("/api/status")
            require_no_credentials(document, "status API")
            self.status = require_dict(document.value, "/api/status")
        return self.status

    def _load_settings(self) -> dict[str, Any]:
        if self.settings is None:
            document = self.client.json("/api/integrations/settings")
            require_no_credentials(document, "integration settings API")
            self.settings = require_dict(
                document.value, "/api/integrations/settings"
            )
        return self.settings

    def check_ollama_status(self) -> str:
        status = self._load_status()
        ollama = require_dict(status.get("ollama"), "status ollama")
        if ollama.get("reachable") is not True:
            raise AcceptanceError("configured Ollama endpoint is not reachable")
        configured = ollama.get("configured_model")
        installed = require_list(ollama.get("installed_models"), "installed models")
        loaded = require_list(ollama.get("loaded_models"), "loaded models")
        if not isinstance(configured, str) or not configured.strip():
            raise AcceptanceError("configured Ollama model is empty")
        if not installed or not all(
            isinstance(model, str) and model.strip() for model in installed
        ):
            raise AcceptanceError("Ollama tags reported no installed models")
        canonical_installed = {canonical_model(model) for model in installed}
        if canonical_model(configured) not in canonical_installed:
            raise AcceptanceError("configured model is absent from Ollama tags")
        if not all(isinstance(model, str) and model.strip() for model in loaded):
            raise AcceptanceError("Ollama loaded-model status is invalid")
        active_expected = canonical_model(configured) in {
            canonical_model(model) for model in loaded
        }
        if ollama.get("active_model_loaded") is not active_expected:
            raise AcceptanceError("active-model residency flag disagrees with Ollama ps")
        if not self.options.allow_cold_model and not active_expected:
            raise AcceptanceError(
                "configured model is installed but not resident; model loading is unproven"
            )

        settings = self._load_settings()
        revision = settings.get("revision")
        if not isinstance(revision, str) or not re.fullmatch(r"[0-9a-f]{64}", revision):
            raise AcceptanceError("model settings revision is invalid")
        integrations = require_list(
            settings.get("integrations"), "integration settings entries"
        )
        integration = next(
            (
                entry
                for entry in integrations
                if isinstance(entry, dict) and entry.get("id") == "ollama"
            ),
            None,
        )
        integration = require_dict(integration, "Ollama integration")
        fields = require_list(integration.get("fields"), "Ollama settings fields")
        by_key = {
            field.get("key"): field
            for field in fields
            if isinstance(field, dict) and isinstance(field.get("key"), str)
        }
        model_field = require_dict(by_key.get("default_model"), "model switch field")
        options = require_list(model_field.get("options"), "model switch options")
        if canonical_model(configured) not in {
            canonical_model(model) for model in options if isinstance(model, str)
        }:
            raise AcceptanceError("configured model is absent from switch options")
        api_key = require_dict(by_key.get("api_key"), "Ollama API key field")
        if api_key.get("current_value") not in {None, ""}:
            raise AcceptanceError("Ollama API key was returned unmasked")
        if api_key.get("has_value") and not is_masked_value(api_key.get("masked_value")):
            raise AcceptanceError("Ollama API key mask is invalid")

        route_probe = self.client.request(
            "/api/integrations/ollama/credentials", accepted=(405,)
        )
        allow = route_probe.headers.get("allow", "").upper()
        if allow and "PUT" not in allow:
            raise AcceptanceError("model switch route does not advertise PUT")
        resident = "resident" if active_expected else "not resident"
        return (
            f"{len(installed)} installed, {len(loaded)} loaded; configured model "
            f"is {resident}; switch route probed with GET only"
        )

    def check_raw_ollama(self) -> str:
        raw_target = Target(
            kind="ollama-loopback",
            scheme="http",
            host=self.target.host,
            gateway_port=self.options.ollama_port,
        )
        raw_client = SafeHttpClient(raw_target, None, self.options.timeout)
        tags = raw_client.json("/api/tags")
        ps = raw_client.json("/api/ps")
        installed = model_names(tags.value)
        loaded = model_names(ps.value)
        if not installed:
            raise AcceptanceError("raw Ollama tags endpoint returned no models")

        status_ollama = require_dict(self._load_status().get("ollama"), "status ollama")
        status_installed = require_list(
            status_ollama.get("installed_models"), "status installed models"
        )
        status_loaded = require_list(
            status_ollama.get("loaded_models"), "status loaded models"
        )
        if {canonical_model(model) for model in installed} != {
            canonical_model(model) for model in status_installed
        }:
            raise AcceptanceError("gateway status disagrees with raw Ollama tags")
        if {canonical_model(model) for model in loaded} != {
            canonical_model(model) for model in status_loaded
        }:
            raise AcceptanceError("gateway status disagrees with raw Ollama ps")
        return (
            f"raw tags/status agree with gateway ({len(installed)} installed, "
            f"{len(loaded)} loaded)"
        )

    def check_connections(self) -> str:
        document = self.client.json("/api/connections")
        require_no_credentials(document, "connections API")
        body = require_dict(document.value, "/api/connections")
        for required in ("github", "ollama", "memory", "discord", "tailscale"):
            require_dict(body.get(required), f"connection {required}")
        tailscale = require_dict(body["tailscale"], "Tailscale connection")
        state = tailscale.get("status")
        if state not in {"up", "down", "unavailable"}:
            raise AcceptanceError("Tailscale status is invalid")
        if self.target.kind == "tail" and state != "up":
            raise AcceptanceError("Tail target does not report live Tailscale")

        if state == "up":
            raw_ipv4 = tailscale.get("ipv4")
            try:
                ipv4 = ipaddress.ip_address(str(raw_ipv4))
            except ValueError:
                raise AcceptanceError("live Tailscale status has no valid IPv4") from None
            if not isinstance(ipv4, ipaddress.IPv4Address) or not is_tailscale_address(ipv4):
                raise AcceptanceError("reported Tailscale IPv4 is outside tailnet space")
            warnings = tailscale.get("health_warnings")
            if not isinstance(warnings, int) or warnings < 0:
                raise AcceptanceError("Tailscale health warning count is invalid")

            advertised = Target(
                kind="advertised-tail",
                scheme="http",
                host=str(ipv4),
                gateway_port=self.target.gateway_port,
            )
            advertised_health = SafeHttpClient(
                advertised, None, self.options.timeout
            ).json("/health")
            advertised_body = require_dict(
                advertised_health.value, "advertised Tailscale health"
            )
            if advertised_body.get("status") != "ok":
                raise AcceptanceError("advertised Tailscale gateway is not healthy")

            if self.target.kind == "tail":
                dns_name = str(tailscale.get("dns_name") or "").rstrip(".").lower()
                target_host = self.target.host.rstrip(".").lower()
                if target_host not in {str(ipv4), dns_name} and not dns_name.startswith(
                    f"{target_host}."
                ):
                    raise AcceptanceError("Tail target disagrees with advertised identity")
            return f"Tailscale up and independently reachable; warnings={warnings}"
        return f"Tailscale truthfully reports {state}"

    def check_database(self) -> str:
        discovery_probe = self.client.request("/api/db/discover", accepted=(405,))
        allow = discovery_probe.headers.get("allow", "").upper()
        if allow and "POST" not in allow:
            raise AcceptanceError("database discovery route does not advertise POST")

        document = self.client.json("/api/db/connections")
        require_no_credentials(document, "database connection list")
        body = require_dict(document.value, "/api/db/connections")
        connections = require_list(body.get("connections"), "database connections")
        if not connections:
            raise AcceptanceError(
                "autoconnection has not produced a configured database connection"
            )

        normalized: list[dict[str, Any]] = []
        for connection in connections:
            item = require_dict(connection, "database connection")
            if item.get("uri") != MASKED_SECRET:
                raise AcceptanceError("database list contains an unmasked URI")
            if not isinstance(item.get("name"), str) or not item["name"].strip():
                raise AcceptanceError("database connection has no name")
            normalized.append(item)

        selected = None
        if self.options.db_connection is not None:
            selected = next(
                (
                    connection
                    for connection in normalized
                    if connection["name"] == self.options.db_connection
                ),
                None,
            )
            if selected is None:
                raise AcceptanceError("selected database connection was not found")
        else:
            selected = normalized[0]
        if selected.get("read_only") is not True:
            raise AcceptanceError("selected database connection is not read-only")

        encoded = urllib.parse.quote(selected["name"], safe="")
        schema_document = self.client.json(f"/api/db/{encoded}/schema")
        if URI_USERINFO_PATTERN.search(schema_document.raw):
            raise AcceptanceError("database schema response exposed a credential-bearing URI")
        schema = require_dict(schema_document.value, "database schema")
        driver = schema.get("driver")
        if driver != selected.get("driver"):
            raise AcceptanceError("database schema driver disagrees with connection list")
        tables = require_list(schema.get("tables"), "database schema tables")
        for table in tables:
            table_body = require_dict(table, "database schema table")
            require_list(table_body.get("columns"), "database schema columns")
            if table_body.get("kind") not in {"table", "view", "collection"}:
                raise AcceptanceError("database schema contains an invalid object kind")
        return (
            f"discovery route present; {len(normalized)} masked connection(s); "
            f"selected read-only schema returned {len(tables)} object(s)"
        )

    def _load_config(self) -> dict[str, Any]:
        if self.config is None:
            document = self.client.json("/api/config")
            body = require_dict(document.value, "/api/config")
            if body.get("format") != "toml" or not isinstance(body.get("content"), str):
                raise AcceptanceError("live config response is not TOML")
            try:
                parsed = tomllib.loads(body["content"])
            except tomllib.TOMLDecodeError:
                raise AcceptanceError("live masked config is invalid TOML") from None
            config_document = JsonDocument(value=parsed, raw=body["content"])
            require_no_credentials(config_document, "live config API")
            self.config = require_dict(parsed, "live config")
        return self.config

    def _load_tools(self) -> list[Any]:
        if self.tools is None:
            document = self.client.json("/api/tools")
            require_no_credentials(document, "tool registry")
            body = require_dict(document.value, "/api/tools")
            self.tools = require_list(body.get("tools"), "registered tools")
        return self.tools

    def check_devsecops(self) -> str:
        config = self._load_config()
        agents = require_dict(config.get("agents"), "configured agents")
        devsecops = require_dict(agents.get("devsecops"), "DevSecOps agent")
        routes = require_list(config.get("model_routes"), "model routes")
        if not any(
            isinstance(route, dict)
            and str(route.get("hint", "")).lower() == "devsecops"
            for route in routes
        ):
            raise AcceptanceError("DevSecOps model route is missing")
        tools = require_list(devsecops.get("allowed_tools"), "DevSecOps allowed tools")
        missing = EXPECTED_DEVSECOPS_TOOLS.difference(
            tool for tool in tools if isinstance(tool, str)
        )
        if missing:
            raise AcceptanceError(
                f"DevSecOps agent lacks {len(missing)} required tool(s)"
            )
        return "live masked config includes agent, model route, and deployment tools"

    def check_soul_absence(self) -> str:
        config = self._load_config()
        if SOUL_PATTERN.search(json.dumps(config, separators=(",", ":"))):
            raise AcceptanceError("live config API still exposes SOUL")

        surfaces = [
            ("/api/config/presets", "config presets"),
            ("/api/workspace-files/AGENTS.md", "workspace prompt"),
        ]
        for path, label in surfaces:
            document = self.client.json(path)
            require_no_credentials(document, label)
            if SOUL_PATTERN.search(document.raw):
                raise AcceptanceError(f"{label} still exposes SOUL")
        tools = self._load_tools()
        if SOUL_PATTERN.search(json.dumps(tools, separators=(",", ":"))):
            raise AcceptanceError("tool registry still exposes SOUL")

        rejection = self.client.request(
            "/api/workspace-files/SOUL.md", accepted=(404,)
        )
        if rejection.status != 404:
            raise AcceptanceError("SOUL prompt-editor route was not rejected")

        queued = [""]
        visited: set[str] = set()
        entries_checked = 0
        while queued:
            relative = queued.pop()
            if relative in visited:
                continue
            visited.add(relative)
            if len(visited) > self.options.workspace_directory_limit:
                raise AcceptanceError("workspace scan exceeded its directory limit")
            suffix = (
                f"?path={urllib.parse.quote(relative, safe='')}" if relative else ""
            )
            document = self.client.json(f"/api/workspace/browser{suffix}")
            body = require_dict(document.value, "workspace browser")
            entries = require_list(body.get("entries"), "workspace entries")
            for entry in entries:
                item = require_dict(entry, "workspace entry")
                name = item.get("name")
                path = item.get("path")
                if workspace_has_soul_name(name) or workspace_has_soul_name(path):
                    raise AcceptanceError("workspace browser still exposes SOUL.md")
                if not isinstance(path, str):
                    raise AcceptanceError("workspace entry has no relative path")
                if item.get("kind") == "directory":
                    queued.append(path)
                elif item.get("kind") != "file":
                    raise AcceptanceError("workspace entry has an invalid kind")
                entries_checked += 1

        index = self.public_client.request("/")
        try:
            index_text = index.body.decode("utf-8")
        except UnicodeDecodeError:
            raise AcceptanceError("dashboard index is not UTF-8") from None
        if SOUL_PATTERN.search(index_text):
            raise AcceptanceError("dashboard index still exposes SOUL")
        assets = asset_paths(index_text, self.target.origin)
        for path in assets:
            asset = self.public_client.request(path)
            text = asset.body.decode("utf-8", errors="replace")
            if SOUL_PATTERN.search(text):
                raise AcceptanceError("dashboard static asset still exposes SOUL")
        return (
            f"prompt/config/tool APIs, {entries_checked} workspace entries, "
            f"and {len(assets) + 1} static file(s) are clean"
        )

    def check_cron(self) -> str:
        document = self.client.json("/api/cron")
        require_no_credentials(document, "cron API")
        body = require_dict(document.value, "/api/cron")
        jobs = require_list(body.get("jobs"), "cron jobs")
        history_count = 0
        for job in jobs:
            item = require_dict(job, "cron job")
            for field_name in (
                "id",
                "schedule",
                "next_run",
                "last_run",
                "last_status",
                "last_output",
                "enabled",
            ):
                if field_name not in item:
                    raise AcceptanceError(
                        "cron list is missing latest-run history fields"
                    )
            schedule = require_dict(item["schedule"], "cron schedule")
            if schedule.get("kind") not in {"cron", "at", "every"}:
                raise AcceptanceError("cron schedule kind is invalid")
            if item["last_run"] is not None:
                history_count += 1
                if not isinstance(item["last_status"], str):
                    raise AcceptanceError("completed cron job has no last status")
                if item["last_output"] is not None and not isinstance(
                    item["last_output"], str
                ):
                    raise AcceptanceError("cron last output is invalid")
        registered_tools = {
            tool.get("name")
            for tool in self._load_tools()
            if isinstance(tool, dict) and isinstance(tool.get("name"), str)
        }
        if not {"cron_list", "cron_runs"}.issubset(registered_tools):
            raise AcceptanceError("cron list/history tools are not both registered")
        return (
            f"{len(jobs)} job(s) listed; latest-run fields cover "
            f"{history_count} job(s), and full-history tool is registered"
        )

    def check_docker_ports(self) -> str:
        docker = shutil.which("docker")
        if docker is None:
            raise AcceptanceError(
                "Docker CLI is unavailable (use --skip-docker-port-map only intentionally)"
            )
        try:
            result = subprocess.run(
                [
                    docker,
                    "container",
                    "inspect",
                    "--format",
                    "{{json .NetworkSettings.Ports}}",
                    self.options.container,
                ],
                check=False,
                capture_output=True,
                text=True,
                timeout=8,
            )
        except (OSError, subprocess.TimeoutExpired):
            raise AcceptanceError("Docker port inspection failed") from None
        if result.returncode != 0:
            raise AcceptanceError("LlamaFarm container could not be inspected")
        port_map = published_port_bindings(result.stdout.strip())

        status = self._load_status()
        internal_gateway = status.get("gateway_port")
        if not isinstance(internal_gateway, int):
            raise AcceptanceError("status gateway port is invalid")
        expected: dict[int, int] = {
            internal_gateway: self.target.gateway_port,
            DEFAULT_OLLAMA_PORT: self.options.ollama_port,
            DEFAULT_STOCK_PORT: self.options.stock_port,
        }
        expected.update({port: port for port in self.options.managed_ports})
        missing = 0
        wrong_host_port = 0
        for container_port, host_port in expected.items():
            bindings = port_map.get(container_port, [])
            if not bindings:
                missing += 1
                continue
            if not any(binding_port == host_port for _, binding_port in bindings):
                wrong_host_port += 1
        if missing or wrong_host_port:
            raise AcceptanceError(
                f"Docker port map has {missing} missing and "
                f"{wrong_host_port} mismatched binding(s)"
            )

        ollama_bindings = port_map[DEFAULT_OLLAMA_PORT]
        for host_ip, _ in ollama_bindings:
            try:
                address = ipaddress.ip_address(host_ip)
            except ValueError:
                raise AcceptanceError("raw Ollama binding address is invalid") from None
            if not address.is_loopback:
                raise AcceptanceError("raw Ollama port is exposed beyond loopback")
        return f"Docker publishes all {len(expected)} expected bindings"

    def switch_model(self) -> str:
        target_model = (self.options.switch_model or "").strip()
        if not target_model:
            raise AcceptanceError("switch model is empty")
        status = self._load_status()
        ollama = require_dict(status.get("ollama"), "status ollama")
        if ollama.get("model_environment_override") is not None:
            raise AcceptanceError("deployment model environment override blocks switching")
        installed = require_list(ollama.get("installed_models"), "installed models")
        if canonical_model(target_model) not in {
            canonical_model(model) for model in installed if isinstance(model, str)
        }:
            raise AcceptanceError("requested switch target is not installed")
        settings = self._load_settings()
        revision = settings.get("revision")
        document = self.client.json(
            "/api/integrations/ollama/credentials",
            method="PUT",
            payload={
                "revision": revision,
                "fields": {"default_model": target_model},
            },
        )
        require_no_credentials(document, "model switch response")
        response = require_dict(document.value, "model switch response")
        if response.get("status") != "ok":
            raise AcceptanceError("model switch did not return ok")

        self.status = None
        self.settings = None
        updated = self._load_status()
        updated_ollama = require_dict(updated.get("ollama"), "updated Ollama status")
        if canonical_model(str(updated_ollama.get("configured_model", ""))) != canonical_model(
            target_model
        ):
            raise AcceptanceError("model switch was not reflected in live status")
        return (
            "explicitly requested model selection persisted and is live; "
            "the switch endpoint does not load weights"
        )


def load_token(args: argparse.Namespace) -> str | None:
    if args.token_file and args.token_env != "LLAMAFARM_GATEWAY_TOKEN":
        raise AcceptanceError("--token-file and a custom --token-env are mutually exclusive")
    if args.token_file:
        try:
            token = Path(args.token_file).read_text(encoding="utf-8").strip()
        except OSError:
            raise AcceptanceError("gateway token file could not be read") from None
    else:
        token = os.environ.get(args.token_env, "").strip()
    return token or None


def validate_token_transport(
    targets: Iterable[Target],
    token: str | None,
    allow_insecure_token: bool,
) -> None:
    if not token or allow_insecure_token:
        return
    if any(
        target.scheme != "https" and not is_local_host(target.host)
        for target in targets
    ):
        raise AcceptanceError(
            "refusing to send a bearer token over off-loopback HTTP; use HTTPS "
            "or explicitly pass --allow-insecure-token"
        )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description=(
            "Read-only LlamaFarm node acceptance. Defaults to localhost and "
            "never mutates a DB, cron job, workspace, or config."
        )
    )
    parser.add_argument(
        "--lan-host",
        action="append",
        default=[],
        metavar="HOST",
        help="also check a private LAN host (repeatable)",
    )
    parser.add_argument(
        "--tail-host",
        action="append",
        default=[],
        metavar="HOST",
        help="also check a Tailscale IP or MagicDNS host (repeatable)",
    )
    parser.add_argument(
        "--no-localhost",
        action="store_true",
        help="check only explicitly supplied LAN/Tail hosts",
    )
    parser.add_argument(
        "--gateway-port", type=int, default=DEFAULT_GATEWAY_PORT
    )
    parser.add_argument("--stock-port", type=int, default=DEFAULT_STOCK_PORT)
    parser.add_argument("--ollama-port", type=int, default=DEFAULT_OLLAMA_PORT)
    parser.add_argument(
        "--managed-ports",
        default=os.environ.get(
            "LLAMAFARM_ACCEPTANCE_MANAGED_PORTS", DEFAULT_MANAGED_PORTS
        ),
        metavar="RANGE",
        help=f"managed port set (default: {DEFAULT_MANAGED_PORTS})",
    )
    parser.add_argument(
        "--db-connection",
        help="configured read-only DB connection to inspect (default: first)",
    )
    parser.add_argument("--timeout", type=float, default=4.0)
    parser.add_argument("--container", default="LlamaFarm")
    parser.add_argument(
        "--skip-docker-port-map",
        action="store_true",
        help="skip authoritative Docker publication check on localhost",
    )
    parser.add_argument(
        "--allow-cold-model",
        action="store_true",
        help="allow an installed configured model that is not currently resident",
    )
    parser.add_argument(
        "--require-managed-port",
        action="append",
        default=[],
        type=int,
        metavar="PORT",
        help="require this managed listener to be reachable (repeatable)",
    )
    parser.add_argument(
        "--workspace-directory-limit", type=int, default=2000
    )
    parser.add_argument(
        "--switch-model",
        metavar="MODEL",
        help=(
            "OPT-IN MUTATION: persist this installed model through the live "
            "switch endpoint; allowed for one target only"
        ),
    )
    parser.add_argument(
        "--token-env",
        default="LLAMAFARM_GATEWAY_TOKEN",
        metavar="NAME",
        help="environment variable containing the gateway token",
    )
    parser.add_argument(
        "--token-file",
        metavar="PATH",
        help="read the gateway token from this file instead of the environment",
    )
    parser.add_argument(
        "--allow-insecure-token",
        action="store_true",
        help="allow a bearer token over off-loopback HTTP (unsafe; explicit opt-in)",
    )
    return parser


def targets_from_args(args: argparse.Namespace) -> list[Target]:
    if not 1 <= args.gateway_port <= 65535:
        raise AcceptanceError("gateway port is outside 1-65535")
    targets: list[Target] = []
    if not args.no_localhost:
        targets.append(
            parse_target("127.0.0.1", "local", args.gateway_port)
        )
    targets.extend(
        parse_target(host, "lan", args.gateway_port) for host in args.lan_host
    )
    targets.extend(
        parse_target(host, "tail", args.gateway_port) for host in args.tail_host
    )
    deduplicated: list[Target] = []
    seen: set[tuple[str, str, int]] = set()
    for target in targets:
        identity = (target.scheme, target.host.lower(), target.gateway_port)
        if identity not in seen:
            seen.add(identity)
            deduplicated.append(target)
    if not deduplicated:
        raise AcceptanceError("at least one localhost, LAN, or Tail target is required")
    return deduplicated


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        managed_ports = parse_ports(args.managed_ports)
        if not 1 <= args.stock_port <= 65535 or not 1 <= args.ollama_port <= 65535:
            raise AcceptanceError("stock/Ollama port is outside 1-65535")
        if args.timeout <= 0 or args.timeout > 60:
            raise AcceptanceError("timeout must be greater than 0 and at most 60 seconds")
        if not 1 <= args.workspace_directory_limit <= 100_000:
            raise AcceptanceError("workspace directory limit is outside 1-100000")
        targets = targets_from_args(args)
        if args.switch_model is not None and len(targets) != 1:
            raise AcceptanceError("--switch-model requires exactly one target")
        required_managed_ports = tuple(sorted(set(args.require_managed_port)))
        if not set(required_managed_ports).issubset(managed_ports):
            raise AcceptanceError(
                "every required managed listener must be inside --managed-ports"
            )
        for target in targets:
            validate_target_scope(target)
        token = load_token(args)
        validate_token_transport(targets, token, args.allow_insecure_token)
    except AcceptanceError as error:
        parser.error(str(error))

    options = Options(
        stock_port=args.stock_port,
        ollama_port=args.ollama_port,
        managed_ports=managed_ports,
        timeout=args.timeout,
        db_connection=args.db_connection,
        container=args.container,
        skip_docker_port_map=args.skip_docker_port_map,
        allow_cold_model=args.allow_cold_model,
        required_managed_ports=required_managed_ports,
        workspace_directory_limit=args.workspace_directory_limit,
        switch_model=args.switch_model,
    )
    reporter = Reporter()
    if args.switch_model is None:
        print(
            "MODE read-only: GET/TCP checks only; no DB discovery/query or "
            "cron/config mutation",
            flush=True,
        )
    else:
        print(
            "MODE opt-in model switch: one model-selection PUT is enabled; "
            "DB/cron/workspace remain read-only",
            flush=True,
        )
    print(
        "DB schema note: MongoDB inspection lists collections and samples one "
        "document per collection; it never writes",
        flush=True,
    )
    if options.skip_docker_port_map and any(
        target.kind == "local" for target in targets
    ):
        reporter.warning(
            "local",
            "docker-published-ports",
            "authoritative local publication check explicitly skipped",
        )
    if token and args.allow_insecure_token and any(
        target.scheme != "https" and not is_local_host(target.host)
        for target in targets
    ):
        reporter.warning(
            "transport",
            "bearer-token",
            "off-loopback plaintext token transport explicitly enabled",
        )

    for target in targets:
        NodeAcceptance(target, options, reporter, token).run()
    reporter.summary()
    return 1 if reporter.failures else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except KeyboardInterrupt:
        print("Interrupted.", file=sys.stderr)
        raise SystemExit(130) from None
