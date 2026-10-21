# LlamaFarm V3 — Product and System Requirements

**Status:** Draft v0.1, implementation-ready baseline; not approved or built
**Date:** 2026-10-21
**Based on:** `loomissamk/LlamaFarm` branch `v2`, source-inspected; full audit in `V2_CODE_AUDIT.md`
**Target:** V3 developed from the exact V2 commit eventually promoted to `main`
**Owner:** LlamaFarm project
**Requirement IDs:** Stable. Never reuse an ID after removal; mark deprecated.

## 1. Executive objective

Make LlamaFarm a **persistent, local-first AI workspace** in which a user can:

1. Ask questions with no tools, choose an assistant with read-only or approval-gated tools, or launch a multi-step autonomous run—**without changing conversations**.
2. See real shell, files, diffs, browser results, logs and delegated tasks **inline in chat**, with full evidence available on demand.
3. Resume conversations, active jobs and artifacts from web, phone, terminal, local IDE, Claude Code and Codex on the same authorized runtime.
4. Delegate work to local and federated specialist models intelligently while the system schedules GPU resources without avoidable model thrash.
5. Trust completion labels only when actual tool evidence and deterministic checks support them.

This is an evolution of the existing Rust/Tokio/Axum codebase. Do not replace existing run ledger, tool registry, WebSocket agent, chat mode, Monaco IDE or Ollama baseline without a demonstrable reason.

## 2. Release intent and baseline

- `v2` is 62 commits ahead of `main` at review; inspected HEAD `f66e920adda88afaf311d9c176ed8cf5ee153eb9` (August 20 Eastern). The separate UX branch is 5 ahead, has newer October commits, and diverged from V2; it must be evaluated selectively, not indiscriminately merged.
- V2 already has a **real chat mode** (`agent_mode=chat`, tool exclusion), agent/persona mode choice, `/ws/chat`, `/agent`, persisted session discovery, detachable runs, verification ledger, tool router, roles and federation.
- The previous stand-alone Python MCP `server.py` from transcript is a chat-completion proxy. Do not treat it as equivalent to full LlamaFarm delegation.
- V2 authoritative `TODO.md` acknowledges missing transactional sessions, restart-resumable compute, durable federation jobs and durable content-addressed artifacts.
- The V2 deployment note dated 2026-08-13 says the `.154` node used RTX 5070 Ti and the V100 was physically removed pending cooling. V3 MUST discover actual hardware at runtime; no document or scheduler may assume a V100 is present.
- V2 shall be tagged/validated and promoted to `main` under a separate release gate. V3 must use that exact accepted content as its foundation, and preserve existing data/models.

## 3. Users and usage environments

**Operator:** Single individual using a local workstation, lab server, phone and terminal; expects full autonomy on designated disposable targets but ordinary operation on safe systems.

**Developer:** Works in VS Code/Codex/Claude Code and wants LlamaFarm to act as either an explicit specialist subagent or a first-class standalone orchestrator.

**Observer:** Watches long-running jobs via a different browser/device; needs progress, history, artifacts and cancellation where authorized, not shell access by default.

**Execution surfaces:** Browser SPA, mobile browser, Rust CLI, stdio MCP client, authenticated LAN/Tailscale MCP client, future VS Code extension, federated worker.

## 4. Product principles and non-goals

**Principles:** Rust-first service ownership; local inference by default; single source of truth for conversation/run state; reliable typed events; honest execution claims; progressively disclosed complexity; no silent loss of chat history; explicit capabilities and scoped approvals; verified, measured performance changes.

**Not V3 core:** mandatory commercial inference, rewriting the entire frontend framework, replacing Ollama without benchmark evidence, unconditional parallel loading of arbitrary models, bypassing operating-system security policies, production-use destructive automation without isolation, automatic merging/deletion of Git branches, or promising resumability of non-idempotent OS processes after a crash.

## 5. Definitions

