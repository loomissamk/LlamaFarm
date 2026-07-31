# LlamaFarm

LlamaFarm is an Ollama-first, local-first autonomous operator runtime for disposable lab environments.

## What It Is

- Ollama-only model runtime (no cloud fallback required for core operation)
- Agentic tool-use loop (plan, execute, verify, retry)
- Hybrid local retrieval (repo/docs/logs) with citations
- Multi-model routing + bakeoff scoring for task-specific model selection
- `chaos_lab` execution mode for destructive sandbox experimentation
- Full run tracing (prompts, tool calls, retries, outputs, completion state)

## Disposable Target Contract

LlamaFarm is designed for:

- throwaway VMs
- disposable containers
- isolated lab hosts
- reimageable test hardware

Do not run high-autonomy/destructive workflows on shared or production systems.

## Quick Start (Bundle)

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle.sh
```

Open `http://127.0.0.1:42617`.

The bundle includes:

1. LlamaFarm gateway/web UI
2. Local Ollama runtime in-container
3. Local Chromium + chromedriver
4. Prepull support for default model set

## NVIDIA Quick Start

```bash
./scripts/docker/up-bundle-nvidia.sh
```

Exact rebundle command:

```bash
docker compose -f docker-compose.bundle.yml -f docker-compose.bundle.nvidia.yml up -d --build
```

## Two-node NVIDIA deployment

Use the checked-in profiles for the 8 GB RTX 4070 laptop and the 16 GB RTX
5070 Ti coordinator. They set a quality-first Qwen 3.5 lane, q8_0 KV cache,
Flash Attention, a single model stream, bounded agent history, and asymmetric
context policies. Neither LlamaFarm nor Qdrant has a Compose CPU, memory, swap,
or PID ceiling. Full details, model promotion rules, durable working-state
memory, and the browser acceptance procedure are in
[`deploy/node-profiles/README.md`](deploy/node-profiles/README.md).

```bash
./scripts/docker/up-node.sh rtx4070-laptop
./scripts/docker/up-node.sh rtx5070ti-16gb
```

The bundle has no browser or gateway pairing flow: the `/pair` endpoint and
dashboard gate are removed, and stale paired config cannot re-enable them.
The two hosts still authenticate federation task traffic with the private
`LLAMAFARM_FEDERATION_TOKEN`; raw Ollama and Qdrant ports remain loopback-only.

`up-node.sh` requires NVIDIA Container Toolkit 1.19.1+ and verifies a real
`nvidia-smi -L` GPU container before deployment. Upgrade/configure the toolkit
using NVIDIA's official instructions, restart Docker, then recreate the
bundle; do not delete its named model or Qdrant volumes.

## Next-gen agent core

The current pass adds an evidence-first execution core:

- **Durable run ledger** — every run persists planner → executor → verifier
  records to `<workspace>/state/runs/<run_id>.ledger.json`: plan steps with
  allowed tools, dependencies, and expected-evidence patterns, plus every
  executed tool call (scrubbed args, output digest + excerpt, duration,
  artifacts).
- **Evidence-gated completion** — a deterministic verifier marks a plan step
  verified only when it is completed *and* backed by successful tool
  evidence with its dependencies verified. Autonomous runs cannot report
  success on model prose alone; unverified claims end as
  `completed_unverified`, never as success.
- **Run inspector** — the **Runs** page (and `GET /api/runs`,
  `GET /api/runs/{id}`) shows live and historical runs: plan state with
  per-step verifier results, the tool timeline, artifacts, attempts, and
  retry reasons, auto-refreshing while a run is live.
- **Hardened command policy** — argument-level guards back the wildcard
  allowlist: unsafe `rm` deletes, `find -exec`, and `git config`/alias
  injection are rejected at every autonomy level; `sudo` requires the
  full-autonomy profile; single-`&` chains validate both sides.

Next on the roadmap (tracked in `TODO.md`): Qdrant-backed cross-session chat
memory, a drop-a-document RAG inbox, run-ledger RAG for evidence-grounded
planning, real TTFT/TPS metrics in the chat UI, self-authored skills, and a
durable federation queue.

## Unattended agent runs

Long jobs use an executable task plan: each item is marked **completed** only
after concrete evidence, while **failed**, **blocked**, and **skipped**
preserve an honest terminal result without trapping the run in pointless
retries. A “test all tools” request remains a real autonomous integration run;
it probes applicable tools, records evidence, and ends with a repair matrix
rather than claiming that registered tools are automatically functional.

The committed [platform backlog](TODO.md) is the durable record of operator
requests and next-generation work. Update it alongside implementation; do not
rely on a transient chat context to preserve requested features.

Local Ollama inference has no response wall-clock deadline. The per-node
`LLAMAFARM_AGENT_MAX_OUTPUT_TOKENS` value is a generation checkpoint, not a job
timeout: if a model reaches it while thinking or answering, LlamaFarm preserves
the run/plan state and continues in another segment. Long plans retain their
task-plan ledger, compact completed item context into
`memory/WORKING_STATE.md`, and require a final evidence pass before returning.
The hot prompt intentionally loads `AGENTS.md`, `TOOLS.md`, `USER.md`, and the
bounded working-state checkpoint rather than redundant persona files.

### Following up while a run is active

The Agent composer remains available during a run. Sending another message
queues it on the same session, cleanly ends the current inference segment, and
immediately resumes from the verified transcript and artifacts with the new
direction. The UI reports the queued transition; completed side effects and
tool evidence are retained.

### Stopping a live Agent run

While a web Agent response is active, the composer shows **Stop**. It sends a
session-scoped cancellation frame without closing the chat: the model stream
and cancellation-aware tool loop stop, completed tool results stay in the
transcript, and accepted federation tasks receive a remote cancel request.
The UI then records a clear stopped terminal state and permits an intentional
follow-up. Stop cannot roll back a side effect that had already completed
before the request arrived.

## Agentic Online Research Example

```bash
llamafarm agent --provider ollama --model qwen3.5:9b \
  --autonomy-level full \
  --message "Look online and do an in-depth comparison of Rust web frameworks."
```

For research-style prompts, the loop can chain:
`web_search_tool -> web_fetch -> additional fetches -> grounded synthesis`.

## Useful Ops

```bash
docker logs -f LlamaFarm
docker exec LlamaFarm ollama list
docker exec LlamaFarm sh -lc 'tail -n 80 /tmp/ollama.log'
```

## Docs

- [Architecture](docs/ARCHITECTURE.md)
- [One-Click Bootstrap](docs/one-click-bootstrap.md)
- [Getting Started](docs/getting-started/README.md)
- [Docs Hub](docs/README.md)
