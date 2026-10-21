# V3 PR-01 seed: actual code and current limits

## Present in bootstrap

- `src/conversation_events.rs` (new, additive): typed serde event envelope, event types, input bounds, schema version check, contiguous replay page validation, V2 WS-frame conversion for common events, recursive key-based redaction.
- `src/conversation_store.rs`: transactional SQLite event store with atomic sequence allocation, stable event-ID idempotency, persisted replay, schema version guard and concurrent writer tests.
- `src/lib.rs`: script registers `conversation_events` and `conversation_store` modules so library tests compile them.
- Tests in the Rust module cover serialization roundtrip, unknown version/type/key, invalid identifiers, size/shape limits, missing replay events, duplicate events, cross-conversation replay, redaction, unsupported legacy frames.
- `scripts/launch_qwen_v3.sh` submits bounded PR-01 follow-through to LlamaFarm's real tool-enabled `/agent` API.

## NOT yet implemented

- V2 WebSocket code still emits legacy frames; the adapter is not yet used by gateway.
- SQLite event store module is implemented but not yet wired to the gateway; no replay REST endpoints.
- No Rust-native MCP, Assist mode, full event fan-out or React activity cards.
- Redaction handles known credential *keys*, not secret values embedded inside unstructured stdout. Never use this as the only security gate.
- No verified successful Cargo check or Rust test run in the generating environment (Rust toolchain absent); local Bash bootstrap/branch/push sequence passed in a throwaway bare Git repository only.

## Required next steps for Qwen (PR-01 continuation)

1. Validate the exact Rust code against `v3` (Cargo format, check, both module test suites).
2. Compare the adapter against actual V2 WS event variants, including their payload keys.
3. Add a nonbreaking gateway event-mapping integration test without changing V2 client transport.
4. Confirm schema and Rust field constraints match; adjust tests and schema deliberately.
5. Commit a reviewable PR-01 on `v3` and report real tool output.

## GitHub status

The connected integration returned 403 `Resource not accessible by integration` on creating `v3` even though GitHub reports user-side admin/push access. The bootstrap must run on a credentialed checkout. The user created/pushed the actual `v3` branch. No starter code has been pushed by ChatGPT; the integration still rejects writes with HTTP 403.
