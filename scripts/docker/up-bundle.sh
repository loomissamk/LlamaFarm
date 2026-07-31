#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BASE_COMPOSE="$ROOT_DIR/docker-compose.bundle.yml"
GPU_MODE="${LLAMAFARM_GPU_MODE:-auto}"
DEFAULT_PULL_MODELS=""
OLLAMA_IMAGE_DEFAULT="ollama/ollama:latest"
OLLAMA_IMAGE_ROCM="ollama/ollama:rocm"
TMP_OVERRIDE=""
# Default to root: this is a disposable, single-tenant lab container, and the
# agent needs unrestricted filesystem/package access inside it. Set
# LLAMAFARM_RUNTIME_UID/GID explicitly to map to a host user instead (e.g. for
# strict host bind-mount write compatibility).
RUNTIME_UID="${LLAMAFARM_RUNTIME_UID:-0}"
RUNTIME_GID="${LLAMAFARM_RUNTIME_GID:-0}"

cleanup() {
  if [ -n "$TMP_OVERRIDE" ] && [ -f "$TMP_OVERRIDE" ]; then
    rm -f "$TMP_OVERRIDE"
  fi
}

trap cleanup EXIT

if [ ! -f "$BASE_COMPOSE" ]; then
  echo "bundle compose file not found: $BASE_COMPOSE" >&2
  exit 1
fi

have_render_device() {
  compgen -G '/dev/dri/renderD*' >/dev/null
}

have_kfd_device() {
  [ -e /dev/kfd ]
}

have_accel_device() {
  compgen -G '/dev/accel/*' >/dev/null
}

have_nvidia_gpu() {
  # Fast path: nvidia-smi is present and reports at least one GPU.
  command -v nvidia-smi >/dev/null 2>&1 && nvidia-smi -L >/dev/null 2>&1
}

docker_nvidia_works() {
  have_nvidia_gpu || return 1
  # Verify NVML, not merely a /dev/nvidia* node. The latter can pass after a
  # legacy NVIDIA Container Toolkit cgroup failure while inference cannot use
  # the GPU and `nvidia-smi` reports "Unknown Error" in the container.
  docker run --rm --gpus all --entrypoint /bin/sh ollama/ollama:latest \
    -lc 'nvidia-smi -L >/dev/null' >/dev/null 2>&1
}

resolve_backend() {
  case "$GPU_MODE" in
    auto)
      if docker_nvidia_works; then
        printf 'nvidia'
      elif have_kfd_device && have_render_device; then
        printf 'rocm'
      elif have_render_device; then
        printf 'vulkan'
      else
        printf 'cpu'
      fi
      ;;
    nvidia)
      if ! docker_nvidia_works; then
        echo "LLAMAFARM_GPU_MODE=nvidia requested, but Docker GPU passthrough is not working." >&2
        exit 1
      fi
      printf 'nvidia'
      ;;
    vulkan)
      if ! have_render_device; then
        echo "LLAMAFARM_GPU_MODE=vulkan requested, but /dev/dri/renderD* was not found." >&2
        exit 1
      fi
      printf 'vulkan'
      ;;
    rocm)
      if ! have_kfd_device || ! have_render_device; then
        echo "LLAMAFARM_GPU_MODE=rocm requested, but /dev/kfd and /dev/dri/renderD* are required." >&2
        exit 1
      fi
      printf 'rocm'
      ;;
    cpu)
      printf 'cpu'
      ;;
    *)
      echo "Unsupported LLAMAFARM_GPU_MODE: $GPU_MODE (expected auto|nvidia|rocm|vulkan|cpu)" >&2
      exit 1
      ;;
  esac
}

