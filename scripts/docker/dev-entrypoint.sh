#!/bin/sh
set -eu

DATA_DIR="/llamafarm-data"
CONFIG_DIR="$DATA_DIR/.llamafarm"
WORKSPACE_DIR="$DATA_DIR/workspace"
CONFIG_TEMPLATE="/usr/share/llamafarm/config.template.toml"
DEFAULT_AGENTS="/usr/share/llamafarm/workspace.preset.god.AGENTS.md"

ensure_runtime_layout() {
  mkdir -p "$CONFIG_DIR" "$WORKSPACE_DIR"

  if [ ! -f "$CONFIG_DIR/config.toml" ] && [ -f "$CONFIG_TEMPLATE" ]; then
    cp "$CONFIG_TEMPLATE" "$CONFIG_DIR/config.toml"
    chmod 600 "$CONFIG_DIR/config.toml"
  fi

  if [ ! -f "$WORKSPACE_DIR/AGENTS.md" ] && [ -f "$DEFAULT_AGENTS" ]; then
    cp "$DEFAULT_AGENTS" "$WORKSPACE_DIR/AGENTS.md"
  fi

  # SOUL.md is never injected into the prompt; remove any stale copy.
  rm -f "$WORKSPACE_DIR/SOUL.md"
}

if [ "$(id -u)" = "0" ]; then
  ensure_runtime_layout
  chown -R 65534:65534 "$DATA_DIR"
  exec setpriv --reuid=65534 --regid=65534 --clear-groups "$@"
fi

ensure_runtime_layout
exec "$@"
