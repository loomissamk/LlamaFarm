# LlamaFarm V2 Source Audit — Baseline for V3

**Reviewed:** 2026-10-21
**Repository:** `loomissamk/LlamaFarm`
**Source ref:** `v2` (NOT `main`, NOT the parallel UX branch)
**Method:** GitHub connector source inspection and branch comparison. No local compile, live deployment, or integration-test run was possible here. Features reported by source/README/TODO are not automatically operationally verified.

## Branch status at review

- `v2` is **62 commits ahead** of `main`, zero behind. Its inspected HEAD is `f66e920adda88afaf311d9c176ed8cf5ee153eb9`, dated 2026-08-21 03:21 UTC (2026-08-20 Eastern).
- `fix/durable-chat-replay` is 49 commits ahead of `main`.
- `feat/workspace-automation` is 44 commits ahead of `main`.
- `ux/mobile-sidebar-and-federation-declutter` is 5 commits ahead of `main`.
- `v2` and the UX branch **diverged**. UX changes are not inherently included in `v2`. The UX branch had newer October 21 commits. Thus **V2 is the larger intended release baseline, not the most recently pushed branch chronologically**.
- Desired release policy: validate/promote `v2` to `main`; use V2 content as the V3 development baseline; never delete other branches until uniquely useful changes are assessed and release rollback points are retained.

## Unique parallel UX work requiring port/review

The separate UX branch includes a mobile drawer fix (`7aa0800`), redundant-UI removal and app-wide cleanup (`9cff4da`), native TerminAI planner registration (`9212513`), and an invalid-command-argument warning (`b3b2639`). V2 already contains a mobile app drawer and a chat-mode dropdown, so review line-level differences rather than broadly cherry-picking commits. In V3, preserve current V2 functionality, explicitly test any extracted improvements, and treat incomplete TerminAI argument validation as a safety item.

## Evidence matrix

| Area | V2 evidence | V3 delta / verification required |
|---|---|---|
| Core backend | `Cargo.toml` includes Tokio, Axum, serde, tracing, rusqlite; Rust agent, gateway, tool and federation modules present. | Keep Rust core; refactor into a shared service, not a second agent loop. |
| Agent chat | `src/gateway/ws.rs` has `/ws/chat` with `chunk`, `tool_call`, `tool_result`, `done`, `cancelled` events; `web/src/pages/AgentChat.tsx` handles streaming and inline tool states. | Typed versioned event envelope, event replay and richer cards. |
| Plain chat mode | `src/gateway/ws.rs` `resolve_mode_tools_and_temperature` excludes all tools when `agent_mode=chat`; `resolve_mode_prompt_effect` replaces the system prompt; `AgentChat.tsx` exposes agent-mode dropdown. | Preserve existing chat and custom persona modes; add a clearly permissioned Assist mode, with continuous conversation state. |
| CLI/API | `src/gateway/mod.rs` exposes `POST /agent`, `GET /ws/chat`, `GET /v1/models`, `POST /v1/chat/completions`. | Extract shared run service; don't treat text completion as full agent invocation. |
| OpenAI compatibility | `src/gateway/openai_compat.rs` request struct declares `model`, `messages`, `temperature`, `stream`; no `tools` field in that request type. | Either add genuinely conformant tool calls or explicitly document completions as text-only; MCP should use agent service directly. |
| Chat persistence | `src/gateway/ws.rs` persists chats to `state/web-chat-sessions.json`, uses in-memory active run registry and an async file write lock. `TODO.md` acknowledges incomplete transactionality. | SQLite transactional message/event store with stable IDs, revisioned writes, disconnect/restart handling. |
| Detached runs | `TODO.md` says browser detach does not cancel server-owned compute and Runs API can cancel. | Test in both hardware nodes; resume compute after process/container restart remains **not implemented** in the source backlog. |
| Run evidence | `src/agent/run_ledger.rs` records planner, executor, verifier, tool evidence and routes; Runs endpoints in gateway. | Link run events to chat and MCP calls; artifact hashes; ensure final success is evidence-gated. |
| Tool routing | `src/agent/tool_router.rs` deterministic IDF-weighted per-turn tool ranking. | Reuse contract on CLI, chat, MCP, remote workers; don't send every tool schema per turn. |
| Model routes | `TODO.md` records task/role model routing and bakeoff primitives. | Add bounded multi-model residency and resource admission based on actual per-node VRAM, measured load/TTFT, and placement constraints. |
| Federation | `src/federation/mod.rs`, remote subagent and registry exist. `TODO.md` says task queue is in-memory and lacks durable leases/restart recovery. | Durable queue, worker leases, idempotency, cancellation, and honest failure states. |
| IDE | V2 backlog marks lazy-loaded Monaco delivered, and `AgentChat.tsx` embeds IDE view. | Diff/apply/reject/rollback, dirty-draft conflict handling, terminal persistence and artifact-centric workspace. |
| Responsive UI | V2 `Sidebar.tsx` has mobile drawer, focus behavior, escape-close. | Complete real mobile acceptance and simplify chrome; do not copy changes blindly from separate UX branch. |
| GPU truth | `TODO.md` 2026-08-13 authoritative note: `.154` ran RTX 5070 Ti; V100 removed pending cooled reinstall. | Detect inventory at runtime; never assume the V100 is currently physically installed. |
| Security model | V2 targets private LAN and disposable labs. Pairing disabled by deployment decision. | Introduce explicit per-client MCP authentication, authorization and command policy independent of network location. |

