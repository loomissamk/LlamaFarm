#!/usr/bin/env python3
"""End-to-end audit of LlamaFarm's advertised model/tool loop.

The harness intentionally drives the same WebSocket chat path a user drives.  It
does not call tools through a test-only or production execution endpoint.

Quick start:

    python3 scripts/tests/tool_catalog_e2e.py \
      --base-url http://127.0.0.1:42617 \
      --checkpoint ~/.local/state/llamafarm/tool-catalog-e2e.jsonl

Resume an interrupted run (completed action checkpoints are not repeated):

    python3 scripts/tests/tool_catalog_e2e.py \
      --base-url http://127.0.0.1:42617 \
      --checkpoint ~/.local/state/llamafarm/tool-catalog-e2e.jsonl --resume

There is deliberately no model-turn or tool-result timeout.  Ctrl-C is the
operator-controlled stop mechanism; durable JSONL records make the next
``--resume`` continue at the first unfinished action.
"""

from __future__ import annotations

import argparse
import asyncio
import copy
import datetime as dt
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Mapping

try:
    import websockets
except ImportError as exc:  # pragma: no cover - environment diagnostic
    raise SystemExit(
        "The 'websockets' package is required: python3 -m pip install websockets"
    ) from exc


EXPECTED_TOOL_NAMES = (
    "cron_add",
    "cron_list",
    "cron_remove",
    "cron_update",
    "cron_run",
    "cron_runs",
    "memory_store",
    "memory_recall",
    "memory_forget",
    "schedule",
    "task_plan",
    "model_routing_config",
    "ollama_model",
    "proxy_config",
    "pushover",
    "sop_list",
    "sop_status",
    "sop_execute",
    "sop_approve",
    "sop_advance",
    "shell",
    "process",
    "git_operations",
    "docker",
    "package_manager",
    "service_control",
    "file_read",
    "file_write",
    "file_edit",
    "apply_patch",
    "glob_search",
    "content_search",
    "workspace_rag",
    "code_run",
    "packet_capture",
    "git_worktree",
    "browser",
    "http_request",
    "web_fetch",
    "web_search_tool",
    "pdf_read",
    "arxiv_search",
    "db_schema",
    "db_query",
    "screenshot",
    "image_info",
    "delegate",
    "delegate_coordination_status",
    "subagent_spawn",
    "subagent_list",
    "subagent_manage",
    "agents_list",
    "agents_send",
    "agents_inbox",
    "state_get",
    "state_set",
    "host_exec",
)

# Enum coverage is a contract independent of the model's wording.  Keep this
# source-side baseline so a matrix edit cannot hide a newly added action while
# validating against an older deployment.  validate_live_catalog() separately
# derives every top-level enum and oneOf branch from /api/tools and requires
# exact live coverage, so an expanded runtime schema cannot be silently skipped.
REQUIRED_ENUM_VALUES: Mapping[tuple[str, str], frozenset[str]] = {
    ("cron_add", "job_type"): frozenset({"shell", "agent"}),
    ("cron_add", "session_target"): frozenset({"isolated", "main"}),
    ("schedule", "action"): frozenset(
        {"create", "add", "once", "list", "get", "cancel", "remove", "pause", "resume"}
    ),
    ("task_plan", "action"): frozenset({"create", "add", "update", "list", "delete"}),
    ("model_routing_config", "action"): frozenset(
        {
            "get",
            "list_hints",
            "set_default",
            "upsert_scenario",
            "remove_scenario",
            "upsert_agent",
            "remove_agent",
        }
    ),
    ("ollama_model", "action"): frozenset(
        {"list", "pull", "delete", "show", "running"}
    ),
    ("proxy_config", "action"): frozenset(
        {"get", "set", "list_services", "apply_env", "clear_env", "disable"}
    ),
    ("process", "action"): frozenset({"spawn", "list", "output", "kill"}),
    ("git_operations", "operation"): frozenset(
        {
            "status",
            "diff",
            "log",
            "branch",
            "commit",
            "add",
            "checkout",
            "stash",
            "clone",
            "pull",
            "fetch",
            "push",
        }
    ),
    ("git_operations", "action"): frozenset({"push", "pop", "list", "drop"}),
    ("docker", "action"): frozenset(
        {
            "run",
            "exec",
            "ps",
            "stop",
            "rm",
            "kill",
            "logs",
            "build",
            "images",
            "pull_image",
            "rmi",
            "compose_up",
            "compose_down",
            "compose_restart",
            "compose_logs",
            "inspect",
            "stats",
        }
    ),
    ("package_manager", "operation"): frozenset(
        {"install", "remove", "upgrade", "list", "search", "hold"}
    ),
    # The runtime schema is executable-aware. These managers are part of the
    # bundled image contract; any additional manager advertised by a node is
    # discovered and required by validate_live_catalog().
    ("package_manager", "manager"): frozenset(
        {"apt", "apt-get", "pip", "cargo"}
    ),
    ("service_control", "operation"): frozenset(
        {
            "start",
            "stop",
            "restart",
            "reload",
            "enable",
            "disable",
            "status",
            "logs",
            "daemon-reload",
            "is-active",
            "is-enabled",
            "is-failed",
            "list-units",
        }
    ),
    ("host_exec", "action"): frozenset(
        {"health", "exec", "spawn", "status", "redeploy"}
    ),
    ("workspace_rag", "action"): frozenset({"status", "search"}),
    ("git_worktree", "action"): frozenset({"create", "list", "adopt", "discard"}),
    # The bundled rust-native backend advertises the DOM actions below.  An
    # explicitly configured computer-use backend advertises six additional
    # OS actions; live-schema validation will require their cases in a matrix
    # intended for that deployment.
    ("browser", "action"): frozenset(
        {
            "open",
            "snapshot",
            "click",
            "fill",
            "type",
            "get_text",
            "get_title",
            "get_url",
            "screenshot",
            "wait",
            "press",
            "hover",
            "scroll",
            "is_visible",
            "close",
            "find",
        }
    ),
    ("browser", "direction"): frozenset({"up", "down", "left", "right"}),
    ("browser", "by"): frozenset(
        {"role", "text", "label", "placeholder", "testid"}
    ),
    ("browser", "find_action"): frozenset(
        {"click", "fill", "text", "hover", "check"}
    ),
    ("content_search", "output_mode"): frozenset(
        {"content", "files_with_matches", "count"}
    ),
    ("code_run", "language"): frozenset(
        {"python", "javascript", "typescript", "c", "cpp", "go", "rust", "bash"}
    ),
    ("task_plan", "status"): frozenset(
        {"pending", "in_progress", "completed", "failed", "blocked", "skipped"}
    ),
    ("sop_advance", "status"): frozenset({"completed", "failed", "skipped"}),
    ("subagent_list", "status"): frozenset(
        {"running", "completed", "failed", "killed", "all"}
    ),
    ("subagent_manage", "action"): frozenset({"status", "kill"}),
}

