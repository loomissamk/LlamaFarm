#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BASE_COMPOSE="$ROOT_DIR/docker-compose.bundle.yml"
GPU_MODE="${LLAMAFARM_GPU_MODE:-auto}"
DEFAULT_PULL_MODELS=""
OLLAMA_IMAGE_DEFAULT="ollama/ollama:latest"
OLLAMA_IMAGE_ROCM="ollama/ollama:rocm"
TMP_OVERRIDE=""
RUNTIME_UID="${LLAMAFARM_RUNTIME_UID:-$(id -u)}"
RUNTIME_GID="${LLAMAFARM_RUNTIME_GID:-$(id -g)}"

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
  # Verify Docker can actually expose the GPU before committing to nvidia mode.
  docker run --rm --gpus all --entrypoint /bin/sh ollama/ollama:latest \
    -lc 'test -e /dev/nvidiactl' >/dev/null 2>&1
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
write_override "$BACKEND" "$EXPOSE_DRI" "$EXPOSE_KFD" "$EXPOSE_ACCEL" "${GROUP_IDS[@]}"

export OLLAMA_PULL_MODELS="${OLLAMA_PULL_MODELS-$DEFAULT_PULL_MODELS}"
export LLAMAFARM_RUNTIME_UID="$RUNTIME_UID"
export LLAMAFARM_RUNTIME_GID="$RUNTIME_GID"

echo "LlamaFarm bundle GPU backend: $BACKEND"
echo "LlamaFarm bundle Ollama preload models: $OLLAMA_PULL_MODELS"
echo "LlamaFarm bundle runtime user: ${LLAMAFARM_RUNTIME_UID}:${LLAMAFARM_RUNTIME_GID}"
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
  OLLAMA_PULL_IMAGE="$OLLAMA_IMAGE_DEFAULT"
  [ "$BACKEND" = "rocm" ] && OLLAMA_PULL_IMAGE="$OLLAMA_IMAGE_ROCM"
  echo "Pulling Ollama base image: $OLLAMA_PULL_IMAGE"
  docker pull "$OLLAMA_PULL_IMAGE"
fi

exec docker compose -f "$BASE_COMPOSE" -f "$TMP_OVERRIDE" "$@"
