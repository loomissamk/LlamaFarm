# LlamaFarm V3 — Technical Architecture and Interface Contracts

**Status:** Proposed architecture grounded in reviewed V2 source; not implemented
**Baseline:** V2 branch of `loomissamk/LlamaFarm`, inspected 2026-10-21

## A. Design decision

**One Rust-owned execution service and one durable conversation/event model.** Browser WebSocket, `POST /agent`, CLI, cron, MCP, federation and future VS Code extension become front-end adapters. They may use different presentation or session transports; none may create a second independent agent loop or bypass tool policy.

Do **not** translate the Python MCP prototype line-for-line. It forwards plain model completions, which is not the verified tool-enabled agent path. V2 `src/gateway/mod.rs` already defines a tool-enabled `POST /agent`, and V2 `src/gateway/ws.rs` runs its existing tool loop. First refactor common semantics from those paths.

```
  Web/mobile SPA      Rust CLI        Claude Code/Codex      VS Code (later)
       |                 |                   |                    |
       +---------+-------+             stdio MCP / HTTPS MCP ----+
                 |                              |
          /api/v3 + WS/SSE                 Rust rmcp adapter
                 |                              |
                 +------------------------------+
                                |
                      ExecutionService (Rust)
             admission -> mode/policy -> planner/tool loop
                                |
       +------------------------+--------------------------+
       |                        |                          |
   SessionStore            Run/Evidence               ModelScheduler
   events/messages         tool registry               node inventory
   revisions               approvals                   residency
   artifacts               verifier                    VRAM/queue
       |                        |                          |
     SQLite           local filesystem/tools       Ollama/local models
       |                   + federation                  / specialist C++
       +-------------------+---- durable artifacts / leases
```

### Preserved V2 modules

- `src/agent/loop_.rs` — core tool loop; extract reusable call shape rather than copy the logic.
- `src/agent/run_ledger.rs` — verified plan/tool evidence; add event+artifact references.
- `src/agent/tool_router.rs` — deterministic per-turn subset selection; apply to all surfaces.
- `src/tools/mod.rs` — canonical registry and policy-enforced executor.
- `src/gateway/ws.rs` — keep protocol compatibility as an adapter; move persistence and run lifecycle ownership out of global statics.
- `src/gateway/mod.rs` — preserve `POST /agent`, `/ws/chat`, `/v1/*`, `/api/runs` and cross-device session endpoints for V2 clients.
- `src/federation/*` — implement durable queue behind existing peer/task logic.
- `web/src/pages/AgentChat.tsx` — extract chat view model, run activity components, history/mobile drawer without losing existing Monaco/federation functionality.

### Proposed new modules; names provisional

```
src/runtime/execution_service.rs  // single shared turn/run admission and lifecycle
src/runtime/mode_policy.rs        // chat, assist, agent, persona resolution
src/runtime/events.rs             // schema-versioned event envelope, subscribers
src/runtime/model_scheduler.rs    // loaded-model and worker resource admission
src/state/conversations.rs        // SQLite conversation/message persistence
src/state/event_log.rs            // durable ordered append + replay
src/state/run_checkpoints.rs      // recoverable steps and uncertain operations
src/artifacts/store.rs            // durable hash-addressed blobs + metadata
src/mcp/server.rs                 // Rust rmcp MCP server; no independent agent loop
src/federation/task_queue.rs      // persisted queue and worker leases
web/src/features/chat/*           // shell, diff, browser, plan, approval, status cards
web/src/features/workspace/*      // optional Monaco/artifact inspection panel
```

Exact directory names should follow existing crate boundaries. Avoid circular imports and do not move V2 APIs just for stylistic tidiness.

## B. Execution service contract

A logical command (conceptual Rust interfaces):

```rust
pub struct SubmitTurn {
    pub conversation_id: ConversationId,
    pub client_id: ClientId,
    pub idempotency_key: String,
    pub expected_revision: Option<i64>,
    pub content: String,
    pub mode: Mode,                 // Chat | Assist | Agent | configured persona
    pub model_preference: Option<ModelId>,
    pub workspace_scope: WorkspaceScope,
    pub permission_grant: GrantId,
}

pub struct AcceptedTurn {
    pub turn_id: TurnId,
    pub run_id: Option<RunId>,
    pub conversation_revision: i64,
    pub initial_event_seq: i64,
}

// ExecutionService::submit_turn is used by HTTP, WS, CLI and MCP adapters.
```

**Rules:**

