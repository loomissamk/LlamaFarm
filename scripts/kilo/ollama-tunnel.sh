#!/usr/bin/env bash
set -euo pipefail

# Run in a terminal on the VS Code client. SSH exits on disconnect; retry until
# interrupted. Ollama stays bound to the remote host's loopback interface.
target="${LLAMAFARM_SSH_TARGET:?Set LLAMAFARM_SSH_TARGET to the V100 SSH user and Tailscale host}"
local_port="${LLAMAFARM_LOCAL_OLLAMA_PORT:-11435}"

trap 'exit 0' INT TERM
while true; do
  ssh -N -o BatchMode=yes -o ExitOnForwardFailure=yes \
    -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
    -L "127.0.0.1:${local_port}:127.0.0.1:11434" "$target" || true
  sleep 3
done