# These enums enumerate configured resource identities, not behavioral
# branches. Driver behavior is covered by dedicated SQLite/Postgres/MySQL/
# MongoDB cases; requiring every operator connection name would make a static
# reversible matrix depend on unrelated private configuration.
RESOURCE_SELECTOR_ENUM_PROPERTIES = frozenset(
    {("db_schema", "connection"), ("db_query", "connection")}
)

# Every non-enum union currently advertised by the catalogue is a top-level
# string-or-array input. Keep a source baseline as well as live derivation so
# neither a stale runtime nor a matrix edit can silently erase a branch.
REQUIRED_ONE_OF_TYPES: Mapping[tuple[str, str], frozenset[str]] = {
    ("model_routing_config", "keywords"): frozenset({"string", "array"}),
    ("model_routing_config", "patterns"): frozenset({"string", "array"}),
    ("model_routing_config", "allowed_tools"): frozenset({"string", "array"}),
    ("proxy_config", "no_proxy"): frozenset({"string", "array"}),
    ("proxy_config", "services"): frozenset({"string", "array"}),
}

# A non-success tool_result may be an expected deployment-configuration result
# only when the feature truly needs an external credential.  All other
# configuration/policy/backend failures remain ordinary failures.
CREDENTIAL_BLOCKABLE_TOOLS = frozenset({"pushover", "web_search_tool"})
TERMINAL_EVENT_TYPES = frozenset({"done", "cancelled", "error"})
PLACEHOLDER_RE = re.compile(r"\$\{([A-Z][A-Z0-9_]*)\}")


class HarnessError(RuntimeError):
    """An actionable harness or contract failure."""


@dataclass(frozen=True)
class ParsedEvent:
    kind: str
    payload: dict[str, Any]

    @property
    def terminal(self) -> bool:
        return self.kind in TERMINAL_EVENT_TYPES


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def parse_ws_event(raw: str | bytes) -> ParsedEvent:
    """Parse and minimally validate one server WebSocket event."""

    if isinstance(raw, bytes):
        raw = raw.decode("utf-8")
    try:
        payload = json.loads(raw)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise HarnessError(f"invalid WebSocket JSON event: {exc}") from exc
    if not isinstance(payload, dict):
        raise HarnessError("WebSocket event must be a JSON object")
    kind = payload.get("type")
    if not isinstance(kind, str) or not kind:
        raise HarnessError("WebSocket event is missing non-empty string 'type'")
    return ParsedEvent(kind=kind, payload=payload)


def _matrix_action(case: Mapping[str, Any]) -> str | None:
    if isinstance(case.get("action"), str):
        return str(case["action"])
    args = case.get("args")
    if not isinstance(args, dict):
        return None
    value = args.get("action", args.get("operation"))
    return value if isinstance(value, str) and "${" not in value else None


def _matrix_enum_values(
    cases: Iterable[Mapping[str, Any]], property_name: str
) -> set[str]:
    values: set[str] = set()
    for case in cases:
        args = case.get("args")
        if not isinstance(args, dict):
            continue
        value = args.get(property_name)
        if isinstance(value, str) and "${" not in value:
            values.add(value)
    return values


def _json_schema_type(value: Any) -> str:
    if isinstance(value, bool):
        return "boolean"
    if isinstance(value, str):
        return "string"
    if isinstance(value, list):
        return "array"
    if isinstance(value, dict):
        if set(value) == {"$var", "type"}:
            declared = value.get("type")
            if declared == "int":
                return "integer"
            if declared in {"string", "bool"}:
                return {"string": "string", "bool": "boolean"}[declared]
        return "object"
    if isinstance(value, int):
        return "integer"
    if isinstance(value, float):
        return "number"
    if value is None:
        return "null"
    return "unknown"


def _matrix_property_types(
    cases: Iterable[Mapping[str, Any]], property_name: str
) -> set[str]:
    result: set[str] = set()
    for case in cases:
        args = case.get("args")
        if isinstance(args, dict) and property_name in args:
            result.add(_json_schema_type(args[property_name]))
    return result


def _one_of_types(property_schema: Mapping[str, Any]) -> set[str]:
    branches = property_schema.get("oneOf")
    if not isinstance(branches, list):
        return set()
    result: set[str] = set()
    for branch in branches:
        if not isinstance(branch, dict):
            continue
        branch_type = branch.get("type")
        if isinstance(branch_type, str):
            result.add(branch_type)
        elif isinstance(branch_type, list):
            result.update(
                value for value in branch_type if isinstance(value, str)
            )
    return result