collect_group_ids() {
  local ids=()
  local gid
  local candidate

  for gid in $(id -G 2>/dev/null); do
    ids+=("$gid")
  done

  for candidate in /dev/dri/renderD* /dev/dri/card* /dev/kfd /dev/accel/*; do
    [ -e "$candidate" ] || continue
    ids+=("$(stat -c '%g' "$candidate")")
  done

  if [ "${#ids[@]}" -eq 0 ]; then
    return 0
  fi

  printf '%s\n' "${ids[@]}" | awk '!seen[$0]++'
}

write_override() {
  local backend="$1"
  local expose_dri="$2"
  local expose_kfd="$3"
  local expose_accel="$4"
  shift 4
  local groups=("$@")

  TMP_OVERRIDE="$(mktemp "${TMPDIR:-/tmp}/llamafarm-bundle.XXXXXX.yml")"

  {
    echo "services:"
    echo "  llamafarm:"
    if [ "$backend" = "nvidia" ]; then
      echo "    gpus: all"
    fi
    if [ "$backend" = "rocm" ]; then
      echo "    build:"
      echo "      args:"
      echo "        OLLAMA_BASE_IMAGE: $OLLAMA_IMAGE_ROCM"
    fi
    if [ "$expose_dri" = "1" ] || [ "$expose_kfd" = "1" ] || [ "$expose_accel" = "1" ]; then
      echo "    devices:"
      if [ "$expose_dri" = "1" ]; then
        echo "      - /dev/dri:/dev/dri"
      fi
      if [ "$expose_kfd" = "1" ]; then
        echo "      - /dev/kfd:/dev/kfd"
      fi
      if [ "$expose_accel" = "1" ]; then
        echo "      - /dev/accel:/dev/accel"
      fi
    fi
    if [ -S /run/tailscale/tailscaled.sock ]; then
      echo "    volumes:"
      # Mount the directory rather than the socket inode so a tailscaled
      # restart can recreate the socket without leaving a stale bind mount.
      echo "      - /run/tailscale:/run/tailscale-host:ro"
    fi
    if [ "${#groups[@]}" -gt 0 ]; then
      echo "    group_add:"
      local gid
      for gid in "${groups[@]}"; do
        echo "      - \"$gid\""
      done
    fi
    echo "    environment:"
    echo "      - LLAMAFARM_GPU_BACKEND=$backend"
    if [ "${#groups[@]}" -gt 0 ]; then
      echo "      - LLAMAFARM_SUPP_GROUPS=$(IFS=,; echo "${groups[*]}")"
    fi
    if [ "$backend" = "nvidia" ]; then
      echo "      - NVIDIA_VISIBLE_DEVICES=all"
      echo "      - NVIDIA_DRIVER_CAPABILITIES=compute,utility"
    fi
    if [ "$backend" = "vulkan" ]; then
      echo "      - OLLAMA_VULKAN=1"
    fi
  } >"$TMP_OVERRIDE"
}

BACKEND="$(resolve_backend)"
EXPOSE_DRI=0
EXPOSE_KFD=0
EXPOSE_ACCEL=0

if have_render_device; then
  EXPOSE_DRI=1
fi

if have_kfd_device; then
  EXPOSE_KFD=1
fi

if have_accel_device; then
  EXPOSE_ACCEL=1
fi

mapfile -t GROUP_IDS < <(collect_group_ids)

detect_lan_ip() {
  ip -4 route get 1.1.1.1 2>/dev/null \
    | awk '{for (i = 1; i <= NF; i++) if ($i == "src") {print $(i + 1); exit}}' \
    || true
}

detect_tailscale_ip() {
  command -v tailscale >/dev/null 2>&1 || return 0
  tailscale ip -4 2>/dev/null | awk 'NF {print; exit}' || true
}

if [ -z "${LLAMAFARM_LAN_IP:-}" ]; then
  LLAMAFARM_LAN_IP="$(detect_lan_ip)"
fi
if [ -z "${LLAMAFARM_TAILSCALE_IP:-}" ]; then
  LLAMAFARM_TAILSCALE_IP="$(detect_tailscale_ip)"
fi
if [ -z "${LLAMAFARM_PUBLIC_APP_HOSTS:-}" ]; then
  LLAMAFARM_PUBLIC_APP_HOSTS="$LLAMAFARM_LAN_IP"
  if [ -n "$LLAMAFARM_TAILSCALE_IP" ]; then
    LLAMAFARM_PUBLIC_APP_HOSTS="${LLAMAFARM_PUBLIC_APP_HOSTS:+${LLAMAFARM_PUBLIC_APP_HOSTS},}${LLAMAFARM_TAILSCALE_IP}"
  fi
fi

export LLAMAFARM_LAN_IP
export LLAMAFARM_TAILSCALE_IP
export LLAMAFARM_PUBLIC_APP_HOSTS
write_override "$BACKEND" "$EXPOSE_DRI" "$EXPOSE_KFD" "$EXPOSE_ACCEL" "${GROUP_IDS[@]}"

export OLLAMA_PULL_MODELS="${OLLAMA_PULL_MODELS-$DEFAULT_PULL_MODELS}"
export LLAMAFARM_RUNTIME_UID="$RUNTIME_UID"
export LLAMAFARM_RUNTIME_GID="$RUNTIME_GID"

echo "LlamaFarm bundle GPU backend: $BACKEND"
echo "LlamaFarm bundle Ollama preload models: $OLLAMA_PULL_MODELS"
echo "LlamaFarm bundle runtime user: ${LLAMAFARM_RUNTIME_UID}:${LLAMAFARM_RUNTIME_GID}"
echo "LlamaFarm managed app ports: ${LLAMAFARM_MANAGED_APP_PORTS:-8501-8599} (${LLAMAFARM_RESERVED_APP_PORTS:-5000} reserved)"
echo "LlamaFarm public app hosts: ${LLAMAFARM_PUBLIC_APP_HOSTS:-none detected}"
if [ "$EXPOSE_DRI" = "1" ]; then
  echo "Exposing /dev/dri to the container"
fi
if [ "$EXPOSE_KFD" = "1" ]; then
  echo "Exposing /dev/kfd to the container"
fi
if [ "$EXPOSE_ACCEL" = "1" ]; then
  echo "Exposing /dev/accel to the container"
fi

cd "$ROOT_DIR"

if [ "$#" -eq 0 ]; then
  set -- up -d --build
fi

# Pull the Ollama base image before rebuilding so the container gets the newest
# Ollama binary (required for new model architectures like gemma4).
WILL_BUILD=0
for arg in "$@"; do
  [ "$arg" = "--build" ] && WILL_BUILD=1 && break
done
if [ "$WILL_BUILD" = "1" ]; then
  if [ -z "${LLAMAFARM_BUILD_COMMIT:-}" ]; then
    LLAMAFARM_BUILD_COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD 2>/dev/null || true)"
  fi
  if [ -z "${LLAMAFARM_BUILD_TIME:-}" ]; then
    LLAMAFARM_BUILD_TIME="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"
  fi
  export LLAMAFARM_BUILD_COMMIT
  export LLAMAFARM_BUILD_TIME
  echo "LlamaFarm build provenance: ${LLAMAFARM_BUILD_COMMIT:-unknown} @ $LLAMAFARM_BUILD_TIME"

  OLLAMA_PULL_IMAGE="$OLLAMA_IMAGE_DEFAULT"
  [ "$BACKEND" = "rocm" ] && OLLAMA_PULL_IMAGE="$OLLAMA_IMAGE_ROCM"
  echo "Pulling Ollama base image: $OLLAMA_PULL_IMAGE"
  # A registry hiccup (TLS timeout, offline LAN) must not abort the whole
  # deploy: if the pull fails but the image is already cached locally, keep
  # going with the cached copy. Only fail when there is no local image at all.
  if ! docker pull "$OLLAMA_PULL_IMAGE"; then
    if docker image inspect "$OLLAMA_PULL_IMAGE" >/dev/null 2>&1; then
      echo "Pull failed; using cached $OLLAMA_PULL_IMAGE." >&2
    else
      echo "Pull failed and no cached $OLLAMA_PULL_IMAGE is available." >&2
      exit 1
    fi
  fi
fi

# ── Health-gated deploy with automatic rollback ─────────────────────────────
# For `up` commands: snapshot the current image as last-green when the running
# container is healthy, deploy, then require /health to come back within
# LLAMAFARM_HEALTH_TIMEOUT seconds. On failure, retag last-green and recreate
# so a bad build never leaves the node down. Non-up commands exec as before.

IS_UP=0
[ "${1:-}" = "up" ] && IS_UP=1

if [ "$IS_UP" != "1" ]; then
  exec docker compose -f "$BASE_COMPOSE" -f "$TMP_OVERRIDE" "$@"
fi

BUNDLE_IMAGE="llamafarm-local:bundle"
GREEN_IMAGE="llamafarm-local:last-green"
HEALTH_URL="http://localhost:${LLAMAFARM_PORT:-42617}/health"
HEALTH_TIMEOUT="${LLAMAFARM_HEALTH_TIMEOUT:-180}"

wait_healthy() {
  local deadline=$(( $(date +%s) + HEALTH_TIMEOUT ))
  while [ "$(date +%s)" -lt "$deadline" ]; do
    if curl -sf -m 3 "$HEALTH_URL" >/dev/null 2>&1; then
      return 0
    fi
    sleep 3
  done
  return 1
}

if docker image inspect "$BUNDLE_IMAGE" >/dev/null 2>&1 \
  && docker inspect -f '{{.State.Health.Status}}' LlamaFarm 2>/dev/null | grep -q healthy; then
  docker tag "$BUNDLE_IMAGE" "$GREEN_IMAGE"
  echo "Rollback point saved: $GREEN_IMAGE"
fi

# A failed build/up must not abort before we can report state: a build
# failure never swaps the container, so the node keeps running the previous
# image and no rollback is needed — say so explicitly instead of dying on
# set -e mid-script.
if ! docker compose -f "$BASE_COMPOSE" -f "$TMP_OVERRIDE" "$@"; then
  echo "Deploy command failed (likely image build error, e.g. no network for package downloads)." >&2
  if docker inspect -f '{{.State.Health.Status}}' LlamaFarm 2>/dev/null | grep -q healthy; then
    echo "Container was NOT swapped — node is still healthy on the previous image." >&2
    exit 3
  fi
  echo "Container is not healthy — attempting rollback path…" >&2
fi

if wait_healthy; then
  echo "Deploy healthy: $HEALTH_URL"
  exit 0
fi

echo "Deploy FAILED health check after ${HEALTH_TIMEOUT}s." >&2
if docker image inspect "$GREEN_IMAGE" >/dev/null 2>&1; then
  echo "Rolling back to $GREEN_IMAGE…" >&2
  docker tag "$GREEN_IMAGE" "$BUNDLE_IMAGE"
  docker compose -f "$BASE_COMPOSE" -f "$TMP_OVERRIDE" up -d --no-build --force-recreate llamafarm
  if wait_healthy; then
    echo "Rollback succeeded — node is on the last green image." >&2
    exit 2
  fi
  echo "Rollback did NOT become healthy — manual intervention required." >&2
fi
exit 1
