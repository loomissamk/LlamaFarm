# LlamaFarm V3 — Development Specification Package

**Source baseline:** `loomissamk/LlamaFarm` branch `v2` inspected 2026-10-21, HEAD `f66e920adda88afaf311d9c176ed8cf5ee153eb9` (August 20 Eastern). The separate UX branch has newer October commits but diverged from V2.

V3 is an upgrade to the existing Rust LlamaFarm, not a new framework. Its purpose is **one persistent workspace for conversation, assisted inline tool execution, autonomous plans, and delegated agents**, available from browser, mobile, terminal, Claude Code, Codex and eventually VS Code.

## Documents

1. [`V2_CODE_AUDIT.md`](V2_CODE_AUDIT.md) — inspected V2 features, actual known gaps, branch status and source map.
2. [`V3_REQUIREMENTS.md`](V3_REQUIREMENTS.md) — product and technical requirements with stable IDs, acceptance criteria, constraints and nonfunctional targets.
3. [`V3_ARCHITECTURE.md`](V3_ARCHITECTURE.md) — proposed Rust services, persistence model, event protocol, API/MCP surfaces, scheduler and execution policies.
4. [`V3_IMPLEMENTATION_PLAN.md`](V3_IMPLEMENTATION_PLAN.md) — PR-sized delivery sequence, dependencies, tests, migration and release gates.
5. [`schemas/session-event.schema.json`](schemas/session-event.schema.json) — draft machine-readable event envelope, *not* a claim that V2 emits this schema.

## Branch / release rule

Keep V2 as the production candidate. **Validate and promote V2 to `main`** through your normal release procedure, leaving a V2 tag and rollback point. The `v3` branch was created from the existing `v2` HEAD (`f66e920`) and pushed successfully by the operator on 2026-10-21. V3 should remain a separate development branch while V2 is validated and promoted to `main` through the normal release procedure. Reconcile any later V2 fixes deliberately; never automatically rebase working V3 commits. Do not merge or delete unrelated branches merely because they exist.

The `v3` branch exists remotely. The ChatGPT GitHub connector can read it, but attempts to write files still return `403 Resource not accessible by integration`; this package must be applied from an authenticated local checkout until connector write access is fixed.

## Required values

Local-first, free-by-default; Ollama as the baseline model backend; no cloud dependency for core operation; safe permission boundaries; verified evidence before claiming success; current node configuration respected; preserve existing model and Qdrant volumes during upgrades; avoid destructive rebuilds.

## V3 starter code (additive; not connected to the running gateway)

The V3 bootstrap package contains `src/conversation_events.rs`: a typed
serde conversation event envelope, schema/version/size checks, contiguous replay
validation, known-key argument redaction, a limited V2 WebSocket adapter and
Rust unit tests. It also contains `src/conversation_store.rs`: a transactional
SQLite append/replay store with idempotent retries, atomic sequence allocation,
reconnect replay, database version guard and concurrent-writer tests. The
bootstrapping script registers both modules
in `src/lib.rs` and validates with Cargo **on the operator machine**, before
committing/pushing. The event types are not yet consumed by the V2 gateway and
no gateway integration or existing V2 persistence migration has taken place. See `PR01_CODE_STATUS.md`.

`./scripts/launch_qwen_v3.sh` submits a real PR-01 task to the existing
tool-enabled LlamaFarm `POST /agent` route, with the checkout path explicitly
set by `LF_AGENT_CHECKOUT` and the model selected in the LlamaFarm UI. It does
not choose the model itself or create a background job from this chat.
