#!/usr/bin/env bash
# moab.sh — status + self-repair for the LlamaFarm/Ollama/Kilo stack on pop-os-1.
# Run directly on pop-os-1, or: ssh bat@100.107.226.49 'bash ~/LlamaFarm-dual-v100-deploy/scripts/kilo/moab.sh'
#
# Usage:
#   ./moab.sh                 status + auto-repair known issues (safe, idempotent)
#   ./moab.sh enable-gpu1     register a second Ollama worker on GPU1 (refuses if GPU1 is busy)
#   ./moab.sh disable-gpu1    tear the GPU1 worker back down

set -uo pipefail

CONTAINER="LlamaFarm"
CONFIG="/llamafarm-data/.llamafarm/config.toml"
GPU1_UUID="GPU-4dda4033-eb45-c5a7-fe70-16979f13147c"
GPU1_PORT=11436
ROWS=()
REPAIRS=()

row() { ROWS+=("$1|$2|$3"); }
repair() { REPAIRS+=("$1"); }

# --- container health -------------------------------------------------
if docker inspect "$CONTAINER" >/dev/null 2>&1; then
  status=$(docker inspect "$CONTAINER" --format '{{.State.Status}}')
  health=$(docker inspect "$CONTAINER" --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}n/a{{end}}')
  if [ "$status" = "running" ]; then
    row "LlamaFarm container" "OK" "running, health=$health"
  else
    row "LlamaFarm container" "FAIL" "status=$status"
    docker start "$CONTAINER" >/dev/null 2>&1 && repair "Started stopped LlamaFarm container"
  fi
else
  row "LlamaFarm container" "FAIL" "container not found"
fi

# --- debris detection (containers matching known junk patterns, not the real one) ---
debris=$(docker ps -a --format '{{.Names}}' | grep -E '^llamafarm-(gpu.*-conditional|dual-oversized|gpu-router|ollama-worker-gpu[0-9]+)$' || true)
if [ -n "$debris" ]; then
  row "Debris containers" "FAIL" "$(echo "$debris" | tr '\n' ' ')"
else
  row "Debris containers" "OK" "none found"
fi

