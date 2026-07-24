#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_REPO="$(cd "$SCRIPT_DIR/.." && pwd -P)"
REPO_DIR="$DEFAULT_REPO"
BUILD=1
ENABLE_COMPOSE=1

usage() {
  cat <<'EOF'
Install the LlamaFarm host runner as a systemd user service (no sudo).

Usage: scripts/install-host-runner.sh [options]

Options:
  --repo PATH            Repository used by the fixed redeploy job
  --no-build             Install target/release/llamafarm without rebuilding
  --no-compose-enable    Do not opt the Docker bundle into host_exec
  -h, --help             Show this help
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --repo)
      [ "$#" -ge 2 ] || { echo "--repo needs a path" >&2; exit 2; }
      REPO_DIR="$2"
      shift 2
      ;;
    --no-build)
      BUILD=0
      shift
      ;;
    --no-compose-enable)
      ENABLE_COMPOSE=0
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

REPO_DIR="$(cd "$REPO_DIR" && pwd -P)"
SOURCE_UNIT="$REPO_DIR/deploy/systemd/llamafarm-host-runner.service"
SOURCE_BINARY="$REPO_DIR/target/release/llamafarm"
INSTALL_DIR="$HOME/.local/libexec/llamafarm"
INSTALL_BINARY="$INSTALL_DIR/host-runner"
USER_UNIT_DIR="$HOME/.config/systemd/user"
USER_CONFIG_DIR="$HOME/.config/llamafarm"
UNIT_PATH="$USER_UNIT_DIR/llamafarm-host-runner.service"
ENV_PATH="$USER_CONFIG_DIR/host-runner.env"
SOCKET_PATH="$HOME/.llamafarm/run/host-runner.sock"
STATE_DIR="$HOME/.local/state/llamafarm/host-runner"
RUNNER_PATH="${PATH:-/usr/local/bin:/usr/bin:/bin}"

if [ ! -f "$SOURCE_UNIT" ]; then
  echo "Host-runner unit not found: $SOURCE_UNIT" >&2
  exit 1
fi
if ! command -v systemctl >/dev/null 2>&1; then
  echo "systemctl is required for the user service" >&2
  exit 1
fi

if [ "$BUILD" = "1" ]; then
  (
    cd "$REPO_DIR"
    cargo build --release --locked --bin llamafarm
  )
fi
if [ ! -x "$SOURCE_BINARY" ]; then
  echo "Built binary not found: $SOURCE_BINARY" >&2
  exit 1
fi

install -d -m 0700 "$INSTALL_DIR" "$USER_CONFIG_DIR" \
  "$HOME/.llamafarm/run" "$STATE_DIR"
install -d -m 0755 "$USER_UNIT_DIR"
install -m 0755 "$SOURCE_BINARY" "$INSTALL_BINARY"
install -m 0644 "$SOURCE_UNIT" "$UNIT_PATH"

reject_multiline() {
  case "$1" in
    *$'\n'*|*$'\r'*)
      echo "Host-runner paths cannot contain newlines" >&2
      exit 1
      ;;
  esac
}

systemd_quote() {
  local value="$1"
  value="${value//\\/\\\\}"
  value="${value//\"/\\\"}"
  printf '"%s"' "$value"
}

reject_multiline "$SOCKET_PATH"
reject_multiline "$STATE_DIR"
reject_multiline "$REPO_DIR"
reject_multiline "$RUNNER_PATH"
umask 077
{
  printf 'PATH='
  systemd_quote "$RUNNER_PATH"
  printf '\nLLAMAFARM_HOST_RUNNER_SOCKET='
  systemd_quote "$SOCKET_PATH"
  printf '\nLLAMAFARM_HOST_RUNNER_STATE_DIR='
  systemd_quote "$STATE_DIR"
  printf '\nLLAMAFARM_HOST_RUNNER_REPO='
  systemd_quote "$REPO_DIR"
  printf '\n'
} > "$ENV_PATH"
chmod 0600 "$ENV_PATH"

if [ "$ENABLE_COMPOSE" = "1" ]; then
  COMPOSE_ENV="$REPO_DIR/.env"
  COMPOSE_ENV_TMP="$(mktemp "$REPO_DIR/.env.host-runner.XXXXXX")"
  cleanup() {
    rm -f "$COMPOSE_ENV_TMP"
  }
  trap cleanup EXIT
  if [ -f "$COMPOSE_ENV" ]; then
    awk '
      !/^LLAMAFARM_HOST_RUNNER_ENABLED=/ &&
      !/^LLAMAFARM_HOST_RUNNER_SOCKET=/
    ' "$COMPOSE_ENV" > "$COMPOSE_ENV_TMP"
  fi
  {
    printf '\n# Opt-in host runner installed by scripts/install-host-runner.sh\n'
    printf 'LLAMAFARM_HOST_RUNNER_ENABLED=true\n'
    printf 'LLAMAFARM_HOST_RUNNER_SOCKET=%s\n' "$SOCKET_PATH"
  } >> "$COMPOSE_ENV_TMP"
  chmod 0600 "$COMPOSE_ENV_TMP"
  mv -f "$COMPOSE_ENV_TMP" "$COMPOSE_ENV"
  trap - EXIT
fi

systemctl --user daemon-reload
systemctl --user enable --now llamafarm-host-runner.service
"$INSTALL_BINARY" host-runner health --socket "$SOCKET_PATH"

echo
echo "Host runner is active at $SOCKET_PATH"
echo "Audit log: $STATE_DIR/audit.jsonl"
if [ "$ENABLE_COMPOSE" = "1" ]; then
  echo "Docker bundle opt-in written to $REPO_DIR/.env"
  echo "Recreate LlamaFarm once so it receives host_exec:"
  echo "  $REPO_DIR/scripts/docker/up-bundle.sh up -d --build"
fi
echo "The user service starts with your login. Boot-before-login requires"
echo "administrator-managed user lingering; this installer intentionally uses no sudo."
