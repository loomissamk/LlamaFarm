# Two-node local-agent profiles

These profiles turn the two NVIDIA hosts into an asymmetric local agent cluster:

| Node | Role | Inference profile |
| --- | --- | --- |
| RTX 4070 Laptop, 8 GB | fast IDE / fallback worker | 32K, one stream |
| RTX 5070 Ti, 16 GB | coordinator / RAG / quality worker | 64K, one stream |

They deliberately do **not** turn the model's 262K architectural context into
an allocation target. Context, model concurrency, and KV cache all compete for
VRAM. One well-provisioned stream produces much better agent behavior than six
simultaneous, swapping streams.

## Bring-up

Copy the example into a private host configuration on each machine. Do not put
LAN addresses or federation secrets into Git.

```bash
mkdir -p ~/.config/llamafarm
cp deploy/node-profiles/node.env.example ~/.config/llamafarm/node.env
chmod 600 ~/.config/llamafarm/node.env
```

Set `LLAMAFARM_MANUAL_PEERS` on each host to the other host's gateway address,
and set the same long random `LLAMAFARM_FEDERATION_TOKEN` on both hosts. The
token authenticates node-to-node task traffic in addition to the LAN-only
network check. The selected Ollama model comes from LlamaFarm's persisted
configuration, so changing it in the dashboard survives profile-driven
container restarts. Set `LLAMAFARM_MODEL` in the private `node.env` (or export
it before launch) only when an immutable deployment-level override is
intentional. Then run the matching profile:

```bash
./scripts/docker/up-node.sh rtx4070-laptop
./scripts/docker/up-node.sh rtx5070ti-16gb
```

The dashboard is direct-access: browser/gateway pairing and the `/pair`
endpoint are removed from this deployment path. Federation worker calls still
require the shared node token. Ollama (`11434`) and Qdrant (`6333`) stay on
loopback, so use the LlamaFarm gateway or an SSH tunnel for remote access
rather than exposing raw inference/control ports.

After startup, verify both nodes:

```bash
./scripts/docker/check-node.sh rtx4070-laptop
./scripts/docker/check-node.sh rtx5070ti-16gb
```

## Browser acceptance

The bundle includes Chromium and ChromeDriver, so the acceptance test drives
the real direct-access dashboard rather than calling privileged HTTP endpoints. It
checks the visible tool registry, WebSocket chat, code write/run/read, free web
search, local arXiv RAG, and (with the flag below) selected-worker federation.
It saves a screenshot, DOM snapshot, and concise result JSON under
`workspace/acceptance-artifacts/` in the persisted LlamaFarm volume.

Run it once on each node after both profiles report a healthy peer:

```bash
docker exec -i \
  -e LLAMAFARM_ACCEPTANCE_NODE=rtx4070-laptop \
  LlamaFarm python3 - --with-federation < scripts/docker/ui-acceptance.py
```

Use `rtx5070ti-16gb` as the node label on the 5070 Ti host. Each run delegates
one marker task to the selected remote worker and requires the completed task
to appear in the Federation dashboard, so running it on both nodes validates
both directions. A nonzero exit means an expected UI event was not observed;
inspect the matching persisted artifact before considering the node accepted.

## Turbo quant and useful context

`q8_0` KV-cache quantization plus Flash Attention is the quality-first
"turbo-quant" setting. It roughly halves KV cache pressure compared with F16,
while normally retaining reliable long-context behavior. `q4_0` is a benchmark
fallback only; it should not be enabled merely to advertise a larger number.

LlamaFarm sends `OLLAMA_NUM_CTX` with each model request. `OLLAMA_CONTEXT_LENGTH`
is set to the same value so direct Ollama calls behave consistently. Keep
`OLLAMA_NUM_PARALLEL=1`: Ollama reserves context/KV capacity per parallel
request, so concurrency multiplies the allocation.

Promotion path:

1. Keep the laptop at 24–32K.
2. Keep the 5070 Ti at 64K and measure real tool/RAG tasks.
3. Test 96K on the 5070 Ti only after it remains stable at 64K under a full
   single-stream workload.
4. If a workload needs more than that, retrieve a small relevant subset rather
   than forcing a 131K × 6 allocation.

`docker exec LlamaFarm ollama ps` shows the actual loaded context and GPU/CPU
offload after a request. This is the source of truth, not the model card's
architectural maximum.

The bundle explicitly enables its free DuckDuckGo `web_search_tool` by default.
Set `LLAMAFARM_WEB_SEARCH_ENABLED=false` in a private node override if that
outbound capability is not wanted on a particular node.

