#!/usr/bin/env bash
# Move GPU0's tailnet Ollama port from the pinned standalone container into
# the GPU0 LlamaFarm bundle. Does not delete the old container or model volume.
set -euo pipefail
cd /home/bat/LlamaFarm-dual-v100-deploy
compose=(docker compose -f docker-compose.bundle.yml -f docker-compose.dual-v100.yml)

if [[ "$(docker inspect -f '{{.State.Running}}' llamafarm-ollama-gpu0 2>/dev/null || true)" != true ]]; then
  echo 'Standalone GPU0 Ollama is not running; inspect current topology first' >&2
  exit 2
fi
util=$(nvidia-smi --query-gpu=utilization.gpu --format=csv,noheader,nounits | sed -n '1p' | tr -d ' %')
if (( util > 20 )); then
  echo "GPU0 is generating (${util}%); preserve the active OpenCode request" >&2
  exit 3
fi
if ss -Htn state established '( sport = :11434 )' | grep -q .; then
  echo 'Port 11434 has an established client; preserve the active OpenCode request' >&2
  exit 3
fi
if [[ "${1:-}" == --check ]]; then
  echo 'GPU0 is idle with no established 11434 client; cutover gate passed'
  exit 0
fi
if [[ "${1:-}" != --apply ]]; then
  echo 'usage: dual-v100-cutover.sh --check|--apply' >&2
  exit 2
fi

backup=/home/bat/.local/share/llamafarm-backups/gpu0-cutover-$(date +%Y%m%d-%H%M%S)
mkdir -p "$backup"
cp -p .env "$backup/.env"
docker cp LlamaFarm:/llamafarm-data/.llamafarm/config.toml "$backup/config.toml"
rollback() {
  code=$?
  trap - EXIT
  if (( code == 0 )); then return; fi
  echo "Cutover failed; restoring config and standalone GPU0 Ollama from $backup" >&2
  cp -p "$backup/.env" .env
  docker cp "$backup/config.toml" LlamaFarm:/llamafarm-data/.llamafarm/config.toml || true
  docker image tag llamafarm-local:bundle-pre-dual-20261117 llamafarm-local:bundle || true
  "${compose[@]}" up -d --no-build --no-deps llamafarm || true
  docker start llamafarm-ollama-gpu0 || true
}
trap rollback EXIT

docker stop llamafarm-ollama-gpu0
docker exec -i LlamaFarm python3 - <<'PY'
from pathlib import Path
import re, tomllib
p=Path('/llamafarm-data/.llamafarm/config.toml')
s=p.read_text()
for key in ('api_url', 'endpoint'):
    old=f'{key} = "http://gpu0-ollama:11434"'
    new=f'{key} = "http://127.0.0.1:11434"'
    assert s.count(old)==1, (key,s.count(old))
    s=s.replace(old,new,1)
tomllib.loads(s)
p.write_text(s)
PY
python3 - <<'PY'
from pathlib import Path
p=Path('.env');s=p.read_text();old='OLLAMA_PORT=19999';assert s.count(old)==1;s=s.replace(old,'OLLAMA_PORT=11434',1);p.write_text(s)
PY
"${compose[@]}" config --quiet
"${compose[@]}" up -d --no-build --no-deps llamafarm
for _ in $(seq 1 90); do
  if curl -fsS --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && curl -fsS --max-time 2 http://127.0.0.1:42617/health >/dev/null 2>&1; then break; fi
  sleep 1
done
python3 - <<'PY'
import json,urllib.request
base='http://127.0.0.1:11434'
payload={'model':'qwen3.8-unqwen:27b-256k','prompt':'Say OK.','stream':False,'think':False,'options':{'num_ctx':262144,'num_predict':8},'keep_alive':'24h'}
r=urllib.request.Request(base+'/api/generate',data=json.dumps(payload).encode(),headers={'Content-Type':'application/json'})
with urllib.request.urlopen(r,timeout=240) as response: json.load(response)
with urllib.request.urlopen(base+'/api/ps',timeout=10) as response: models=json.load(response)['models']
model=next((m for m in models if m['name']=='qwen3.8-unqwen:27b-256k'),None)
if not model or model['context_length']!=262144 or model['size']!=model['size_vram']:
    raise SystemExit(f'GPU0 model is not fully resident at 256K: {model}')
print(json.dumps({'model':model['name'],'context':model['context_length'],'size_vram':model['size_vram'],'size':model['size']}))
PY
if [[ "$(docker inspect -f '{{.State.Running}}' llamafarm-ollama-gpu0)" != false ]]; then
  echo 'Old GPU0 standalone container unexpectedly running' >&2
  exit 1
fi
docker inspect LlamaFarm --format '{{json .HostConfig.DeviceRequests}}' | python3 -c 'import json,sys; x=json.load(sys.stdin); assert len(x)==1 and x[0]["DeviceIDs"]==["0"],x'
echo 'GPU0 bundle cutover complete; standalone container preserved stopped'
