#!/usr/bin/env bash
# Launch LlamaFarm with ALL compute: RTX 4070 + Intel iGPU + Intel NPU
set -euo pipefail
cd "$(dirname "$0")/../.."

echo "Starting LlamaFarm — all-devices stack..."
echo "  GPU:  RTX 4070 Laptop (primary, port 11434)"
echo "  iGPU: Intel Arc Xe via ipex-llm (port 11435)"
echo "  NPU:  Intel AI Boost via OpenVINO (port 11436)"
echo ""

docker compose \
  -f docker-compose.bundle.yml \
  -f docker-compose.bundle.nvidia.yml \
  -f docker-compose.bundle.alldevices.yml \
  up -d "$@"

echo ""
echo "Stack up. After healthy:"
echo "  OpenWebUI:    http://localhost:3000"
echo "  LlamaFarm:    http://localhost:42617"
echo "  RTX 4070:     http://localhost:11434"
echo "  Intel iGPU:   http://localhost:11435"
echo "  Intel NPU:    http://localhost:11436/health"
echo ""
echo "In OpenWebUI → Settings → Connections → add:"
echo "  http://localhost:11435  (Intel iGPU/CPU models)"
echo "  http://localhost:11436  (NPU embeddings)"
