# Cross-device background-run acceptance

`background_run_acceptance.py` verifies the real server-owned chat lifecycle
from any machine that can reach a LlamaFarm gateway. It performs two unique,
non-temporary turns through `/ws/chat` using the `llamafarm.v1` subprotocol:

1. submit a `task_plan` + `shell` marker turn, drop the WebSocket transport,
   then use a fresh REST client to require session discovery, a visible
   completed run with tool evidence, and a hydrated durable result;
2. submit a second turn, detach, wait for harmless marker evidence followed by
   a sleeping shell call, observe that exact process through a self-excluding
   read-only `/proc` probe, cancel it through `/api/runs/{id}/cancel`, and
   require durable `cancelled` status plus the persisted user request.

The prompts allow only `task_plan` and `shell`. Shell actions are limited to
printing a unique marker and sleeping; the harness does not write files, touch
memory, use the network from a tool, delegate work, or clear existing records.
Cancellation does not fabricate an assistant final or a failed tool result for
an interrupted call; the harness instead rejects any false successful result.
It leaves the two uniquely named chat/run records in place as inspection
evidence.

Run it from a different LAN or tailnet client to exercise cross-device access:

```bash
python3 scripts/tests/background_run_acceptance.py \
  --base-url http://llamafarm-node:42617
```

For a token-protected gateway, prefer the environment so the token is not in
shell history or the process list:

```bash
LLAMAFARM_GATEWAY_TOKEN='...' \
python3 scripts/tests/background_run_acceptance.py \
  --base-url https://llamafarm-node.example \
  --deadline-seconds 2400
```

`--deadline-seconds` is only the harness observer deadline. It is never sent
to LlamaFarm and does not impose a model, tool, or run timeout. HTTP and
WebSocket operations use the remaining observer deadline. The default is 30
minutes. The literal `sleep 3600` is only the second run's inert, cancellable
workload; it is not sent or configured as a LlamaFarm timeout or run limit.

Offline checks (no gateway or network required):

```bash
python3 -m unittest discover \
  -s scripts/tests -p 'test_background_run_acceptance.py' -v
python3 -m py_compile \
  scripts/tests/background_run_acceptance.py \
  scripts/tests/test_background_run_acceptance.py
```
