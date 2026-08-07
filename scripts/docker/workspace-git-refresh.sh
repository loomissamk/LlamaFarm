#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="${LLAMAFARM_WORKSPACE_HOST_DIR:-/var/lib/docker/volumes/llamafarm-bundle-data/_data/workspace}"

# Root-owned Docker volume parents are commonly not traversable by the user
# timer. Use the running bundle as the filesystem boundary in that case.
if [ ! -d "$WORKSPACE_DIR/.git" ] && docker inspect LlamaFarm >/dev/null 2>&1; then
  exec docker exec -e HOME=/home/bat LlamaFarm sh -lc '
    workspace=/llamafarm-data/workspace
    git config --global --add safe.directory "$workspace"
    git -C "$workspace" fetch --quiet origin
    [ "$(git -C "$workspace" branch --show-current)" = main ] || exit 0
    [ -z "$(git -C "$workspace" status --porcelain)" ] || exit 0
    git -C "$workspace" merge --ff-only --quiet origin/main
  '
fi

[ -d "$WORKSPACE_DIR/.git" ] || exit 0

git config --global --add safe.directory "$WORKSPACE_DIR" 2>/dev/null || true
git -C "$WORKSPACE_DIR" fetch --quiet origin

# Fetch is always non-mutating. Only update files when main is clean and the
# remote is a strict fast-forward; agents handle branches, commits, and merges.
[ "$(git -C "$WORKSPACE_DIR" branch --show-current)" = main ] || exit 0
[ -z "$(git -C "$WORKSPACE_DIR" status --porcelain)" ] || exit 0
git -C "$WORKSPACE_DIR" merge --ff-only --quiet origin/main
