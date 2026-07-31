# Model/tool catalogue E2E audit

`tool_catalog_e2e.py` tests the deployed runtime through `/ws/chat`, using the
actual model-directed tool loop. It does not add or use a direct tool-execution
endpoint. The matrix currently requires all 57 deployed tools, including
`host_exec`, and contains 259 action-level cases. Every advertised top-level
`action` and `operation` enum must have a real case or validation fails.

The harness has no model-turn or tool-result deadline. It writes every
`tool_call`, `tool_result`, and terminal event to a flushed JSONL checkpoint.
Ctrl-C is the operator stop; `--resume` continues after the last terminal
action checkpoint.

## One-time deployment fixtures

Install the deterministic SOP before starting LlamaFarm so the in-memory SOP
engine loads it:

```bash
WORKSPACE=/absolute/path/to/the/runtime/workspace
install -d "$WORKSPACE/sops/catalog-e2e"
cp scripts/tests/fixtures/sops/catalog-e2e/SOP.toml \
  scripts/tests/fixtures/sops/catalog-e2e/SOP.md \
  "$WORKSPACE/sops/catalog-e2e/"
```

Add a dedicated SQLite connection to the runtime config. The query is a
constant `SELECT`, so it neither requires nor mutates a table:

```toml
[[db_connections]]
name = "catalog-e2e-sqlite"
driver = "sqlite"
uri = "/absolute/path/to/the/runtime/workspace/catalog-e2e.sqlite3"
read_only = false
max_rows = 10
label = "Catalogue E2E fixture"
```

The advertised `web_search_tool` should use its credential-free provider for a
full pass:

```toml
[web_search]
enabled = true
provider = "duckduckgo"
```

The harness starts a unique local audit HTTP server for the browser, HTTP,
web-fetch, process-lifecycle, and Pushover cases. It appends mock Pushover
values to the workspace `.env`, preserving the original file and restoring it
during cleanup.

The Git action cases require a Git workspace with no tracked changes. They
switch to a unique audit branch, commit only the audit-owned path, exercise a
stash, a local `git daemon`, a disposable bare remote, and two audit worktrees,
then return to the original branch and delete only audit-owned state. They
never reset or restore operator paths.

The full run requires two distinct online federation workers. It selects each
peer explicitly and completes a real remote `delegate` task on both peers from
the node under test. `--expected-online-peers` may raise the required
minimum but cannot reduce it below two for a full execution.

Service-control cases install unique temporary systemd user units through the
host runner and remove them again. The last case is an actual host-runner
redeploy. The harness waits without a total deadline for the old gateway to go
away and the new gateway to recover, then revalidates the entire live tool
catalogue before passing.

## Run

First verify the exact live catalogue and every matrix argument against the
advertised schemas:

```bash
python3 scripts/tests/tool_catalog_e2e.py \
  --base-url http://127.0.0.1:42617 \
  --validate-only
```

Run the complete audit:

```bash
python3 scripts/tests/tool_catalog_e2e.py \
  --base-url http://127.0.0.1:42617 \
  --checkpoint ~/.local/state/llamafarm/tool-catalog-e2e.jsonl
```

For a token-protected gateway, set `LLAMAFARM_GATEWAY_TOKEN`; it is never
written to the checkpoint. Raw tool results can contain deployment data, so
keep the checkpoint outside the repository and never commit or publish it.
Resume or retry failed actions with:

```bash
python3 scripts/tests/tool_catalog_e2e.py \
  --base-url http://127.0.0.1:42617 \
  --checkpoint ~/.local/state/llamafarm/tool-catalog-e2e.jsonl \
  --resume

python3 scripts/tests/tool_catalog_e2e.py \
  --base-url http://127.0.0.1:42617 \
  --checkpoint ~/.local/state/llamafarm/tool-catalog-e2e.jsonl \
  --resume --retry-failures
```

`configuration_blocked` is recorded separately and fails the full audit.
`--allow-config-blocked` exists only for credential-diagnostic runs. Use
`--cleanup-only --resume --retry-failures` to retry unfinished teardown.

Common per-host overrides are:

```bash
export LLAMAFARM_E2E_OLLAMA_MODEL=qwen3.5:9b
export LLAMAFARM_E2E_HOST_CWD=/absolute/allowed/host/path
export LLAMAFARM_E2E_DOCKER_IMAGE=alpine:3.20
export LLAMAFARM_E2E_HTTP_PORT=18765
export LLAMAFARM_E2E_DB_CONNECTION=catalog-e2e-sqlite
export LLAMAFARM_E2E_DB_QUERY="SELECT 'CATALOG_DB_OK' AS marker"
export LLAMAFARM_E2E_SOP_NAME=catalog-e2e
```

## Unit checks

```bash
python3 -m unittest discover \
  -s scripts/tests -p 'test_tool_catalog_e2e.py' -v
python3 -m py_compile \
  scripts/tests/tool_catalog_e2e.py \
  scripts/tests/test_tool_catalog_e2e.py
```
