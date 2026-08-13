#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE_DIR="$ROOT_DIR/deploy/node-profiles"

if [ "$#" -ne 1 ]; then
  echo "Usage: scripts/docker/check-node.sh <rtx3050ti-windows-wsl|rtx4070-laptop|rtx5070ti-16gb|v100-32gb>" >&2
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

EXPECTED_V100_UUID=""
if [ "$1" = v100-32gb ]; then
  V100_HOST_CONFIG="${LLAMAFARM_V100_HOST_CONFIG:-/etc/llamafarm/v100-cuda-5070-nvk.env}"
  if [ ! -r "$V100_HOST_CONFIG" ]; then
    echo "V100 host configuration is not readable: $V100_HOST_CONFIG" >&2
    exit 1
  fi
  # shellcheck disable=SC1090
  source "$V100_HOST_CONFIG"
  if [[ ! "${V100_GPU_UUID:-}" =~ ^GPU-[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]]; then
    echo "V100_GPU_UUID in $V100_HOST_CONFIG is missing or invalid." >&2
    exit 1
  fi
  case "${LLAMAFARM_NVIDIA_VISIBLE_DEVICES:-}" in
    ""|all|ALL|*,*)
      echo "v100-32gb requires exactly one configured V100 UUID; broad visibility is rejected." >&2
      exit 1
      ;;
  esac
  if [ "$LLAMAFARM_NVIDIA_VISIBLE_DEVICES" != "$V100_GPU_UUID" ]; then
    echo "LLAMAFARM_NVIDIA_VISIBLE_DEVICES does not match the configured V100 UUID." >&2
    exit 1
  fi
  EXPECTED_V100_UUID="$V100_GPU_UUID"
  "$ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh" verify "$V100_HOST_CONFIG"
fi

echo "== GPU =="
nvidia-smi --query-gpu=name,memory.total,memory.used,driver_version --format=csv,noheader
echo
echo "== Container health =="
docker inspect --format 'LlamaFarm: {{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' LlamaFarm
docker inspect --format 'Qdrant: {{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{end}}' Qdrant
echo
echo "== GPU visible inside bundle =="
docker exec LlamaFarm nvidia-smi --query-gpu=name,memory.total,memory.used --format=csv,noheader
if [ -n "$EXPECTED_V100_UUID" ]; then
  mapfile -t CONTAINER_GPU_UUIDS < <(
    docker exec LlamaFarm nvidia-smi --query-gpu=uuid --format=csv,noheader,nounits
  )
  if [ "${#CONTAINER_GPU_UUIDS[@]}" -ne 1 ] \
    || [ "${CONTAINER_GPU_UUIDS[0]//[[:space:]]/}" != "$EXPECTED_V100_UUID" ]; then
    echo "Container does not expose exactly the configured V100 UUID." >&2
    exit 1
  fi
  CONTAINER_VISIBLE="$(docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' LlamaFarm \
    | awk -F= '$1 == "NVIDIA_VISIBLE_DEVICES" {sub(/^[^=]*=/, ""); print; exit}')"
  if [ "$CONTAINER_VISIBLE" != "$EXPECTED_V100_UUID" ]; then
    echo "Container NVIDIA_VISIBLE_DEVICES is not pinned to the configured V100 UUID." >&2
    exit 1
  fi
  if docker exec LlamaFarm sh -lc \
    'for device in /dev/dri/*; do [ ! -e "$device" ] || exit 0; done; exit 1'; then
    echo "V100 compute-only container unexpectedly exposes a /dev/dri device." >&2
    exit 1
  fi
  echo "Container GPU pin verified: exactly one configured V100 UUID."
fi
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
echo "== Persistent workspace Git and SOPs =="
docker exec LlamaFarm sh -lc '
  cd "${LLAMAFARM_WORKSPACE:-/llamafarm-data/workspace}"
  git rev-parse --is-inside-work-tree
  git remote -v
  test -f workflows/tool-capability-audit.md
  test -f sops/authorized-wlan-audit/SOP.toml
'
docker logs --since 10m LlamaFarm 2>&1 | grep -E 'SOP engine loaded [1-9][0-9]* SOPs' | tail -1
echo
echo "== Cgroup status (max/0 means unlimited) =="
docker inspect --format 'LlamaFarm: {{.HostConfig.Memory}} / {{.HostConfig.NanoCPUs}} / {{.HostConfig.PidsLimit}}' LlamaFarm
docker inspect --format 'Qdrant: {{.HostConfig.Memory}} / {{.HostConfig.NanoCPUs}} / {{.HostConfig.PidsLimit}}' Qdrant
