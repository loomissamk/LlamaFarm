#!/usr/bin/env python3
"""Exercise a running LlamaFarm dashboard through its own browser UI.

This deliberately speaks only to the container-local ChromeDriver and then
uses the rendered dashboard.  Direct gateway access, the WebSocket agent chat,
registered tools, and federation therefore take the same path as a human
browser rather than being accepted through a privileged API shortcut.

Run inside a bundled LlamaFarm container, for example:

  docker exec -i -e LLAMAFARM_ACCEPTANCE_NODE=rtx4070-laptop LlamaFarm \
    python3 - < scripts/docker/ui-acceptance.py --with-federation

The runner writes a screenshot, DOM snapshot, and JSON result into the
persisted workspace acceptance-artifacts/ directory.  It never writes
federation tokens into those artifacts.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import sys
import time
import traceback
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Callable


DRIVER_URL = os.environ.get("LLAMAFARM_CHROMEDRIVER_URL", "http://127.0.0.1:9515")
APP_URL = os.environ.get("LLAMAFARM_UI_URL", "http://127.0.0.1:42617")
ARTIFACT_ROOT = Path(
    os.environ.get(
        "LLAMAFARM_ACCEPTANCE_ARTIFACT_DIR",
        "/llamafarm-data/workspace/acceptance-artifacts",
    )
)
ELEMENT_KEY = "element-6066-11e4-a52e-4f735466cecf"


class AcceptanceError(RuntimeError):
    """A UI condition needed for acceptance was not observed."""


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
            f"{self.base_url}{path}", data=body, headers=headers, method=method
        )
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                raw = response.read().decode("utf-8", errors="replace")
        except urllib.error.HTTPError as error:
            detail = error.read().decode("utf-8", errors="replace")[-1200:]
            raise AcceptanceError(
                f"ChromeDriver {method} {path} returned HTTP {error.code}: {detail}"
            ) from error
        except urllib.error.URLError as error:
            raise AcceptanceError(f"ChromeDriver unavailable at {self.base_url}: {error}") from error

        try:
            decoded = json.loads(raw) if raw else {}
        except json.JSONDecodeError as error:
            raise AcceptanceError(
                f"ChromeDriver {method} {path} did not return JSON: {raw[-800:]}"
            ) from error

        value = decoded.get("value") if isinstance(decoded, dict) else decoded
        if isinstance(value, dict) and value.get("error"):
            message = value.get("message", value["error"])
            raise AcceptanceError(f"ChromeDriver {method} {path}: {message}")
        return value

    def start(self) -> None:
        capabilities = {
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
                }
            }
        }
        value = self.request("POST", "/session", capabilities)
        if not isinstance(value, dict):
            raise AcceptanceError("ChromeDriver returned an invalid session response")
        self.session_id = value.get("sessionId")
        if not self.session_id:
            raise AcceptanceError(f"ChromeDriver did not return a session id: {value}")

    def close(self) -> None:
        if self.session_id:
            try:
                self.request("DELETE", f"/session/{self.session_id}")
            except AcceptanceError:
                pass
            self.session_id = None

    def _path(self, suffix: str) -> str:
        if not self.session_id:
            raise AcceptanceError("WebDriver session has not started")
        return f"/session/{self.session_id}{suffix}"

    def navigate(self, url: str) -> None:
        self.request("POST", self._path("/url"), {"url": url})

    def execute(self, script: str, args: list[Any] | None = None) -> Any:
        return self.request(
            "POST", self._path("/execute/sync"), {"script": script, "args": args or []}
        )

    def exists(self, selector: str) -> bool:
        return bool(
            self.execute("return Boolean(document.querySelector(arguments[0]));", [selector])
        )

    def find(self, selector: str) -> str:
        value = self.request(
            "POST", self._path("/element"), {"using": "css selector", "value": selector}
        )
        if not isinstance(value, dict):
            raise AcceptanceError(f"No element object returned for selector {selector!r}")
        element_id = value.get(ELEMENT_KEY) or value.get("ELEMENT")
        if not element_id:
            raise AcceptanceError(f"No element id returned for selector {selector!r}: {value}")
        return str(element_id)

    def click(self, element_id: str) -> None:
        self.request("POST", self._path(f"/element/{element_id}/click"), {})

    def clear(self, element_id: str) -> None:
        self.request("POST", self._path(f"/element/{element_id}/clear"), {})

    def send_keys(self, element_id: str, value: str) -> None:
        self.request(
            "POST",
            self._path(f"/element/{element_id}/value"),
            {"text": value, "value": list(value)},
        )

    def body_text(self) -> str:
        value = self.execute("return document.body ? document.body.innerText : '';" )
        return value if isinstance(value, str) else str(value)

    def screenshot(self) -> bytes:
        value = self.request("GET", self._path("/screenshot"))
        if not isinstance(value, str):
            raise AcceptanceError("ChromeDriver returned an invalid screenshot payload")
        return base64.b64decode(value)


def wait_for(label: str, predicate: Callable[[], Any], timeout: float = 60.0) -> Any:
    deadline = time.monotonic() + timeout
    last_error: Exception | None = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except Exception as error:  # Retry while a SPA route is still mounting.
            last_error = error
        time.sleep(0.35)
    suffix = f" (last error: {last_error})" if last_error else ""
    raise AcceptanceError(f"Timed out after {timeout:.0f}s waiting for {label}{suffix}")


def safe_node_name(value: str) -> str:
    cleaned = re.sub(r"[^A-Za-z0-9_.-]+", "-", value.strip()).strip(".-")
    return cleaned or "llamafarm-node"


class UiAcceptance:
    def __init__(self, node: str) -> None:
        self.node = safe_node_name(node)
        self.driver = WebDriver(DRIVER_URL)
        self.results: list[dict[str, Any]] = []
        self.artifact_dir = ARTIFACT_ROOT / f"{self.node}-{int(time.time())}"
        self.isolated_chat_created = False

    def record(self, name: str, evidence: str) -> None:
        self.results.append({"test": name, "ok": True, "evidence": evidence})
        print(f"PASS {name}: {evidence}", flush=True)

    def navigate(self, route: str) -> None:
        self.driver.navigate(f"{APP_URL.rstrip('/')}{route}")

    def assert_direct_access(self) -> None:
        self.navigate("/")
        wait_for("dashboard shell", lambda: bool(self.driver.body_text().strip()), 45)
        if self.driver.exists('[data-testid="pairing-gate"]'):
            raise AcceptanceError(
                "The retired browser pairing gate was rendered; direct LAN access is required"
            )
        self.record("ui-direct-access", "dashboard rendered without a browser pairing gate")

    def tool_registry(self) -> None:
        self.navigate("/tools")
        required = ["web_search_tool", "file_write", "file_read", "shell", "arxiv_search"]
        for tool in required:
            wait_for(
                f"{tool} in the Tools dashboard",
                lambda tool=tool: tool in self.driver.body_text(),
                60,
            )
        self.record("ui-tool-registry", "visible tools: " + ", ".join(required))

    def ensure_agent_connected(self) -> None:
        self.navigate("/agent")
        wait_for(
            "agent chat textarea",
            lambda: self.driver.exists('[data-testid="agent-chat-input"]'),
            60,
        )
        wait_for(
            "agent WebSocket connection",
            lambda: "Connected" in self.driver.body_text(),
            75,
        )
        if not self.isolated_chat_created:
            clicked = self.driver.execute(
                """
                const button = Array.from(document.querySelectorAll('button'))
                  .find((candidate) => (candidate.textContent || '').trim() === 'Temporary');
                if (!button) return false;
                button.click();
                return true;
                """
            )
            if not clicked:
                raise AcceptanceError("Could not create an isolated temporary acceptance chat")
            wait_for(
                "empty isolated temporary chat",
                lambda: len(self.agent_messages()) == 0,
                30,
            )
            self.isolated_chat_created = True
            self.record(
                "ui-isolated-chat",
                "acceptance work uses a fresh temporary chat and cannot consume operator history",
            )

    def agent_messages(self) -> list[dict[str, str]]:
        value = self.driver.execute(
            """
            return Array.from(document.querySelectorAll('[data-message-role="agent"]')).map((el) => ({
              kind: el.dataset.messageKind || '', text: el.innerText || ''
            }));
            """
        )
        if not isinstance(value, list):
            return []
        messages: list[dict[str, str]] = []
        for item in value:
            if isinstance(item, dict):
                messages.append(
                    {"kind": str(item.get("kind", "")), "text": str(item.get("text", ""))}
                )
        return messages

    def send_chat(self, prompt: str, timeout: float = 600.0) -> list[dict[str, str]]:
        self.ensure_agent_connected()
        before = self.agent_messages()
        input_element = self.driver.find('[data-testid="agent-chat-input"]')
        self.driver.clear(input_element)
        self.driver.send_keys(input_element, prompt)
        self.driver.click(self.driver.find('[data-testid="agent-chat-send"]'))

        def complete() -> list[dict[str, str]] | None:
            current = self.agent_messages()
            fresh = current[len(before) :]
            errors = [message["text"] for message in fresh if message["kind"] == "error"]
            if errors:
                raise AcceptanceError("Agent returned a UI error: " + " | ".join(errors[-2:]))
            # The dashboard adds kind=message only after the agent's terminal done event.
            if any(message["kind"] == "message" for message in fresh):
                return fresh
            return None

        return wait_for("completed agent response", complete, timeout)

    @staticmethod
    def joined(messages: list[dict[str, str]]) -> str:
        return "\n".join(message["text"] for message in messages)

    def expect_marker(self, name: str, messages: list[dict[str, str]], marker: str) -> None:
        if marker not in self.joined(messages):
            raise AcceptanceError(f"{name} completed but its agent-visible response lacks {marker!r}")

    def run_tool_test(
        self,
        name: str,
        prompt: str,
        tool: str,
        marker: str | None = None,
        timeout: float = 900.0,
    ) -> list[dict[str, str]]:
        messages = self.send_chat(prompt, timeout)
        rendered = self.joined(messages)
        if f"[Tool Call] {tool}" not in rendered:
            # A second explicitly corrective turn makes the test stable against a
            # model answering in prose on the first turn, while still requiring a
            # genuine rendered tool event to pass.
            messages = self.send_chat(
                f"Acceptance retry: use the {tool} tool now for the prior request. "
                "Do not answer with prose until the tool result is available.",
                timeout,
            )
            rendered = self.joined(messages)
        if f"[Tool Call] {tool}" not in rendered:
            raise AcceptanceError(f"{name} did not render a {tool} tool call in the UI")
        if f"[Tool SUCCESS] {tool}" not in rendered:
            raise AcceptanceError(f"{name} rendered {tool} but did not show a successful tool result")
        if marker:
            self.expect_marker(name, messages, marker)
        self.record(name, f"{tool} call and successful result rendered in agent chat")
        return messages

    def agent_and_tool_workflow(self) -> None:
        chat_marker = f"UI_CHAT_{self.node}"
        messages = self.send_chat(
            f"Reply with exactly {chat_marker} and nothing else. Do not call a tool.", 120
        )
        self.expect_marker("ui-basic-chat", messages, chat_marker)
        self.record("ui-basic-chat", "agent returned the unique chat marker")

        # Start a real tool-enabled agent turn, then stop it through the same
        # browser control an operator uses.  The prompt is intentionally not a
        # direct-shell shortcut: it must enter the normal model/tool loop so
        # the test proves the cancellation token is observed while work is
        # inflight.  A subsequent chat turn confirms the session remains
        # usable rather than merely closing the socket.
        stop_before = self.agent_messages()
        stop_input = self.driver.find('[data-testid="agent-chat-input"]')
        self.driver.clear(stop_input)
        self.driver.send_keys(
            stop_input,
            "Use the shell tool exactly once to run sleep 45. Do not answer until the tool returns.",
        )
        self.driver.click(self.driver.find('[data-testid="agent-chat-send"]'))
        wait_for(
            "visible Stop control for an active agent turn",
            lambda: self.driver.exists('[data-testid="agent-chat-stop"]'),
            60,
        )
        self.driver.click(self.driver.find('[data-testid="agent-chat-stop"]'))

        def stopped() -> list[dict[str, str]] | None:
            fresh = self.agent_messages()[len(stop_before) :]
            if any(message["kind"] == "error" for message in fresh):
                raise AcceptanceError("Stop produced an agent error instead of a clean cancellation")
            if any(
                message["kind"] == "status" and "[Stopped]" in message["text"]
                for message in fresh
            ):
                return fresh
            return None

        wait_for("clean stopped state in the Agent UI", stopped, 120)
        stop_followup_marker = f"UI_STOP_FOLLOWUP_{self.node}"
        followup = self.send_chat(
            f"Reply with exactly {stop_followup_marker} and nothing else. Do not call a tool.",
            180,
        )
        self.expect_marker("ui-stop-followup", followup, stop_followup_marker)
        self.record(
            "ui-stop",
            "Stop produced a clean terminal UI state and the same chat accepted a follow-up",
        )

        # A follow-up sent while work is active is distinct from Stop: it must
        # become the next turn automatically while preserving the current
        # transcript. This exercises the browser composer and WebSocket queue,
        # not a direct gateway shortcut.
        queued_before = self.agent_messages()
        queued_input = self.driver.find('[data-testid="agent-chat-input"]')
        self.driver.clear(queued_input)
        self.driver.send_keys(
            queued_input,
            "Use the shell tool exactly once to run sleep 45. Do not answer until it returns.",
        )
        self.driver.click(self.driver.find('[data-testid="agent-chat-send"]'))
        wait_for(
            "active turn before queued follow-up",
            lambda: self.driver.exists('[data-testid="agent-chat-stop"]'),
            90,
        )
        queued_marker = f"UI_QUEUED_FOLLOWUP_{self.node}"
        queued_input = self.driver.find('[data-testid="agent-chat-input"]')
        self.driver.clear(queued_input)
        self.driver.send_keys(
            queued_input,
            f"Reply with exactly {queued_marker} and nothing else. Do not call a tool.",
        )
        self.driver.click(self.driver.find('[data-testid="agent-chat-send"]'))

        def queued_followup_completed() -> list[dict[str, str]] | None:
            fresh = self.agent_messages()[len(queued_before) :]
            errors = [message["text"] for message in fresh if message["kind"] == "error"]
            if errors:
                raise AcceptanceError(
                    "Queued follow-up returned a UI error: " + " | ".join(errors[-2:])
                )
            if queued_marker in self.joined(fresh):
                return fresh
            return None

        wait_for("queued follow-up completion", queued_followup_completed, 600)
        self.record(
            "ui-inflight-followup",
            "an active run accepted a new direction and resumed in the same chat",
        )

        file_name = f"acceptance/ui_accept_{self.node}.py"
        code_marker = f"UI_CODE_{self.node}"
        self.run_tool_test(
            "ui-file-write",
            f"write_file {file_name} containing print('{code_marker}')",
            "file_write",
        )

        run_messages = self.run_tool_test(
            "ui-code-run",
            f"python3 {file_name}",
            "shell",
        )
        if code_marker not in self.joined(run_messages):
            raise AcceptanceError("ui-code-run shell result did not include the Python program output")

        docker_marker = f"UI_DOCKER_{self.node}"
        self.run_tool_test(
            "ui-docker-control",
            "Use the docker tool exactly once with its ps action. Do not use the shell tool. "
            "Confirm that the local LlamaFarm container is visible, then end with "
            f"{docker_marker}.",
            "docker",
            docker_marker,
        )

        read_messages = self.run_tool_test(
            "ui-file-read",
            f"read {file_name} and show its contents",
            "file_read",
        )
        if code_marker not in self.joined(read_messages):
            raise AcceptanceError("ui-file-read tool result did not show the exact written code")

        web_marker = f"UI_WEB_{self.node}"
        self.run_tool_test(
            "ui-web-search",
            "Use web_search_tool exactly once now for `official Rust programming language website`. "
            "Do not use another search or fetch tool. After the search result, give one returned URL and "
            f"end with {web_marker}.",
            "web_search_tool",
            web_marker,
            300,
        )

        rag_marker = f"UI_RAG_{self.node}"
        self.run_tool_test(
            "ui-local-rag",
            "Use arxiv_search exactly once now for `transformer attention architecture`, with a small result "
            "limit. Do not use web search. Give one title and arXiv ID returned by the local corpus, then end with "
            f"{rag_marker}.",
            "arxiv_search",
            rag_marker,
            300,
        )

    def federation(self) -> None:
        self.navigate("/federation")
        checkbox_selector = 'input[data-testid^="federation-peer-"]'
        wait_for("a federation peer selector", lambda: self.driver.exists(checkbox_selector), 90)

        peer_state = self.driver.execute(
            """
            const peer = document.querySelector(arguments[0]);
            if (!peer) return null;
            return {
              testid: peer.getAttribute('data-testid'),
              checked: peer.checked,
              disabled: peer.disabled,
              text: peer.closest('div.rounded-xl')?.innerText || ''
            };
            """,
            [checkbox_selector],
        )
        if not isinstance(peer_state, dict):
            raise AcceptanceError("Federation UI did not expose a selectable peer")
        if peer_state.get("disabled"):
            raise AcceptanceError(
                "Federation peer is visible but not ready for delegation: "
                + str(peer_state.get("text", ""))
            )
        if not peer_state.get("checked"):
            self.driver.click(self.driver.find(checkbox_selector))
            wait_for(
                "selected federation peer",
                lambda: bool(
                    self.driver.execute(
                        "return Boolean(document.querySelector(arguments[0])?.checked);",
                        [checkbox_selector],
                    )
                ),
                30,
            )
        peer_id = str(peer_state.get("testid", "")).removeprefix("federation-peer-")
        if not peer_id:
            raise AcceptanceError("Federation peer test id was empty")
        self.record("ui-federation-ready", "online peer selected through the Federation dashboard")

        federation_marker = f"FEDERATION_{self.node}_TO_{peer_id[:8]}"
        messages = self.send_chat(
            "Use the delegate tool now to send this exact task to the selected remote worker: "
            f"reply with exactly {federation_marker} and nothing else. Do not solve it locally. "
            "After the remote worker finishes, relay its exact response.",
            420,
        )
        rendered = self.joined(messages)
        if "[Tool Call] delegate" not in rendered or "[Tool SUCCESS] delegate" not in rendered:
            raise AcceptanceError("Federation request did not show a successful delegate tool call in agent chat")

        self.navigate("/federation")
        task_selector = '[data-testid="federation-task-done"]'
        task_text = wait_for(
            "completed remote task in Federation dashboard",
            lambda: (
                self.driver.execute(
                    """
                    const tasks = Array.from(document.querySelectorAll(arguments[0]));
                    const found = tasks.find((task) => (task.innerText || '').includes(arguments[1]));
                    return found ? found.innerText : null;
                    """,
                    [task_selector, federation_marker],
                )
            ),
            120,
        )
        if federation_marker not in str(task_text):
            raise AcceptanceError("Federation task finished without the expected remote marker")
        self.record(
            "ui-federation-delegation",
            "delegate tool and completed Remote Task History entry were visible in the UI",
        )

    def save_artifacts(self, failure: str | None = None) -> None:
        try:
            self.artifact_dir.mkdir(parents=True, exist_ok=True)
            summary = {
                "node": self.node,
                "app_url": APP_URL,
                "driver_url": DRIVER_URL,
                "finished_at_unix": int(time.time()),
                "results": self.results,
                "failure": failure,
            }
            (self.artifact_dir / "summary.json").write_text(
                json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8"
            )
            if self.driver.session_id:
                (self.artifact_dir / "dashboard.txt").write_text(
                    self.driver.body_text(), encoding="utf-8"
                )
                (self.artifact_dir / "dashboard.png").write_bytes(self.driver.screenshot())
        except Exception as error:  # Artifacts must not hide the primary test result.
            print(f"WARN unable to save UI acceptance artifacts: {error}", file=sys.stderr)

    def run(self, with_federation: bool) -> None:
        self.driver.start()
        try:
            self.assert_direct_access()
            self.tool_registry()
            self.agent_and_tool_workflow()
            if with_federation:
                self.federation()
            self.save_artifacts()
        finally:
            self.driver.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--node",
        default=os.environ.get("LLAMAFARM_ACCEPTANCE_NODE", "llamafarm-node"),
        help="safe label used for unique test markers and artifact paths",
    )
    parser.add_argument(
        "--with-federation",
        action="store_true",
        help="also select a remote worker and validate dashboard delegation",
    )
    args = parser.parse_args()
    acceptance = UiAcceptance(args.node)
    try:
        acceptance.run(args.with_federation)
    except Exception as error:
        failure = f"{type(error).__name__}: {error}"
        acceptance.save_artifacts(failure)
        print(f"FAIL {failure}", file=sys.stderr)
        traceback.print_exc(file=sys.stderr)
        return 1
    print(f"UI acceptance completed for {acceptance.node}; artifacts: {acceptance.artifact_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
