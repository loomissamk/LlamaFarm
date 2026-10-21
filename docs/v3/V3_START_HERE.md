# V3 — First Implementation Handoff

This is the suggested **first message to send to Codex or Claude Code inside a real LlamaFarm checkout** after creating `v3` from the accepted V2 release commit.

> You are working on **LlamaFarm V3**, a Rust/Tokio/Axum + React evolution of V2. First read `docs/v3/README.md`, `V2_CODE_AUDIT.md`, `V3_REQUIREMENTS.md`, `V3_ARCHITECTURE.md`, and `V3_IMPLEMENTATION_PLAN.md`. Confirm `git status` is clean and that the checked-out `v3` branch descends from the accepted V2 release commit. Do not change `main` or delete any branches. Respect the existing tool loop, V2 chat mode, run ledger, WebSocket, federation and Monaco IDE; do not rewrite them.
>
> Implement **PR-01 only**: introduce typed, versioned conversation event envelopes matching `docs/v3/schemas/session-event.schema.json` using serde. Write tests for serialization/deserialization, strict event-name/version validation, monotonic replay ordering, JSON payload size bounds, and redacted tool arguments. Use the included transactional SQLite event store with idempotent server-owned sequence allocation, restart replay and concurrent-writer unit tests. Include a compatibility adapter from the current `src/gateway/ws.rs` event kinds without breaking existing clients. If refactoring is unavoidable, keep it small and separate.
>
> Required verification: `cargo fmt --all -- --check`, `cargo check --workspace`, `cargo test --lib`, and `cd web && npm run build`; inspect actual project scripts before invoking others. Report command output and failing tests accurately, provide a complete diff and list which requirements are implemented. Do not claim runtime acceptance merely because compilation succeeded. Do not modify live Docker, GPUs, credentials, or existing V2 database schemas in this first PR. A new isolated V3 event-store file is allowed. Stop at a reviewable commit on `v3`.

## How to copy these docs into repo

Unzip the package at the repository root. Its top-level folder is `docs/v3/`. If downloaded to a separate folder rather than into the repository, copy the contents to `docs/v3/` and inspect changes with `git status` before committing.

## Why this first PR

- It creates a stable shared event contract for later persistence, chat, MCP and UI work.
- It is an actual code/test change, but low risk to currently working V2 functions.
- It avoids building MCP over the wrong text-only `/v1/chat/completions` endpoint.
- It gives the future agent a concrete, verifiable task with a defined stopping point.

**Repository-write status when this package was generated:** GitHub connector can read V2 but returned HTTP 403 on new-branch creation. The file package is real, but the operator created and pushed V3 on 2026-10-21. Direct connector writes remain forbidden; no files from this package are yet committed.