def validate_matrix(matrix: Any) -> list[dict[str, Any]]:
    """Validate static matrix shape, catalogue names, enums, and union branches."""

    if not isinstance(matrix, dict) or matrix.get("version") != 1:
        raise HarnessError("matrix must be an object with version=1")
    tools = matrix.get("tools")
    if not isinstance(tools, list):
        raise HarnessError("matrix.tools must be a list")

    names = [entry.get("name") for entry in tools if isinstance(entry, dict)]
    if len(names) != len(tools) or any(not isinstance(name, str) for name in names):
        raise HarnessError("every matrix tool entry needs a string name")
    duplicates = sorted({name for name in names if names.count(name) > 1})
    if duplicates:
        raise HarnessError(f"duplicate matrix tool entries: {duplicates}")

    expected = set(EXPECTED_TOOL_NAMES)
    actual = set(names)
    missing = sorted(expected - actual)
    extra = sorted(actual - expected)
    if missing or extra:
        raise HarnessError(
            f"matrix/catalogue name mismatch: missing={missing}, extra={extra}"
        )

    case_ids: set[str] = set()
    ordered_cases: list[dict[str, Any]] = []
    for tool in tools:
        name = tool["name"]
        cases = tool.get("cases")
        if not isinstance(cases, list) or not cases:
            raise HarnessError(f"matrix tool {name!r} needs at least one case")
        for case in cases:
            if not isinstance(case, dict):
                raise HarnessError(f"{name}: every case must be an object")
            case_id = case.get("id")
            if not isinstance(case_id, str) or not case_id:
                raise HarnessError(f"{name}: every case needs a non-empty id")
            if case_id in case_ids:
                raise HarnessError(f"duplicate matrix case id: {case_id}")
            case_ids.add(case_id)
            if not isinstance(case.get("args"), dict):
                raise HarnessError(f"{case_id}: args must be an object")
            phase = case.get("phase", "main")
            if phase not in {"setup", "main", "cleanup"}:
                raise HarnessError(f"{case_id}: invalid phase {phase!r}")
            patterns = case.get("config_block_patterns", [])
            if patterns and name not in CREDENTIAL_BLOCKABLE_TOOLS:
                raise HarnessError(
                    f"{case_id}: configuration-blocked is only allowed for "
                    f"{sorted(CREDENTIAL_BLOCKABLE_TOOLS)}"
                )
            if not isinstance(patterns, list) or not all(
                isinstance(item, str) and item for item in patterns
            ):
                raise HarnessError(
                    f"{case_id}: config_block_patterns must be strings"
                )
            item = copy.deepcopy(case)
            item["tool"] = name
            item["phase"] = phase
            ordered_cases.append(item)

        for (tool_name, property_name), required_values in REQUIRED_ENUM_VALUES.items():
            if tool_name != name:
                continue
            actual_values = _matrix_enum_values(cases, property_name)
            if required_values - actual_values:
                raise HarnessError(
                    f"{name}: missing required {property_name} cases "
                    f"{sorted(required_values - actual_values)}"
                )
        for (tool_name, property_name), required_types in REQUIRED_ONE_OF_TYPES.items():
            if tool_name != name:
                continue
            actual_types = _matrix_property_types(cases, property_name)
            if required_types - actual_types:
                raise HarnessError(
                    f"{name}: missing required {property_name} oneOf types "
                    f"{sorted(required_types - actual_types)}"
                )

    for case in ordered_cases:
        deps = case.get("depends_on", [])
        if not isinstance(deps, list) or not all(isinstance(dep, str) for dep in deps):
            raise HarnessError(f"{case['id']}: depends_on must be a list of case ids")
        unknown = sorted(set(deps) - case_ids)
        if unknown:
            raise HarnessError(f"{case['id']}: unknown dependencies {unknown}")
        captures = case.get("captures", [])
        if not isinstance(captures, list):
            raise HarnessError(f"{case['id']}: captures must be a list")
        for capture in captures:
            if not isinstance(capture, dict) or not isinstance(
                capture.get("name"), str
            ):
                raise HarnessError(f"{case['id']}: invalid capture entry")
            has_regex = isinstance(capture.get("regex"), str)
            has_json_path = isinstance(capture.get("json_path"), str)
            if has_regex == has_json_path:
                raise HarnessError(
                    f"{case['id']}: capture needs exactly one of regex/json_path"
                )
    # Stable dependency order.  This permits a reversible fixture chain to
    # cross tool entries (for example git commit -> file edit -> git stash)
    # without duplicating a tool entry in the catalogue matrix.
    by_id = {case["id"]: case for case in ordered_cases}
    original_index = {case["id"]: index for index, case in enumerate(ordered_cases)}
    remaining = set(by_id)
    resolved: set[str] = set()
    sorted_cases: list[dict[str, Any]] = []
    phase_rank = {"setup": 0, "main": 1, "cleanup": 2}
    while remaining:
        ready = [
            case_id
            for case_id in remaining
            if set(by_id[case_id].get("depends_on", [])) <= resolved
        ]
        if not ready:
            raise HarnessError(
                f"matrix dependency cycle among {sorted(remaining)}"
            )
        ready.sort(
            key=lambda case_id: (
                phase_rank[by_id[case_id]["phase"]],
                original_index[case_id],
            )
        )
        case_id = ready[0]
        remaining.remove(case_id)
        resolved.add(case_id)
        sorted_cases.append(by_id[case_id])
    return sorted_cases


def validate_live_catalog(
    matrix_cases: Iterable[Mapping[str, Any]], tools: list[dict[str, Any]]
) -> None:
    """Hard-fail live name or basic per-case schema mismatch."""

    by_name: dict[str, dict[str, Any]] = {}
    for tool in tools:
        if not isinstance(tool, dict) or not isinstance(tool.get("name"), str):
            raise HarnessError("/api/tools returned an invalid tool entry")
        by_name[tool["name"]] = tool
    live = set(by_name)
    expected = set(EXPECTED_TOOL_NAMES)
    if live != expected:
        raise HarnessError(
            "live /api/tools mismatch: "
            f"missing={sorted(expected - live)}, extra={sorted(live - expected)}"
        )

    case_list = list(matrix_cases)
    for case in case_list:
        schema = by_name[case["tool"]].get("parameters")
        if not isinstance(schema, dict):
            raise HarnessError(f"{case['tool']}: missing parameters schema")
        properties = schema.get("properties", {})
        if not isinstance(properties, dict):
            raise HarnessError(f"{case['tool']}: properties is not an object")
        unknown = sorted(set(case["args"]) - set(properties))
        if unknown:
            raise HarnessError(f"{case['id']}: args absent from live schema: {unknown}")
        required = schema.get("required", [])
        if required is None:
            required = []
        missing = sorted(set(required) - set(case["args"]))
        if missing:
            raise HarnessError(
                f"{case['id']}: missing live-schema required args {missing}"
            )
        for property_name, value in case["args"].items():
            property_schema = properties.get(property_name)
            if not isinstance(property_schema, dict):
                continue
            enum = property_schema.get("enum")
            if (
                isinstance(enum, list)
                and isinstance(value, str)
                and "${" not in value
                and value not in enum
            ):
                raise HarnessError(
                    f"{case['id']}: {property_name} {value!r} absent from "
                    f"live enum {enum}"
                )

    for tool_name, tool in by_name.items():
        schema = tool.get("parameters")
        properties = schema.get("properties", {}) if isinstance(schema, dict) else {}
        tool_cases = [case for case in case_list if case["tool"] == tool_name]
        for property_name, property_schema in properties.items():
            if not isinstance(property_schema, dict):
                continue
            union_types = _one_of_types(property_schema)
            if union_types:
                covered_types = _matrix_property_types(tool_cases, property_name)
                missing_types = sorted(union_types - covered_types)
                if missing_types:
                    raise HarnessError(
                        f"{tool_name}: live schema advertises untested "
                        f"{property_name} oneOf types {missing_types}"
                    )
            enum = property_schema.get("enum")
            if not isinstance(enum, list):
                continue
            if (tool_name, property_name) in RESOURCE_SELECTOR_ENUM_PROPERTIES:
                continue
            if (
                tool_name == "browser"
                and property_name == "button"
                and "mouse_click"
                not in properties.get("action", {}).get("enum", [])
            ):
                # Mouse button variants are genuinely unavailable on the
                # deployed rust-native backend. An explicit computer_use
                # backend advertises mouse_click and must add real cases.
                continue
            advertised = {
                value for value in enum if isinstance(value, str) and value
            }
            covered = _matrix_enum_values(tool_cases, property_name)
            missing = sorted(advertised - covered)
            if missing:
                raise HarnessError(
                    f"{tool_name}: live schema advertises untested "
                    f"{property_name} values {missing}"
                )


