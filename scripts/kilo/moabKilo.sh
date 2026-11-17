#!/usr/bin/env bash
# moab-kilo.sh — status + self-repair for Kilo's config on this laptop.
# Run locally: bash moab-kilo.sh
# Companion to moab.sh (which handles the pop-os-1/V100 side).

set -uo pipefail

KILO_CONF="$HOME/.config/kilo/kilo.jsonc"
HOST="100.107.226.49"
PORT=11434
ROWS=()
REPAIRS=()
row() { ROWS+=("$1|$2|$3"); }
repair() { REPAIRS+=("$1"); }

CANONICAL=$(cat <<'JSON'
{
  "$schema": "https://app.kilo.ai/config.json",
  "enabled_providers": ["ollamav100"],
  "model": "ollamav100/qwen3.8-unqwen:27b-256k",
  "small_model": "ollamav100/qwen3.5-rana:9b-256k",
  "provider": {
    "ollamav100": {
      "npm": "@ai-sdk/openai-compatible",
      "options": {
        "baseURL": "http://100.107.226.49:11434/v1",
        "apiKey": "none",
        "includeUsage": true,
        "timeout": false
      },
      "models": {
        "qwen3.8-unqwen:27b-256k": {
          "name": "UnQwen 3.8 27B | 256K | V100 direct Ollama",
          "tool_call": true,
          "limit": { "context": 262144, "output": 131072 }
        },
        "qwen3.5-rana:9b-256k": {
          "name": "Rana 3.5 9B | 256K | V100 direct Ollama",
          "tool_call": true,
          "limit": { "context": 262144, "output": 131072 }
        }
      }
    }
  },
  "agent": {
    "code": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
    "plan": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
    "debug": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
    "review": {
      "description": "Read-only code reviewer for independent worktrees",
      "mode": "all",
      "model": "ollamav100/qwen3.8-unqwen:27b-256k",
      "temperature": 0.1,
      "prompt": "Review changes using repository files and diffs. Report findings with file paths and evidence. Do not edit files or run commands.",
      "permission": { "edit": "deny", "bash": "deny" }
    }
  },
  "permission": { "*": "allow" },
  "reasoning_display": "preview",
  "terminal_command_display": "collapsed",
  "code_edit_display": "collapsed",
  "mcp_tool_display": "collapsed",
  "snapshot": true
}
JSON
)

# --- config file exists and matches canonical shape ---------------------
if [ -f "$KILO_CONF" ]; then
  row "kilo.jsonc exists" "OK" "$KILO_CONF"
  if grep -q '"baseURL": "http://100.107.226.49:11434/v1"' "$KILO_CONF" 2>/dev/null; then
    row "Provider baseURL" "OK" "direct Ollama, port 11434"
  elif grep -q '42617' "$KILO_CONF" 2>/dev/null; then
    row "Provider baseURL" "FAIL" "still pointed at LlamaFarm gateway (42617)"
    cp "$KILO_CONF" "${KILO_CONF}.before-moab-autofix-$(date +%Y%m%d-%H%M%S)"
    echo "$CANONICAL" > "$KILO_CONF"
    repair "baseURL was pointed at the gateway (42617) instead of direct Ollama — restored canonical config"
  else
    row "Provider baseURL" "FAIL" "unrecognized / missing"
    cp "$KILO_CONF" "${KILO_CONF}.before-moab-autofix-$(date +%Y%m%d-%H%M%S)" 2>/dev/null
    echo "$CANONICAL" > "$KILO_CONF"
    repair "baseURL missing or unrecognized — restored canonical config"
  fi
  if grep -q '"mcp"' "$KILO_CONF" 2>/dev/null && grep -q 'llamafarm' "$KILO_CONF" 2>/dev/null; then
    row "LlamaFarm MCP block" "FAIL" "present (should be removed per design decision)"
  else
    row "LlamaFarm MCP block" "OK" "absent, as intended"
  fi
else
  row "kilo.jsonc exists" "FAIL" "missing"
  mkdir -p "$(dirname "$KILO_CONF")"
  echo "$CANONICAL" > "$KILO_CONF"
  repair "kilo.jsonc was missing — created canonical config"
fi

# --- connectivity ---------------------------------------------------------
if command -v tailscale >/dev/null 2>&1; then
  ts_status=$(tailscale status 2>/dev/null | grep -c "pop-os-1" || true)
  if [ "$ts_status" -gt 0 ]; then
    row "Tailscale" "OK" "pop-os-1 visible"
  else
    row "Tailscale" "FAIL" "pop-os-1 not visible in tailnet"
  fi
else
  row "Tailscale" "FAIL" "tailscale CLI not found"
fi

if curl -s --max-time 5 "http://${HOST}:${PORT}/api/tags" | grep -q 'qwen3.8-unqwen'; then
  row "Ollama reachable" "OK" "models visible from this machine"
else
  row "Ollama reachable" "FAIL" "cannot reach http://${HOST}:${PORT}"
fi

if [ -d "$HOME/.vscode/extensions" ] && ls "$HOME/.vscode/extensions" 2>/dev/null | grep -qi kilocode; then
  row "Kilo extension" "OK" "installed"
else
  row "Kilo extension" "FAIL" "not found — install Kilo Code from the VS Code marketplace"
fi

# --- print clean table -----------------------------------------------------
echo
echo "================================================================"
echo "  MOABKILO — $(hostname) status"
echo "  $(date '+%Y-%m-%d %H:%M:%S %Z')"
echo "================================================================"
printf "%-24s %-7s %s\n" "COMPONENT" "STATUS" "DETAIL"
echo "----------------------------------------------------------------"
for r in "${ROWS[@]}"; do
  IFS='|' read -r name status detail <<< "$r"
  printf "%-24s %-7s %s\n" "$name" "$status" "$detail"
done
echo "----------------------------------------------------------------"
if [ ${#REPAIRS[@]} -gt 0 ]; then
  echo "REPAIRS APPLIED THIS RUN:"
  for r in "${REPAIRS[@]}"; do echo "  - $r"; done
  echo ">>> Restart VS Code (full quit, not just reload) to pick up the fix. <<<"
else
  echo "No repairs needed."
fi
echo "================================================================"
