# LlamaFarm

Local Ollama agent runtime with a web UI, bundled tools, and a one-container
local stack option.

## Quick Start

Clone the repo and launch the bundled local stack:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle.sh
```

Then open:

```text
http://127.0.0.1:42617
```

What the bundled stack includes:

1. LlamaFarm web UI and gateway
2. Internal Ollama runtime
3. Internal Chromium + chromedriver
4. Automatic model pulls for `qwen3.5:9b` and `devstral-small-2:latest`

## Gaming / NVIDIA Quick Start

If the machine has an NVIDIA GPU and you want to force the CUDA path:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle-nvidia.sh
```

## Backend Selection

The bundled launcher auto-detects the best backend in this order:

1. NVIDIA / CUDA
2. AMD ROCm
3. Vulkan (`/dev/dri`)
4. CPU

You can force a backend manually:

```bash
LLAMAFARM_GPU_MODE=nvidia ./scripts/docker/up-bundle.sh
LLAMAFARM_GPU_MODE=rocm ./scripts/docker/up-bundle.sh
LLAMAFARM_GPU_MODE=vulkan ./scripts/docker/up-bundle.sh
LLAMAFARM_GPU_MODE=cpu ./scripts/docker/up-bundle.sh
```

## Useful Commands

```bash
docker logs -f LlamaFarm
docker exec LlamaFarm ollama list
docker exec LlamaFarm sh -lc 'tail -n 80 /tmp/ollama.log'
```

## More Docs

- [One-Click Bootstrap](docs/one-click-bootstrap.md)
- [Getting Started](docs/getting-started/README.md)
- [Docs Hub](docs/README.md)
