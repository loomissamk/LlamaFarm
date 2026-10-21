# LlamaFarm V3 — Development Plan, PR Slices and Release Gates

**Status:** Proposed, not executed. Built against V2 source inspection.
**Policy:** Merge well-tested vertical slices; never swap out the working V2 runtime wholesale. All code changes go to V3 development branch, not `main` or `v2` directly.

## Branch and release sequence

1. Snapshot, test and tag V2 at its actual accepted commit.
2. Promote V2 to `main` via normal Git review/CI/deployment acceptance.
3. Create `v3` from that exact promoted V2 commit (or rebase preexisting `v3` only after verifying ancestry).
4. Cherry-pick **selected** unique changes from the older UX branch only after compare/review; do not accidentally revert newer V2 work.
5. Implement V3 as PR-sized commits on `v3`, with tests and reversible migration flag.
6. GA: version tag + controlled promotion to `main`; keep V2 rollback tag and data backup. Delete old branches only by separate, verified housekeeping decision.

**Current tooling limitation:** GitHub connector rejected `create_branch(v3, base_ref=v2)` with HTTP 403. These documents are packaged locally rather than committed.

## First implementation batch — prioritize actual working slices

### PR-00 — V2 freeze, tests and feature audit

**Change:** no functional redesign. Add status of each existing feature and snapshot V2 real behavior.

**Read:** `TODO.md` authoritative first ~180 lines, `src/gateway/ws.rs`, `src/gateway/mod.rs`, `src/agent/loop_.rs`, `src/agent/run_ledger.rs`, `web/src/pages/AgentChat.tsx`, node profiles.

**Tasks:** capture current `main`/`v2` SHAs; compare branches; document working `chat` mode, tool loop, detached runs, server chat sessions; collect current `cargo test` and `web` build baselines; capture actual deployment node GPU and model list; do not assume V100 present.

**Gate:** branch ancestry and baseline/limitations recorded; reproducible build/test commands; no secrets recorded.

### PR-01 — Introduce typed V3 events and validators (no migration yet)

**Change:** add event envelope Rust types and serde validators consistent with `schemas/session-event.schema.json`; write adapter functions from V2 WebSocket events to V3 canonical events, without changing existing browser transport.

**Tests:** unknown event type/version rejected or handled explicitly; UUID/sequence constraints; bounded content; tool stdout not accidentally encoded as trusted model answer; stable event representations.

**Gate:** existing V2 WS tests pass; new parser fixtures pass; no client break.

### PR-02 — Transactional conversation store

**Change:** SQLite WAL schema + migrations for conversations, turns, messages and events; append-only sequence per conversation, idempotent request admission, optimistic revisions. Read-only import from JSON session store.

**Tests:** crash during batch insertion, racing duplicate client submits, two devices editing same title, JSON import count/hash/chronology, restart replay.

**Gate:** copied production-like V2 data imports losslessly with rollback; V2 old store remains preserved.

### PR-03 — Shared `ExecutionService`

**Change:** extract single Rust turn admission and run execution API; adapt V2 WebSocket and `/agent` route. Preserve V2 mode/persona semantics and existing `run_tool_call_loop`/ledger. Add Assist mode policy.

**Tests:** V2 Chat no tools; Assist read-only calls; Assist mutation approval; Agent evidence, cancellation and run correlation. Same tool scope from UI and REST.

**Gate:** `WF-01` first complete vertical slice without MCP; no duplicated agent loop.

### PR-04 — Structured inline activity UI

**Change:** React typed event reducer/transport adapter, ShellCard, PlanCard, DiffCard, ArtifactCard, ApprovalCard; optional details panel. Keep Monaco lazy. Move small-screen history controls to persistent accessible header and remove redundant banners.

**Tests:** mobile 390px drawer history and composer, terminal error, tool success, approval/reject/apply, screen reader roles, React focus. UX screenshot review. No old deep links lost.

