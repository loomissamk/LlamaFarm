# LlamaFarm MCP bridge

This stdio MCP server makes LlamaFarm's chat, agent, run-ledger, and batch
tools available to any MCP client, including Codex and Claude. It runs on the
client machine and calls the remote LlamaFarm gateway over the configured URL.

```sh
python3 -m venv .venv
.venv/bin/python -m pip install -r requirements.txt
export LLAMAFARM_BASE_URL=http://V100_TAILSCALE_IP:42617
export LLAMAFARM_MODEL=qwen3.5:9b
.venv/bin/python server.py
```

Register `.venv/bin/python` with `server.py` as its argument in the MCP
client. Set `LLAMAFARM_BASE_URL`, `LLAMAFARM_MODEL`, and optionally
`LLAMAFARM_TIMEOUT` in that client registration. `profiles/` has example
values for the V100 node. The `fast` and `heavy` profiles point to the same
gateway; they do not create separate GPU workers.

`llamafarm_delegate_batch` runs 1–4 text-only delegates. For real node-side
tool use, `llamafarm_agent_batch` runs 1–4 independent `/agent` requests and
returns a result for every task. A run ID is included when the gateway
returns one. These calls overlap at the HTTP layer; actual model decoding
may queue. Keep tasks bounded and inspect each result before accepting its
claims or labels.

```sh
.venv/bin/python -m unittest -q test_agent test_agent_batch test_delegates
LLAMAFARM_TEST_AGENT_BATCH=1 .venv/bin/python e2e_test.py
```

The live E2E test uses two read-only arithmetic tasks. It checks a fresh
MCP process, tool discovery, both agent responses, and gateway health.