- **Conversation:** Durable user-visible thread, identified by `conversation_id`, containing messages, events, settings and linked runs/artifacts.
- **Turn:** One user request and its assistant response, optionally including tool events and a run.
- **Run:** Server-owned executable job with `run_id`, tracked steps, policy, evidence and lifecycle.
- **Chat mode:** V2-compatible text conversation with no executable tools.
- **Assist mode:** Tool-enabled conversation with read-only tools available and operator approval for writes/unsafe side effects.
- **Agent mode:** Long-running plan/execute/verify loop with bounded autonomy and recorded evidence.
- **Artifact:** Durable file, screenshot, diff, report, log or structured result with checksum and references.
- **Worker:** Local or remote execution/inference node reporting capabilities and resource pressure.
- **Replay:** Reconstruct visible events from a persisted event cursor. **Not** reexecute effects.
- **Resume:** Continue a checkpointed logical plan, checking which operations did/did not commit and requiring idempotency/approval where necessary.
- **MCP server:** External client-facing tool/resource interface; does not own a second agent loop.
- **MCP client:** Optional integration for consuming external tool servers inside the normal LlamaFarm registry.

## 6. Functional requirements

### 6.1 Unified conversational workflow

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| CHAT-001 | P0 | Keep existing `chat` mode fully tool-free, including no hidden tool executions. | In Chat, attempted shell call is absent from both transcript and tool audit. |
| CHAT-002 | P0 | Add `assist` as a distinct policy while retaining V2's custom persona/AGENTS variants. | Read-only tool executes inline; write tool requires explicit authorization. |
| CHAT-003 | P0 | Retain current `agent` plan/execute/verify behavior with the existing run ledger. | Multi-step run writes a verifiable ledger and does not claim success without evidence. |
| CHAT-004 | P0 | Mode changes operate on the **same** conversation, not a replacement thread. | Chat→Assist→Agent→Chat maintains IDs, messages and artifact links. |
| CHAT-005 | P0 | Each turn persists its resolved mode, model, tool policy and agent/persona metadata. | Reload/hydrate reproduces the state used for earlier turns, without guessing from current defaults. |
| CHAT-006 | P0 | Tool calls/results surface inline as structured entries, initially for shell, file, diff, browser, subagent and plan. | Event card shows input summary, running state, result, duration, evidence and error with detail disclosure. |
| CHAT-007 | P0 | The same composer accepts follow-ups while a run executes; messages are durably queued with explicit ordering. | Two quick follow-ups are each accepted once; no dropped/repeated turn on reconnect. |
| CHAT-008 | P0 | Stop is a persistent session/run control; distinguish stop request, accepted cancellation and already-completed side effects. | Stop from another device yields truthful terminal event; already-run shell side effects aren't reported as reversed. |
| CHAT-009 | P1 | Provide an explicit “Turn this into an agent task” action without restating context. | Promote a conversation objective to a run and link both IDs. |
| CHAT-010 | P1 | Support switching the selected model or specialist on a new turn, preserving history provenance. | New run uses chosen model; old turn retains its actual model ID/digest. |
| CHAT-011 | P1 | Let the operator fork conversation at a chosen event/turn with artifact references and lineage. | Fork preserves parent link; edits in fork do not modify parent timeline. |
| CHAT-012 | P2 | Compact successful tool traces by default while keeping original evidence available. | UI shows short result; inspector can open full persisted tool record. |

### 6.2 Durable conversation, event and run state

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| STATE-001 | P0 | Use a server-authoritative transactional store for conversations, messages, runs and ordered events. SQLite WAL is the baseline; existing JSON remains a migration input. | Restart after accepted request never loses or duplicates the durable prompt. |
| STATE-002 | P0 | Assign immutable `conversation_id`, `turn_id`, `message_id`, `run_id`, `event_id`, and monotonic per-conversation `sequence`. | Concurrent writers create unique IDs and gap-detectable ordered event sequence. |
| STATE-003 | P0 | Use idempotency keys for message submission and run creation, plus optimistic revision controls for edits. | Retrying the same accepted POST results in one user turn, never duplicate tool execution. |
| STATE-004 | P0 | Implement event replay with `after_seq` cursor, snapshot/replay and reconnect reconciliation. | Disconnect during a tool run; reconnect from phone to see missed events in order. |
| STATE-005 | P0 | Browser disconnect does not terminate server-owned run; server reboot marks uncertain active operations and rehydrates durable state. | Kill browser, then daemon; viewer recovery and run state are truthful. |
| STATE-006 | P0 | For logical restart-resume, checkpoint step dependencies, execution intent, idempotency and commits, not just final prose. | Resume verified idempotent step, while uncertain non-idempotent step pauses for operator review. |
| STATE-007 | P0 | Retain evidence/run ledger as the authoritative source of verified completion status. | A model message saying “finished” cannot override failed checks or missing tool evidence. |
| STATE-008 | P0 | Migrate existing V2 JSON sessions transactionally; preserve old data until validated. | Migration test on copied V2 fixture preserves content and IDs where available; rollback possible. |
| STATE-009 | P1 | Persist output artifacts content-addressed with hash, MIME type, size, owner, source run and retention policy. | Large output survives restart and can be downloaded/previewed via authenticated reference. |
| STATE-010 | P1 | Support explicit retention for temporary chats, logs, screenshots and artifacts, without accidental deletion of linked required evidence. | Cleanup respects TTL, active references, pins and exported/retained status. |
| STATE-011 | P1 | State changes are observable to multiple clients without trusting browser localStorage as authoritative. | Laptop+phone see the same added message/mode/run status without manual reload. |
| STATE-012 | P2 | Provide exports/imports for conversations and evidence in portable, versioned format. | Export then import into clean test store with checksums and event counts preserved. |

