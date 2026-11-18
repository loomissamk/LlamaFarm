# Dual V100 LlamaFarm deployment

The `docker-compose.bundle.yml` and `docker-compose.dual-v100.yml` pair runs two LlamaFarm bundles. Each bundle starts its own Ollama server. Docker gives `LlamaFarm` GPU 0 and `LlamaFarm-GPU1` GPU 1. Both containers mount the same Ollama model store, so they share downloaded blobs while maintaining separate running model weights and separate LlamaFarm state.

GPU 0 publishes the LlamaFarm gateway on port 42617 and its bundled Ollama on host loopback port 11434. Tailscale Serve exposes 11434 to the tailnet for OpenCode. GPU 1's gateway stays on the Compose network as `llamafarm-gpu1:42617` for federation; its bundled Ollama publishes host loopback port 11436, exposed to the tailnet for Kilo. OpenCode can also delegate a useful task to GPU 1 through its `gpu1-specialist` agent. The primary agent reviews delegated work, integrates changes, runs tests, and completes the request.

## Models and capacity

| Selection | GPU 1 model | Context | Ollama slots | OpenCode child limit |
| --- | --- | ---: | ---: | ---: |
| A or `q8-64k` | UnQwen 27B Q8 | 65,536 | 1 | 1 |
| `q8-32k` | UnQwen 27B Q8 | 32,768 | 1 | 1 |
| `q4-256k` | UnQwen 27B Q4 | 262,144 | 1 | 1 |
| B or `rana2` | Rana 9B Q8 | 32,768 | 2 | 2 |
| C or `rana3` | Rana 9B Q8 | 32,768 | 3 | 3 |
| D or `rana1` | Rana 9B Q8 | 32,768 | 1 | 1 |
| D256 or `rana256` | Rana 9B Q8 | 262,144 | 1 | 1 |

GPU 0 is fixed at `qwen3.8-unqwen:27b-256k`, Q4_K_M, 262,144 context, one Ollama slot. The Q8 64K GPU 1 default measured 30,377,540,320 bytes for both `size` and `size_vram` in Ollama `/api/ps`, with 66/66 layers on the GPU. The Q4 256K GPU 0 model measured 26,816,607,681 bytes for both values with 66/66 layers. The Ollama runner logged an 8,704 MiB GPU KV buffer for GPU 0 at 256K and a 2,176 MiB GPU KV buffer for GPU 1 at 64K. Both runners logged 66/66 GPU layers. The model's native context ceiling is 262,144; 300K is not supported by this model tag. Context is a shared input and output budget, not a guarantee that a 256K output is practical. At 256K, one Q4 slot is the full-GPU limit on each V100: a two-request GPU 1 probe allocated 39,379,040,987 bytes total but only 31,968,544,684 bytes in VRAM, spilling 7,410,496,303 bytes to CPU. The test restored one slot, and three full-size slots were not attempted. More slots require shorter per-request contexts and separate benchmarking.

The GPU 1 bundle sets `OLLAMA_NUM_CTX`, `OLLAMA_CONTEXT_LENGTH`, and `LLAMAFARM_ADAPTIVE_CONTEXT_MAX` from `GPU1_CONTEXT` in `.env`. LlamaFarm's adaptive maximum is essential: setting only Ollama's context environment variables allowed a LlamaFarm `/agent` request to allocate 262,144 context for the 64K Q8 tag, causing CPU spill. After the cap was applied, a real `/agent` file-read task completed and `/api/ps` reported 65,536 context with `size_vram == size`.
The GPU 1 bundle disables LlamaFarm internal reasoning after a measured 13-minute reasoning turn hit an Ollama 500; a focused tool task then completed in 37 seconds. Kilo uses Ollama directly and is unaffected by that LlamaFarm setting.

## Runtime selection

On the Mac, run `python3 ~/.config/opencode/gpu1-profile.py A|B|C|D|q4-256k|status`. Run `python3 ~/.config/opencode/gpu1-profile.py q4-256k` to give Kilo the same Q4 256K model that OpenCode uses on GPU 0, but loaded independently on GPU 1. The wrapper calls `scripts/kilo/gpu1-profile.sh` on the V100, then updates OpenCode and Kilo model choices together and reloads both clients. It preserves both applications' session databases. Kilo's picker lists only the active GPU 1 profile; the GPU 0 provider and inactive model choices are removed to prevent unplanned model swaps. The wrapper refuses a profile change while a GPU 1 specialist or Kilo turn is active; GPU 0 primary turns can continue. The remote script also refuses a change while GPU 1 is generating; it verifies Docker GPU assignment, requested context, weight quantization, and full VRAM residency before reporting success. `OLLAMA_MAX_QUEUE=32` bounds the Ollama queue. The OpenCode plugin queues child requests beyond the selected child limit; it never creates a child just to fill a free slot.
The selector reads `scripts/kilo/gpu1-capacity.json`. The bundled benchmark found that three Rana requests are slower than two, so the tested cap is **2**. Selecting C now automatically uses two Rana slots for both Ollama and OpenCode; D remains one slot.

The Mac VS Code extensions are `sst-dev.opencode` and `kilocode.kilo-code`. OpenCode's primary provider and Kilo's GPU 1 provider use the Tailscale Ollama ports, not a local inference process. The gateway's MCP endpoint provides a separate LlamaFarm agent and tool path.
The old `llamafarm-fast` Codex MCP entry was removed because it still targeted the GPU 0 gateway with a stale Rana tag and could evict the primary model; GPU 1 delegation is provided by OpenCode and LlamaFarm federation.

## Verification

Check `docker inspect` DeviceRequests, `docker exec LlamaFarm ollama ps`, `docker exec LlamaFarm-GPU1 ollama ps`, and `nvidia-smi` together. For each loaded model, require `size_vram == size` and the requested context; a high GPU utilization percentage alone does not prove the model is wholly on the card. Verify each selected profile with real OpenCode delegation and completed parent review, not only direct Ollama chat. Validate Kilo tool execution, LlamaFarm `/agent` tool execution, MCP, and GPU 0 responsiveness after any change.

The [2026-11-17 bundled benchmark](../benchmarks/2026-11-17-dual-v100/README.md) used distinct 11,809-token coding prompts and a real structured function call. All seven calls were valid. Rana completed 12.06, 13.46, and 12.83 tasks/min with one, two, and three slots; GPU KV allocation was 544, 1,088, and 1,632 MiB respectively. All profiles stayed fully GPU resident. Direct benchmark success does not replace real OpenCode delegation and parent review.
