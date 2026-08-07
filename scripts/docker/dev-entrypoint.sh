#!/bin/sh
set -eu

DATA_DIR="/llamafarm-data"
CONFIG_DIR="$DATA_DIR/.llamafarm"
WORKSPACE_DIR="$DATA_DIR/workspace"
CONFIG_TEMPLATE="/usr/share/llamafarm/config.template.toml"
DEFAULT_AGENTS="/usr/share/llamafarm/workspace.preset.god.AGENTS.md"
BUILTIN_AGENT_MIGRATION="/usr/local/lib/llamafarm/merge_builtin_agents.py"

ensure_runtime_layout() {
  mkdir -p "$CONFIG_DIR" "$WORKSPACE_DIR"

  if [ ! -f "$CONFIG_DIR/config.toml" ] && [ -f "$CONFIG_TEMPLATE" ]; then
    cp "$CONFIG_TEMPLATE" "$CONFIG_DIR/config.toml"
    chmod 600 "$CONFIG_DIR/config.toml"
  fi

  if [ ! -f "$WORKSPACE_DIR/AGENTS.md" ] && [ -f "$DEFAULT_AGENTS" ]; then
    cp "$DEFAULT_AGENTS" "$WORKSPACE_DIR/AGENTS.md"
  fi

  if [ -f "$BUILTIN_AGENT_MIGRATION" ] && [ -f "$CONFIG_TEMPLATE" ]; then
    python3 "$BUILTIN_AGENT_MIGRATION" \
      "$CONFIG_DIR/config.toml" "$CONFIG_TEMPLATE"
  fi

  # SOUL.md is never injected into the prompt; remove any stale copy.
  rm -f "$WORKSPACE_DIR/SOUL.md"

  if [ -n "${LLAMAFARM_WORKSPACE_GIT_REMOTE:-}" ]; then
    git config --global --add safe.directory "$WORKSPACE_DIR"
    if [ ! -d "$WORKSPACE_DIR/.git" ]; then
      git -C "$WORKSPACE_DIR" init -b main
      git -C "$WORKSPACE_DIR" remote add origin "$LLAMAFARM_WORKSPACE_GIT_REMOTE"
    elif ! git -C "$WORKSPACE_DIR" remote get-url origin >/dev/null 2>&1; then
      git -C "$WORKSPACE_DIR" remote add origin "$LLAMAFARM_WORKSPACE_GIT_REMOTE"
    fi
    git -C "$WORKSPACE_DIR" config user.name \
      "${LLAMAFARM_WORKSPACE_GIT_NAME:-LlamaFarmAgent}"
    git -C "$WORKSPACE_DIR" config user.email \
      "${LLAMAFARM_WORKSPACE_GIT_EMAIL:-llamafarm-agent@users.noreply.github.com}"
  fi
}

if [ "$(id -u)" = "0" ]; then
  ensure_runtime_layout
  chown -R 65534:65534 "$DATA_DIR"
  exec setpriv --reuid=65534 --regid=65534 --clear-groups "$@"
fi

ensure_runtime_layout
exec "$@"
