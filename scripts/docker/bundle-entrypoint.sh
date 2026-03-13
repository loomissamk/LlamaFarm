#!/usr/bin/env bash
set -euo pipefail

DATA_DIR="/llamafarm-data"
CONFIG_DIR="$DATA_DIR/.llamafarm"
WORKSPACE_DIR="$DATA_DIR/workspace"
CONFIG_TEMPLATE="/usr/share/llamafarm/config.template.toml"
DEFAULT_AGENTS="/usr/share/llamafarm/workspace.preset.god.AGENTS.md"
DEFAULT_SOUL="/usr/share/llamafarm/workspace.preset.god.SOUL.md"
OLLAMA_HTTP_URL="http://127.0.0.1:11434"
CHROMEDRIVER_STATUS_URL="http://127.0.0.1:${CHROMEDRIVER_PORT:-9515}/status"
DEFAULT_PULL_MODELS="qwen3.5:9b,devstral-small-2:latest"

OLLAMA_PID=""
CHROMEDRIVER_PID=""
APP_PID=""
PULL_PID=""

ensure_runtime_layout() {
  mkdir -p "$CONFIG_DIR" "$WORKSPACE_DIR" "$DATA_DIR/.ollama"
  mkdir -p "${OLLAMA_MODELS:-$DATA_DIR/.ollama/models}"

  if [ ! -f "$CONFIG_DIR/config.toml" ] && [ -f "$CONFIG_TEMPLATE" ]; then
    cp "$CONFIG_TEMPLATE" "$CONFIG_DIR/config.toml"
    chmod 600 "$CONFIG_DIR/config.toml"
  fi

  if [ ! -f "$WORKSPACE_DIR/AGENTS.md" ] && [ -f "$DEFAULT_AGENTS" ]; then
    cp "$DEFAULT_AGENTS" "$WORKSPACE_DIR/AGENTS.md"
  fi

  if [ ! -f "$WORKSPACE_DIR/SOUL.md" ] && [ -f "$DEFAULT_SOUL" ]; then
    cp "$DEFAULT_SOUL" "$WORKSPACE_DIR/SOUL.md"
  fi
}

wait_for_http() {
  local name="$1"
  local url="$2"

  for _ in $(seq 1 300); do
    if curl -fsS "$url" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done

  echo "$name did not become ready at $url" >&2
  return 1
}

model_present() {
  local model="$1"
  ollama list | awk 'NR > 1 { print $1 }' | grep -Fxq "$model"
}

ensure_models() {
  local raw_models
  raw_models="${OLLAMA_PULL_MODELS:-$DEFAULT_PULL_MODELS}"
  IFS=',' read -r -a requested_models <<<"$raw_models"

  for raw_model in "${requested_models[@]}"; do
    local model
    model="$(printf '%s' "$raw_model" | sed 's/^[[:space:]]*//; s/[[:space:]]*$//')"
    [ -n "$model" ] || continue

    if model_present "$model"; then
      echo "ollama model already present: $model"
      continue
    fi

    echo "pulling ollama model: $model"
    ollama pull "$model"
  done
}

pull_models_background() {
  (
    if ! ensure_models; then
      echo "background ollama model pull failed" >&2
    fi
  ) >/tmp/ollama-pull.log 2>&1 &
  PULL_PID=$!
}

start_ollama() {
  /usr/bin/ollama serve >/tmp/ollama.log 2>&1 &
  OLLAMA_PID=$!
}

start_chromedriver() {
  /usr/bin/chromedriver \
    --port="${CHROMEDRIVER_PORT:-9515}" \
    --allowed-origins='*' \
    >/tmp/chromedriver.log 2>&1 &
  CHROMEDRIVER_PID=$!
}

cleanup() {
  for pid in "$APP_PID" "$CHROMEDRIVER_PID" "$OLLAMA_PID" "$PULL_PID"; do
    if [ -n "$pid" ] && kill -0 "$pid" >/dev/null 2>&1; then
      kill "$pid" >/dev/null 2>&1 || true
      wait "$pid" >/dev/null 2>&1 || true
    fi
  done
}

monitor_children() {
  "$@" &
  APP_PID=$!

  while true; do
    if ! kill -0 "$OLLAMA_PID" >/dev/null 2>&1; then
      echo "ollama exited unexpectedly" >&2
      return 1
    fi

    if ! kill -0 "$CHROMEDRIVER_PID" >/dev/null 2>&1; then
      echo "chromedriver exited unexpectedly" >&2
      return 1
    fi

    if ! kill -0 "$APP_PID" >/dev/null 2>&1; then
      wait "$APP_PID"
      return $?
    fi

    sleep 2
  done
}

if [ "$(id -u)" = "0" ]; then
  ensure_runtime_layout
  chown -R 65534:65534 "$DATA_DIR"
  drop_args=(--reuid=65534 --regid=65534)
  if [ -n "${LLAMAFARM_SUPP_GROUPS:-}" ]; then
    drop_args+=(--groups "${LLAMAFARM_SUPP_GROUPS}")
  else
    drop_args+=(--clear-groups)
  fi
  exec setpriv "${drop_args[@]}" /usr/local/bin/bundle-entrypoint.sh "$@"
fi

ensure_runtime_layout
start_ollama
wait_for_http "ollama" "$OLLAMA_HTTP_URL/api/tags"
start_chromedriver
wait_for_http "chromedriver" "$CHROMEDRIVER_STATUS_URL"
pull_models_background

trap cleanup EXIT INT TERM
monitor_children "$@"
