#!/usr/bin/env python3
"""Unit tests for the host-side model/tool catalogue harness."""

from __future__ import annotations

import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import tool_catalog_e2e as harness  # noqa: E402


MATRIX_PATH = Path(__file__).with_name("tool_catalog_matrix.json")


def load_matrix() -> dict:
    return json.loads(MATRIX_PATH.read_text(encoding="utf-8"))


def synthetic_live_tools(matrix: dict) -> list[dict]:
    """Build schema-shaped data sufficient to test matrix/live validation."""

    result = []
    for tool in matrix["tools"]:
        properties: dict[str, dict] = {}
        for case in tool["cases"]:
            for key in case["args"]:
                properties.setdefault(key, {"type": "string"})
        for selector in ("action", "operation"):
            values = sorted(
                {
                    case["args"][selector]
                    for case in tool["cases"]
                    if isinstance(case["args"].get(selector), str)
                }
            )
            if values:
                properties[selector] = {"type": "string", "enum": values}
        result.append(
            {
                "name": tool["name"],
                "parameters": {
                    "type": "object",
                    "properties": properties,
                    "required": [],
                },
            }
        )
    return result


class EventParsingTests(unittest.TestCase):
    def test_parses_text_event(self) -> None:
        event = harness.parse_ws_event('{"type":"tool_call","name":"shell"}')
        self.assertEqual(event.kind, "tool_call")
        self.assertFalse(event.terminal)

    def test_parses_binary_terminal_event(self) -> None:
        event = harness.parse_ws_event(b'{"type":"done","full_response":"ok"}')
        self.assertEqual(event.kind, "done")
        self.assertTrue(event.terminal)

    def test_rejects_invalid_json(self) -> None:
        with self.assertRaisesRegex(harness.HarnessError, "invalid WebSocket JSON"):
            harness.parse_ws_event("{")

    def test_rejects_non_object(self) -> None:
        with self.assertRaisesRegex(harness.HarnessError, "JSON object"):
            harness.parse_ws_event("[]")

    def test_rejects_missing_type(self) -> None:
        with self.assertRaisesRegex(harness.HarnessError, "missing"):
            harness.parse_ws_event('{"content":"orphan"}')


class MatrixContractTests(unittest.TestCase):
    def test_matrix_is_complete_and_action_expanded(self) -> None:
        matrix = load_matrix()
        cases = harness.validate_matrix(matrix)
        self.assertEqual(
            {tool["name"] for tool in matrix["tools"]},
            set(harness.EXPECTED_TOOL_NAMES),
        )
        self.assertEqual(len(matrix["tools"]), 57)
        self.assertGreater(len(cases), len(matrix["tools"]))

    def test_matrix_matches_schema_shaped_live_catalogue(self) -> None:
        matrix = load_matrix()
        cases = harness.validate_matrix(matrix)
        harness.validate_live_catalog(cases, synthetic_live_tools(matrix))

    def test_missing_tool_is_hard_failure(self) -> None:
        matrix = load_matrix()
        matrix["tools"].pop()
        with self.assertRaisesRegex(harness.HarnessError, "name mismatch"):
            harness.validate_matrix(matrix)

    def test_extra_live_tool_is_hard_failure(self) -> None:
        matrix = load_matrix()
        cases = harness.validate_matrix(matrix)
        live = synthetic_live_tools(matrix)
        live.append(
            {
                "name": "unexpected_tool",
                "parameters": {"properties": {}, "required": []},
            }
        )
        with self.assertRaisesRegex(harness.HarnessError, "live /api/tools mismatch"):
            harness.validate_live_catalog(cases, live)

    def test_new_live_action_is_hard_failure(self) -> None:
        matrix = load_matrix()
        cases = harness.validate_matrix(matrix)
        live = synthetic_live_tools(matrix)
        process = next(tool for tool in live if tool["name"] == "process")
        process["parameters"]["properties"]["action"]["enum"].append("new_action")
        with self.assertRaisesRegex(
            harness.HarnessError, "live schema advertises untested action"
        ):
            harness.validate_live_catalog(cases, live)

    def test_missing_required_action_is_hard_failure(self) -> None:
        matrix = load_matrix()
        process = next(tool for tool in matrix["tools"] if tool["name"] == "process")
        process["cases"] = [
            case for case in process["cases"] if case.get("action") != "kill"
        ]
        with self.assertRaisesRegex(harness.HarnessError, "missing required action"):
            harness.validate_matrix(matrix)

    def test_only_credential_tools_may_declare_configuration_block(self) -> None:
        matrix = load_matrix()
        shell = next(tool for tool in matrix["tools"] if tool["name"] == "shell")
        shell["cases"][0]["config_block_patterns"] = ["not configured"]
        with self.assertRaisesRegex(
            harness.HarnessError, "configuration-blocked is only allowed"
        ):
            harness.validate_matrix(matrix)

    def test_dependencies_are_topologically_ordered(self) -> None:
        cases = harness.validate_matrix(load_matrix())
        positions = {case["id"]: index for index, case in enumerate(cases)}
        for case in cases:
            for dependency in case.get("depends_on", []):
                self.assertLess(positions[dependency], positions[case["id"]])

    def test_host_redeploy_is_the_final_case(self) -> None:
        cases = harness.validate_matrix(load_matrix())
        self.assertEqual(cases[-1]["id"], "host.redeploy")
        self.assertTrue(cases[-1].get("require_gateway_recovery"))

    def test_schema_unknown_argument_is_hard_failure(self) -> None:
        matrix = load_matrix()
        cases = harness.validate_matrix(matrix)
        live = synthetic_live_tools(matrix)
        shell = next(tool for tool in live if tool["name"] == "shell")
        shell["parameters"]["properties"].pop("command")
        with self.assertRaisesRegex(harness.HarnessError, "absent from live schema"):
            harness.validate_live_catalog(cases, live)

    def test_matrix_never_resets_operator_git_state(self) -> None:
        encoded = json.dumps(load_matrix())
        self.assertNotIn("reset --hard", encoded)
        self.assertNotIn("checkout --", encoded)

    def test_rag_search_asserts_the_audit_marker(self) -> None:
        matrix = load_matrix()
        rag = next(tool for tool in matrix["tools"] if tool["name"] == "workspace_rag")
        search = next(case for case in rag["cases"] if case["id"] == "rag.search")
        self.assertIn("CATALOG_RAG_MARKER_", search["args"]["query"])
        self.assertTrue(search["output_patterns"])

    def test_delegate_exercises_both_remote_workers(self) -> None:
        matrix = load_matrix()
        delegate = next(tool for tool in matrix["tools"] if tool["name"] == "delegate")
        remote = [
            case
            for case in delegate["cases"]
            if case["id"].startswith("delegate.remote_")
        ]
        self.assertEqual(len(remote), 2)
        self.assertEqual(
            {case["args"]["agent"] for case in remote},
            {"${REMOTE_AGENT_PRIMARY}", "${REMOTE_AGENT_SECONDARY}"},
        )
        self.assertTrue(all(case.get("federation_peer_ids") for case in remote))