**Gate:** a real backend run displays actual tool evidence inline, not hardcoded mock text.

### PR-05 — Native Rust MCP server

**Change:** pin/test official `rmcp`; implement standalone stdio service reusing `ExecutionService`, then authenticated HTTP MCP transport. Initial operations: delegate, status, cancel, agents, models, health, artifact. Add client grants and delegated run handles.

**Tests:** MCP initialization, tool list, actual tool-executing delegated run, evidence retrieval, cancellation, HTTP unauthorized denial, Origin restrictions, cyclic delegation refusal. Smoke-test against real Claude Code and Codex.

**Gate:** `WF-02` passes with actual permitted tool action and recorded run evidence—not just an echo.

### PR-06 — Checkpointed logical run restart

**Change:** durable run intent and checkpoint stages; restore interrupted runs into `recovering` or `uncertain`, resolve idempotent actions. Preserve V2 detachable-run behavior and run ledger.

**Tests:** daemon kill during inference, tool dispatch, after tool side effect/before ack, during artifact write; duplicate submit; lost ack. Do not blindly replay non-idempotent shell commands.

**Gate:** `WF-03` passes; unknown effects marked `uncertain` and require review.

### PR-07 — Durable artifacts and IDE diff lifecycle

**Change:** content-addressed blob store metadata, download/view endpoints, checksum, retention, auth. File diffs with optimistic source checksum and apply/reject/rollback. Browser screenshot and shell-log artifacts.

**Tests:** corruption and hash mismatch; unauthorized retrieval; large output; conflict on changing source; no leaked absolute host secret; cleanup TTL.

**Gate:** artifact link works after daemon restart and from second authorized device.

### PR-08 — VRAM-aware scheduler and multiple model residency

**Change:** track actual device UUIDs, model size/context/KV, loaded state, queue, embedding requests, per-request placement and latency. Expose UI status. Preserve strict model pinning. Configurable resident models and per-model parallelism.

**Tests:** absent GPU, resource overcommit, forced model eviction, embedding contention, 2 concurrent chat sessions and node-offline fallback. Hardware benchmarking at identical model/context/digest.

**Gate:** `WF-04` passes on at least one real node, with no OOM or silent model substitution; publish before/after metrics.

### PR-09 — Durable federation and DAG delegation

**Change:** persisted queue, idempotent remote work, lease/heartbeat, expiry and reconciliation; run trace across nodes, cancellation, node selection. Typed DAG with bounded workers and verifier.

**Tests:** remote reboot, lost ack, duplicate dispatch, worker timeout, cancel, artifact evidence sync, cycle prevention.

**Gate:** successful two-node job with worker restart and verified result, no duplicate effects.

### PR-10 — Workspace and model-management polish

**Change:** optional persistent terminal; incremental workspace retrieval; model management/status and role bakeoff UI; comprehensive routes/keyboard/mobile/accessible tests; docs and release playbook.

**Gate:** release candidate `WF-01` through `WF-06` and NFR requirements reviewed; full regression.

## Release milestones

- **M0 Baseline frozen:** PR-00.
- **M1 First unified conversation:** PR-01 to PR-04; initial V3 MVP.
- **M2 Genuine cross-app agent delegation:** PR-05 + PR-06.
- **M3 Durable artifacts and resource-aware multi-model:** PR-07 + PR-08.
- **M4 Multi-node GA:** PR-09 + PR-10, including realistic hardware gates.

Later milestones must not block delivering a usable, verified M1/M2.

## Work sequencing and parallelism

**Sequential critical path:** PR-00 → PR-01 → PR-02 → PR-03 → PR-05 → PR-06. Event schema and persistence contracts must stabilize before independent client integrations.

**Parallel once PR-03 contract is stable:** UI components (PR-04), artifact metadata (PR-07) and model-scheduler instrumentation (PR-08) can proceed behind feature flags with integration tests. Federation work (PR-09) starts after stable run state, retries and model scheduler.

