import asyncio
import json
import threading
import unittest
from unittest.mock import patch

import server


class DelegatesTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        server.DELEGATE_SLOTS = asyncio.Semaphore(4)

    async def test_concurrent_isolated_and_ordered(self):
        barrier = threading.Barrier(4, timeout=3)

        def backend(messages, max_tokens, timeout):
            barrier.wait()
            return {"model": "test-model", "choices": [{"message": {
                "content": json.dumps(messages)}}]}

        tasks = [{"id": str(n), "system": f"role-{n}", "prompt": f"task-{n}"}
                 for n in range(4)]
        with patch.object(server, "_post_chat", backend):
            data = json.loads(await server.llamafarm_delegate_batch(tasks))
        self.assertTrue(data["all_ok"])
        for n, result in enumerate(data["results"]):
            self.assertEqual(result["id"], str(n))
            self.assertEqual(json.loads(result["text"]), [
                {"role": "system", "content": f"role-{n}"},
                {"role": "user", "content": f"task-{n}"}])

    async def test_partial_failure_retains_success(self):
        def backend(messages, *args):
            if messages[-1]["content"] == "fail":
                raise RuntimeError("fixture failure")
            return {"choices": [{"message": {"content": "ok"}}]}
        with patch.object(server, "_post_chat", backend):
            data = json.loads(await server.llamafarm_delegate_batch([
                {"id": "a", "prompt": "ok"}, {"id": "b", "prompt": "fail"}]))
        self.assertFalse(data["all_ok"])
        self.assertTrue(data["results"][0]["ok"])
        self.assertFalse(data["results"][1]["ok"])

    async def test_validation_before_inference(self):
        bad_batches = [[], [{"id": "a", "prompt": "x"}] * 5,
                       [{"id": "a", "prompt": "x"}] * 2,
                       [{"id": "a", "prompt": ""}],
                       [{"id": "a", "prompt": "x", "tools": "shell"}],
                       [{"id": "a", "prompt": "x", "system": None}]]
        with patch.object(server, "_post_chat") as backend:
            for tasks in bad_batches:
                with self.assertRaises(ValueError):
                    await server.llamafarm_delegate_batch(tasks)
            for limit in (0, 2049, True, 1.5):
                with self.assertRaises(ValueError):
                    await server.llamafarm_delegate_batch(
                        [{"id": "a", "prompt": "x"}], limit)
            backend.assert_not_called()


if __name__ == "__main__":
    unittest.main()