1. Validate identity, workspace scope, mode and tool permissions before admission.
2. Transactionally insert accepted user turn and reserve ordered conversation event ID; retries with same idempotency key return original accepted turn.
3. Decide if a run is required; create and link `run_id` transactionally.
4. Execute asynchronously. The browser/MCP stream is a **viewer**, never the run owner.
5. Stream small typed events, record authoritative append-only events and store large outputs as artifact references.
6. Independent verifier computes terminal status based on tool evidence; model-provided text is not the status authority.
7. Cancellation requests update durable desired state; if already completed or a side effect cannot be interrupted, report that truthfully.

### Modes

- **Chat:** reuse V2 `agent_mode=chat` semantics; tool registry set empty by server policy.
- **Assist:** permitted read-only tools execute as conversational turns; mutating operations create pending approvals scoped to operation hash, workspace and expiry; destructive denied by default.
- **Agent:** reuse V2 verified run loop and task plan, subject to specific approval policies and budgets. Explicit agent invocation may be long-running and detached.
- **Persona:** retain V2 custom AGENTS variants. A persona modifies prompting/role, not the user/client's maximum tool privileges.

Mode is recorded **per turn** and may change without changing `conversation_id`.

## C. Durable data layout

SQLite WAL is the initial single-coordinator store. On shared/multi-host installations each node owns a local database; the coordinator owns the authoritative conversation and task queue. Distributed SQL is not required for MVP. Never share one SQLite file over a remote filesystem.

Illustrative normalized tables:

| Table | Essential fields | Constraint |
|---|---|---|
| `conversations` | id, title, created_at, updated_at, revision, owner, retention, deleted_at | Stable ID, optimistic revisions |
| `turns` | id, conversation_id, client_id, mode, requested_model, admitted_at, idempotency_key, status | Unique (`client_id`, `idempotency_key`) |
| `messages` | id, conversation_id, turn_id, role, content, seq, visibility, created_at | Content/role validated; stable message ID |
| `events` | id, conversation_id, seq, turn_id, run_id, type, schema_version, data_json, time | Unique (`conversation_id`, `seq`), ordered append |
| `runs` | id, conversation_id, turn_id, mode, actor, selected_model, worker_id, status, phase, version, checkpoint | Work survives viewer detach |
| `steps` | id, run_id, dependency_ids, status, idempotency_kind, intent_hash, evidence_ids | Step transitions auditable |
| `tool_calls` | id, run_id, step_id, tool, policy_grant, args_digest, status, started, ended | No success without result/evidence |
| `artifacts` | id, digest, mime, size, source_run, source_tool, path, retention, created_at | Stored by digest, authenticated downloads |
| `approvals` | id, tool_call_id, operation_hash, client_id, scope, expiry, decision, decided_at | A replayed approval cannot expand scope |
| `workers` | id, last_seen, capability_digest, device_ids, model_residency, queue_depth | Stale data expires |
| `federation_tasks` | id, run_id, worker_id, lease_until, attempt, idempotency_key, status | Durable outbox/inbox semantics |
| `schema_migrations` | id, applied_at, checksum | Versioned and reversible where feasible |

**V2 migration:**

1. Snapshot `state/web-chat-sessions.json`, existing run ledger and workspace. Verify source files and hash.
2. Import non-temporary sessions into SQLite in a transaction; synthesize IDs only where V2 lacked them, recording provenance.
3. Preserve original JSON in read-only archive. Check message counts, chronology and recoverable artifact links.
4. Run in a dual-read migration window if necessary; only switch writes to V3 after complete verification. Never independently write both JSON and SQL as authoritative stores.
5. Ensure exported backups and restore procedure tested before deploying to either hardware node.

## D. Versioned event envelope

Draft machine-readable contract: `schemas/session-event.schema.json`.

```json
{
  "schema_version": 1,
  "event_id": "evt_01J...",
  "conversation_id": "conv_01J...",
  "sequence": 391,
  "turn_id": "turn_01J...",
  "run_id": "run_01J...",
  "timestamp": "2026-10-21T20:10:00Z",
  "type": "tool.completed",
  "data": {
    "tool_call_id": "call_01J...",
    "name": "shell",
    "success": false,
    "exit_code": 1,
    "duration_ms": 1423,
    "artifact_ids": ["art_01J..."],
    "display_excerpt": "Tests failed: 1 of 8"
  }
}
```

**Invariants:** monotonic `sequence` per conversation, unique event IDs, valid message/run relationships, UTC timestamps, payload schema, idempotent consumption, redacted arguments. Artifacts stored separately from bounded event body. Every WebSocket/HTTP subscriber gets the same canonical event semantics.

