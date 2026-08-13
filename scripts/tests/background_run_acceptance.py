#!/usr/bin/env python3
"""Cross-device acceptance check for detached LlamaFarm chat runs.

The harness uses only the Python standard library. It submits through the real
``/ws/chat`` endpoint, drops the WebSocket without a close frame, and then uses
fresh REST clients to prove that the server owns the run and its durable state.
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import http.client
import json
import math
import os
import queue
import re
import shlex
import socket
import ssl
import struct
import sys
import threading
import time
import urllib.parse
import uuid
from dataclasses import dataclass
from typing import Any, Callable, Mapping, TypeVar


WS_SUBPROTOCOL = "llamafarm.v1"
TERMINAL_STATUSES = frozenset(
    {"completed", "completed_unverified", "failed", "cancelled"}
)
MAX_HTTP_RESPONSE_BYTES = 8 * 1024 * 1024
MAX_HANDSHAKE_BYTES = 64 * 1024
DEFAULT_DEADLINE_SECONDS = 30 * 60
DEFAULT_POLL_INTERVAL_SECONDS = 2.0
CANCEL_WAIT_SECONDS = 3600
PROCESS_PROBE_PATH = "/api/workspace/exec"
PROCESS_MATCH_OUTPUT = "LLAMAFARM_WAIT_PROCESS_FOUND"
CLEANUP_DEADLINE_SECONDS = 90


class AcceptanceError(RuntimeError):
    """A required background-run behavior was not observed."""


class TransientAcceptanceError(AcceptanceError):
    """A transport or server condition that a bounded poll may retry."""


@dataclass(frozen=True)
class Target:
    scheme: str
    host: str
    port: int
    path_prefix: str

    @property
    def authority(self) -> str:
        host = f"[{self.host}]" if ":" in self.host else self.host
        default_port = 443 if self.scheme == "https" else 80
        return host if self.port == default_port else f"{host}:{self.port}"

    @property
    def origin(self) -> str:
        return f"{self.scheme}://{self.authority}{self.path_prefix}"

    def http_url(self, path: str) -> str:
        if not path.startswith("/"):
            raise AcceptanceError("internal HTTP path must start with '/'")
        return f"{self.scheme}://{self.authority}{self.path_prefix}{path}"

    @property
    def websocket_path(self) -> str:
        return f"{self.path_prefix}/ws/chat"


def parse_target(raw: str) -> Target:
    candidate = raw.strip().rstrip("/")
    if not candidate:
        raise AcceptanceError("base URL is empty")
    if "://" not in candidate:
        candidate = f"http://{candidate}"
    parsed = urllib.parse.urlsplit(candidate)
    if parsed.scheme not in {"http", "https"}:
        raise AcceptanceError("base URL scheme must be http or https")
    if not parsed.hostname:
        raise AcceptanceError("base URL must include a host")
    if parsed.username is not None or parsed.password is not None:
        raise AcceptanceError("credentials are not allowed in the base URL")
    if parsed.query or parsed.fragment:
        raise AcceptanceError("base URL must not include a query or fragment")
    try:
        port = parsed.port or (443 if parsed.scheme == "https" else 80)
    except ValueError as error:
        raise AcceptanceError(f"invalid base URL port: {error}") from None
    prefix = parsed.path.rstrip("/")
    return Target(parsed.scheme, parsed.hostname, port, prefix)


class Deadline:
    """One observer deadline; it is never sent to the LlamaFarm runtime."""

    def __init__(
        self,
        seconds: float,
        *,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if seconds <= 0:
            raise AcceptanceError("deadline must be greater than zero")
        self._clock = clock
        self._end = clock() + seconds

    def remaining(self) -> float:
        return max(0.0, self._end - self._clock())

    def require_remaining(self, label: str) -> float:
        remaining = self.remaining()
        if remaining <= 0:
            raise AcceptanceError(f"harness deadline expired while waiting for {label}")
        return remaining

    def io_timeout(self, label: str) -> float:
        return max(0.1, min(30.0, self.require_remaining(label)))


T = TypeVar("T")


def poll_until(
    label: str,
    probe: Callable[[], T],
    accept: Callable[[T], bool],
    deadline: Deadline,
    interval: float,
    *,
    sleep: Callable[[float], None] = time.sleep,
) -> T:
    """Poll until ``accept`` succeeds, retaining the last transient failure."""

    last_error: Exception | None = None
    while True:
        deadline.require_remaining(label)
        try:
            value = probe()
            if accept(value):
                return value
            last_error = None
        except TransientAcceptanceError as error:
            last_error = error

        remaining = deadline.require_remaining(label)
        sleep(min(interval, remaining))
        if deadline.remaining() <= 0:
            suffix = f"; last error: {last_error}" if last_error else ""
            raise AcceptanceError(f"harness deadline expired waiting for {label}{suffix}")


class JsonHttpClient:
    def __init__(self, target: Target, token: str | None, deadline: Deadline) -> None:
        self.target = target
        self.token = token
        self.deadline = deadline
    def json(
        self,
        method: str,
        path: str,
        payload: Mapping[str, Any] | None = None,
    ) -> Any:
        body = None
        headers = {"Accept": "application/json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        if payload is not None:
            body = json.dumps(payload, separators=(",", ":")).encode("utf-8")
            headers["Content-Type"] = "application/json"
        label = f"{method} {path}"
        connection = http.client.HTTPConnection(
            self.target.host,
            self.target.port,
            timeout=self.deadline.io_timeout(label),
        )
        try:
            connection.sock = open_target_socket(self.target, self.deadline, label)
            connection.sock.settimeout(self.deadline.io_timeout(label))
            connection.request(
                method,
                f"{self.target.path_prefix}{path}",
                body=body,
                headers=headers,
            )
            connection.sock.settimeout(self.deadline.io_timeout(label))
            response = connection.getresponse()
            chunks: list[bytes] = []
            size = 0
            try:
                while True:
                    if connection.sock is not None:
                        connection.sock.settimeout(self.deadline.io_timeout(label))
                    chunk = response.read(64 * 1024)
                    self.deadline.require_remaining(label)
                    if not chunk:
                        break
                    size += len(chunk)
                    if size > MAX_HTTP_RESPONSE_BYTES:
                        raise AcceptanceError(f"{label} response exceeded size limit")
                    chunks.append(chunk)
            finally:
                response.close()
            raw = b"".join(chunks)
            if not 200 <= response.status < 300:
                detail = raw[:1200].decode("utf-8", errors="replace")
                if self.token:
                    detail = detail.replace(self.token, "[redacted]")
                error_type = (
                    TransientAcceptanceError
                    if response.status in {408, 425, 429} or response.status >= 500
                    else AcceptanceError
                )
                raise error_type(
                    f"{label} returned HTTP {response.status}: {detail}"
                )
        except AcceptanceError:
            raise
        except (OSError, TimeoutError, http.client.HTTPException) as error:
            raise TransientAcceptanceError(f"{label} failed: {error}") from None
        finally:
            connection.close()
        try:
            return json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise AcceptanceError(f"{label} did not return valid JSON") from None


def resolve_target(
    target: Target, deadline: Deadline, label: str
) -> list[tuple[int, int, int, str, tuple[Any, ...]]]:
    """Resolve without allowing libc DNS to block the harness past its deadline."""

    results: queue.Queue[object] = queue.Queue(maxsize=1)

    def worker() -> None:
        try:
            results.put(socket.getaddrinfo(target.host, target.port, type=socket.SOCK_STREAM))
        except OSError as error:
            results.put(error)

    threading.Thread(target=worker, daemon=True).start()
    try:
        resolved = results.get(timeout=deadline.require_remaining(label))
    except queue.Empty:
        raise AcceptanceError(f"harness deadline expired while resolving {target.host}") from None
    if isinstance(resolved, OSError):
        raise TransientAcceptanceError(
            f"could not resolve {target.host}: {resolved}"
        ) from None
    return resolved  # type: ignore[return-value]


def open_target_socket(
    target: Target, deadline: Deadline, label: str
) -> socket.socket | ssl.SSLSocket:
    last_error: OSError | None = None
    for family, socktype, proto, _, address in resolve_target(target, deadline, label):
        connection = socket.socket(family, socktype, proto)
        try:
            connection.settimeout(deadline.io_timeout(label))
            connection.connect(address)
            if target.scheme == "https":
                connection.settimeout(deadline.io_timeout(label))
                return ssl.create_default_context().wrap_socket(
                    connection, server_hostname=target.host
                )
            return connection
        except OSError as error:
            last_error = error
            connection.close()
    raise TransientAcceptanceError(
        f"could not connect to {target.authority}: {last_error or 'no addresses'}"
    )


def websocket_accept_value(key: str) -> str:
    digest = hashlib.sha1(  # noqa: S324 - required by RFC 6455
        (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
    ).digest()
    return base64.b64encode(digest).decode("ascii")


def parse_handshake_response(raw: bytes, key: str) -> Mapping[str, str]:
    try:
        text = raw.decode("iso-8859-1")
    except UnicodeDecodeError:
        raise AcceptanceError("WebSocket handshake was not valid HTTP") from None
    lines = text.split("\r\n")
    if not lines or not re.match(r"^HTTP/1\.1 101(?:\s|$)", lines[0]):
        status = lines[0] if lines else "empty response"
        raise AcceptanceError(f"WebSocket upgrade failed: {status}")
    headers: dict[str, str] = {}
    for line in lines[1:]:
        if not line:
            break
        if ":" not in line:
            raise AcceptanceError("WebSocket handshake contained a malformed header")
        name, value = line.split(":", 1)
        normalized = name.strip().lower()
        headers[normalized] = ",".join(
            part for part in (headers.get(normalized), value.strip()) if part
        )
    if headers.get("sec-websocket-accept") != websocket_accept_value(key):
        raise AcceptanceError("WebSocket handshake accept value did not match")
    if headers.get("sec-websocket-protocol") != WS_SUBPROTOCOL:
        raise AcceptanceError(
            f"WebSocket server did not select {WS_SUBPROTOCOL!r}"
        )
    connection_tokens = {
        token.strip().lower()
        for token in headers.get("connection", "").split(",")
        if token.strip()
    }
    if "upgrade" not in connection_tokens:
        raise AcceptanceError("WebSocket handshake lacked Connection: Upgrade")
    if headers.get("upgrade", "").lower() != "websocket":
        raise AcceptanceError("WebSocket handshake lacked Upgrade: websocket")
    if headers.get("sec-websocket-extensions", "").strip():
        raise AcceptanceError("WebSocket server selected an unsolicited extension")
    return headers


def encode_client_text_frame(text: str, mask_key: bytes | None = None) -> bytes:
    payload = text.encode("utf-8")
    mask = os.urandom(4) if mask_key is None else mask_key
    if len(mask) != 4:
        raise ValueError("WebSocket mask must be exactly four bytes")
    length = len(payload)
    if length <= 125:
        header = bytes((0x81, 0x80 | length))
    elif length <= 0xFFFF:
        header = bytes((0x81, 0x80 | 126)) + struct.pack("!H", length)
    else:
        header = bytes((0x81, 0x80 | 127)) + struct.pack("!Q", length)
    masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
    return header + mask + masked


def submit_and_detach(
    target: Target,
    token: str | None,
    deadline: Deadline,
    payload: Mapping[str, Any],
) -> None:
    connection: socket.socket | ssl.SSLSocket | None = None
    try:
        connection = open_target_socket(target, deadline, "WebSocket connection")
        connection.settimeout(deadline.io_timeout("WebSocket handshake"))
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        headers = [
            f"GET {target.websocket_path} HTTP/1.1",
            f"Host: {target.authority}",
            "Upgrade: websocket",
            "Connection: Upgrade",
            f"Sec-WebSocket-Key: {key}",
            "Sec-WebSocket-Version: 13",
            f"Sec-WebSocket-Protocol: {WS_SUBPROTOCOL}",
            "User-Agent: LlamaFarm-background-run-acceptance/1",
        ]
        if token:
            if "\r" in token or "\n" in token:
                raise AcceptanceError("gateway token contains an invalid header character")
            headers.append(f"Authorization: Bearer {token}")
        request = ("\r\n".join(headers) + "\r\n\r\n").encode("iso-8859-1")
        connection.settimeout(deadline.io_timeout("WebSocket handshake write"))
        connection.sendall(request)

        response = bytearray()
        while b"\r\n\r\n" not in response:
            connection.settimeout(deadline.io_timeout("WebSocket handshake read"))
            chunk = connection.recv(4096)
            deadline.require_remaining("WebSocket handshake read")
            if not chunk:
                raise AcceptanceError("WebSocket server closed during handshake")
            response.extend(chunk)
            if len(response) > MAX_HANDSHAKE_BYTES:
                raise AcceptanceError("WebSocket handshake exceeded size limit")
        raw_headers, _ = bytes(response).split(b"\r\n\r\n", 1)
        parse_handshake_response(raw_headers + b"\r\n\r\n", key)

        serialized = json.dumps(payload, separators=(",", ":"))
        connection.settimeout(deadline.io_timeout("WebSocket message write"))
        connection.sendall(encode_client_text_frame(serialized))
        deadline.require_remaining("WebSocket message write")
        # Intentionally omit a WebSocket close frame. Closing the transport is
        # the browser/network-loss condition the server-owned run must survive.
    except AcceptanceError:
        raise
    except (OSError, UnicodeEncodeError) as error:
        raise AcceptanceError(f"WebSocket submit failed: {error}") from None
    finally:
        if connection is not None:
            connection.close()


def find_run_for_session(document: Any, session_id: str) -> Mapping[str, Any] | None:
    if not isinstance(document, Mapping) or not isinstance(document.get("runs"), list):
        raise AcceptanceError("GET /api/runs response lacks a runs array")
    for run in document["runs"]:
        if isinstance(run, Mapping) and run.get("session_id") == session_id:
            if not isinstance(run.get("run_id"), str) or not run["run_id"]:
                raise AcceptanceError("matching run lacks run_id")
            return run
    return None


def session_is_discoverable(document: Any, session_id: str) -> bool:
    if not isinstance(document, Mapping) or not isinstance(document.get("sessions"), list):
        raise AcceptanceError("GET /api/chat-sessions response lacks a sessions array")
    return any(
        isinstance(session, Mapping) and session.get("session_id") == session_id
        for session in document["sessions"]
    )


def parse_run_detail(document: Any, expected_run_id: str) -> Mapping[str, Any]:
    if not isinstance(document, Mapping) or not isinstance(document.get("run"), Mapping):
        raise AcceptanceError("run detail response lacks a run object")
    run = document["run"]
    meta = run.get("meta")
    if not isinstance(meta, Mapping) or meta.get("run_id") != expected_run_id:
        raise AcceptanceError("run detail does not match requested run_id")
    if not isinstance(meta.get("status"), str):
        raise AcceptanceError("run detail lacks status")
    if not isinstance(run.get("events"), list):
        raise AcceptanceError("run detail lacks events")
    return run


def run_status(run: Mapping[str, Any]) -> str:
    return str(run["meta"]["status"]).lower()


def has_successful_tool_evidence(
    run: Mapping[str, Any], tool: str, marker: str
) -> bool:
    return any(
        isinstance(event, Mapping)
        and event.get("tool") == tool
        and event.get("success") is True
        and marker in str(event.get("output_excerpt", ""))
        for event in run.get("events", [])
    )


def transcript_has_result(
    document: Any, session_id: str, request_marker: str, result_marker: str
) -> bool:
    if not isinstance(document, Mapping) or document.get("session_id") != session_id:
        raise AcceptanceError("chat-session detail does not match requested session")
    messages = document.get("messages")
    if not isinstance(messages, list):
        raise AcceptanceError("chat-session detail lacks messages")
    user_checkpoint = any(
        isinstance(message, Mapping)
        and message.get("role") == "user"
        and request_marker in str(message.get("content", ""))
        for message in messages
    )
    durable_result = any(assistant_final_contains(message, result_marker) for message in messages)
    return user_checkpoint and durable_result


def assistant_final_contains(message: Any, marker: str) -> bool:
    if not isinstance(message, Mapping) or message.get("role") != "assistant":
        return False
    content = message.get("content", "")
    if not isinstance(content, str):
        return False
    try:
        parsed = json.loads(content)
    except json.JSONDecodeError:
        return marker in content
    if isinstance(parsed, Mapping) and parsed.get("tool_calls"):
        return False
    if isinstance(parsed, Mapping):
        return marker in str(parsed.get("content", ""))
    return marker in content


def transcript_has_user_checkpoint(
    document: Any, session_id: str, request_marker: str
) -> bool:
    if not isinstance(document, Mapping) or document.get("session_id") != session_id:
        raise AcceptanceError("chat-session detail does not match requested session")
    messages = document.get("messages")
    if not isinstance(messages, list):
        raise AcceptanceError("chat-session detail lacks messages")
    return any(
        isinstance(message, Mapping)
        and message.get("role") == "user"
        and request_marker in str(message.get("content", ""))
        for message in messages
    )


def plan_has_task_plan_evidence(run: Mapping[str, Any]) -> bool:
    plan = run.get("plan")
    events = run.get("events")
    return (
        isinstance(plan, list)
        and bool(plan)
        and isinstance(events, list)
        and any(
            isinstance(event, Mapping)
            and event.get("tool") == "task_plan"
            and event.get("success") is True
            for event in events
        )
    )


def shell_event_matches_command(
    run: Mapping[str, Any], command: str, *, success: bool | None = None
) -> bool:
    encoded_command = json.dumps(command, separators=(",", ":"))[1:-1]
    return any(
        isinstance(event, Mapping)
        and event.get("tool") == "shell"
        and (success is None or event.get("success") is success)
        and encoded_command in str(event.get("args_summary", ""))
        for event in run.get("events", [])
    )


def shell_events(run: Mapping[str, Any]) -> list[Mapping[str, Any]]:
    return [
        event
        for event in run.get("events", [])
        if isinstance(event, Mapping) and event.get("tool") == "shell"
    ]


def workspace_exec_result(document: Any, marker: str) -> bool:
    if not isinstance(document, Mapping):
        raise AcceptanceError("workspace exec response is not an object")
    return document.get("exit_code") == 0 and marker in str(document.get("stdout", ""))


def completion_prompt(request_marker: str, result_marker: str) -> str:
    return f"""Background-run acceptance request {request_marker}.
