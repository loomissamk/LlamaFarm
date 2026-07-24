#!/usr/bin/env python3
"""Focused installer tests for host-runner service readiness."""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts" / "install-host-runner.sh"


class InstallHostRunnerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp_dir = Path(tempfile.mkdtemp(prefix="llamafarm-host-runner-install-"))
        self.addCleanup(lambda: shutil.rmtree(self.temp_dir, ignore_errors=True))
        self.home = self.temp_dir / "home"
        self.repo = self.temp_dir / "repo"
        self.fake_bin = self.temp_dir / "bin"
        (self.repo / "deploy" / "systemd").mkdir(parents=True)
        (self.repo / "target" / "release").mkdir(parents=True)
        self.fake_bin.mkdir()
        self.home.mkdir()

        (self.repo / "deploy" / "systemd" / "llamafarm-host-runner.service").write_text(
            "[Service]\nExecStart=/unused\n",
            encoding="utf-8",
        )
        source_binary = self.repo / "target" / "release" / "llamafarm"
        source_binary.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                counter="${TEST_HEALTH_COUNTER:?}"
                count="$(cat "$counter" 2>/dev/null || printf '0')"
                count=$((count + 1))
                printf '%s\n' "$count" > "$counter"
                if [ "$count" -lt "${TEST_HEALTH_SUCCEED_ATTEMPT:-1}" ]; then
                  echo "host runner is still starting" >&2
                  exit 1
                fi
                printf '{"success":true,"result":{"status":"ok"}}\n'
                """
            ),
            encoding="utf-8",
        )
        source_binary.chmod(0o755)

        socket_helper = self.temp_dir / "create_socket.py"
        socket_helper.write_text(
            textwrap.dedent(
                """\
                import socket
                import sys
                import time

                time.sleep(float(sys.argv[2]))
                listener = socket.socket(socket.AF_UNIX)
                listener.bind(sys.argv[1])
                listener.close()
                """
            ),
            encoding="utf-8",
        )

        fake_systemctl = self.fake_bin / "systemctl"
        fake_systemctl.write_text(
            textwrap.dedent(
                """\
                #!/usr/bin/env bash
                if [[ " $* " == *" enable --now "* ]] &&
                  [ "${TEST_CREATE_SOCKET:-1}" = "1" ]
                then
                  "$TEST_PYTHON" "$TEST_SOCKET_HELPER" \
                    "$TEST_SOCKET_PATH" "${TEST_SOCKET_DELAY:-0}" \
                    </dev/null >/dev/null 2>&1 &
                fi
                exit 0
                """
            ),
            encoding="utf-8",
        )
        fake_systemctl.chmod(0o755)

        self.health_counter = self.temp_dir / "health-count"
        self.env = os.environ.copy()
        self.env.update(
            {
                "HOME": str(self.home),
                "PATH": f"{self.fake_bin}:{self.env['PATH']}",
                "TEST_CREATE_SOCKET": "1",
                "TEST_HEALTH_COUNTER": str(self.health_counter),
                "TEST_HEALTH_SUCCEED_ATTEMPT": "3",
                "TEST_PYTHON": sys.executable,
                "TEST_SOCKET_DELAY": "0.15",
                "TEST_SOCKET_HELPER": str(socket_helper),
                "TEST_SOCKET_PATH": str(
                    self.home / ".llamafarm" / "run" / "host-runner.sock"
                ),
                "LLAMAFARM_HOST_RUNNER_START_TIMEOUT_SECONDS": "3",
                "LLAMAFARM_HOST_RUNNER_START_POLL_SECONDS": "0.05",
            }
        )

    def run_installer(self) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [
                "bash",
                str(INSTALLER),
                "--repo",
                str(self.repo),
                "--no-build",
                "--no-compose-enable",
            ],
            text=True,
            capture_output=True,
            check=False,
            env=self.env,
        )

    def test_waits_for_socket_and_retries_health_until_ready(self) -> None:
        result = self.run_installer()

        self.assertEqual(result.returncode, 0, msg=result.stderr)
        self.assertEqual(self.health_counter.read_text(encoding="utf-8").strip(), "3")
        self.assertIn('"status":"ok"', result.stdout)
        self.assertIn("Host runner is active", result.stdout)

    def test_reports_bounded_timeout_when_socket_never_appears(self) -> None:
        self.env["TEST_CREATE_SOCKET"] = "0"
        self.env["LLAMAFARM_HOST_RUNNER_START_TIMEOUT_SECONDS"] = "1"

        result = self.run_installer()

        self.assertEqual(result.returncode, 1)
        self.assertIn("did not become healthy within 1s", result.stderr)
        self.assertIn("did not create its Unix socket", result.stderr)
        self.assertFalse(self.health_counter.exists())


if __name__ == "__main__":
    unittest.main()