def render(value: Any, variables: Mapping[str, str]) -> Any:
    """Recursively expand strict ${NAME} placeholders."""

    if isinstance(value, dict):
        if set(value) == {"$var", "type"}:
            name = value["$var"]
            kind = value["type"]
            if not isinstance(name, str) or name not in variables:
                raise HarnessError(f"unresolved typed variable {name!r}")
            raw = variables[name]
            if kind == "bool":
                normalized = raw.strip().lower()
                if normalized not in {"true", "false", "1", "0"}:
                    raise HarnessError(f"{name}={raw!r} is not a boolean")
                return normalized in {"true", "1"}
            if kind == "int":
                try:
                    return int(raw)
                except ValueError as exc:
                    raise HarnessError(f"{name}={raw!r} is not an integer") from exc
            if kind == "json":
                try:
                    return json.loads(raw)
                except json.JSONDecodeError as exc:
                    raise HarnessError(f"{name} is not JSON") from exc
            if kind == "string":
                return raw
            raise HarnessError(f"unsupported typed variable kind {kind!r}")
        return {key: render(item, variables) for key, item in value.items()}
    if isinstance(value, list):
        return [render(item, variables) for item in value]
    if not isinstance(value, str):
        return value

    def replace(match: re.Match[str]) -> str:
        name = match.group(1)
        if name not in variables or variables[name] == "":
            raise HarnessError(f"unresolved placeholder ${{{name}}}")
        return variables[name]

    rendered = PLACEHOLDER_RE.sub(replace, value)
    if PLACEHOLDER_RE.search(rendered):
        raise HarnessError(f"recursive unresolved placeholder in {value!r}")
    return rendered


def _json_value(output: str, path: str) -> Any:
    try:
        value: Any = json.loads(output)
    except json.JSONDecodeError:
        starts = [idx for idx in (output.find("{"), output.find("[")) if idx >= 0]
        if not starts:
            raise HarnessError("capture output does not contain JSON")
        decoder = json.JSONDecoder()
        value, _ = decoder.raw_decode(output[min(starts) :])
    for part in path.split("."):
        if not part:
            continue
        if isinstance(value, list):
            try:
                value = value[int(part)]
            except (ValueError, IndexError) as exc:
                raise HarnessError(f"invalid JSON capture path {path!r}") from exc
        elif isinstance(value, dict) and part in value:
            value = value[part]
        else:
            raise HarnessError(f"JSON capture path {path!r} not found")
    return value


def extract_captures(
    specs: Iterable[Mapping[str, Any]], output: str
) -> dict[str, str]:
    captures: dict[str, str] = {}
    for spec in specs:
        name = str(spec["name"])
        if "regex" in spec:
            match = re.search(str(spec["regex"]), output, re.MULTILINE | re.DOTALL)
            if not match:
                raise HarnessError(
                    f"capture {name}: regex did not match tool output"
                )
            group = int(spec.get("group", 1))
            value: Any = match.group(group)
        else:
            value = _json_value(output, str(spec["json_path"]))
        if value is None or isinstance(value, (dict, list)):
            value = json.dumps(value, separators=(",", ":"))
        elif isinstance(value, bool):
            value = "true" if value else "false"
        captures[name] = str(value)
    return captures


class Checkpoint:
    def __init__(self, path: Path):
        self.path = path
        self.path.parent.mkdir(parents=True, exist_ok=True)

    def append(self, record: Mapping[str, Any]) -> None:
        payload = dict(record)
        payload.setdefault("timestamp", utc_now())
        with self.path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(payload, sort_keys=True) + "\n")
            handle.flush()
            os.fsync(handle.fileno())

    def records(self) -> list[dict[str, Any]]:
        if not self.path.exists():
            return []
        records: list[dict[str, Any]] = []
        for number, line in enumerate(
            self.path.read_text(encoding="utf-8").splitlines(), 1
        ):
            if not line.strip():
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                raise HarnessError(
                    f"{self.path}:{number}: invalid checkpoint JSON"
                ) from exc
            if not isinstance(record, dict):
                raise HarnessError(
                    f"{self.path}:{number}: checkpoint record is not an object"
                )
            records.append(record)
        return records


def load_resume_state(
    records: Iterable[Mapping[str, Any]],
) -> tuple[str | None, dict[str, str], dict[str, str]]:
    run_id: str | None = None
    variables: dict[str, str] = {}
    outcomes: dict[str, str] = {}
    for record in records:
        if record.get("record_type") == "run_start":
            candidate = record.get("run_id")
            if isinstance(candidate, str):
                run_id = candidate
            stored = record.get("variables")
            if isinstance(stored, dict):
                variables.update(
                    {str(key): str(value) for key, value in stored.items()}
                )
        if record.get("record_type") == "case_outcome":
            case_id = record.get("case_id")
            status = record.get("status")
            if isinstance(case_id, str) and isinstance(status, str):
                outcomes[case_id] = status
            stored = record.get("captures")
            if isinstance(stored, dict):
                variables.update(
                    {str(key): str(value) for key, value in stored.items()}
                )
    return run_id, variables, outcomes


def unresolved_dependencies(
    case: Mapping[str, Any], outcomes: Mapping[str, str]
) -> list[str]:
    """Return dependency ids that prevent an action from starting.

    Cleanup edges only impose ordering.  Once the prerequisite action has a
    terminal checkpoint, cleanup is attempted even if that prerequisite failed.
    """

    dependencies = case.get("depends_on", [])
    if case.get("phase") == "cleanup":
        return [dependency for dependency in dependencies if dependency not in outcomes]
    return [
        dependency
        for dependency in dependencies
        if outcomes.get(dependency) not in {"passed", "configuration_blocked"}
    ]


def fetch_tools(base_url: str, token: str | None) -> list[dict[str, Any]]:
    url = base_url.rstrip("/") + "/api/tools"
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        # This bounds only the catalogue HTTP request, never a model/tool turn.
        with urllib.request.urlopen(request, timeout=15) as response:
            payload = json.load(response)
    except (urllib.error.URLError, json.JSONDecodeError) as exc:
        raise HarnessError(f"failed to fetch {url}: {exc}") from exc
    tools = payload.get("tools") if isinstance(payload, dict) else None
    if not isinstance(tools, list):
        raise HarnessError(f"{url} response is missing a tools list")
    return tools