Create one pending task_plan step, mark it in progress, then invoke shell
exactly once with `echo {result_marker}`.
Mark the plan step completed and finish with a short answer containing that
same result marker. Do not read or write files, access memory or the network,
delegate work, or perform any other action."""


def cancellation_prompt(request_marker: str, result_marker: str) -> str:
    wait_command = cancellation_wait_command(result_marker)
    return f"""Detached-cancellation acceptance request {request_marker}.
Use only the task_plan and shell tools. Create two plan steps. Complete the
first with task_plan only. Then mark the second step in progress and invoke
shell exactly once with `{wait_command}`. Do not finish the second step early;
an operator will cancel it while that exact command is running. Do not read or
write files, access memory or the network, delegate work, or perform any other
action."""


def completion_command(result_marker: str) -> str:
    return f"echo {result_marker}"


def cancellation_wait_command(result_marker: str) -> str:
    program = (
        "import time; "
        f"print({result_marker!r}, flush=True); "
        f"time.sleep({CANCEL_WAIT_SECONDS})"
    )
    return f"python3 -c {shlex.quote(program)}"


def cancellation_process_probe_command(result_marker: str) -> str:
    """Build a read-only /proc probe that cannot match its own command line.

    The target process carries ``result_marker`` literally in its ``python -c``
    argument. The probe carries only the marker's base64 encoding, decodes it
    in memory, and skips its own PID before inspecting other command lines.
    """

    marker_b64 = base64.b64encode(result_marker.encode("utf-8")).decode("ascii")
    program = f"""import base64