### 6.3 Native MCP integration and developer clients

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| MCP-001 | P0 | Build the MCP **server in Rust**, sharing core execution service, using maintained `rmcp` SDK after version pinning/testing. | Stdio initialize, tools/list, resources/read and tools/call pass integration tests. |
| MCP-002 | P0 | Expose a `delegate_task` operation that executes an actual LlamaFarm tool-capable run, not a plain completion proxy. | Claude invokes task that calls a permitted LlamaFarm tool; run ledger contains that call. |
| MCP-003 | P0 | Expose `get_run`, `cancel_run`, `list_agents`, `list_models`, `get_artifact` and `health` with scoped permissions. | Separate MCP client retrieves original run evidence and respects cancellation. |
| MCP-004 | P0 | `delegate_task` may return a short result or accepted asynchronous run handle. Long jobs must never require keeping a model response connection open. | 5-minute task returns handle promptly; another client polls/subscribes and receives result. |
| MCP-005 | P0 | Support stdio locally and authenticated MCP-over-HTTP remotely, pinned to documented protocol/client compatibility. | Claude Code stdio and remote client pass same operation/evidence tests. |
| MCP-006 | P0 | Bind each MCP client identity to authorization grants; MCP calls cannot bypass Tool/Shell policy or acquire a stronger mode implicitly. | Untrusted MCP client cannot request destructive shell even if agent instruction asks it to. |
| MCP-007 | P0 | Prevent nested delegation loops and cyclic worker calls with trace/depth/TTL, policy and cost budgets. | A→B→A job stops with a diagnosable cycle/budget error, no runaway tasks. |
| MCP-008 | P1 | Support continuing an existing conversation/run from another MCP client via explicit IDs and grants. | An IDE delegate can retrieve and continue authorized browser conversation without copying text. |
| MCP-009 | P1 | Provide first-party MCP client support as a tool provider in the existing registry, with capability discovery and policy filtering. | External MCP tool appears in catalog; only entitled tools reach agent prompt/execution. |
| MCP-010 | P1 | Provide tested configuration examples for Claude Code, Codex CLI and VS Code's supported integration paths. | Documented smoke tests pass on supported client versions. |
| MCP-011 | P2 | Add dedicated VS Code extension for editing/diffs, current repository, execution status and conversation handoff. | Open same conversation from extension; inspect a diff produced by browser run. |
| MCP-012 | P2 | Consider direct native OpenAI-compatible tool calling separately from MCP, with contract tests; do not falsely advertise unsupported `tools`. | Either tool-call fixtures pass or endpoint explicitly rejects unsupported shape. |

