#!/usr/bin/env python3
"""Append missing bundled agents to a persisted LlamaFarm TOML config.

The migration is deliberately additive: existing agent and model-route tables
are authoritative and are never replaced. The original config text is retained
byte-for-byte as a prefix so comments, ordering, custom values, and encrypted
secrets survive a container upgrade.
"""

from __future__ import annotations

import argparse
import os
from pathlib import Path
import re
import stat
import tempfile
import tomllib


BUILTIN_AGENT_NAMES = ("devsecops",)
# Horizontal whitespace is intentional here. ``\s`` also consumes newlines
# under the multiline/DOTALL matcher and can make a block swallow every table
# that follows it (for example the global ``[arxiv_rag]`` table).
_HSPACE = r"[ \t]*"
_TABLE_BOUNDARY = (
    rf"(?=^{_HSPACE}\[\[?[^\]\n]+\]\]?{_HSPACE}(?:\#[^\n]*)?$|\Z)"
)


def _parse(raw: str, source: Path) -> dict:
    try:
        parsed = tomllib.loads(raw)
    except tomllib.TOMLDecodeError as error:
        raise ValueError(f"{source} is not valid TOML: {error}") from error
    if not isinstance(parsed, dict):
        raise ValueError(f"{source} must contain a TOML table")
    return parsed


def _extract_agent_table(template: str, name: str) -> str:
    pattern = re.compile(
        rf"(?ms)^{_HSPACE}\[agents\.{re.escape(name)}\]"
        rf"{_HSPACE}(?:\#[^\n]*)?\n.*?{_TABLE_BOUNDARY}"
    )
    match = pattern.search(template)
    if match is None:
        raise ValueError(f"bundled template is missing [agents.{name}]")
    block = match.group(0).strip()
    parsed = tomllib.loads(block)
    if name not in parsed.get("agents", {}):
        raise ValueError(f"could not parse bundled [agents.{name}] table")
    return block


def _extract_model_route(template: str, hint: str) -> str:
    pattern = re.compile(
        rf"(?ms)^{_HSPACE}\[\[model_routes\]\]"
        rf"{_HSPACE}(?:\#[^\n]*)?\n.*?{_TABLE_BOUNDARY}"
    )
    for match in pattern.finditer(template):
        block = match.group(0).strip()
        parsed = tomllib.loads(block)
        routes = parsed.get("model_routes", [])
        if routes and routes[0].get("hint") == hint:
            return block
    raise ValueError(f"bundled template is missing model route {hint!r}")


def merge_builtin_agents(config_text: str, template_text: str) -> tuple[str, list[str]]:
    """Return additive merged TOML and a list describing inserted tables."""

    config = _parse(config_text, Path("config"))
    template = _parse(template_text, Path("template"))
    configured_agents = config.get("agents", {})
    configured_routes = config.get("model_routes", [])

    if not isinstance(configured_agents, dict):
        raise ValueError("config [agents] value must be a table")
    if not isinstance(configured_routes, list):
        raise ValueError("config model_routes value must be an array of tables")

    route_hints = {
        route.get("hint")
        for route in configured_routes
        if isinstance(route, dict) and isinstance(route.get("hint"), str)
    }
    additions: list[str] = []
    added_names: list[str] = []

    for name in BUILTIN_AGENT_NAMES:
        template_agents = template.get("agents", {})
        if name not in template_agents:
            raise ValueError(f"bundled template is missing agent {name!r}")

        if name not in route_hints:
            additions.append(_extract_model_route(template_text, name))
            added_names.append(f"model_routes:{name}")

        if name not in configured_agents:
            additions.append(_extract_agent_table(template_text, name))
            added_names.append(f"agents:{name}")

    if not additions:
        return config_text, []

    separator = "" if config_text.endswith("\n") else "\n"
    merged = (
        config_text
        + separator
        + "\n# Added by the idempotent built-in-agent startup migration.\n"
        + "\n\n".join(additions)
        + "\n"
    )
    _parse(merged, Path("merged config"))
    return merged, added_names


def _atomic_write(path: Path, content: str) -> None:
    current_stat = path.stat()
    current_mode = stat.S_IMODE(current_stat.st_mode)
    temp_name: str | None = None
    try:
        with tempfile.NamedTemporaryFile(
            mode="w",
            encoding="utf-8",
            dir=path.parent,
            prefix=f".{path.name}.builtin-agents-",
            delete=False,
        ) as temporary:
            temp_name = temporary.name
            temporary.write(content)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.chmod(temp_name, current_mode)
        if os.geteuid() == 0:
            os.chown(temp_name, current_stat.st_uid, current_stat.st_gid)
        os.replace(temp_name, path)
        temp_name = None
        directory_fd = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if temp_name is not None:
            try:
                os.unlink(temp_name)
            except FileNotFoundError:
                pass


def migrate_file(config_path: Path, template_path: Path) -> list[str]:
    config_text = config_path.read_text(encoding="utf-8")
    template_text = template_path.read_text(encoding="utf-8")
    merged, additions = merge_builtin_agents(config_text, template_text)
    if additions:
        _atomic_write(config_path, merged)
    return additions


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("config", type=Path)
    parser.add_argument("template", type=Path)
    args = parser.parse_args()

    additions = migrate_file(args.config, args.template)
    if additions:
        print("built-in agent migration added: " + ", ".join(additions))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
