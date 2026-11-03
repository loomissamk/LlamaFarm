#!/usr/bin/env python3
"""Keep the Mac Kilo picker limited to tested V100 Ollama models.

Run from launchd after the Ollama tunnel starts. A busy Kilo project returns
HTTP 409 on reload; the next run retries without interrupting its session.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


CONFIG = Path.home() / ".config/kilo/kilo.jsonc"
STATE = Path.home() / ".config/kilo/.approved-models-reloaded.json"
CATALOG = "http://127.0.0.1:11435/api/tags"
PROVIDER = "llamafarm-v100"
MODELS = {
    "qwen3.5:9b-256k": ("Qwen 3.5 9B", 262144),
    "qwen3.5:9b-128k": ("Qwen 3.5 9B", 131072),
    "qwen3.8:27b-256k": ("Qwen 3.8 27B", 262144),
    "qwen3.8:27b-128k": ("Qwen 3.8 27B", 131072),
    "qwen3.8:27b": ("Qwen 3.8 27B", 32768),
    "qwen3.5:9b": ("Qwen 3.5 9B", 32768),
    "gpt-oss:20b-128k": ("GPT-OSS 20B", 131072),
    "muse-glimmer:30b-128k": ("Muse Glimmer 30B", 131072),
}


def write_json(path: Path, value: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=f".{path.name}-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as output:
            json.dump(value, output, indent=2)
            output.write("\n")
        os.chmod(temporary, path.stat().st_mode & 0o777 if path.exists() else 0o600)
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def kilo_servers() -> list[tuple[str, str, str]]:
    processes = subprocess.run(
        ["/bin/ps", "-axo", "pid=,command="], capture_output=True, text=True, check=True
    ).stdout
    servers = []
    for line in processes.splitlines():
        if not re.search(r"/bin/kilo serve --port(?: 0)?(?:\s|$)", line):
            continue
        pid = line.strip().split(None, 1)[0]
        environment = subprocess.run(
            ["/bin/ps", "eww", "-p", pid], capture_output=True, text=True, check=True
        ).stdout
        match = re.search(r"(?:^| )KILO_SERVER_PASSWORD=([^ ]+)", environment)
        if not match:
            continue
        listeners = subprocess.run(
            ["/usr/sbin/lsof", "-nP", "-a", "-p", pid, "-iTCP", "-sTCP:LISTEN", "-F", "n"],
            capture_output=True, text=True, check=True,
        ).stdout
        for listener in listeners.splitlines():
            found = re.fullmatch(r"n127\.0\.0\.1:(\d+)", listener)
            if found:
                servers.append((pid, found.group(1), match.group(1)))
    return servers


def reload_server(port: str, password: str) -> bool:
    token = base64.b64encode(f"kilo:{password}".encode()).decode()
    request = Request(
        f"http://127.0.0.1:{port}/instance/reload",
        data=b"{}",
        headers={"Authorization": f"Basic {token}", "Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urlopen(request, timeout=10) as response:
            return response.status == 200 and json.load(response) is True
    except HTTPError as error:
        if error.code == 409:
            return False
        raise


def main() -> None:
    try:
        with urlopen(CATALOG, timeout=5) as response:
            available = {item["name"] for item in json.load(response)["models"]}
    except (OSError, URLError, KeyError, ValueError):
        return  # Tunnel or Ollama is unavailable; retain the last working config.

    config = json.loads(CONFIG.read_text())
    provider = config["provider"][PROVIDER]
    approved = {
        name: {
            "name": f"{label} | {context // 1024}K | V100 via Tailscale",
            "tool_call": True,
            "limit": {"context": context, "output": 4096},
        }
        for name, (label, context) in MODELS.items()
        if name in available
    }
    if not approved:
        return
    if provider.get("models") != approved:
        provider["models"] = approved
        write_json(CONFIG, config)
        print(f"Synced {len(approved)} approved models to {CONFIG}")

    digest = hashlib.sha256(CONFIG.read_bytes()).hexdigest()
    previous = json.loads(STATE.read_text()) if STATE.exists() else {}
    current = {}
    for pid, port, password in kilo_servers():
        if previous.get(pid) == digest:
            current[pid] = digest
            continue
        try:
            if reload_server(port, password):
                current[pid] = digest
                print(f"Reloaded Kilo server {pid}")
        except (OSError, URLError, ValueError):
            pass  # Retry while Kilo is running on the next launchd interval.
    if current != previous:
        write_json(STATE, current)


if __name__ == "__main__":
    main()
