#!/usr/bin/env bash
# gpu1-profile.sh -- switch GPU1's single Ollama container between selectable
# profiles. One user, one GPU1 inference slot by default -- OLLAMA_NUM_PARALLEL
# is a container-level setting, not per-model, so switching profiles means
# recreating the container with the right settings, not just swapping models.
# Rana 9B is the one deliberate exception: it's small enough that a few
# concurrent slots cost almost nothing, unlike the 27B where concurrency was
# reserving VRAM for simultaneous users that don't exist.
#
# Usage: ./gpu1-profile.sh <profile>
#   q8-64k   (default) UnQwen 27B Q8, 64K ctx, 1 slot -- 30.2GB, measured fully GPU-resident
#   q8-32k   UnQwen 27B Q8, 32K ctx, 1 slot -- 28.96GB
#   q8-80k   UnQwen 27B Q8, 80K ctx, 1 slot -- 31.03GB, ~1.9GB headroom
#   q8-98k   UnQwen 27B Q8, 98K ctx, 1 slot -- 31.69GB, ~1GB headroom (tight, not recommended as default)
#   rana     Rana 9B Q8, 32K ctx, 3 slots -- cheap concurrent subagent pool
#   status   show current model + settings, no change

set -euo pipefail
CONTAINER="opencode-ollama-gpu1-c"
PORT=11436

status() {
  echo "--- current ---"
  docker inspect "$CONTAINER" --format 'NUM_PARALLEL env: {{range .Config.Env}}{{if (index (split . "=") 0) eq "OLLAMA_NUM_PARALLEL"}}{{.}}{{end}}{{end}}' 2>/dev/null \
    | grep OLLAMA_NUM_PARALLEL || docker exec "$CONTAINER" env 2>/dev/null | grep OLLAMA_NUM_PARALLEL
  curl -s http://127.0.0.1:${PORT}/api/ps 2>/dev/null
  echo
}

recreate() {
  local parallel="$1"
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  docker run -d --name "$CONTAINER" --restart unless-stopped \
    --gpus device=1 \
    -p 127.0.0.1:${PORT}:11434 \
    -v llamafarm-bundle-data:/llamafarm-data \
    -e OLLAMA_MODELS=/llamafarm-data/.ollama/models \
    -e OLLAMA_HOST=0.0.0.0:11434 \
    -e OLLAMA_KEEP_ALIVE=24h \
    -e OLLAMA_NUM_PARALLEL="$parallel" \
    -e OLLAMA_MAX_LOADED_MODELS=1 \
    -e OLLAMA_FLASH_ATTENTION=1 \
    -e OLLAMA_KV_CACHE_TYPE=q8_0 \
    ollama/ollama:latest >/dev/null
  sleep 4
}

load_and_verify() {
  local model="$1"
  curl -s http://127.0.0.1:${PORT}/api/generate \
    -d "{\"model\":\"${model}\",\"prompt\":\"hi\",\"keep_alive\":\"24h\"}" \
    -o /dev/null -w 'load: %{http_code}\n'
  sleep 8
  local ps
  ps=$(curl -s http://127.0.0.1:${PORT}/api/ps)
  echo "$ps"
  python3 -c "
import json,sys
d = json.loads('''$ps''')
for m in d.get('models', []):
    full = m['size'] == m['size_vram']
    print(f\"{m['name']}: fully_gpu_resident={full} size_vram={m['size_vram']} size={m['size']}\")
"
}

profile="${1:-status}"
case "$profile" in
  status) status ;;
  q8-32k) recreate 1; load_and_verify "qwen3.8-unqwen:27b-q8-32k" ;;
  q8-64k) recreate 1; load_and_verify "qwen3.8-unqwen:27b-q8-64k" ;;
  q8-80k) recreate 1; load_and_verify "qwen3.8-unqwen:27b-q8-80k" ;;
  q8-98k) recreate 1; load_and_verify "qwen3.8-unqwen:27b-q8-98k" ;;
  rana)   recreate 3; load_and_verify "abliterant/qwen3.5-rana:9b-q8_0" ;;
  *) echo "Unknown profile: $profile" >&2; echo "Valid: q8-32k q8-64k q8-80k q8-98k rana status" >&2; exit 2 ;;
esac
