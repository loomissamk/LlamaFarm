#!/usr/bin/env bash
# Start a bounded V3 coding task through LlamaFarm's *tool-enabled* POST /agent.
# Uses the model already selected in LlamaFarm configuration/dashboard.
set -euo pipefail

: "${LF_AGENT_CHECKOUT:?Set LF_AGENT_CHECKOUT to the repo path visible INSIDE the LlamaFarm agent runtime}"
GATEWAY="${LF_GATEWAY_URL:-http://127.0.0.1:42617}"
command -v python3 >/dev/null || { echo 'python3 missing' >&2; exit 2; }
command -v curl >/dev/null || { echo 'curl missing' >&2; exit 2; }

read -r -d '' PROMPT <<EOF || true
You are implementing LlamaFarm V3 in the checkout at: ${LF_AGENT_CHECKOUT}
This is REAL engineering work, not a speculative answer. You should have shell, read, write and test tools available.

FIRST: prove the checkout exists, that git branch is v3, that its ancestry includes v2, and that docs/v3/V3_START_HERE.md and src/conversation_events.rs and src/conversation_store.rs are present. If any check fails, stop and report the exact blocker. Do not touch v2 or main.

Read docs/v3/README.md, V2_CODE_AUDIT.md, V3_REQUIREMENTS.md, V3_ARCHITECTURE.md, V3_IMPLEMENTATION_PLAN.md, V3_START_HERE.md, and the Rust event module. Complete PR-01: inspect/fix the typed event envelope and transactional SQLite event store against the supplied JSON schema; run idempotency, concurrent writer and replay tests; implement additional compatibility adapter tests for the actual V2 WebSocket frames, and ensure safe redaction and replay semantics. Integrate additively; leave the existing WebSocket protocol backward-compatible. Do not modify running Docker containers, credentials, GPU settings, or model volumes.

Run cargo fmt --all -- --check, cargo check --workspace, cargo test --lib conversation_events, cargo test --lib conversation_store, and other relevant regression checks. Record exact command outputs and failures. Only commit on v3 after all targeted checks pass. If a test fails, repair it rather than claiming success. Do not mark tests as passed unless observed. Report commit SHA, changed files, implemented requirement IDs, and evidence. STOP after PR-01; don't launch unbounded loops or start PR-02 without a reviewed commit.
EOF
export LF_PROMPT="$PROMPT"
JSON_PAYLOAD=$(python3 - <<'PY'
import json, os
print(json.dumps({'message':os.environ['LF_PROMPT']}))
PY
)

headers=(-H 'Content-Type: application/json')
if [[ -n "${LF_GATEWAY_BEARER:-}" ]]; then
  headers+=(-H "Authorization: Bearer ${LF_GATEWAY_BEARER}")
fi
printf 'Submitting PR-01 implementation task to %s/agent\n' "$GATEWAY"
printf 'Ensure the dashboard is using your intended Qwen model; /agent follows the current runtime selection.\n'
curl --fail-with-body --show-error --silent --max-time "${LF_AGENT_TIMEOUT:-7200}" \
  "${headers[@]}" \
  -X POST "$GATEWAY/agent" --data-binary "$JSON_PAYLOAD"
printf '\n'