**Avoid:** simultaneously refactoring `ws.rs`, rewriting `AgentChat.tsx`, implementing MCP and changing the model scheduler in one giant PR. The huge V2 files already contain valuable tested compatibility logic.

## Suggested validation commands (run in actual repo checkout)

```bash
# Preserve your current state before any edits
git status --short
git fetch origin
git rev-parse origin/v2 origin/main
git log --oneline --decorate -n 20 origin/v2
# after release verification (not automatic):
# git switch main && git merge --ff-only v2 && git push origin main
# git tag v2-release-accepted && git push origin v2-release-accepted
# git switch -c v3 main

# Rust baseline; use project-approved flags/features
cargo fmt --all -- --check
cargo check --workspace
cargo test --lib

# Frontend baseline
cd web
npm ci
npm run build
# run repository-configured test runner (inspect package.json first)
```

The merge/tag/branch lines above are deliberately commented: perform only after actual V2 acceptance and confirmation of the desired release SHA. `git merge --ff-only` may fail if remote main advances; reconcile rather than force-push.

## Test harness scenarios

| Test ID | Must demonstrate | Positive/negative assertions |
|---|---|---|
| E2E-01 | Chat→Assist→Agent→Chat | One ID, permissioned tools, linked artifacts and verified result |
| E2E-02 | Remote Claude/Codex delegation | Real tool invoked, run handle, status and result, unauthorized denied |
| E2E-03 | Mobile history controls | Drawer no content squeeze; accessible open/close and composer reachable |
| E2E-04 | Cross-device catchup | Event gap replay, no duplicates, same run state |
| E2E-05 | Daemon restart midrun | Idempotent work resumed, uncertain effects blocked, no fabricated success |
| E2E-06 | Multi-model concurrent sessions | No silent model replacement, measured VRAM/prefill/queue/eviction |
| E2E-07 | Federated worker failure | Task eventually reconciled, no double side effects, cancel works |
| E2E-08 | Evidence integrity | Failed test not `verified`; artifact hash correct and accessible |
| E2E-09 | Security policy | MCP client grant intersection enforced; no LAN auth bypass |
| E2E-10 | Upgrade / rollback | All V2 chat snapshots preserved; model/Qdrant data untouched |

## Definition of done for every PR

- Has a requirement ID and one concrete real user workflow it improves.
- Does not regress V2 `chat` mode, existing agent run evidence or hardware profiles.
- Compiles, Rust unit/integration tests pass, frontend build passes where applicable.
- Tests failures and cancellation as well as success, with factual tool evidence.
- Adds or updates architecture/API/docs and appropriate UX screenshot/test coverage.
- Does not introduce hardcoded private keys, passwords, addresses or local machine paths.
- Has a rollback switch/migration plan for schema and runtime changes.
- Includes exact commands and observed results in review notes, not assumed success.

## Operator / Codex handoff task for first PR

> Work only from branch `v3`, cut from the accepted V2 release commit. Read `docs/v3/V2_CODE_AUDIT.md`, `V3_REQUIREMENTS.md`, and `V3_ARCHITECTURE.md`. Do not rewrite the existing Rust agent loop, frontend, or Python MCP proxy. First implement PR-01: typed V3 session events with validators and V2 WebSocket adapters, behind feature flags. Write unit tests for event serializing, schema version, IDs, redaction and stable ordering. Run Rust tests and frontend build; record actual results. Do not modify production deployment or main branch. Show a diff and stop for review before PR-02.

## Decisions and risks that need explicit signoff

- Exact accepted V2 tag/SHA; branch creation permission.
- Runtime mode defaults and per-client/worker grants.
- Chosen event transport (SSE or WebSocket) for V3 while retaining V2 WS compatibility.
- SQLite sizing/backup policy and event retention.
- Real model concurrency limits by actual hardware profile, not guesses.
- Which, if any, 5 UX-branch commits should be cherry-picked after comparison.