def fetch_remote_workers(
    base_url: str, token: str | None, minimum: int
) -> list[dict[str, str]]:
    url = base_url.rstrip("/") + "/api/federation/peers"
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            payload = json.load(response)
    except (urllib.error.URLError, json.JSONDecodeError) as exc:
        raise HarnessError(f"failed to fetch {url}: {exc}") from exc
    if not isinstance(payload, dict) or payload.get("enabled") is not True:
        raise HarnessError("federation must be enabled for the full catalogue audit")

    distinct: dict[str, dict[str, str]] = {}
    for peer in payload.get("peers", []):
        if not isinstance(peer, dict) or peer.get("online") is not True:
            continue
        if peer.get("allow_remote_subagents") is not True:
            continue
        if peer.get("assigned_role") not in {"worker", "both"}:
            continue
        if peer.get("role_support") not in {"worker", "both"}:
            continue
        node_id = peer.get("node_id")
        peer_id = peer.get("peer_id")
        agent = peer.get("delegate_agent")
        if not all(isinstance(value, str) and value for value in (node_id, peer_id, agent)):
            continue
        distinct.setdefault(
            node_id,
            {"node_id": node_id, "peer_id": peer_id, "agent": agent},
        )

    workers = sorted(distinct.values(), key=lambda peer: peer["node_id"])
    if len(workers) < minimum:
        raise HarnessError(
            f"expected at least {minimum} distinct online federation workers; "
            f"got {len(workers)}"
        )
    return workers


def api_json(
    base_url: str,
    token: str | None,
    method: str,
    path: str,
    body: Mapping[str, Any] | None = None,
) -> Any:
    headers = {"Accept": "application/json"}
    payload = None
    if token:
        headers["Authorization"] = f"Bearer {token}"
    if body is not None:
        headers["Content-Type"] = "application/json"
        payload = json.dumps(body, separators=(",", ":")).encode("utf-8")
    request = urllib.request.Request(
        base_url.rstrip("/") + path,
        data=payload,
        headers=headers,
        method=method,
    )
    try:
        with urllib.request.urlopen(request, timeout=15) as response:
            return json.load(response)
    except urllib.error.HTTPError as exc:
        detail = exc.read().decode("utf-8", errors="replace")[-800:]
        raise HarnessError(f"{method} {path} returned HTTP {exc.code}: {detail}") from exc
    except (urllib.error.URLError, json.JSONDecodeError) as exc:
        raise HarnessError(f"{method} {path} failed: {exc}") from exc


def configure_database_fixtures(
    base_url: str, token: str | None, variables: Mapping[str, str]
) -> None:
    fixture_root = variables["FIXTURE_ROOT"]
    fixtures = (
        {
            "name": variables["DB_SQLITE_CONNECTION"],
            "driver": "sqlite",
            "uri": f"/llamafarm-data/workspace/{fixture_root}/catalog.sqlite",
            "database": None,
        },
        {
            "name": variables["DB_POSTGRES_CONNECTION"],
            "driver": "postgres",
            "uri": "postgresql://catalog:catalog@127.0.0.1:5432/catalog",
            "database": "catalog",
        },
        {
            "name": variables["DB_MYSQL_CONNECTION"],
            "driver": "mysql",
            "uri": "mysql://catalog:catalog@127.0.0.1:3306/catalog",
            "database": "catalog",
        },
        {
            "name": variables["DB_MONGODB_CONNECTION"],
            "driver": "mongodb",
            "uri": "mongodb://127.0.0.1:27017",
            "database": "catalog",
        },
    )
    existing_payload = api_json(
        base_url, token, "GET", "/api/db/connections"
    )
    existing = {
        item.get("name"): item
        for item in existing_payload.get("connections", [])
        if isinstance(item, dict) and isinstance(item.get("name"), str)
    }
    for fixture in fixtures:
        current = existing.get(fixture["name"])
        if current is not None:
            if current.get("driver") != fixture["driver"]:
                raise HarnessError(
                    f"database fixture {fixture['name']!r} exists with the wrong driver"
                )
            continue
        result = api_json(
            base_url,
            token,
            "POST",
            "/api/db/connections",
            {
                **fixture,
                "label": f"Catalogue E2E {fixture['driver']}",
                "read_only": True,
                "max_rows": 20,
            },
        )
        if not isinstance(result, dict) or result.get("status") != "ok":
            raise HarnessError(
                f"failed to add database fixture {fixture['name']!r}"
            )


def remove_database_fixtures(
    base_url: str, token: str | None, variables: Mapping[str, str]
) -> None:
    for key in (
        "DB_SQLITE_CONNECTION",
        "DB_POSTGRES_CONNECTION",
        "DB_MYSQL_CONNECTION",
        "DB_MONGODB_CONNECTION",
    ):
        name = variables.get(key)
        if not name:
            continue
        path = f"/api/db/connections/{urllib.parse.quote(name, safe='')}"
        try:
            api_json(base_url, token, "DELETE", path)
        except HarnessError as exc:
            if "HTTP 404" not in str(exc):
                raise


def fetch_health_snapshot(base_url: str, token: str | None) -> dict[str, Any]:
    url = base_url.rstrip("/") + "/health"
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = urllib.request.Request(url, headers=headers)
    with urllib.request.urlopen(request, timeout=10) as response:
        payload = json.load(response)
    if not isinstance(payload, dict) or payload.get("status") != "ok":
        raise HarnessError(f"{url} did not report status=ok")
    return payload


def _health_identity(snapshot: Mapping[str, Any]) -> tuple[int | None, int | None]:
    runtime = snapshot.get("runtime")
    if not isinstance(runtime, dict):
        return None, None
    pid = runtime.get("pid")
    uptime = runtime.get("uptime_seconds")
    return (
        pid if isinstance(pid, int) else None,
        uptime if isinstance(uptime, int) else None,
    )


async def wait_for_gateway_recovery(
    base_url: str,
    token: str | None,
    before: Mapping[str, Any],
) -> dict[str, Any]:
    """Wait without a total deadline for a redeploy to replace and recover."""

    before_pid, before_uptime = _health_identity(before)
    observed_unavailable = False
    while True:
        try:
            current = await asyncio.to_thread(fetch_health_snapshot, base_url, token)
        except (OSError, urllib.error.URLError, json.JSONDecodeError, HarnessError):
            observed_unavailable = True
            await asyncio.sleep(1)
            continue

        current_pid, current_uptime = _health_identity(current)
        restarted = (
            before_pid is not None
            and current_pid is not None
            and current_pid != before_pid
        ) or (
            before_uptime is not None
            and current_uptime is not None
            and current_uptime + 2 < before_uptime
        )
        if observed_unavailable or restarted:
            return current
        await asyncio.sleep(1)