### 6.4 Unified workspace and UI

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| UX-001 | P0 | Default screen is conversation-first: title, compact model/node status, clear mode selector, messages and composer. | At 390px, critical controls remain usable without persistent horizontal overflow. |
| UX-002 | P0 | Primary navigation is 6–7 discoverable groups; preserve redirects/deep links for all V2 routes. | Route/deep-link matrix passes, including refresh. |
| UX-003 | P0 | On mobile, history and app navigation use accessible, dismissible drawers with thumb-reachable controls, focus trapping and Escape/backdrop dismissal. | 390x844 and 768x1024 browser tests pass for opening/closing history and navigation. |
| UX-004 | P0 | Agent progress is concise in chat, with optional expansion to step timeline, tool I/O, evidence and diagnostic details. | Default chat has no permanently expanded raw trace/federation banner; user can inspect all details. |
| UX-005 | P0 | Inline shell result shows working directory, command display, timing, exit status, sanitized output excerpt and full artifact link; risky commands show approval. | Shell succeeds/fails with matching status and expandable real stdout/stderr. |
| UX-006 | P0 | Inline file edit offers review diff, apply, reject and rollback where feasible. | Rejected edit never writes; applied edit records checksum and reversible patch if possible. |
| UX-007 | P1 | Inline browser result supports screenshot, URL, navigation/evidence and accessible preview. | Screenshot artifact accessible with keyboard and from other device. |
| UX-008 | P1 | Show subagent/worker status, selected model/node, queue and resulting artifacts as expandable activity. | Delegated run card displays actual worker, model and completion state. |
| UX-009 | P1 | Keep Monaco IDE as lazily loaded optional right workspace panel with responsive mobile full-screen presentation. | Main chat initial bundle doesn't load Monaco until requested. |
| UX-010 | P1 | Provide persistent terminal sessions with cancel, resize, working directory and safe disconnection semantics. | Active terminal can be reattached from authorized browser; processes have explicit lifecycle. |
| UX-011 | P1 | Composer supports attaching local files/project references, with privacy and file-size rules. | Attached file generates stored reference, readable by authorized tools with provenance. |
| UX-012 | P1 | Expose honest inference metrics on demand (real load/prefill/decode, not estimated tokens/second) and explain missing values. | UI matches backend measurements; no invented TPS when provider omits it. |
| UX-013 | P2 | Accessible light/dark/system themes and keyboard shortcuts with configurable high-density diagnostic view. | WCAG 2.2 AA test plan, keyboard-only flows, reduced-motion and screen-reader labels. |
| UX-014 | P2 | Surface one-click task-to-project promotion with durable workspace pointers and references. | New project opens original context and linked artifacts. |

### 6.5 Model scheduling, inference and parallel work

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| MODEL-001 | P0 | Discover actual GPU(s), free VRAM, resident models, engine config and node capabilities dynamically; never hardcode target GPU. | With V100 absent, scheduler never targets it; removing a GPU degrades gracefully. |
| MODEL-002 | P0 | Separate **resident model count** from **per-model parallel request count**, including context/KV memory admission. | Two resident models and parallel turns fit within profiled VRAM without surprise eviction loops. |
| MODEL-003 | P0 | Support multiple simultaneous independent conversations/jobs using bounded per-node/model queues. | Concurrent small requests stay responsive and receive deterministic queue admission outcomes. |
| MODEL-004 | P0 | Respect operator pinning, min-free-VRAM reserve, per-model context window, model digest and quantization. | Requests exceeding capacity queue/offload/fail honestly instead of silently changing the model. |
| MODEL-005 | P0 | Make selected chat model independent from subagent model and embedding model. | Qwen conversation delegates to TerminAI or worker model without changing chat selection. |
| MODEL-006 | P1 | Optimize cold/warm reuse and avoid embedding evicting primary chat model when spare memory/worker is available. | Warm-turn benchmarks demonstrate fewer reloads with unchanged output correctness. |
| MODEL-007 | P1 | Use measured task-route quality and latency for planner/coder/verifier/terminal specialist selection, with explicit fallback. | Versioned bakeoff controls promotions; failure triggers logged fallback. |
| MODEL-008 | P1 | Support node-aware multi-model federation routing using VRAM, queue, context and capability facts. | Busy-node request routes to eligible healthy node or queues with a reason. |
| MODEL-009 | P1 | Expose model load/unload/pull UI with progress, conflicts, model residency and consent for evictions. | Loaded models dashboard reflects real Ollama state after operations. |
| MODEL-010 | P2 | Evaluate optional different serving backends or speculative decoding only against equal-model, equal-context, quality-equivalent baselines. | Benchmark report includes quality, TTFT, tokens/s, VRAM, resource contention and reproducibility. |

