#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE_DIR="$ROOT_DIR/deploy/node-profiles"

usage() {
  cat <<'USAGE'
Usage: scripts/docker/up-node.sh <rtx3050ti-windows-wsl|rtx4070-laptop|rtx5070ti-16gb|v100-32gb> [docker-compose up arguments]

Starts the NVIDIA bundle with a checked-in GPU profile and an optional private
host override at ~/.config/llamafarm/node.env (or $LLAMAFARM_NODE_ENV).

Examples:
  ./scripts/docker/up-node.sh rtx4070-laptop
  ./scripts/docker/up-node.sh rtx3050ti-windows-wsl up -d --build
  ./scripts/docker/up-node.sh rtx5070ti-16gb up -d --build
  ./scripts/docker/up-node.sh v100-32gb up -d --build
  ./scripts/docker/up-node.sh rtx5070ti-16gb config
USAGE
}

if [ "$#" -lt 1 ]; then
  usage >&2
  exit 2
fi

PROFILE_NAME="$1"
shift
PROFILE_FILE="$PROFILE_DIR/$PROFILE_NAME.env"
if [ ! -f "$PROFILE_FILE" ]; then
  echo "Unknown node profile: $PROFILE_NAME" >&2
  usage >&2
  exit 2
fi

NODE_ENV="${LLAMAFARM_NODE_ENV:-${XDG_CONFIG_HOME:-$HOME/.config}/llamafarm/node.env}"

# Profiles and private host overrides are shell-style KEY=value files. Export
# their values only for this deployment process; do not persist secrets in the
# repository or the Docker image.
set -a
# shellcheck disable=SC1090
source "$PROFILE_FILE"
if [ -f "$NODE_ENV" ]; then
  # shellcheck disable=SC1090
  source "$NODE_ENV"
else
  echo "No private node override at $NODE_ENV (federation peer list is empty)." >&2
fi
set +a

if [ "$PROFILE_NAME" = v100-32gb ]; then
  # These are invariants of this profile, not private override knobs.
  LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=true
  LLAMAFARM_NVIDIA_COMPUTE_ONLY=true
  LLAMAFARM_EXPOSE_DRI=false
  V100_HOST_CONFIG="${LLAMAFARM_V100_HOST_CONFIG:-/etc/llamafarm/v100-cuda-5070-nvk.env}"
  if [ ! -r "$V100_HOST_CONFIG" ]; then
    echo "V100 host configuration is not readable: $V100_HOST_CONFIG" >&2
    exit 1
  fi
  # The installed root-owned profile contains hardware identifiers only.
  # shellcheck disable=SC1090
  source "$V100_HOST_CONFIG"
  if [[ ! "${V100_GPU_UUID:-}" =~ ^GPU-[[:xdigit:]]{8}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{4}-[[:xdigit:]]{12}$ ]]; then
    echo "V100_GPU_UUID in $V100_HOST_CONFIG is missing or invalid." >&2
    exit 1
  fi
  case "${LLAMAFARM_NVIDIA_VISIBLE_DEVICES:-}" in
    ""|all|ALL|*,*)
      echo "v100-32gb requires exactly one V100 GPU UUID; 'all', empty, and lists are rejected." >&2
      exit 1
      ;;
  esac
  if [ "$LLAMAFARM_NVIDIA_VISIBLE_DEVICES" != "$V100_GPU_UUID" ]; then
    echo "LLAMAFARM_NVIDIA_VISIBLE_DEVICES must exactly match V100_GPU_UUID in $V100_HOST_CONFIG." >&2
    exit 1
  fi
  LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID=true
  LLAMAFARM_NVIDIA_COMPUTE_ONLY=true
  LLAMAFARM_EXPOSE_DRI=false
  export V100_GPU_UUID LLAMAFARM_NVIDIA_REQUIRE_EXACT_UUID \
    LLAMAFARM_NVIDIA_COMPUTE_ONLY LLAMAFARM_EXPOSE_DRI
  "$ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh" verify "$V100_HOST_CONFIG"
fi

for required in \
  LLAMAFARM_NODE_NAME \
  LLAMAFARM_MANUAL_PEERS \
  LLAMAFARM_FEDERATION_TOKEN; do
  if [ -z "${!required:-}" ]; then
    echo "Profile is missing required setting: $required" >&2
    exit 1
  fi
done

if [ "$LLAMAFARM_FEDERATION_TOKEN" = "<shared-random-federation-token>" ]; then
  echo "Replace the federation-token placeholder in $NODE_ENV before starting the node." >&2
  exit 1
fi

require_nvidia_toolkit() {
  local minimum="1.19.1"
  local installed

  if ! command -v nvidia-container-cli >/dev/null 2>&1; then
    echo "NVIDIA Container Toolkit is required (minimum ${minimum}). See deploy/node-profiles/README.md." >&2
    exit 1
  fi

  installed="$(nvidia-container-cli --version | awk -F': ' '/^cli-version:/{print $2; exit}')"
  if [ -z "$installed" ] || [ "$(printf '%s\n' "$minimum" "$installed" | sort -V | head -n1)" != "$minimum" ]; then
    echo "NVIDIA Container Toolkit ${minimum}+ is required; found ${installed:-unknown}." >&2
    echo "Upgrade it before recreating the GPU bundle; see deploy/node-profiles/README.md." >&2
    exit 1
  fi
}

require_nvidia_toolkit

cd "$ROOT_DIR"

# Non-up compose commands are inspection operations.
if [ "$#" -gt 0 ] && [ "$1" != "up" ]; then
  exec env LLAMAFARM_GPU_MODE=nvidia ./scripts/docker/up-bundle-nvidia.sh "$@"
fi

env LLAMAFARM_GPU_MODE=nvidia ./scripts/docker/up-bundle-nvidia.sh "$@"

echo "Applied $PROFILE_NAME with no Compose CPU, memory, swap, or PID ceilings."
