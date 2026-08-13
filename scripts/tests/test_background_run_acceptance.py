#!/usr/bin/env python3
"""Offline unit tests for the background-run acceptance harness."""

from __future__ import annotations

import base64
import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import background_run_acceptance as harness  # noqa: E402


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


class TargetParsingTests(unittest.TestCase):
    def test_parses_cross_device_origin_and_prefix(self) -> None:
        target = harness.parse_target("https://node.example:8443/llamafarm/")
        self.assertEqual(target.host, "node.example")
        self.assertEqual(target.port, 8443)
        self.assertEqual(target.websocket_path, "/llamafarm/ws/chat")
        self.assertEqual(
            target.http_url("/api/runs"),
            "https://node.example:8443/llamafarm/api/runs",
        )

    def test_rejects_credentials_in_base_url(self) -> None:
        with self.assertRaisesRegex(harness.AcceptanceError, "credentials"):
            harness.parse_target("http://user:secret@node.example")


class WebSocketProtocolTests(unittest.TestCase):
    def test_validates_upgrade_and_selected_subprotocol(self) -> None:
        key = base64.b64encode(b"0123456789abcdef").decode("ascii")
        raw = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {harness.websocket_accept_value(key)}\r\n"
            "Sec-WebSocket-Protocol: llamafarm.v1\r\n\r\n"
        ).encode("ascii")
        headers = harness.parse_handshake_response(raw, key)
        self.assertEqual(headers["sec-websocket-protocol"], "llamafarm.v1")

    def test_rejects_missing_selected_subprotocol(self) -> None:
        key = base64.b64encode(b"0123456789abcdef").decode("ascii")
        raw = (
            "HTTP/1.1 101 Switching Protocols\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Accept: {harness.websocket_accept_value(key)}\r\n\r\n"
        ).encode("ascii")
        with self.assertRaisesRegex(harness.AcceptanceError, "did not select"):
            harness.parse_handshake_response(raw, key)

    def test_client_text_frame_is_masked_and_round_trips(self) -> None:
        frame = harness.encode_client_text_frame("hello", b"abcd")
        self.assertEqual(frame[:2], bytes((0x81, 0x85)))
        mask = frame[2:6]
        decoded = bytes(value ^ mask[index % 4] for index, value in enumerate(frame[6:]))
        self.assertEqual(decoded, b"hello")


class RestDocumentParsingTests(unittest.TestCase):
    def test_finds_run_by_exact_session(self) -> None:
        run = harness.find_run_for_session(
            {
                "runs": [
                    {"run_id": "run-a", "session_id": "other", "status": "running"},
                    {"run_id": "run-b", "session_id": "wanted", "status": "completed"},
                ]
            },
            "wanted",
        )
        self.assertEqual(run["run_id"], "run-b")

    def test_requires_non_user_result_during_hydration(self) -> None:
        user_only = {
            "session_id": "session-a",
            "messages": [
                {"role": "user", "content": "REQUEST RESULT"},
            ],
        }
        self.assertFalse(
            harness.transcript_has_result(user_only, "session-a", "REQUEST", "RESULT")
        )
        hydrated = json.loads(json.dumps(user_only))
        hydrated["messages"].append({"role": "assistant", "content": "RESULT"})
        self.assertTrue(
            harness.transcript_has_result(hydrated, "session-a", "REQUEST", "RESULT")
        )

    def test_matches_successful_tool_output_evidence(self) -> None:
        run = {
            "meta": {"run_id": "run-a", "status": "cancelled"},
            "events": [
                {
                    "tool": "shell",
                    "success": True,
                    "output_excerpt": "EVIDENCE_MARKER\n",
                }
            ],
        }
        self.assertTrue(
            harness.has_successful_tool_evidence(run, "shell", "EVIDENCE_MARKER")
        )
        self.assertFalse(harness.has_successful_tool_evidence(run, "shell", "missing"))

    def test_wire_payload_is_persisted_and_tool_scoped(self) -> None:
        payload = harness.message_payload("session-a", "safe prompt")
        self.assertIs(payload["temporary"], False)
        self.assertEqual(payload["allowed_tools"], ["task_plan", "shell"])
        self.assertEqual(payload["history_seed"], [{"role": "user", "content": "safe prompt"}])

    def test_cancellation_prompt_uses_only_bounded_safe_commands(self) -> None:
        prompt = harness.cancellation_prompt("REQUEST", "RESULT")
        wait_command = harness.cancellation_wait_command("RESULT")
        self.assertIn(wait_command, prompt)
        self.assertIn(f"time.sleep({harness.CANCEL_WAIT_SECONDS})", prompt)
        probe = harness.cancellation_process_probe_command("RESULT")
        self.assertNotIn("RESULT", probe)
        self.assertIn(base64.b64encode(b"RESULT").decode("ascii"), probe)
        self.assertIn("os.getpid()", probe)


class PollingTests(unittest.TestCase):
    def test_poll_retries_without_network_until_value_is_ready(self) -> None:
        clock = FakeClock()
        values = iter([None, None, {"run_id": "run-a"}])
        result = harness.poll_until(
            "fixture run",
            lambda: next(values),
            lambda value: value is not None,
            harness.Deadline(5, clock=clock.monotonic),
            1,
            sleep=clock.sleep,
        )
        self.assertEqual(result, {"run_id": "run-a"})
        self.assertEqual(clock.now, 2)

    def test_poll_reports_harness_deadline(self) -> None:
        clock = FakeClock()
        with self.assertRaisesRegex(harness.AcceptanceError, "deadline expired"):
            harness.poll_until(
                "never ready",
                lambda: None,
                lambda value: value is not None,
                harness.Deadline(2, clock=clock.monotonic),
                1,
                sleep=clock.sleep,
            )


if __name__ == "__main__":
    unittest.main()