### 6.6 Tool registry, specialists and evidence

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| TOOL-001 | P0 | Share one tool registry/authorization contract across browser chat, `/agent`, CLI, native MCP, scheduled jobs and subagents. | Same client policy yields same allow/deny result from every surface. |
| TOOL-002 | P0 | Advertise only relevant, entitled tool schemas per turn; preserve deterministic fallback. | Prompt contains selected tools only; omitted tools cannot be invoked. |
| TOOL-003 | P0 | Keep status distinctions **registered**, **configured**, **available**, **smoke-tested** and **verified**. | Offline external tool is not presented as operational merely because registered. |
| TOOL-004 | P0 | Preserve independent evidence records and deterministic verification; no model narration may mark a failed tool successful. | Inject fabricated “all tests passed” text; run remains failed or unverified. |
| TOOL-005 | P0 | Treat TerminAI native C++ as a **planning specialist**, not implicit command execution; validate schema, required args, and risk before approval/execution. | Empty argument and missing required path prevent command execution. |
| TOOL-006 | P1 | Tool outputs can attach typed artifacts with hashes, display adapters, truncation info and inspectable raw output. | Output over UI excerpt limit retains accessible full artifact. |
| TOOL-007 | P1 | Add per-tool cancellation/idempotency/compensation metadata. | Retry never silently repeats non-idempotent effect, and failure semantics are explicit. |
| TOOL-008 | P2 | Optional skills/recipes can be defined through typed declarative manifests with tested dependencies. | Skill install validates schema and can be disabled without breaking baseline tools. |

### 6.7 Federation and durable work orchestration

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| FED-001 | P0 | Persist remote work queue, task ID, owner, lease, heartbeat, attempts, idempotency key and terminal state. | Coordinator restart reconstructs pending/running/uncertain jobs. |
| FED-002 | P0 | Authenticate nodes and validate capability/policy at dispatch and at tool execution. | Forged peer cannot execute or read unauthorized run results. |
| FED-003 | P0 | Use at-least-once delivery with exactly-once **logical effect** for idempotent operations; uncertain side effects pause/reconcile. | Lost ack retry does not duplicate filesystem mutation. |
| FED-004 | P1 | Propagate cancel/stop and audit cancellation acknowledgement across network failures. | Cancel remote job; reconnect eventually shows acknowledged/stopped or explicitly uncertain state. |
| FED-005 | P1 | Support planner→specialist worker(s)→verifier DAG with bounded parallelism and no default fan-out beyond measured resources. | DAG verifier gates final completion after all dependencies provide evidence. |
| FED-006 | P2 | Disconnected workers retain outputs until reconciled; coordinator records artifact origin and integrity. | Worker reconnect performs checksummed artifact synchronization. |

### 6.8 Retrieval, memory and project context

| ID | Priority | Requirement | Acceptance test |
|---|---|---|---|
| MEM-001 | P0 | Preserve existing local retrieval and provenance; never mix unverified retrieved text with trusted system/tool permissions. | Prompt-injection fixture cannot change tool policy. |
| MEM-002 | P1 | Persist incremental git-aware indexes for workspace files, with exact file/line citation anchors and cache invalidation. | A changed source file becomes retrievable without rebuilding full corpus. |
| MEM-003 | P1 | Share bounded embedding cache for memory, tool search and document retrieval, without causing primary model eviction. | Duplicate query reuses same cached embedding, preserving source version. |
| MEM-004 | P1 | Link memory entries to provenance (`conversation`, `message`, `run`, `artifact`, timestamps and confidence). | UI opens source evidence and distinguishes stale facts. |
| MEM-005 | P2 | Provide dedupe/merge/expiry with preview and undo for memory operations. | Bulk operation preview and undo behave deterministically. |
| MEM-006 | P2 | Provide opt-in document inbox with citeable excerpts and cleanup control. | Imported document citations resolve to stored file/version. |

## 7. Non-functional requirements