# --- gateway health -----------------------------------------------------
health_json=$(curl -s --max-time 5 http://127.0.0.1:42617/api/health 2>/dev/null || true)
if echo "$health_json" | grep -q '"status":"ok"'; then
  row "Gateway health" "OK" "all components ok"
else
  row "Gateway health" "FAIL" "unhealthy or unreachable"
fi

# --- api_url sanity + auto-repair ---------------------------------------
api_url_line=$(docker exec "$CONTAINER" grep '^api_url' "$CONFIG" 2>/dev/null || true)
if echo "$api_url_line" | grep -q '127.0.0.1:11434'; then
  row "Gateway api_url" "OK" "127.0.0.1:11434"
else
  row "Gateway api_url" "FAIL" "$api_url_line"
  docker exec "$CONTAINER" cp "$CONFIG" "${CONFIG}.before-moab-autofix-$(date +%Y%m%d-%H%M%S)" 2>/dev/null
  docker exec "$CONTAINER" sed -i 's|^api_url = .*|api_url = "http://127.0.0.1:11434"|' "$CONFIG" 2>/dev/null
  docker restart "$CONTAINER" >/dev/null 2>&1
  sleep 5
  repair "api_url pointed at a dead host — repaired to 127.0.0.1:11434 and restarted gateway"
fi

# --- tailscale serve for 11434 -------------------------------------------
if tailscale serve status 2>/dev/null | grep -q ':11434'; then
  row "Tailscale serve 11434" "OK" "exposed to tailnet"
else
  row "Tailscale serve 11434" "FAIL" "not exposed"
  sudo tailscale serve --bg --tcp=11434 tcp://127.0.0.1:11434 >/dev/null 2>&1 && \
    repair "Tailscale serve for 11434 was missing — re-enabled"
fi

# --- models + context ------------------------------------------------
tags_json=$(curl -s --max-time 5 http://127.0.0.1:11434/api/tags 2>/dev/null || true)
check_model() {
  local tag="$1" label="$2"
  if echo "$tags_json" | grep -q "\"$tag\""; then
    local ctx
    ctx=$(curl -s --max-time 5 http://127.0.0.1:11434/api/show -d "{\"model\":\"$tag\"}" 2>/dev/null \
      | python3 -c 'import json,sys; d=json.load(sys.stdin); print(d.get("parameters",""))' 2>/dev/null \
      | grep num_ctx || true)
    if echo "$ctx" | grep -q 262144; then
      row "$label" "OK" "num_ctx=262144"
    else
      row "$label" "FAIL" "missing num_ctx override"
    fi
  else
    row "$label" "FAIL" "tag not found"
  fi
}
check_model "qwen3.8-unqwen:27b-256k" "Model: unqwen-27b"
check_model "qwen3.5-rana:9b-256k" "Model: rana-9b-256k"
if ! echo "$tags_json" | grep -q 'qwen3.5-rana:9b-256k'; then
  cat > /tmp/rana-256k.Modelfile <<'EOF'
FROM abliterant/qwen3.5-rana:9b-q8_0
PARAMETER num_ctx 262144
EOF
  ollama create qwen3.5-rana:9b-256k -f /tmp/rana-256k.Modelfile >/dev/null 2>&1 && \
    repair "Rebuilt missing qwen3.5-rana:9b-256k with num_ctx=262144"
fi

# --- GPU status ----------------------------------------------------------
gpu_line_fn() {
  nvidia-smi --query-gpu=index,memory.used,memory.total,utilization.gpu --format=csv,noheader,nounits | sed -n "$1p"
}
for i in 0 1; do
  line=$(gpu_line_fn $((i+1)))
  used=$(echo "$line" | awk -F', ' '{print $2}')
  total=$(echo "$line" | awk -F', ' '{print $3}')
  util=$(echo "$line" | awk -F', ' '{print $4}')
  if [ "${util:-100}" -lt 20 ] 2>/dev/null; then
    row "GPU$i" "FREE" "${util}% util, ${used}/${total} MB"
  else
    row "GPU$i" "BUSY" "${util}% util, ${used}/${total} MB"
  fi
done

# --- gpu1 worker enable/disable subcommands -------------------------------
cmd="${1:-status}"
if [ "$cmd" = "enable-gpu1" ]; then
  gpu1_util=$(gpu_line_fn 2 | awk -F', ' '{print $4}')
  if [ "${gpu1_util:-100}" -ge 20 ]; then
    echo "REFUSED: GPU1 is busy (${gpu1_util}% util). Not starting a second worker."
    exit 1
  fi
  if ! docker ps --format '{{.Names}}' | grep -q '^llamafarm-ollama-worker-gpu1$'; then
    docker run -d --name llamafarm-ollama-worker-gpu1 --restart unless-stopped \
      --gpus '"device=1"' -e CUDA_VISIBLE_DEVICES=1 \
      -p 127.0.0.1:${GPU1_PORT}:11434 \
      -v llamafarm-bundle-data:/llamafarm-data \
      llamafarm-local:gpu-layout-20261116 /usr/bin/ollama serve >/dev/null 2>&1
    sleep 3
  fi
  sudo tailscale serve --bg --tcp=${GPU1_PORT} tcp://127.0.0.1:${GPU1_PORT} >/dev/null 2>&1
  echo "GPU1 worker started on port ${GPU1_PORT} (http://100.107.226.49:${GPU1_PORT})."
  echo "Point a second Kilo provider at http://100.107.226.49:${GPU1_PORT}/v1 — do not route this through the gateway."
  exit 0
elif [ "$cmd" = "disable-gpu1" ]; then
  docker rm -f llamafarm-ollama-worker-gpu1 >/dev/null 2>&1
  tailscale serve --tcp=${GPU1_PORT} off >/dev/null 2>&1
  echo "GPU1 worker stopped and un-exposed."
  exit 0
fi

# --- print clean table -----------------------------------------------
echo
echo "================================================================"
echo "  MOAB — pop-os-1 (dual V100) status"
echo "  $(date '+%Y-%m-%d %H:%M:%S %Z')"
echo "================================================================"
printf "%-24s %-7s %s\n" "COMPONENT" "STATUS" "DETAIL"
echo "----------------------------------------------------------------"
for r in "${ROWS[@]}"; do
  IFS='|' read -r name status detail <<< "$r"
  printf "%-24s %-7s %s\n" "$name" "$status" "$detail"
done
echo "----------------------------------------------------------------"
if [ ${#REPAIRS[@]} -gt 0 ]; then
  echo "REPAIRS APPLIED THIS RUN:"
  for r in "${REPAIRS[@]}"; do echo "  - $r"; done
else
  echo "No repairs needed."
fi
echo "================================================================"
