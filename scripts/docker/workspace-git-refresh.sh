#!/usr/bin/env bash
set -euo pipefail

WORKSPACE_DIR="${LLAMAFARM_WORKSPACE_HOST_DIR:-/var/lib/docker/volumes/llamafarm-bundle-data/_data/workspace}"
[ -d "$WORKSPACE_DIR/.git" ] || exit 0

git config --global --add safe.directory "$WORKSPACE_DIR" 2>/dev/null || true
git -C "$WORKSPACE_DIR" fetch --quiet origin

# Fetch is always non-mutating. Only update files when main is clean and the
# remote is a strict fast-forward; agents handle branches, commits, and merges.
[ "$(git -C "$WORKSPACE_DIR" branch --show-current)" = main ] || exit 0
[ -z "$(git -C "$WORKSPACE_DIR" status --porcelain)" ] || exit 0
git -C "$WORKSPACE_DIR" merge --ff-only --quiet origin/main