| ID | Category | Requirement and measurement |
|---|---|---|
| NFR-001 | Local-first | Full Chat/Assist/Agent/Runs functionality must work without cloud credentials or internet, subject to local models/tools. |
| NFR-002 | Integrity | Every accepted write has transactional durability and replayable revision; migration verification by counts and checksums. |
| NFR-003 | Truthfulness | Never display `verified` without corresponding evidence; use `unverified`, `failed`, `cancelled`, `blocked` or `uncertain` accurately. |
| NFR-004 | Responsiveness | UI input and navigation remain interactive during long runs; no synchronous LLM inference on the web main thread. |
| NFR-005 | Mobile | Support 390x844 phone and 768x1024 tablet, with no persistent horizontal overflow and all primary actions keyboard/touch accessible. |
| NFR-006 | Availability | Graceful failure when one node/model is offline; no data loss or forced restart of healthy unrelated sessions. |
| NFR-007 | Secrets | Credentials never appear in run evidence, traces, browser localStorage, raw MCP tool descriptions or generated docs. |
| NFR-008 | Authorization | Deny by default for state-changing tools from external identities; verify permissions on server, not merely disabled client buttons. |
| NFR-009 | Observability | Record queue wait, model load, prefill/TTFT, prompt/decode tokens, TPS, context, cache behavior, tool timing, VRAM/RAM and worker ID when supported. Mark absent metrics missing. |
| NFR-010 | Backward compatibility | Existing V2 APIs/routes/JSON session imports preserved during migration; deprecations have explicit notices and tests. |
| NFR-011 | Data preservation | Upgrades preserve Ollama model and Qdrant volumes; automatic backup and rollback plan before destructive schema changes. |
| NFR-012 | Boundaries | Finite queues, tool execution limits, approval workflow and cancellation ensure no uncontrolled model recursion or GPU thrash. |
| NFR-013 | Performance gate | Measure P50/P95 chat TTFT, warm/cold request, p95 UI interaction, memory peak, artifact rendering and run-recovery latency against V2 before setting improvement claims. Target no material regression; desired >=20% p50 warm TTFT improvement only if repeatably demonstrated on identical hardware/model/context. |
| NFR-014 | Testability | Unit, property, protocol, E2E, GPU integration, fault-injection and visual/mobile tests; do not mark a test performed unless executed. |
| NFR-015 | Packaging | A single-node Ollama bundle remains the easiest deploy; advanced worker/MCP features are additive and optional. |

**Quantitative gates to finalize after measured V2 baseline:** p95 interaction latency, acceptable cold-model load time, supportable concurrent sessions per node, acceptable failure/reconnect times, minimum role-bakeoff success score, max resource utilization under steady state. Do not invent current production SLOs.

## 8. Primary workflows (must pass end-to-end)

### WF-01: Chat → Assist → Agent → Chat

Given an existing ordinary chat, user asks “show me the current failing tests.” Switch to Assist on same conversation. A read-only test run produces inline output. User asks “fix it and verify” and explicitly starts Agent. Agent writes reviewed changes, runs tests, persists evidence and yields verified/failed outcome. User returns to Chat; all transcript and artifact links survive reload and another device.

### WF-02: Claude Code asks LlamaFarm to work as a subagent

Claude invokes LlamaFarm MCP `delegate_task` with repo/workspace scope, intended role and permissions. LlamaFarm records client identity, conversation/run, resolves model/worker, executes allowed tools with evidence, returns accepted run handle for long tasks, and publishes status/results/artifacts. Claude can cancel and fetch evidence. Plain completion proxy **does not** satisfy WF-02.

### WF-03: Detached and restarted server

Start a long agent run; disconnect browser; open same conversation on mobile; run remains visible. Terminate service mid-step; restart. Reconstruct exactly which events were durably committed. A safe idempotent operation resumes or a non-idempotent uncertain action pauses; no fabricated successful completion or blind rerun.

### WF-04: Local multi-model specialist path

Large model remains the chat model; specialized small model or TerminAI gets a subtask. Scheduler checks actual VRAM, context/KV and queue, chooses eligible node or waits, preserves model selection and records route rationale. Terminal command is independently validated before shell execution.

### WF-05: Mobile UX

At 390x844, open history via a persistent discoverable control, switch to prior conversation, collapse history, send message, inspect one inline result, stop an active run and access artifact. No hidden tiny close button beneath a noisy banner, overlapping composer or squeezed content.

### WF-06: Safety, failure and evidence

Run deliberately incorrect command generation, tool timeouts, remote worker drop, invalid evidence, malicious prompt-injection file content and expired access token. Each case fails with appropriate status; no unauthorized tool action and no false `verified` state.

## 9. API and data contract requirements