Inference is explicitly local: `OLLAMA_NO_CLOUD=true` is baked into the bundle.
It prevents cloud-model routing while preserving ordinary local model pulls and
the LAN gateway. This keeps the model-serving path free and on the node's GPU.

## Durable working state

When conversation history passes the configured budget, LlamaFarm now creates
an atomic, mode-`0600` rolling checkpoint at:

```text
$LLAMAFARM_WORKSPACE/memory/WORKING_STATE.md
```

It contains the compacted decisions, preferences, unresolved work, and current
objective. The old transcript is replaced by its summary in live history,
which releases context. A new session automatically rehydrates this small file;
SQLite memory keeps the existing searchable daily summary as well. The file is
rolling, not a raw chat log, and should be treated as private workspace state.

## RAM tiers

Use the GPUs for active decoding; system RAM is a valuable, slower second tier:

- GPU VRAM: the current interactive model and its KV cache.
- Host RAM: Qdrant, document parsing, indexing, embedding/reranking jobs, and
  deliberate CPU-offloaded quality experiments.
- NVMe: model volumes, source data, and optional future disk-backed KV cache.

The resource ceilings are applied when Compose creates a container. The
5070 Ti host receives 96 GB for the bundle so it can run controlled offloaded
experiments without starving the OS; that does not make CPU-offloaded decoding
as fast as a model that actually fits in VRAM. Never keep an offloaded 30B/35B
quality model resident alongside the 9B interactive lane.

If a ceiling changes, recreate through Compose:

```bash
./scripts/docker/up-node.sh rtx4070-laptop up -d --force-recreate
```

Never use `docker update` on a running GPU bundle. NVIDIA documents that the
legacy container-runtime hook can lose GPU cgroup access after that operation,
leaving `nvidia-smi` with `Failed to initialize NVML: Unknown Error` inside the
container.

## NVIDIA Container Toolkit preflight

`up-node.sh` requires NVIDIA Container Toolkit `1.19.1` or newer and verifies
NVML from a throwaway GPU container before it starts the bundle. This catches
an installed-but-broken GPU runtime before a deploy can claim success.

On Debian/Ubuntu/Pop!_OS, follow NVIDIA's current install guide, then configure
and restart Docker:

```bash
sudo apt-get update
sudo apt-get install -y nvidia-container-toolkit
sudo nvidia-ctk runtime configure --runtime=docker
sudo systemctl restart docker
./scripts/docker/up-node.sh rtx4070-laptop up -d --force-recreate
```

Use the NVIDIA repository from the official guide when the distro package is
older than `1.19.1`. Do not delete named LlamaFarm or Qdrant volumes while
recovering from a GPU-runtime problem.

## Models and future runtimes

Keep Ollama as the production baseline: it already works with both GPUs and
the current bundle. Good next trials on the 5070 Ti are `gpt-oss:20b` as a
planner/reviewer and `qwen3-coder:30b` as a quality coding lane; both need
single-stream benchmarks and may partially offload to RAM. Existing Devstral
Small 2 is also a quality experiment, not a permanent 32K resident service on
16 GB.

For RAG, preserve the existing `nomic-embed-text` collection. Test
Qwen3-Embedding and a Qwen3 reranker in a **new** collection and reindex; never
mix embeddings of different dimensions in the existing one.

SGLang is the preferred later high-context experiment on the 5070 Ti because
its hierarchical KV cache can tier reusable prefixes across GPU VRAM, RAM, and
disk. vLLM is the alternate OpenAI-compatible serving experiment. Run either
in a separate Docker service with its own port and benchmark/rollback path;
neither replaces Ollama until it proves better on this actual hardware.

Do not LAN tensor-parallel the two machines. Route independent subtasks to the
worker instead—network latency makes a split single inference stream worse.

## Rollback

The model data and named volumes are untouched. To revert a profile change,
restore the prior environment values and rerun `up-node.sh`; to return to the
old compose behavior, use the prior compose file revision and restart without
`-v`. Do not delete volumes during a rollback.

## Reach nodes over host Tailscale

Install and join Tailscale on the host, then run the normal node deployment.
The bundle detects the host's live Tailscale address, publishes the gateway and
managed app ports on the configured host bind address, and shows a compact
read-only status only while the host daemon is actually online.

The former container VPN profile was removed because its separate network
namespace did not publish the LlamaFarm gateway. For off-LAN federation, set
`LLAMAFARM_MANUAL_PEERS` to peers' tailnet IPs or MagicDNS names. Use tailnet
ACLs to control access to ports 42617, 5000, and 8501-8599, and keep the shared
federation token as defence-in-depth. Do not expose these ports directly to the
public internet.