**Client reconciliation:** call `GET /conversations/{id}` for a snapshot, then stream/replay events **after** the snapshot's last sequence. De-duplicate by ID, detect gaps and refetch. Treat client-local drafts as drafts until server admission is acknowledged. Avoid trusting a locally guessed run success state.

**Event catalogue (minimum):** `message.accepted`, `assistant.delta`, `assistant.completed`, `run.created`, `run.status`, `step.status`, `tool.started`, `tool.completed`, `artifact.created`, `approval.requested`, `approval.resolved`, `model.metrics`, `worker.status`, `error`, `session.snapshot`.

**Hidden reasoning:** never stream chain-of-thought. Expose concise operational activity and verification evidence rather than model-private reasoning.

## E. Run lifecycle and recovery

```
accepted -> queued -> running -> verifying -> verified
                       |           |          -> completed_unverified
                       |           |          -> failed
                       +-----------> cancellation_requested -> cancelled/uncertain
                       +-----------> blocked (approval/resource/dependency)
                       +-----------> interrupted -> recovering -> queued/uncertain
```

Never convert `interrupted` to `verified` on model narration alone. If a tool side effect could have occurred before crash but its acknowledgement was not persisted, mark **uncertain**. Resume only after deterministic reconciliation, known idempotency, or explicit user instruction.

Step recovery stores a durable intent record and result record. The executor should write intent before launching side-effecting work and commit exact result/evidence after completion. Idempotency tokens must accompany repeatable external actions when supported; for raw shell commands with unknown effects require review.

## F. MCP architecture

