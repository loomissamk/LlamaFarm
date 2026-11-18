#!/usr/bin/env bash
# Select the model served by the GPU1 LlamaFarm bundle. The Mac wrapper updates
# its OpenCode and Kilo model selections after this script succeeds.
set -euo pipefail

cd /home/bat/LlamaFarm-dual-v100-deploy
compose=(docker compose -f docker-compose.bundle.yml -f docker-compose.dual-v100.yml)
profile=${1:-status}
capacity_file=/home/bat/LlamaFarm-dual-v100-deploy/scripts/kilo/gpu1-capacity.json
capacity() {
  python3 - "$capacity_file" <<'CAP'
import json, sys
with open(sys.argv[1]) as handle:
    value = int(json.load(handle)["max_stable_parallel"])
if value not in (1, 2, 3):
    raise SystemExit("invalid GPU1 stable capacity")
print(value)
CAP
}

case "$profile" in
  capacity) capacity; exit 0 ;;
  status)
    docker inspect LlamaFarm-GPU1 --format 'container={{.State.Status}} device={{json .HostConfig.DeviceRequests}}'
    docker exec LlamaFarm-GPU1 env | grep -E '^(LLAMAFARM_MODEL|OLLAMA_NUM_PARALLEL|OLLAMA_CONTEXT_LENGTH)='
    curl -fsS http://127.0.0.1:11436/api/ps
    echo
    exit 0
    ;;
  q8-32k) model=qwen3.8-unqwen:27b-q8-32k; context=32768; parallel=1; quant=Q8_0 ;;
  q8-64k|A) model=qwen3.8-unqwen:27b-q8-64k; context=65536; parallel=1; quant=Q8_0 ;;
  q4-256k) model=qwen3.8-unqwen:27b-256k; context=262144; parallel=1; quant=Q4_K_M ;;
  rana1|D) model=qwen3.5-rana:9b-32k; context=32768; parallel=1; quant=Q8_0 ;;
  rana2|B) model=qwen3.5-rana:9b-32k; context=32768; parallel=2; quant=Q8_0 ;;
  rana3|C) model=qwen3.5-rana:9b-32k; context=32768; parallel=3; quant=Q8_0 ;;
  rana256|D256) model=qwen3.5-rana:9b-256k; context=262144; parallel=1; quant=Q8_0 ;;
  *) echo 'usage: gpu1-profile.sh q8-32k|q8-64k|q4-256k|rana1|rana2|rana3|rana256|status' >&2; exit 2 ;;
esac

if [[ "$model" == qwen3.5-rana:9b-32k ]] && (( parallel > $(capacity) )); then
  parallel=$(capacity)
  echo "Requested $profile exceeds tested GPU1 capacity; using $parallel Rana slot(s)" >&2
fi

if [[ "$(docker inspect -f '{{.State.Running}}' LlamaFarm-GPU1 2>/dev/null || true)" != true ]]; then
  echo 'LlamaFarm-GPU1 is not running' >&2
  exit 1
fi
docker exec LlamaFarm-GPU1 ollama show "$model" >/dev/null
util=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | sed -n '2p' | tr -d ' %')
if (( util > 20 )); then
  echo "GPU1 is busy (${util}%); finish the current Kilo/OpenCode request before switching" >&2
  exit 3
fi

backup_dir=/home/bat/.local/share/llamafarm-backups
mkdir -p "$backup_dir"
backup="$backup_dir/gpu1-profile-env-$(date +%Y%m%d-%H%M%S).env"
cp -p .env "$backup"

rollback() {
  trap - ERR
  echo "Profile switch failed; restoring $backup" >&2
  cp -p "$backup" .env
  "${compose[@]}" up -d --no-build --no-deps llamafarm-gpu1 || true
}
trap rollback ERR

python3 - "$model" "$context" "$parallel" <<'PY'
from pathlib import Path
import os, re, stat, sys

path = Path('.env')
text = path.read_text()
changes = {
    'GPU1_MODEL': sys.argv[1],
    'GPU1_CONTEXT': sys.argv[2],
    'GPU1_OLLAMA_NUM_PARALLEL': sys.argv[3],
}
for key, value in changes.items():
    replacement = f'{key}={value}'
    text, count = re.subn(rf'^{key}=.*$', replacement, text, flags=re.MULTILINE)
    if not count:
        text = text.rstrip('\n') + '\n' + replacement + '\n'
tmp = path.with_suffix('.env.tmp')
tmp.write_text(text)
os.chmod(tmp, stat.S_IMODE(path.stat().st_mode))
os.replace(tmp, path)
PY

"${compose[@]}" up -d --no-build --no-deps llamafarm-gpu1
for _ in $(seq 1 90); do
  if docker exec LlamaFarm-GPU1 curl -fsS --max-time 2 http://127.0.0.1:42617/health >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
docker exec LlamaFarm-GPU1 curl -fsS --max-time 5 http://127.0.0.1:42617/health >/dev/null

python3 - "$model" "$context" "$quant" <<'PY'
import json, sys, urllib.request

model, context, quant = sys.argv[1], int(sys.argv[2]), sys.argv[3]
url = 'http://127.0.0.1:11436'
payload = {'model': model, 'prompt': 'Say OK.', 'stream': False,
           'think': False, 'options': {'num_ctx': context, 'num_predict': 8},
           'keep_alive': '24h'}
request = urllib.request.Request(url + '/api/generate',
    data=json.dumps(payload).encode(), headers={'Content-Type': 'application/json'})
with urllib.request.urlopen(request, timeout=240) as response:
    json.load(response)
with urllib.request.urlopen(url + '/api/ps', timeout=10) as response:
    models = json.load(response)['models']
match = next((item for item in models if item['name'] == model), None)
if not match or match['size_vram'] != match['size'] or match['context_length'] != context:
    raise SystemExit(f'Not fully GPU-resident with requested context: {match}')
if match['details']['quantization_level'] != quant:
    raise SystemExit(f'Wrong weight quantization: {match["details"]}')
print(json.dumps({'profile_model': model, 'context': context,
                  'quantization': quant, 'size_vram': match['size_vram'],
                  'size': match['size']}))
PY

docker inspect LlamaFarm-GPU1 --format '{{json .HostConfig.DeviceRequests}}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert len(x)==1 and x[0]["DeviceIDs"]==["1"],x'
test "$(docker exec LlamaFarm-GPU1 sh -lc 'printenv OLLAMA_NUM_PARALLEL')" = "$parallel"
test "$(docker exec LlamaFarm-GPU1 sh -lc 'printenv LLAMAFARM_ADAPTIVE_CONTEXT_MAX')" = "$context"
trap - ERR
echo "GPU1 profile=$profile model=$model context=$context parallel=$parallel fully_gpu_resident=true"
