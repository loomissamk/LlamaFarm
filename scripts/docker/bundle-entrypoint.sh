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
DEFAULT_PULL_MODELS=""

OLLAMA_PID=""
CHROMEDRIVER_PID=""
APP_PID=""
PULL_PID=""

ensure_runtime_identity() {
  local uid="$1"
  local gid="$2"

  # The bundle intentionally runs the gateway as the host operator's numeric
  # UID so mounted workspaces remain writable.  Minimal base images do not
  # necessarily contain that UID in /etc/passwd, which makes ordinary tools
  # such as `whoami` fail despite a healthy process.  Create a local identity
  # before privileges are dropped; this never changes the host account.
  if ! getent group "$gid" >/dev/null 2>&1; then
    printf 'llamafarm:x:%s:\n' "$gid" >> /etc/group
  fi
  if ! getent passwd "$uid" >/dev/null 2>&1; then
    printf 'llamafarm:x:%s:%s:LlamaFarm runtime:%s:/bin/bash\n' \
      "$uid" "$gid" "$DATA_DIR" >> /etc/passwd
  fi
}

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

  # Default Python environment: the agent's shell tool and the dashboard IDE
  # terminal both prepend this venv to PATH, so it must always exist.
  if [ ! -x "$WORKSPACE_DIR/.venv/bin/python" ]; then
    python3 -m venv "$WORKSPACE_DIR/.venv" || \
      echo "WARN: failed to create workspace .venv" >&2
  fi

  ensure_bundle_config_defaults
}

ensure_bundle_config_defaults() {
  local config_path="$CONFIG_DIR/config.toml"
  [ -f "$config_path" ] || return 0

  python3 - "$config_path" <<'PY'
from pathlib import Path
import sys

config_path = Path(sys.argv[1])
text = config_path.read_text()
required_commands = ["rm"]
marker = "allowed_commands = ["
start = text.find(marker)
if start == -1:
    sys.exit(0)

end = text.find("\n]", start)
if end == -1:
    sys.exit(0)

changed = False
for command in required_commands:
    token = f"\"{command}\""
    if token in text:
        continue
    text = text[:end] + f'  "{command}",\n' + text[end:]
    end += len(f'  "{command}",\n')
    changed = True

if changed:
    config_path.write_text(text)
PY
}

repair_config_for_available_models() {
  local config_path="$CONFIG_DIR/config.toml"
  [ -f "$config_path" ] || return 0

  local replacement_model=""
  if ! model_present "qwen2.5-coder:14b"; then
    if model_present "devstral-small-2:latest"; then
      replacement_model="devstral-small-2:latest"
    elif model_present "qwen3.5:9b"; then
      replacement_model="qwen3.5:9b"
    fi
  fi

  [ -n "$replacement_model" ] || return 0

  python3 - "$config_path" "$replacement_model" <<'PY'
from pathlib import Path
import sys

config_path = Path(sys.argv[1])
replacement = sys.argv[2]
missing = "qwen2.5-coder:14b"
text = config_path.read_text()

if missing not in text:
    sys.exit(0)

config_path.write_text(text.replace(missing, replacement))
PY
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
  local failed=0
  raw_models="${OLLAMA_PULL_MODELS-$DEFAULT_PULL_MODELS}"
  if [ -z "${raw_models//[[:space:],]/}" ]; then
    echo "OLLAMA_PULL_MODELS is empty; skipping model pulls"
    return 0
  fi
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
    if ! ollama pull "$model"; then
      echo "failed to pull ollama model: $model" >&2
      failed=1
    fi
  done

  return "$failed"
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
  target_uid="${LLAMAFARM_RUNTIME_UID:-65534}"
  target_gid="${LLAMAFARM_RUNTIME_GID:-65534}"

  case "$target_uid" in
    ''|*[!0-9]*)
      echo "LLAMAFARM_RUNTIME_UID must be numeric, got: $target_uid" >&2
      exit 1
      ;;
  esac

  case "$target_gid" in
    ''|*[!0-9]*)
      echo "LLAMAFARM_RUNTIME_GID must be numeric, got: $target_gid" >&2
      exit 1
      ;;
  esac

  ensure_runtime_identity "$target_uid" "$target_gid"
  ensure_runtime_layout
  chown -R "$target_uid:$target_gid" "$DATA_DIR"
  drop_args=(--reuid="$target_uid" --regid="$target_gid")
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
repair_config_for_available_models
start_chromedriver
wait_for_http "chromedriver" "$CHROMEDRIVER_STATUS_URL"
pull_models_background

trap cleanup EXIT INT TERM
monitor_children "$@"