Use a maintained version of the official Rust [`rmcp`](https://github.com/modelcontextprotocol/rust-sdk) SDK, pinned for release. As of this draft, the 2026-07-28 protocol supports a POST-based Streamable HTTP transport with version/header requirements; clients with earlier protocol versions require explicitly tested negotiation/compatibility. This protocol-level statelessness is **distinct** from LlamaFarm's durable conversations and jobs.

### Tools exposed by the MCP server (initial)

| Tool | Essential input | Output |
|---|---|---|
| `llamafarm.delegate_task` | objective, role?, conversation_id?, workspace?, allowed_scope?, model_preference?, idempotency_key | `run_id`, acceptance, status, synopsis or result link |
| `llamafarm.get_run` | run_id | authenticated run state, verified steps, evidence IDs |
| `llamafarm.cancel_run` | run_id | requested/terminal state |
| `llamafarm.resume_run` | run_id, confirmed_reconciliation? | resumed/blocked/uncertain outcome |
| `llamafarm.get_artifact` | artifact_id, preview_or_download | metadata, content reference or bounded inline preview |
| `llamafarm.list_agents` | filter? | roles, tool scopes and model capabilities |
| `llamafarm.list_models` | node_id? | installed/resident models and capabilities, not fabricated capacity |
| `llamafarm.health` | none | health, version, node and subsystem state |

Use MCP resources for scoped tool/run descriptions and content previews if supported by clients. Tools that mutate or run code are separately grant-controlled. For long-running work, return a handle and provide status/resource links. If the negotiated MCP tasks feature is supported by client/server, map it to underlying `run_id` rather than starting a second job system.

### Transports and security

- **stdio:** local process spawned by Claude Code/Codex, no port; still pass OS identity and workspace grants rather than grant everything by default.
- **HTTP:** authenticated endpoint (default loopback bind; remote explicitly enabled), TLS at ingress, Origin validation, anti-rebinding, per-client token or suitable authenticated identity, resource/command restrictions, rate limits, audit.
- Do not expose raw Ollama or internal tool-execution endpoints to the LAN for convenience.
- Do not confuse MCP client access to a **model reply** with access to LlamaFarm's **tool-enabled agent**.
- Client compatibility tests must use actual Claude Code and Codex versions, not a stub only.

### Optional outbound MCP client

Use one MCP client integration layer that converts external tool schemas to existing `Tool` trait wrappers. Cache discovery by server/version digest; deny ungranted calls. Tool names are namespaced and pass through the same per-turn tool-router and verification pipeline.

## G. Model scheduler

Ollama `OLLAMA_MAX_LOADED_MODELS` controls maximum **resident models** and `OLLAMA_NUM_PARALLEL` controls parallel requests per model; KV/context usage scales with concurrency and can exceed capacity. Treat them separately. Hardware profiles are defaults, not authoritative facts.

Admission inputs:

```
node: gpu UUID(s), free/reserved VRAM, RAM, real model residency,
      loaded digest/quantization, context budget, current queue, health
request: requested role/model, context tokens, vision/tools needs,
         deadline/priority, permission scope, locality/workspace
```

Ranking policy: eligible local pinned model first; preserve active warm model unless benefit exceeds reload cost; never silently substitute model for a strict pin; route smaller specialists independently from selected chat model; reject/queue clearly when no eligible worker. Separate embedding service when it prevents GPU eviction. Limit per-model concurrent contexts based on measured real usage, not theoretical model parameter count. Record placement decision in run evidence.

**No unconditional one-model limit and no unconditional unlimited concurrency.** Configuration can allow 1 model/stream on small nodes and multiple resident models/requests on capable nodes.

Telemetry: queue wait, model load time, TTFT, prefill TPS, generation TPS, prompt/decode tokens, context/compaction, true residency, VRAM/RAM peaks and evictions. Missing provider metrics remain absent rather than fabricated.

## H. UI component structure

```
AppShell
  NavigationDrawer (mobile / grouped desktop)
  ConversationView
    ConversationHistoryDrawer
    Header [conversation title | model/node | mode | run]
    Transcript [Message | ToolCard | DiffCard | BrowserCard | PlanCard]
    Composer [attachments | mode | send | followup | stop]
  WorkspaceInspector (optional)
    MonacoEditor | DiffViewer | ArtifactPreview | RunTimeline | Terminal
  AdvancedArea
    Models | Federation | Knowledge | Tools | Cron | DB | Settings
```

Presentation driven by typed events and artifact references, not string parsing from assistant output. Keep mode selector at composer or compact header; no always-expanded federation ribbon. Mobile drawers must not reduce central content width; compositor respects safe-area insets and keyboard. Persist *user layout preferences* locally but keep *conversation truth* on server.

Frontend migration: start by extracting event handling logic from `AgentChat.tsx` into typed hooks/reducers. Continue to support V2 WebSocket event names until adapters ship. Avoid rewriting 2700 lines in one PR.

## I. Test matrix and upgrade safety

- **Unit:** mode authorization, tool schema routing, event schema, ID uniqueness, store revisions, approval expiry, idempotency and step state machine.
- **Integration:** Axum API, SQLite WAL transactions, MCP stdio and HTTP, resource auth, tool dispatch, run evidence, artifact digests.
- **Fault injection:** WebSocket disconnect, power/process kill during tool, lost MCP reply, lost federation ack, worker disappearance, duplicate event, out-of-order stream.
- **Real clients:** Claude Code, Codex; validate list/call/run/cancel/artifact flows.
- **Browser:** mobile/tablet/desktop Playwright with screenshot review; keyboard and focus behavior; Monaco lazy load, conflicts and diff.
- **GPU:** exact hardware UUID, installed quant/model digest, cold/warm runs, 1-vs-2 resident models, embeddings concurrency, queue scheduling and VRAM peak.
- **Operations:** snapshot/restore of chat JSON, SQLite, run ledgers, Qdrant/model volumes; rollback after deploy.

All data migration or scheduler changes remain feature-flagged until tests pass on a copied workspace and at least one actual node. The second node is a later acceptance gate, not a prerequisite for basic single-node correctness.

## J. Open decisions to settle in early PRs

1. Whether Chat/Assist/Agent preferences persist per conversation or per user session. Requirement: persist actual mode **per turn** regardless.
2. SQLite as the canonical coordinator store (recommended) versus optional Postgres for advanced distributed deployments (defer unless measured need).
3. Whether `/api/v3/...` stream uses SSE + control HTTP versus WebSocket with replay. Recommend **SSE for events plus HTTP controls**; retain V2 WebSocket adapter to avoid breaking clients.
4. What level of command compensation is possible. Default: filesystem patch rollback only when verified and applicable; no promise to undo arbitrary shell effects.
5. How to manage lab-mode grants. Recommend explicit workspace/target bound grants with expiry and human confirmation for destructive actions; do not accept model-requested escalation.
6. MCP long-running task capability negotiation against actual installed clients. Fall back to run IDs + `get_run` as the portable base.

## K. External references

- MCP Streamable HTTP 2026-07-28: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/transports/streamable-http.mdx
- Rust MCP SDK: https://github.com/modelcontextprotocol/rust-sdk
- Ollama concurrency: https://github.com/ollama/ollama/blob/main/docs/faq.mdx