## Findings that should change implementation priorities

1. **Don't rebuild chat mode.** Chat already exists and excludes tools. The V3 job is a coherent user-facing Chat / Assist / Agent policy and persistent transitions.
2. **Don't introduce a Python orchestrator.** The core is Rust and already has a tool loop, ledger, subagents and gateway. A native MCP adapter can call the shared execution service.
3. **Don't claim MCP readiness from echo tests.** Prior `server.py` in an uploaded transcript forwarded `/v1/chat/completions`. That proves basic generation, not actual tool execution, run tracking or resumability. There is already a real `POST /agent` in the Rust router.
4. **Don't claim restart recovery exists.** Detached viewers work in principle; daemon restart is a different fault domain. V2 TODO explicitly defers checkpointed compute resume.
5. **Don't optimize on hearsay.** V2 backlog documents a previous erroneous CPU-spill diagnosis and a verified model-reload issue. Require instrumentation to distinguish queue, load, prefill and decode.
6. **Don't merge UX branches blindly.** V2 and UX diverged. Compare distinct mobile/usability changes, pick only the missing improvements, and test regressions.
7. **Don't imply native OpenAI function calling.** `ChatCompletionsRequest` in V2 lacks `tools`; V3 must specify whether it intends to implement that standard independently of MCP.
8. **Don't silently expose shell tools to an external MCP client.** V3 authorization requires explicit client grants and tool-class policies.

## Source anchors

- `README.md` — overview and evidence-first execution.
- `TODO.md` lines 8–180 — authoritative deployment status and outstanding work.
- `src/gateway/mod.rs` — `/agent`, `/ws/chat`, `/v1/chat/completions`, `/api/chat-sessions`, `/api/runs` routes.
- `src/gateway/ws.rs` — chat/session store, active run registry, agent modes, tool loop and WS events.
- `src/gateway/openai_compat.rs` — actual completion request/response contract.
- `src/agent/run_ledger.rs` — run evidence and verification.
- `src/agent/tool_router.rs` — tool routing.
- `src/tools/mod.rs` — Tool trait registry and capability entries.
- `web/src/pages/AgentChat.tsx`, `web/src/components/layout/Sidebar.tsx` — current chat and mobile presentation.
- `src/federation/mod.rs` — federation entrypoint.

## Source of external standards

- MCP transport specification (2026-07-28): https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/transports/streamable-http.mdx
- Official Rust MCP SDK: https://github.com/modelcontextprotocol/rust-sdk
- Ollama concurrency/model residency FAQ: https://github.com/ollama/ollama/blob/main/docs/faq.mdx

## Verification limits

This audit is source-grounded, not a benchmark. No live LlamaFarm instance, Docker host, Claude Code client, or Codex client was exercised. Nothing in this document asserts that V2 has been promoted to `main` or that V3 code has been deployed.
