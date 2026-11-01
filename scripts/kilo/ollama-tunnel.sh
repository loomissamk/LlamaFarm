#!/usr/bin/env bash
set -euo pipefail

# Run in a terminal on the VS Code client. SSH exits on disconnect; retry until
# interrupted. Ollama stays bound to the remote host's loopback interface.
target="${LLAMAFARM_SSH_TARGET:?Set LLAMAFARM_SSH_TARGET to the V100 SSH user and Tailscale host}"
local_port="${LLAMAFARM_LOCAL_OLLAMA_PORT:-11435}"
identity_file="${LLAMAFARM_SSH_IDENTITY_FILE:-}"
ssh_options=(-N -o BatchMode=yes -o ExitOnForwardFailure=yes
  -o ConnectTimeout=10 -o ServerAliveInterval=30 -o ServerAliveCountMax=3)
if [[ -n "$identity_file" ]]; then
  [[ -r "$identity_file" ]] || { echo "SSH identity is not readable: $identity_file" >&2; exit 1; }
  ssh_options+=(-o IdentitiesOnly=yes -i "$identity_file")
fi

trap 'exit 0' INT TERM
while true; do
  ssh "${ssh_options[@]}" \
    -L "127.0.0.1:${local_port}:127.0.0.1:11434" "$target" || true
  sleep 3
done
