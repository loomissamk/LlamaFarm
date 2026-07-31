#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE_DIR="$ROOT_DIR/deploy/node-profiles"

if [ "$#" -ne 1 ]; then
  echo "Usage: scripts/docker/check-node.sh <rtx4070-laptop|rtx5070ti-16gb>" >&2
  exit 2
fi

PROFILE_FILE="$PROFILE_DIR/$1.env"
if [ ! -f "$PROFILE_FILE" ]; then
  echo "Unknown node profile: $1" >&2
  exit 2
fi

NODE_ENV="${LLAMAFARM_NODE_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/llamafarm/node.env}"
set -a
# shellcheck disable=SC1090
source "$PROFILE_FILE"
if [ -f "$NODE_ENV" ]; then
  # shellcheck disable=SC1090
  source "$NODE_ENV"
fi
set +a

echo "== GPU =="
nvidia-smi --query-gpu=name,memory.total,memory.used,driver_version --format=csv,noheader
echo
echo "== Container health =="
docker inspect --format 'LlamaFarm: {{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' LlamaFarm
docker inspect --format 'Qdrant: {{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' Qdrant
echo
echo "== GPU visible inside bundle =="
docker exec LlamaFarm nvidia-smi --query-gpu=name,memory.total,memory.used --format=csv,noheader
echo
echo "== Host Docker control from the agent runtime =="
docker exec -u "${LLAMAFARM_RUNTIME_UID:-$(id -u)}:${LLAMAFARM_RUNTIME_GID:-$(id -g)}" LlamaFarm \
  sh -lc 'docker version --format "client={{.Client.Version}} server={{.Server.Version}}" && docker compose version && docker ps --format "{{.Names}}" >/dev/null'
echo
echo "== Gateway and local-only services =="
curl --fail --silent --show-error "http://127.0.0.1:${HOST_PORT:-42617}/health"
echo
curl --fail --silent --show-error "http://127.0.0.1:${QDRANT_PORT:-6333}/collections" >/dev/null
echo "Qdrant reachable on loopback."
echo
echo "== Federation authentication =="
if [ -z "${LLAMAFARM_FEDERATION_TOKEN:-}" ]; then
  echo "LLAMAFARM_FEDERATION_TOKEN is missing from $NODE_ENV" >&2
  exit 1
fi
curl --fail --silent --show-error \
  -H "x-llamafarm-federation-token: $LLAMAFARM_FEDERATION_TOKEN" \
  "http://127.0.0.1:${HOST_PORT:-42617}/federation/health"
echo
IFS=',' read -r -a PEERS <<<"${LLAMAFARM_MANUAL_PEERS:-}"
for peer in "${PEERS[@]}"; do
  peer="${peer//[[:space:]]/}"
  [ -n "$peer" ] || continue
  curl --fail --silent --show-error \
    -H "x-llamafarm-federation-token: $LLAMAFARM_FEDERATION_TOKEN" \
    "${peer%/}/federation/health"
  echo
done
echo
echo "== Effective Ollama allocation =="
docker exec LlamaFarm ollama ps
echo
echo "== Cgroup status (max/0 means unlimited) =="
docker inspect --format 'LlamaFarm: {{.HostConfig.Memory}} / {{.HostConfig.NanoCPUs}} / {{.HostConfig.PidsLimit}}' LlamaFarm
docker inspect --format 'Qdrant: {{.HostConfig.Memory}} / {{.HostConfig.NanoCPUs}} / {{.HostConfig.PidsLimit}}' Qdrant
