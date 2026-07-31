#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROFILE_DIR="$ROOT_DIR/deploy/node-profiles"

usage() {
  cat <<'USAGE'
Usage: scripts/docker/up-node.sh <rtx4070-laptop|rtx5070ti-16gb> [docker-compose up arguments]

Starts the NVIDIA bundle with a checked-in GPU profile and an optional private
host override at ~/.config/llamafarm/node.env (or $LLAMAFARM_NODE_ENV).

Examples:
  ./scripts/docker/up-node.sh rtx4070-laptop
  ./scripts/docker/up-node.sh rtx5070ti-16gb up -d --build
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
