import asyncio
import json
import threading
import unittest
from unittest.mock import Mock, patch

import server


class AgentBatchTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        server.AGENT_SLOTS = asyncio.Semaphore(4)

    async def test_four_requests_are_concurrent_isolated_and_ordered(self):
        barrier = threading.Barrier(4, timeout=3)

        def backend(prompt, timeout):
            barrier.wait()
            return {"ok": True, "http_status": 200,
                    "run_id": f"run-{prompt}", "response": prompt}

        tasks = [{"id": str(n), "prompt": f"task-{n}"} for n in range(4)]
        with patch.object(server, "_post_agent_batch_task", backend):
            data = json.loads(await server.llamafarm_agent_batch(tasks, 45))
        self.assertTrue(data["all_ok"])
        self.assertEqual(data["mode"], "agent_runs")
        self.assertEqual(
            [(r["id"], r["response"], r["run_id"]) for r in data["results"]],
            [(str(n), f"task-{n}", f"run-task-{n}") for n in range(4)],
        )

    async def test_partial_failure_keeps_other_results(self):
        def backend(prompt, timeout):
            if prompt == "fail":
                return {"ok": False, "http_status": 502,
                        "run_id": "failed-run", "error": "provider unavailable"}
            return {"ok": True, "http_status": 200,
                    "run_id": "good-run", "response": "done"}

        with patch.object(server, "_post_agent_batch_task", backend):
            data = json.loads(await server.llamafarm_agent_batch([
                {"id": "good", "prompt": "work"},
                {"id": "bad", "prompt": "fail"},
            ]))
        self.assertFalse(data["all_ok"])
        self.assertTrue(data["results"][0]["ok"])
        self.assertEqual(data["results"][1]["http_status"], 502)
        self.assertEqual(data["results"][1]["run_id"], "failed-run")

    async def test_validation_before_dispatch(self):
        bad_batches = [[], [{"id": "x", "prompt": "p"}] * 5,
                       [{"id": "x", "prompt": "p"}] * 2,
                       [{"id": "x", "prompt": ""}],
                       [{"id": "x", "prompt": "p", "system": "override"}],
                       [{"id": "x", "prompt": "p", "timeout": 5}],
                       [{"id": "x", "prompt": "p"}, {"id": "y", "prompt": None}]]
        with patch.object(server, "_post_agent_batch_task") as backend:
            for tasks in bad_batches:
                with self.assertRaises(ValueError):
                    await server.llamafarm_agent_batch(tasks)
            for timeout in (0, 3601, True, 1.5):
                with self.assertRaises(ValueError):
                    await server.llamafarm_agent_batch(
                        [{"id": "x", "prompt": "p"}], timeout)
            backend.assert_not_called()


class AgentBatchTransportTest(unittest.TestCase):
    def test_uses_agent_route_and_retains_run_id(self):
        response = Mock(status_code=200)
        response.json.return_value = {"response": "done", "run_id": "run-123"}
        with patch.object(server.requests, "post", return_value=response) as post:
            result = server._post_agent_batch_task("read status", 60)
        post.assert_called_once_with(
            f"{server.BASE_URL}/agent", json={"message": "read status"}, timeout=60
        )
        self.assertEqual(result, {"ok": True, "http_status": 200,
                                  "run_id": "run-123", "response": "done"})

    def test_gateway_error_keeps_status_and_run_id(self):
        response = Mock(status_code=502)
        response.json.return_value = {"error": "provider unavailable", "run_id": "run-456"}
        with patch.object(server.requests, "post", return_value=response):
            result = server._post_agent_batch_task("read status", 60)
        self.assertEqual(result, {"ok": False, "http_status": 502,
                                  "run_id": "run-456", "error": "provider unavailable"})


if __name__ == "__main__":
    unittest.main()