**Versioning:** Use `/api/v3/...` for new surfaces, retain existing V2 routes as compatibility adapters. Version each event independently and include `schema_version`. Exact payload design in `V3_ARCHITECTURE.md` and JSON schema.

**Must-have routes** (illustrative design, not claimed to exist):

- `POST /api/v3/conversations` — new thread.
- `GET /api/v3/conversations` — paginated summaries, ordered by last activity.
- `GET /api/v3/conversations/{id}` — snapshot with revision and access rights.
- `POST /api/v3/conversations/{id}/turns` — idempotent turn admission, mode and policy.
- `GET /api/v3/conversations/{id}/events?after_seq=N` — ordered event replay.
- `GET /api/v3/conversations/{id}/stream` — authenticated SSE/live updates or equivalent WS subscription.
- `POST /api/v3/runs/{id}/cancel` — cooperative cancel request.
- `POST /api/v3/runs/{id}/resume` — safe checkpoint-aware resume.
- `GET /api/v3/runs/{id}` — status, steps, metrics and evidence links.
- `GET /api/v3/artifacts/{id}` — authenticated content metadata or stream.
- `GET /api/v3/workers`, `GET /api/v3/models` — residency, availability and scheduling facts.
- `POST /mcp` (under current transport contract), plus stdio MCP binary mode; never expose private unauthenticated network tools.

**Event classes:** `message.accepted`, `assistant.delta`, `assistant.completed`, `run.created`, `run.status`, `step.status`, `tool.started`, `tool.completed`, `artifact.created`, `approval.requested`, `approval.resolved`, `model.metrics`, `worker.status`, `error`, `session.snapshot`.

**Event invariants:** monotonic sequence per conversation, stable IDs, UTC timestamps, replay cursor, no hidden reasoning, redacted outputs, payload version validation, bounded payload sizes, artifact indirection for large outputs.

## 10. Permissions and operating modes

| Mode | Read-only tools | State-changing tools | Destructive tools | Autonomous continuation |
|---|---|---|---|---|
| Chat | None | None | None | No |
| Assist | Per client/space policy | Approval required | Denied by default | No multi-step unattended loop |
| Agent — safe | Approved allowlist | Approved workspace scope and policy | Explicit approval / isolated lab only | Yes, bounded by run/role policy |
| Agent — lab | Per disposable target policy | Per disposable target policy | Explicit lab profile, isolated target and audit | Yes, within configured scope |
| MCP delegated | Intersection of client grant, agent mode and workspace policy | Never wider than caller grant | Never silently escalated | Explicit role and grant only |

Security requirements also include authenticated HTTP MCP, Origin validation, DNS-rebinding defense, binding to loopback by default, token rotation, session scopes, CSRF protections for browser mutation, execution allowlists and output secret scrubbing. Tailscale alone is not the authorization layer. External MCP services and retrieved documents are untrusted input.

## 11. UI information architecture

**Primary nav target:** Chat (home), Workspace, Runs, Models/Workers, Knowledge, Tools/Integrations, Settings. Deep views like Cron, DB, Doctor, Logs, Federation, Prompts, File Explorer remain discoverable from grouped navigation and routes must continue to work.

**Chat layout desktop:** compact conversation list drawer; central transcript and composer; optional right panel for IDE, artifacts or run inspector. Show current model/node and mode in compact controls. **No mandatory federation/debug banner** at top of conversation.

**Phone:** app navigation and conversation history are distinct drawers; accessible toggle near main header and optional gesture, keyboard/focus, safe-area-aware bottom composer, large enough controls, no fixed-width sidebar or permanent IDE squeeze. Tool cards collapse automatically while retaining error/status signals.

**Inline cards:** Shell (exit/duration/stdout); Diff (apply/reject); Browser (screenshot/URL); Artifact (preview/download/hash); Plan (steps/evidence); Delegation (worker/model/queue); Approval (scope/risk); Diagnostics (opt-in). Render structured data—not model-formatted strings—when tools expose it.

## 12. Security and trust boundaries

User, browser, MCP client, federated worker, LLM, workspace file, retrieval source and shell are separate trust domains. All mutations pass Rust policy execution enforcement. Model text cannot change policy, grant scope or evidence status. Use scoped client identities, allowlisted workspaces, deny-by-default network tool endpoints, redaction, tamper-evident artifact hashes and independent verifier conditions. Never embed literal passwords or host secrets in check-in docs.