def ws_url(base_url: str) -> str:
    parsed = urllib.parse.urlsplit(base_url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc:
        raise HarnessError("--base-url must be an http(s) origin")
    scheme = "wss" if parsed.scheme == "https" else "ws"
    return urllib.parse.urlunsplit((scheme, parsed.netloc, "/ws/chat", "", ""))


def forced_prompt(tool: str, args: Mapping[str, Any], marker: str) -> str:
    encoded = json.dumps(args, separators=(",", ":"), ensure_ascii=False)
    return (
        "Automated catalogue contract check. Call exactly one tool named "
        f"`{tool}` now, with exactly this JSON object as arguments: {encoded}. "
        "Do not call task_plan or any other tool. Do not rewrite, omit, or add "
        f"arguments. After its result, reply only `{marker}`."
    )


async def execute_case(
    *,
    base_url: str,
    token: str | None,
    run_id: str,
    case: Mapping[str, Any],
    args: Mapping[str, Any],
    variables: Mapping[str, str],
    checkpoint: Checkpoint,
) -> tuple[str, dict[str, str], str]:
    """Run one forced tool action and return (status, captures, detail)."""

    marker = f"CATALOG_E2E_{run_id[:8]}_{case['id']}"
    session_id = f"catalog-e2e-{run_id}-{case['id']}"
    request = {
        "type": "message",
        "content": forced_prompt(str(case["tool"]), args, marker),
        "session_id": session_id,
        "temporary": True,
    }
    selected_peer_ids = case.get("federation_peer_ids")
    if selected_peer_ids is not None:
        rendered_peer_ids = render(selected_peer_ids, variables)
        if not isinstance(rendered_peer_ids, list) or not all(
            isinstance(peer_id, str) and peer_id for peer_id in rendered_peer_ids
        ):
            raise HarnessError(
                f"{case['id']}: federation_peer_ids must render to non-empty strings"
            )
        request["federation_peer_ids"] = rendered_peer_ids
    headers = {"Authorization": f"Bearer {token}"} if token else None
    checkpoint.append(
        {
            "record_type": "case_start",
            "run_id": run_id,
            "case_id": case["id"],
            "tool": case["tool"],
            "action": _matrix_action(case),
            "phase": case["phase"],
            "session_id": session_id,
            "args": args,
        }
    )

    tool_calls: list[str] = []
    tool_results: list[dict[str, Any]] = []
    terminal: ParsedEvent | None = None
    sequence = 0
    connect_kwargs: dict[str, Any] = {
        "open_timeout": 15,
        "ping_timeout": None,
        "close_timeout": 10,
        "max_size": None,
    }
    if headers:
        connect_kwargs["additional_headers"] = headers

    # There is intentionally no asyncio.wait_for() and no recv timeout here.
    disconnected_after_result = False
    try:
        async with websockets.connect(ws_url(base_url), **connect_kwargs) as socket:
            await socket.send(json.dumps(request))
            while terminal is None:
                event = parse_ws_event(await socket.recv())
                sequence += 1
                checkpoint.append(
                    {
                        "record_type": "ws_event",
                        "run_id": run_id,
                        "case_id": case["id"],
                        "sequence": sequence,
                        "event": event.payload,
                    }
                )
                if event.kind == "tool_call":
                    name = event.payload.get("name")
                    tool_calls.append(name if isinstance(name, str) else "")
                elif event.kind == "tool_result":
                    tool_results.append(event.payload)
                if event.terminal:
                    terminal = event
    except websockets.ConnectionClosed:
        disconnected_after_result = bool(
            case.get("require_gateway_recovery") and tool_results
        )
        if not disconnected_after_result:
            raise

    expected_tool = case["tool"]
    relevant_calls = [name for name in tool_calls if name != "task_plan" or expected_tool == "task_plan"]
    if relevant_calls != [expected_tool]:
        return (
            "failed",
            {},
            f"expected one {expected_tool} tool_call (auxiliary task_plan is allowed); "
            f"observed {tool_calls}",
        )
    matching = [
        result for result in tool_results if result.get("name") == expected_tool
    ]
    relevant_results = [
        result
        for result in tool_results
        if result.get("name") != "task_plan" or expected_tool == "task_plan"
    ]
    if len(matching) != 1 or len(relevant_results) != 1:
        return (
            "failed",
            {},
            f"expected one matching tool_result; observed {tool_results}",
        )
    result = matching[0]
    output = result.get("output")
    if not isinstance(output, str):
        output = json.dumps(output, sort_keys=True)

    if not bool(result.get("success")):
        combined = "\n".join(
            str(value)
            for value in (
                output,
                result.get("error"),
                terminal.payload.get("message") if terminal else None,
            )
            if value
        )
        for pattern in case.get("config_block_patterns", []):
            if re.search(pattern, combined, re.IGNORECASE | re.DOTALL):
                return "configuration_blocked", {}, combined
        return "failed", {}, combined or "tool_result success=false"
    if (terminal is None or terminal.kind != "done") and not disconnected_after_result:
        return (
            "failed",
            {},
            f"tool succeeded but terminal event was {terminal.kind if terminal else None}",
        )
    for expected in case.get("output_patterns", []):
        pattern = str(render(expected, variables))
        if not re.search(pattern, output, re.IGNORECASE | re.DOTALL):
            return "failed", {}, f"output did not match {pattern!r}: {output}"
    try:
        captures = extract_captures(case.get("captures", []), output)
    except HarnessError as exc:
        return "failed", {}, str(exc)
    return "passed", captures, output


async def run(args: argparse.Namespace) -> int:
    matrix_path = Path(args.matrix).resolve()
    try:
        matrix = json.loads(matrix_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f"cannot load matrix {matrix_path}: {exc}") from exc
    cases = validate_matrix(matrix)
    catalog_cases = cases
    tools = fetch_tools(args.base_url, args.token)
    validate_live_catalog(cases, tools)

    if args.validate_only:
        print(f"PASS: matrix and live catalogue agree on {len(tools)} tools")
        return 0

    selected = set(args.only)
    if selected:
        unknown = selected - set(EXPECTED_TOOL_NAMES)
        if unknown:
            raise HarnessError(f"unknown --only tools: {sorted(unknown)}")
    require_remote_workers = not args.cleanup_only and (
        not selected or "delegate" in selected
    )
    remote_workers: list[dict[str, str]] = []
    if require_remote_workers:
        if args.expected_online_peers < 1:
            raise HarnessError("--expected-online-peers must be at least 1")
        remote_workers = fetch_remote_workers(
            args.base_url, args.token, args.expected_online_peers
        )

    checkpoint = Checkpoint(Path(args.checkpoint).expanduser().resolve())
    existing = checkpoint.records()
    if existing and not args.resume:
        raise HarnessError(
            f"checkpoint already exists: {checkpoint.path}; use --resume or a new path"
        )
    if args.resume and not existing:
        raise HarnessError(f"--resume checkpoint does not exist: {checkpoint.path}")

    prior_run_id, prior_variables, outcomes = load_resume_state(existing)
    run_id = prior_run_id or uuid.uuid4().hex
    http_fixture_port = os.environ.get("LLAMAFARM_E2E_HTTP_PORT", "18765")
    variables = {
        "RUN_ID": run_id,
        "FIXTURE_ROOT": f".llamafarm-tool-audit/{run_id}",
        "MEMORY_KEY": f"catalog_e2e_{run_id}",
        "STATE_KEY": f"catalog_e2e_{run_id}",
        "OLLAMA_TEST_MODEL": os.environ.get(
            "LLAMAFARM_E2E_OLLAMA_MODEL", "qwen3.5:9b"
        ),
        "BROWSER_TEST_URL": os.environ.get(
            "LLAMAFARM_E2E_BROWSER_URL",
            f"http://127.0.0.1:{http_fixture_port}/form",
        ),
        "BROWSER_SCREENSHOT_FILE": f".llamafarm-tool-audit/{run_id}/browser.png",
        "DB_SQLITE_CONNECTION": os.environ.get(
            "LLAMAFARM_E2E_DB_SQLITE_CONNECTION",
            f"catalog-e2e-sqlite-{run_id[:8]}",
        ),
        "DB_POSTGRES_CONNECTION": os.environ.get(
            "LLAMAFARM_E2E_DB_POSTGRES_CONNECTION",
            f"catalog-e2e-postgres-{run_id[:8]}",
        ),
        "DB_MYSQL_CONNECTION": os.environ.get(
            "LLAMAFARM_E2E_DB_MYSQL_CONNECTION",
            f"catalog-e2e-mysql-{run_id[:8]}",
        ),
        "DB_MONGODB_CONNECTION": os.environ.get(
            "LLAMAFARM_E2E_DB_MONGODB_CONNECTION",
            f"catalog-e2e-mongodb-{run_id[:8]}",
        ),
        "DB_SQLITE_QUERY": "SELECT 'CATALOG_DB_SQLITE_OK' AS marker",
        "DB_POSTGRES_QUERY": "SELECT 'CATALOG_DB_POSTGRES_OK' AS marker",
        "DB_MYSQL_QUERY": "SELECT 'CATALOG_DB_MYSQL_OK' AS marker",
        "DB_MONGODB_QUERY": json.dumps(
            {
                "collection": "catalog_e2e",
                "filter": {"marker": "CATALOG_DB_MONGODB_OK"},
                "projection": {"marker": 1, "_id": 0},
                "limit": 1,
            },
            separators=(",", ":"),
        ),
        "SOP_NAME": os.environ.get(
            "LLAMAFARM_E2E_SOP_NAME", "catalog-e2e"
        ),
        "HOST_CWD": os.environ.get("LLAMAFARM_E2E_HOST_CWD", "/tmp"),
        "HTTP_FIXTURE_PORT": os.environ.get(
            "LLAMAFARM_E2E_HTTP_PORT", http_fixture_port
        ),
        "GIT_DAEMON_PORT": os.environ.get(
            "LLAMAFARM_E2E_GIT_DAEMON_PORT", "19418"
        ),
        "GIT_AUDIT_BRANCH": f"catalog-e2e-{run_id[:12]}",
        "WORKTREE_ADOPT_NAME": f"catalog-e2e-adopt-{run_id[:12]}",
        "WORKTREE_DISCARD_NAME": f"catalog-e2e-discard-{run_id[:12]}",
        "SCREENSHOT_FILE": f"catalog-e2e-{run_id[:12]}.png",
        "DOCKER_TEST_IMAGE": os.environ.get(
            "LLAMAFARM_E2E_DOCKER_IMAGE", "alpine:3.20"
        ),
        "DOCKER_STOP_CONTAINER": f"catalog-e2e-stop-{run_id[:12]}",
        "DOCKER_RM_CONTAINER": f"catalog-e2e-rm-{run_id[:12]}",
        "DOCKER_KILL_CONTAINER": f"catalog-e2e-kill-{run_id[:12]}",
        "DOCKER_AUDIT_IMAGE": f"catalog-e2e:{run_id[:12]}",
        "RUNTIME_CONTAINER": os.environ.get(
            "LLAMAFARM_E2E_RUNTIME_CONTAINER", "LlamaFarm"
        ),
        "DB_POSTGRES_CONTAINER": f"catalog-e2e-postgres-{run_id[:12]}",
        "DB_MYSQL_CONTAINER": f"catalog-e2e-mysql-{run_id[:12]}",
        "DB_MONGODB_CONTAINER": f"catalog-e2e-mongodb-{run_id[:12]}",
        "DB_POSTGRES_IMAGE": os.environ.get(
            "LLAMAFARM_E2E_POSTGRES_IMAGE", "postgres:17-alpine"
        ),
        "DB_MYSQL_IMAGE": os.environ.get(
            "LLAMAFARM_E2E_MYSQL_IMAGE", "mysql:8.4"
        ),
        "DB_MONGODB_IMAGE": os.environ.get(
            "LLAMAFARM_E2E_MONGODB_IMAGE", "mongo:8"
        ),
        "OLLAMA_AUDIT_MODEL": os.environ.get(
            "LLAMAFARM_E2E_OLLAMA_AUDIT_MODEL", "smollm2:135m"
        ),
        "OLLAMA_DELETE_MODEL": f"catalog-e2e-delete:{run_id[:12]}",
        "SERVICE_UNIT": f"llamafarm-catalog-e2e-{run_id[:12]}.service",
        "SERVICE_FAIL_UNIT": f"llamafarm-catalog-e2e-fail-{run_id[:12]}.service",
    }
    variables.update(prior_variables)
    if remote_workers:
        secondary = remote_workers[1] if len(remote_workers) > 1 else remote_workers[0]
        variables.update(
            {
                "REMOTE_PEER_ID_PRIMARY": remote_workers[0]["peer_id"],
                "REMOTE_AGENT_PRIMARY": remote_workers[0]["agent"],
                "REMOTE_PEER_ID_SECONDARY": secondary["peer_id"],
                "REMOTE_AGENT_SECONDARY": secondary["agent"],
            }
        )
    for assignment in args.var:
        if "=" not in assignment:
            raise HarnessError(f"--var must be NAME=VALUE, got {assignment!r}")
        name, value = assignment.split("=", 1)
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
            raise HarnessError(f"invalid --var name {name!r}")
        variables[name] = value

    needs_database_fixtures = not args.cleanup_only and (
        not selected or bool({"db_schema", "db_query"} & selected)
    )
    if needs_database_fixtures:
        configure_database_fixtures(args.base_url, args.token, variables)
        tools = fetch_tools(args.base_url, args.token)
        validate_live_catalog(catalog_cases, tools)

    if not existing:
        checkpoint.append(
            {
                "record_type": "run_start",
                "run_id": run_id,
                "base_url": args.base_url,
                "matrix": str(matrix_path),
                "catalogue_names": sorted(tool["name"] for tool in tools),
                # Secrets are never matrix variables and never enter checkpoints.
                "variables": variables,
            }
        )

    if selected:
        cases = [case for case in cases if case["tool"] in selected]
    if args.cleanup_only:
        cases = [case for case in cases if case["phase"] == "cleanup"]

    counts: dict[str, int] = {
        "passed": 0,
        "configuration_blocked": 0,
        "failed": 0,
        "dependency_failed": 0,
    }
    for position, case in enumerate(cases, 1):
        prior = outcomes.get(case["id"])
        if prior and not (args.retry_failures and prior not in {"passed", "configuration_blocked"}):
            print(f"[{position}/{len(cases)}] SKIP {case['id']} ({prior})")
            counts[prior] = counts.get(prior, 0) + 1
            continue

        failed_deps = unresolved_dependencies(case, outcomes)
        if failed_deps:
            status = "dependency_failed"
            detail = f"unfinished or failed dependencies: {failed_deps}"
            captures: dict[str, str] = {}
        else:
            try:
                rendered_args = render(case["args"], variables)
            except HarnessError as exc:
                status, captures, detail = "failed", {}, str(exc)
            else:
                print(
                    f"[{position}/{len(cases)}] RUN {case['id']} "
                    f"({case['tool']}/{_matrix_action(case) or 'default'})",
                    flush=True,
                )
                try:
                    if case.get("remove_database_fixtures"):
                        await asyncio.to_thread(
                            remove_database_fixtures,
                            args.base_url,
                            args.token,
                            variables,
                        )
                    recovery_before = (
                        await asyncio.to_thread(
                            fetch_health_snapshot, args.base_url, args.token
                        )
                        if case.get("require_gateway_recovery")
                        else None
                    )
                    status, captures, detail = await execute_case(
                        base_url=args.base_url,
                        token=args.token,
                        run_id=run_id,
                        case=case,
                        args=rendered_args,
                        variables=variables,
                        checkpoint=checkpoint,
                    )
                    if (
                        status == "passed"
                        and recovery_before is not None
                    ):
                        await wait_for_gateway_recovery(
                            args.base_url, args.token, recovery_before
                        )
                        recovered_tools = await asyncio.to_thread(
                            fetch_tools, args.base_url, args.token
                        )
                        validate_live_catalog(catalog_cases, recovered_tools)
                        detail = (
                            f"{detail}\nGateway restarted, /health recovered, "
                            "and the complete live catalogue still matched."
                        )
                except (OSError, websockets.WebSocketException, HarnessError) as exc:
                    status, captures, detail = "failed", {}, str(exc)

        variables.update(captures)
        outcomes[case["id"]] = status
        counts[status] = counts.get(status, 0) + 1
        checkpoint.append(
            {
                "record_type": "case_outcome",
                "run_id": run_id,
                "case_id": case["id"],
                "tool": case["tool"],
                "action": _matrix_action(case),
                "phase": case["phase"],
                "status": status,
                "captures": captures,
                "detail": detail,
            }
        )
        print(f"[{position}/{len(cases)}] {status.upper()} {case['id']}", flush=True)

    try:
        remove_database_fixtures(args.base_url, args.token, variables)
    except HarnessError as exc:
        counts["failed"] = counts.get("failed", 0) + 1
        checkpoint.append(
            {
                "record_type": "fixture_cleanup",
                "run_id": run_id,
                "fixture": "database_connections",
                "status": "failed",
                "detail": str(exc),
            }
        )
        print("FAILED database fixture connection cleanup", flush=True)

    tool_summary: dict[str, dict[str, Any]] = {}
    for case in cases:
        status = outcomes.get(case["id"], "unfinished")
        entry = tool_summary.setdefault(
            case["tool"], {"status": "passed", "actions": {}}
        )
        entry["actions"][case["id"]] = status
        if status in {"failed", "dependency_failed", "unfinished"}:
            entry["status"] = "failed"
        elif status == "configuration_blocked" and entry["status"] == "passed":
            entry["status"] = "configuration_blocked"

    summary = {
        "record_type": "run_summary",
        "run_id": run_id,
        "counts": counts,
        "tools": tool_summary,
        "checkpoint": str(checkpoint.path),
    }
    checkpoint.append(summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return (
        1
        if counts.get("failed", 0)
        or counts.get("dependency_failed", 0)
        or (
            counts.get("configuration_blocked", 0)
            and not args.allow_config_blocked
        )
        else 0
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--base-url",
        default=os.environ.get("BASE_URL", "http://127.0.0.1:42617"),
        help="LlamaFarm HTTP origin (env: BASE_URL)",
    )
    parser.add_argument(
        "--token",
        default=os.environ.get("LLAMAFARM_GATEWAY_TOKEN"),
        help="optional bearer token (env: LLAMAFARM_GATEWAY_TOKEN)",
    )
    parser.add_argument(
        "--expected-online-peers",
        type=int,
        default=1,
        help="minimum distinct online worker peers required before execution",
    )
    parser.add_argument(
        "--matrix",
        default=str(Path(__file__).with_name("tool_catalog_matrix.json")),
    )
    parser.add_argument(
        "--checkpoint",
        default=os.environ.get(
            "TOOL_CATALOG_CHECKPOINT",
            "~/.local/state/llamafarm/tool-catalog-e2e.jsonl",
        ),
    )
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--retry-failures", action="store_true")
    parser.add_argument("--cleanup-only", action="store_true")
    parser.add_argument("--validate-only", action="store_true")
    parser.add_argument(
        "--allow-config-blocked",
        action="store_true",
        help=(
            "diagnostic mode only: do not fail the process for explicit "
            "credential-backed configuration_blocked results"
        ),
    )
    parser.add_argument(
        "--only",
        action="append",
        default=[],
        metavar="TOOL",
        help="run only one advertised tool (repeatable)",
    )
    parser.add_argument(
        "--var",
        action="append",
        default=[],
        metavar="NAME=VALUE",
        help="override a non-secret matrix variable (repeatable)",
    )
    return parser


def main() -> int:
    try:
        return asyncio.run(run(build_parser().parse_args()))
    except KeyboardInterrupt:
        print("Interrupted; checkpoint is durable. Resume with --resume.", file=sys.stderr)
        return 130
    except HarnessError as exc:
        print(f"HARNESS ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