import os
from pathlib import Path

needle = base64.b64decode({marker_b64!r})
matched = False
for path in Path('/proc').glob('[0-9]*/cmdline'):
    if path.parent.name == str(os.getpid()):
        continue
    try:
        command_line = path.read_bytes()
    except OSError:
        continue
    if needle in command_line:
        matched = True
        break
print({PROCESS_MATCH_OUTPUT!r} if matched else '')
"""
    return f"python3 -c {shlex.quote(program)}"


def message_payload(session_id: str, prompt: str) -> Mapping[str, Any]:
    return {
        "type": "message",
        "content": prompt,
        "session_id": session_id,
        "temporary": False,
        "history_seed": [{"role": "user", "content": prompt}],
        "allowed_tools": ["task_plan", "shell"],
        "agent_mode": "agent",
    }


def encoded(value: str) -> str:
    return urllib.parse.quote(value, safe="")


class BackgroundRunAcceptance:
    def __init__(
        self,
        target: Target,
        token: str | None,
        deadline: Deadline,
        poll_interval: float,
    ) -> None:
        self.target = target
        self.token = token
        self.deadline = deadline
        self.poll_interval = poll_interval
        self.active_run_ids: set[str] = set()
        self.submitted_session_ids: set[str] = set()

    def _fresh_rest_client(self) -> JsonHttpClient:
        return JsonHttpClient(self.target, self.token, self.deadline)

    def _wait_for_session(self, rest: JsonHttpClient, session_id: str) -> None:
        poll_until(
            f"persisted session {session_id}",
            lambda: rest.json("GET", "/api/chat-sessions"),
            lambda document: session_is_discoverable(document, session_id),
            self.deadline,
            self.poll_interval,
        )

    def _wait_for_run(self, rest: JsonHttpClient, session_id: str) -> Mapping[str, Any]:
        return poll_until(
            f"run ledger for session {session_id}",
            lambda: find_run_for_session(rest.json("GET", "/api/runs"), session_id),
            lambda run: run is not None,
            self.deadline,
            self.poll_interval,
        )

    def _run_detail(self, rest: JsonHttpClient, run_id: str) -> Mapping[str, Any]:
        document = rest.json("GET", f"/api/runs/{encoded(run_id)}")
        return parse_run_detail(document, run_id)

    def _wait_for_hydration(
        self,
        rest: JsonHttpClient,
        session_id: str,
        request_marker: str,
        result_marker: str,
    ) -> None:
        poll_until(
            f"durable hydrated result for session {session_id}",
            lambda: rest.json("GET", f"/api/chat-sessions/{encoded(session_id)}"),
            lambda document: transcript_has_result(
                document, session_id, request_marker, result_marker
            ),
            self.deadline,
            self.poll_interval,
        )

    def _completion_case(self) -> Mapping[str, Any]:
        unique = uuid.uuid4().hex
        session_id = f"background-accept-{unique}"
        request_marker = f"BG_REQUEST_{unique}"
        result_marker = f"BG_RESULT_{unique}"
        prompt = completion_prompt(request_marker, result_marker)
        self.submitted_session_ids.add(session_id)
        submit_and_detach(
            self.target,
            self.token,
            self.deadline,
            message_payload(session_id, prompt),
        )

        # This client is intentionally constructed only after transport detach.
        rest = self._fresh_rest_client()
        self._wait_for_session(rest, session_id)
        run_meta = self._wait_for_run(rest, session_id)
        run_id = str(run_meta["run_id"])
        self.active_run_ids.add(run_id)
        run = poll_until(
            f"terminal completion for run {run_id}",
            lambda: self._run_detail(rest, run_id),
            lambda detail: run_status(detail) in TERMINAL_STATUSES,
            self.deadline,
            self.poll_interval,
        )
        status = run_status(run)
        if status != "completed":
            raise AcceptanceError(f"detached completion run ended as {status}, not completed")
        if not has_successful_tool_evidence(run, "shell", result_marker):
            raise AcceptanceError("detached completion run lacks successful shell evidence")
        if not shell_event_matches_command(
            run, completion_command(result_marker), success=True
        ):
            raise AcceptanceError("detached completion used an unexpected shell command")
        if not plan_has_task_plan_evidence(run):
            raise AcceptanceError("detached completion lacks durable task_plan evidence")
        self.active_run_ids.discard(run_id)
        self._wait_for_hydration(
            rest, session_id, request_marker, result_marker
        )
        self.submitted_session_ids.discard(session_id)
        print(
            f"PASS detached completion: session={session_id} run={run_id} status={status}",
            flush=True,
        )
        return {"session_id": session_id, "run_id": run_id, "status": status}

    def _cancellation_case(self) -> Mapping[str, Any]:
        unique = uuid.uuid4().hex
        session_id = f"cancel-accept-{unique}"
        request_marker = f"CANCEL_REQUEST_{unique}"
        result_marker = f"CANCEL_EVIDENCE_{unique}"
        prompt = cancellation_prompt(request_marker, result_marker)
        wait_command = cancellation_wait_command(result_marker)
        process_probe = cancellation_process_probe_command(result_marker)
        self.submitted_session_ids.add(session_id)
        submit_and_detach(
            self.target,
            self.token,
            self.deadline,
            message_payload(session_id, prompt),
        )

        rest = self._fresh_rest_client()
        self._wait_for_session(rest, session_id)
        run_meta = self._wait_for_run(rest, session_id)
        run_id = str(run_meta["run_id"])
        self.active_run_ids.add(run_id)
        observed = poll_until(
            f"in-flight cancellable command for run {run_id}",
            lambda: {
                "run": self._run_detail(rest, run_id),
                "probe": rest.json(
                    "POST", PROCESS_PROBE_PATH, {"command": process_probe}
                ),
            },
            lambda state: run_status(state["run"]) in TERMINAL_STATUSES
            or workspace_exec_result(state["probe"], PROCESS_MATCH_OUTPUT),
            self.deadline,
            self.poll_interval,
        )
        evidenced = observed["run"]
        if run_status(evidenced) != "running":
            raise AcceptanceError(
                f"cancellation run became {run_status(evidenced)} before REST cancellation"
            )
        if not plan_has_task_plan_evidence(evidenced):
            raise AcceptanceError("cancellation run lacks durable task_plan evidence")

        cancelled = rest.json(
            "POST", f"/api/runs/{encoded(run_id)}/cancel", payload={}
        )
        if not isinstance(cancelled, Mapping) or cancelled.get("ok") is not True:
            raise AcceptanceError("run cancel endpoint did not acknowledge cancellation")
        if cancelled.get("live_cancelled") is not True:
            raise AcceptanceError("run cancel endpoint did not cancel the live server-owned run")

        run = poll_until(
            f"cancelled status for run {run_id}",
            lambda: self._run_detail(rest, run_id),
            lambda detail: run_status(detail) in TERMINAL_STATUSES,
            self.deadline,
            self.poll_interval,
        )
        status = run_status(run)
        if status != "cancelled":
            raise AcceptanceError(f"REST-cancelled run ended as {status}, not cancelled")
        if shell_event_matches_command(run, wait_command, success=True):
            raise AcceptanceError("cancelled wait command was incorrectly recorded as successful")
        self.active_run_ids.discard(run_id)
        poll_until(
            f"durable cancelled transcript for session {session_id}",
            lambda: rest.json("GET", f"/api/chat-sessions/{encoded(session_id)}"),
            lambda document: transcript_has_user_checkpoint(
                document, session_id, request_marker
            ),
            self.deadline,
            self.poll_interval,
        )
        self.submitted_session_ids.discard(session_id)
        print(
            f"PASS detached cancellation: session={session_id} run={run_id} status={status}",
            flush=True,
        )
        return {"session_id": session_id, "run_id": run_id, "status": status}

    def _best_effort_cancel_active(self) -> None:
        if not self.active_run_ids and not self.submitted_session_ids:
            return
        cleanup_deadline = Deadline(CLEANUP_DEADLINE_SECONDS)
        rest = JsonHttpClient(self.target, self.token, cleanup_deadline)
        try:
            runs = rest.json("GET", "/api/runs")
            for session_id in self.submitted_session_ids:
                run = find_run_for_session(runs, session_id)
                if run is not None:
                    self.active_run_ids.add(str(run["run_id"]))
        except AcceptanceError:
            pass
        for run_id in tuple(self.active_run_ids):
            if cleanup_deadline.remaining() <= 0:
                break
            try:
                rest.json("POST", f"/api/runs/{encoded(run_id)}/cancel", payload={})
            except AcceptanceError:
                pass

    def run(self) -> Mapping[str, Any]:
        try:
            completed = self._completion_case()
            cancelled = self._cancellation_case()
            return {"status": "pass", "completed": completed, "cancelled": cancelled}
        finally:
            self._best_effort_cancel_active()


def positive_float(value: str) -> float:
    parsed = float(value)
    if not math.isfinite(parsed) or parsed <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return parsed


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base-url",
        default=os.environ.get("LLAMAFARM_BASE_URL", "http://127.0.0.1:42617"),
        help="LlamaFarm origin (default: LLAMAFARM_BASE_URL or localhost)",
    )
    parser.add_argument(
        "--token",
        default=os.environ.get("LLAMAFARM_GATEWAY_TOKEN"),
        help="gateway bearer token (prefer LLAMAFARM_GATEWAY_TOKEN)",
    )
    parser.add_argument(
        "--deadline-seconds",
        type=positive_float,
        default=os.environ.get(
            "LLAMAFARM_BACKGROUND_ACCEPTANCE_DEADLINE",
            str(DEFAULT_DEADLINE_SECONDS),
        ),
        help="overall observer deadline; never sent to the runtime (default: 1800)",
    )
    parser.add_argument(
        "--poll-interval",
        type=positive_float,
        default=DEFAULT_POLL_INTERVAL_SECONDS,
        help="REST polling interval in seconds (default: 2)",
    )
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        target = parse_target(args.base_url)
        deadline = Deadline(args.deadline_seconds)
        result = BackgroundRunAcceptance(
            target, args.token, deadline, args.poll_interval
        ).run()
    except (AcceptanceError, KeyboardInterrupt) as error:
        print(f"FAIL background-run acceptance: {error}", file=sys.stderr, flush=True)
        return 1
    print(json.dumps(result, indent=2, sort_keys=True), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