class CaptureAndResumeTests(unittest.TestCase):
    def test_recursive_and_typed_render(self) -> None:
        value = {
            "text": "${MARKER}",
            "enabled": {"$var": "ENABLED", "type": "bool"},
            "number": {"$var": "COUNT", "type": "int"},
            "data": {"$var": "DATA", "type": "json"},
        }
        rendered = harness.render(
            value,
            {
                "MARKER": "ok",
                "ENABLED": "true",
                "COUNT": "7",
                "DATA": '["a","b"]',
            },
        )
        self.assertEqual(
            rendered,
            {"text": "ok", "enabled": True, "number": 7, "data": ["a", "b"]},
        )

    def test_unresolved_placeholder_fails(self) -> None:
        with self.assertRaisesRegex(harness.HarnessError, "unresolved placeholder"):
            harness.render("${MISSING}", {})

    def test_extracts_regex_and_json_captures(self) -> None:
        captures = harness.extract_captures(
            [
                {"name": "ID", "json_path": "job.id"},
                {"name": "NAME", "regex": '"name":"([^"]+)"'},
            ],
            '{"job":{"id":42},"name":"audit"}',
        )
        self.assertEqual(captures, {"ID": "42", "NAME": "audit"})

    def test_checkpoint_resume_restores_outcomes_and_captures(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            checkpoint = harness.Checkpoint(Path(directory) / "run.jsonl")
            checkpoint.append(
                {
                    "record_type": "run_start",
                    "run_id": "run-a",
                    "variables": {"BASE": "one"},
                }
            )
            checkpoint.append(
                {
                    "record_type": "case_outcome",
                    "case_id": "memory.store",
                    "status": "passed",
                    "captures": {"MEMORY_ID": "two"},
                }
            )
            run_id, variables, outcomes = harness.load_resume_state(
                checkpoint.records()
            )
        self.assertEqual(run_id, "run-a")
        self.assertEqual(variables, {"BASE": "one", "MEMORY_ID": "two"})
        self.assertEqual(outcomes, {"memory.store": "passed"})

    def test_matrix_mutation_does_not_change_fixture(self) -> None:
        original = load_matrix()
        mutated = copy.deepcopy(original)
        mutated["tools"][0]["cases"][0]["id"] = "different"
        self.assertNotEqual(original, mutated)

    def test_cleanup_runs_after_a_failed_dependency(self) -> None:
        case = {
            "phase": "cleanup",
            "depends_on": ["process.output", "docker.logs"],
        }
        outcomes = {"process.output": "failed", "docker.logs": "dependency_failed"}
        self.assertEqual(harness.unresolved_dependencies(case, outcomes), [])

    def test_main_case_remains_blocked_by_failed_dependency(self) -> None:
        case = {"phase": "main", "depends_on": ["setup"]}
        self.assertEqual(
            harness.unresolved_dependencies(case, {"setup": "failed"}), ["setup"]
        )


if __name__ == "__main__":
    unittest.main()
