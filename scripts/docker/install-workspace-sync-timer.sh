#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
USER_SYSTEMD_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$USER_SYSTEMD_DIR"

sed "s|@SCRIPT@|$ROOT_DIR/scripts/docker/workspace-git-refresh.sh|g" \
  "$ROOT_DIR/deploy/systemd/llamafarm-workspace-sync.service.in" \
  > "$USER_SYSTEMD_DIR/llamafarm-workspace-sync.service"
cp "$ROOT_DIR/deploy/systemd/llamafarm-workspace-sync.timer" \
  "$USER_SYSTEMD_DIR/llamafarm-workspace-sync.timer"

systemctl --user daemon-reload
systemctl --user enable --now llamafarm-workspace-sync.timer
systemctl --user start llamafarm-workspace-sync.service
