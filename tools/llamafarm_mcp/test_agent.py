import unittest
from unittest.mock import Mock, patch
from uuid import uuid4

import server


class AgentTest(unittest.TestCase):
    def test_tool_uses_tool_enabled_endpoint(self):
        response = Mock(status_code=200)
        response.json.return_value = {"response": "node-result", "run_id": "run-123"}
        with patch.object(server.requests, "post", return_value=response) as post:
            self.assertEqual(
                server.llamafarm_agent("run hostname", 45),
                "node-result\n\n[llamafarm] run_id=run-123",
            )
        post.assert_called_once_with(
            f"{server.BASE_URL}/agent",
            json={"message": "run hostname"},
            timeout=45,
        )

    def test_invalid_requests_do_not_reach_node(self):
        with patch.object(server.requests, "post") as post:
            for prompt, timeout in (("", 300), ("ok", 0), ("ok", 3601), ("ok", True)):
                with self.assertRaises(ValueError):
                    server.llamafarm_agent(prompt, timeout)
            post.assert_not_called()

    def test_run_tools_use_existing_ledger_routes(self):
        run_id = str(uuid4())
        with patch.object(server, "_get", return_value={"runs": [], "live": []}) as get:
            self.assertIn('"runs"', server.llamafarm_runs())
            get.assert_called_once_with("/api/runs")
        with patch.object(server, "_get", return_value={"run": {"status": "running"}}) as get:
            self.assertIn('"running"', server.llamafarm_run_status(run_id))
            get.assert_called_once_with(f"/api/runs/{run_id}")
        response = Mock(status_code=200)
        response.json.return_value = {"run_id": run_id, "status": "cancelled", "ok": True}
        with patch.object(server.requests, "post", return_value=response) as post:
            self.assertIn('"cancelled"', server.llamafarm_cancel_run(run_id))
            post.assert_called_once_with(
                f"{server.BASE_URL}/api/runs/{run_id}/cancel", timeout=15
            )
        self.assertEqual(server._run_id(f"session-{run_id}"), f"session-{run_id}")

    def test_invalid_run_id_never_reaches_node(self):
        with patch.object(server, "_get") as get, patch.object(server.requests, "post") as post:
            with self.assertRaises(ValueError):
                server.llamafarm_run_status("../health")
            with self.assertRaises(ValueError):
                server.llamafarm_cancel_run("../health")
            get.assert_not_called()
            post.assert_not_called()


if __name__ == "__main__":
    unittest.main()
