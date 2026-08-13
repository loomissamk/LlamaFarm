#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SOURCE_CONFIG="${1:-}"
SERVICE_NAME=llamafarm-v100-cuda-5070-nvk.service
TARGET_CONFIG=/etc/llamafarm/v100-cuda-5070-nvk.env
TARGET_HELPER=/usr/local/libexec/llamafarm/v100-cuda-5070-nvk

if [ -z "$SOURCE_CONFIG" ] || [ "$#" -ne 1 ]; then
  echo "Usage: $0 <completed-host-config>" >&2
  exit 2
fi
[ "$(id -u)" -eq 0 ] || {
  echo "Run this installer as root." >&2
  exit 1
}
[ -f "$SOURCE_CONFIG" ] || {
  echo "Configuration not found: $SOURCE_CONFIG" >&2
  exit 1
}

LLAMAFARM_V100_HOST_CONFIG="$SOURCE_CONFIG" \
  "$ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh" preflight

install -D -m 0644 "$SOURCE_CONFIG" "$TARGET_CONFIG"
install -D -m 0755 "$ROOT_DIR/scripts/host/v100-cuda-5070-nvk.sh" "$TARGET_HELPER"
install -D -m 0644 \
  "$ROOT_DIR/deploy/systemd/llamafarm-v100-cuda-5070-nvk.service" \
  "/etc/systemd/system/$SERVICE_NAME"
systemctl daemon-reload
systemctl enable "$SERVICE_NAME"

cat <<EOF
Installed and enabled $SERVICE_NAME without changing the live GPU binding.
Apply now with: systemctl start $SERVICE_NAME
Or reboot, then verify with: $TARGET_HELPER verify $TARGET_CONFIG
EOF