## 13. Delivery acceptance and definition of done

V3 release candidate requires:

1. **Correct baseline:** V2 promotion documented; V3 based on that verified commit; migrations reversible.
2. **Core workflow:** WF-01 fully passes with single `conversation_id` and linked run/artifact IDs.
3. **MCP:** WF-02 demonstrated from real Claude Code client and Codex client, not only simulated protocol handshake; successful permitted tool execution recorded.
4. **Recovery:** WF-03 demonstrates server detach and crash/restart, truthful uncertain effects, no duplicate committed side effects.
5. **Resource scheduling:** WF-04 benchmarks show correct eligibility and bounded parallelism on actual hardware.
6. **UX:** WF-05 passes mobile, tablet and desktop Playwright/browser checks; basic accessibility acceptance.
7. **Reliability:** WF-06 passes denial, fault injection and evidence integrity tests.
8. **Performance:** report actual before/after model digest, hardware, context, quantization and measured TTFT/TPS/queue/resource usage.
9. **Operational safety:** separate trusted-lab profile, no unauthorized remote MCP, preserved model/Qdrant data, verified backups.
10. **Docs:** API, setup, upgrade, client examples, recovery, rollback and limitations updated and runnable.

## 14. Major risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Competing V2/UX branches | Regressions or accidental lost changes | Compare and cherry-pick specific verified deltas, don't broad merge. |
| SQLite/session migration | Lost conversations | Read-only V2 snapshots, backups, migration dry run, checksums, rollback. |
| Run restart during shell side effect | Duplicate or unknown mutation | Intent/commit journaling, idempotency classification, explicit uncertain state. |
| MCP client privilege escalation | Remote shell misuse | Scope grants, per-tool runtime policy, authenticated transport, audit. |
| Model residency overcommit | Model thrash, OOM, high TTFT | Admission scheduler, context-aware VRAM accounting, dynamic inventory, queue. |
| Wrong claimed results | User trusts false output | Evidence-gated verifier with exact tool result IDs. |
| Tool schema overload | Slow, confused smaller models | Deterministic per-turn routing and benchmark gates. |
| UI redesign loses capability | Deep views become inaccessible | Route compatibility, grouped navigation, tested progressive disclosure. |
| Hardware uncertainty | Unsafe provisioning | Real device discovery and preflight, no assumptions about V100. |
| Protocol drift | MCP integration breaks | Pin/test SDK and clients; capability negotiation/version compliance. |

## 15. Priority cutline

**V3 MVP / P0:** Shared Rust execution contract; Chat/Assist/Agent policy; durable message/event store; replay/reconnect; real MCP stdio + HTTP delegation; honest run statuses; inline shell/diff/tool event cards; small-screen cleanup; actual resource-aware multi-session scheduling; permission boundary; crash-recovery correctness.

**V3 GA / P1:** Durable content-addressed artifacts; durable federation queue; model routing UX and measured bakeoff; richer browser/IDE preview; persistent terminal; workspace retrieval improvements; node-aware worker scheduler.

**Post-GA / P2:** VS Code native extension, advanced workflow templates, knowledge graph/semantic enhancements, rich export and theming. Do not allow P2 features to block core correctness.

## 16. Source references and audit status

See `V2_CODE_AUDIT.md` for line/file anchors. Relevant current project source: `src/gateway/mod.rs`, `src/gateway/ws.rs`, `src/gateway/openai_compat.rs`, `src/agent/run_ledger.rs`, `src/agent/tool_router.rs`, `src/tools/mod.rs`, `src/federation/mod.rs`, `web/src/pages/AgentChat.tsx`, `web/src/components/layout/Sidebar.tsx`, `TODO.md`.

External standards:
- MCP 2026-07-28 Streamable HTTP: https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2026-07-28/basic/transports/streamable-http.mdx
- Official Rust SDK: https://github.com/modelcontextprotocol/rust-sdk
- Ollama FAQ for model loading and parallelism: https://github.com/ollama/ollama/blob/main/docs/faq.mdx

**Truthfulness note:** This is a requirements specification, not an assertion of new V3 implementation or tests. Acceptance criteria are future gates. The review was read-only; no Docker/live run or GitHub write succeeded.
