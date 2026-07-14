#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

exec env LLAMAFARM_GPU_MODE=nvidia ./scripts/docker/up-bundle.sh "$@"
