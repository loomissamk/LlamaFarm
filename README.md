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
