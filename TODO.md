# LlamaFarm platform backlog

This is the durable implementation backlog for the two-node, local-first
LlamaFarm deployment.  Keep this file current when an operator asks for a
feature; do not rely on an agent's transient chat context as the record of
requested work.

## Non-negotiable operating constraints

- [x] Run fully locally and free by default.  Keep Ollama as the baseline;
  only add another serving backend when local evaluations demonstrate a real
  advantage.
- [x] Keep models and Qdrant data volumes intact across redeploys.
- [x] Browser pairing is permanently disabled for this private LAN deployment.
- [x] Support distinct hardware profiles: 8 GB RTX 4070 Laptop and 16 GB RTX
  5070 Ti, each with create-time Docker GPU/resource limits.
- [x] Document deployment, verification, recovery, and acceptance tests in
  the repository for the next redeploy/operator.

## Delivered in the two-node foundation pass

- [ ] Make run-and-forget `test all tools` jobs durable and evidence-based:
  retain the autonomous executable plan, checkpoint every result, classify
  prerequisites/side effects, use disposable fixtures where appropriate, and
  end with a truthful capability matrix rather than model claims.  A quick
  non-destructive preflight is complementary, never a substitute for the
  operator-requested integration run.
- [x] Show live operational progress in the Agent UI, with an expanded by
  default, collapsible live-output panel.  Do not expose hidden reasoning;
  show useful execution state, tools, artifacts, and final answer instead.
- [x] Fix capability/status questions so they do not trigger a false
  “deferred action without a tool call” follow-through retry.
- [ ] Re-run the UI acceptance matrix on both machines after the current
  agent runs finish: chat, tool registry, file write/read, code execution,
  web search, local Qdrant RAG, and federation delegation.

## In progress — evidence-gated run ledger pass (2026-07-16)

State: full `cargo test --lib` suite passes (4187/4187, was 18 failing), web
production build passes. Remaining: laptop redeploy + browser verify; gpu box
redeploy queued until back on the LAN.

Also fixed in this pass:

- Gateway pairing is now hard dead: removed the `LLAMAFARM_REQUIRE_PAIRING`
  env shim and its startup warning, the compose env vars that triggered it,
  the "(browser pairing removed)" banner text, and the `paired`/
  `require_pairing` fields from `/health`. Remaining internal cleanup is
  cosmetic and queued below: rename `PairingGuard` (still used for Telegram
  channel device auth — a different feature), drop the ignored
  `gateway.require_pairing` serde field, and prune unused `auth.pairing_*`
  web i18n strings.
- Removed SOUL.md (repo + workspace). Proof it was dead weight: the prompt
  builder injects only workspace `AGENTS.md` (`src/agent/prompt.rs`) and its
  tests assert SOUL.md content is NOT loaded. The runtime workspace
  `AGENTS.md` (6.5 KB ≈ 1.6k tokens/request) remains the single persona/
  operating-rules file; keep it lean.

- Restored the argument-level command guard the wildcard allowlist comment
  promises: `is_args_safe` now blocks unsafe `rm`/`trash` deletes (via
  `is_safe_delete_command`) at every autonomy level, `find -exec/-ok/-delete`,
  and `git config`/`-c`/alias command injection; `sudo` requires the
  full-autonomy profile (its non-interactive hard-denial gate still applies).
  A single unquoted `&` now splits validation segments so `ls & rm -rf /`
  cannot hide behind `ls` while background jobs stay allowed; `2>&1` redirect
  syntax is preserved.
- Updated stale tests to the v2 wildcard-default contract (curl/python/node
  are operator tools; custom allowlists still exclude them), the delegate
  gating flag (`federation.enable_delegation`), the duplicate-call nudge
  streak, the malformed-payload format-correction prompt, and per-iteration
  argument variation in the channel max-iterations test.

Delivered (all tests green):

- `src/agent/run_ledger.rs` — durable planner→executor→verifier records:
  per-run JSON ledger at `<workspace>/state/runs/<run_id>.ledger.json` with
  plan steps (allowed_tools, depends_on, expected_evidence), scrubbed tool
  events (args summary, output digest+excerpt, duration, artifacts), a
  deterministic verifier (completed steps need ≥1 successful evidence event,
  matched expected-evidence patterns, verified dependencies), task-local
  `RUN_LEDGER` scope, live-run registry, and unit tests (written, not run).
- `src/agent/loop_.rs` — every executed tool call is recorded into the
  in-scope ledger; `task_plan` create/add/update/delete calls are mirrored
  into durable plan records.
- `src/agent/autonomous.rs` — completion is now evidence-gated: model prose
  alone cannot complete a run whose ledger has unresolved/unverified plan
  steps; targeted "Verification Required" retry prompt; ledger finalized on
  every terminal path (Completed / CompletedUnverified / Failed / Cancelled)
  with attempts + retry reason.
- `src/gateway/ws.rs` — webchat turns scope a per-session ledger
  (`session-<id>`), finalized from the turn result.
- `src/gateway/api.rs` + `mod.rs` — run inspector API:
  `GET /api/runs` (live + historical index), `GET /api/runs/{run_id}`
  (full plan/evidence/timeline snapshot).

Remaining to finish this pass, in order:

- [x] Full `cargo test --lib` green (4187 passed; fixed 18 failures).
- [x] Run inspector UI: `web/src/pages/Runs.tsx` + `/runs` route + sidebar
  nav item (plan table with verified/verifier_note, tool timeline with
  duration + excerpt + artifacts, status/attempts/retry reason, live badge,
  5s auto-refresh).
- [x] `npm run build` in `web/` passes.
- [ ] Redeploy THIS laptop via `scripts/docker/up-node.sh rtx4070-laptop up -d
  --build`; verify `/api/health` and `/api/runs` locally, then E2E: create a
  plan via chat, watch evidence attach in the Runs page. The gpu box
  (192.168.1.154) is unreachable off-WLAN — redeploy it next time the laptop
  is back on the LAN (`git pull` + same script with `rtx5070ti-16gb`).
- [ ] Then continue the backlog below (context capsules + final acceptance
  pass are partially covered by the evidence gate; run inspector resume
  controls, token-budget allocator, federation durable queue, workspace RAG,
  eval suite/router, credential broker UI remain). Next Rust pass should also
  send real `InferenceMetrics` (generation_tps/ttft_ms) over the chat
  websocket for the UI TPS indicator (10.3).

## Next-gen "dangerously good" batch (operator request, 2026-07-17)

My honest thoughts on each idea, with a plan:

- [~] **Authorized-lab security toolkit** (started): opt-in Docker build arg
  `LLAMAFARM_LAB_TOOLS=1` adds nmap, tshark/tcpdump, sqlmap, hydra, nikto,
  john, hashcat, etc. — off by default to keep the image lean. Verdict:
  YES, but framed for the operator's OWN authorized lab (this repo's stated
  chaos_lab / ethical-hacking / disposable-target mission). Keep it a build
  flag, never default; document the "own systems / permission only" boundary.
  Do NOT add mass-targeting, DoS, or self-propagating capabilities.
- [ ] **Wireshark/packet capture in the agent**: expose tshark via a
  `packet_capture` tool (bounded duration + packet count, capture to a
  workspace artifact) so the agent can do real network analysis on the lab.
- [ ] **Discord channel**: LlamaFarm already has telegram/matrix/whatsapp/
  etc. channels; add Discord (bot gateway) as another remote-control surface.
  Verdict: YES, straightforward and genuinely useful for driving the node
  from your phone. Medium effort (new channel impl + config).
- [ ] **Friendly Tailscale setup**: a Connections card that takes the auth
  key, brings the `vpn` profile up, and shows the node's tailnet IP + status
  — turns the compose profile into one-click. Verdict: YES, small.
- [ ] **DroneDetect one-click**: now that authenticated `git clone` works, a
  "workspace project" flow that clones loomissamk/DroneDetect, sets up its
  venv (code_run/python), and runs inference on a sample. Verdict: GREAT
  showcase of the whole platform (git + code_run + RAG + run ledger) on your
  own real repo. Generalize to "bootstrap any of my repos as a project."
- [ ] **Auto-connect passwordless DBs on scan** (earlier request): when the
  network scan finds an unauthenticated redis/mongo/pg/etc., offer one-click
  add with schema auto-import. Keep it opt-in per host (never silent), since
  auto-connecting to found services is powerful.

## Next-generation agent workflow

- [x] Add first-class **follow-ups**.  A user message can attach to, amend,
  reprioritize, or cancel an active run without losing its verified steps,
  artifacts, or context. The initial implementation queues the message against
  the active session, checkpoints/cancels the current segment, and starts the
  follow-up immediately. Durable reconnect/restart resume remains part of the
  run-inspector work below.
- [x] Add a real **Stop** control to the Agent UI.  `cancel` is scoped to the
  active chat session, interrupts the cancellation-aware model/tool loop,
  forwards cancellation to accepted federation tasks, reports a clean terminal
  state, retains completed tool evidence, and leaves the next message as an
  intentional follow-up.  See `src/gateway/ws.rs`,
  `src/federation/remote_subagent.rs`, and `web/src/pages/AgentChat.tsx`.
- [ ] Persist planner → executor → verifier records.  Each step needs allowed
  tools, dependencies, expected evidence, artifact paths/hashes, and a
  deterministic verifier.  “Done” must require evidence, not model prose.
- [ ] Make every plan item a task-local context capsule: root goal, item
  acceptance criteria, dependency summaries, relevant artifacts, token/GPU
  budget, and nothing else.  Archive raw tool output to the evidence ledger,
  compact it after verifier success, then run an explicit final acceptance pass
  across the original plan before declaring an unattended run complete.
- [ ] Add a run inspector: plan/state, current model/node, GPU queue,
  context budget, tool timeline, artifact links, elapsed time, retry reason,
  cancel, and resume controls.
- [ ] Make federation a durable queue with idempotency keys, leases,
  cancellation propagation, reconnection/resume, and resource-aware routing.
- [ ] Schedule whole subproblems, not one inference split across unequal GPUs:
  4070 for responsive chat/light tools and 5070 Ti for deeper coding,
  long-context, RAG, and batch evaluation.
- [ ] Add a token-budget allocator and layered memory/project evidence ledger.
  Reserve context for instructions, tools, current task, retrieved evidence,
  and response; summarize old tool output into cited artifact references.
- [x] Enforce per-segment generation/reasoning checkpoints. Preserve deep
  reasoning and autonomous continuation, but checkpoint after a bounded tool
  decision rather than allowing one hidden-thinking segment to monopolize the
  node. Local Ollama inference has no response wall-clock deadline and a
  length-stopped segment continues automatically until a real terminal state
  or an operator presses Stop. Adaptive allocation remains future work.

## Deploy status (2026-07-16)

Pushed to `v2` and code-complete, DEPLOYED & E2E-verified on the laptop:
run ledger + inspector, cross-session memory, playbooks, workspace RAG inbox,
semantic RAG (Ollama embeddings + hybrid fusion + embedding cache), live
TTFT/TPS metrics, code_run, git_worktree, one-place history clear, UI
consolidation (7 tabs), auto-rollback deploys.

Pushed but NOT YET deployed (image rebuild blocked by a Docker Hub outage —
`DeadlineExceeded` pulling the `node:22-bookworm-slim` base image, which is not
cached locally — node stayed healthy on the previous image, no
outage): the **MMR reranker** (63a4abf8). It will land on the next successful
`up-node.sh rtx4070-laptop up -d --build`, or when back on the home LAN.
The deploy script now reports this cleanly (exit 3 = container not swapped,
previous image still healthy) instead of failing silently.

gpu box (192.168.1.154): catch up with `git pull` + `up-node.sh
rtx5070ti-16gb up -d --build` next time on the LAN — it inherits everything.

## Operator directive — UI consolidation + cutting-edge focus (2026-07-16)

The web UI has too many redundant tabs. Operator mandate: do not stop until
the UI is streamlined, best-in-class UX, only what needs to exist. v1 done:
sidebar slimmed 13→8 tabs (Runs/Memory/Logs/Doctor now Dashboard chips),
memory clear scope=all also purges non-live run ledgers/traces. Continue:

- [x] Doctor folded into the Dashboard (DiagnosticsPanel, 2026-07-16).
- [x] Logs live on the Dashboard (LogsPanel embedded, 2026-07-16).
- [x] Memory folded into the Database page (2026-07-16).
- [ ] Replace the **Runs** tab with a compact "recent runs + live badge"
  card on the Dashboard linking to run detail, and surface the active run's
  plan/evidence inline in Agent Chat where it belongs.
- [x] Integrations merged into the Config page; Integrations tab removed
  (2026-07-16). Moving common knobs onto the Dashboard remains open.
- Battery-aware scheduling is dropped — the platform targets big boxes.
- [x] One-place history clearing (2026-07-16): POST /api/history/clear +
  "Clear all history" button in the chat sessions pane — deletes all memory
  entries, non-live run ledgers/traces, the global runtime trace, and
  persisted chat sessions, AND the in-memory runtime log buffer,
  reporting bytes freed. Run records, chat history, and logs are managed
  together, as requested.
- [x] Resizable workspace terminal (2026-07-16): drag handle on the IDE
  terminal panel, height persisted per browser. Scrollback/theme polish
  remains open.

Execution environments (operator request, 2026-07-16): the bundle image
ALREADY ships python3+pip, nodejs, gcc/g++ (build-essential), cmake, go,
java (headless JDK), cargo/rustc, git, docker CLI, jq, chromium — Node and
C++ basics are covered. What was missing is structured use of them:

- [x] **code_run tool** (2026-07-16): write→compile→run for python,
  javascript, typescript, c, cpp, go, rust, bash in a disposable
  `<workspace>/.code_run/` dir with timeout, stdin support, and captured
  stdout/stderr/exit. Registered with the filesystem tool set.
- [ ] Per-workspace persistent environments: a `.venv` exists for Python —
  add the equivalent for Node (workspace `package.json` + node_modules) and
  a build cache dir for C++/Rust so repeated compiles are fast.
- [ ] Toolchain capability report: extend the tools registry/doctor so the
  agent knows which compilers and versions are actually present (evidence,
  not assumption) before promising builds.
- [ ] Consider adding: sqlite3 CLI, ripgrep, shellcheck, valgrind, gdb to
  the image for debugging-grade power (small, high leverage). GPU CUDA
  toolkit stays out — too heavy for the bundle.

Cutting-edge agentic/throughput priorities (operator request):

- [x] Better RAG v1 (2026-07-16): OllamaEmbedding provider (`/api/embeddings`)
  added to the embedding factory ("ollama" / "ollama:URL"); workspace_rag
  embeds chunks lazily and fuses BM25 + vector ranks (RRF) when
  `memory.embedding_provider` is configured (verified E2E: a paraphrase
  query with no shared keywords retrieved the right doc). Content-hash
  embedding cache added so reindex reuses vectors for unchanged chunks.
  Local reranker added (2026-07-16): lexical MMR (Maximal Marginal Relevance, Jaccard diversity) reranks over-fetched results down to the requested limit so passages are relevant AND non-redundant. Remaining: persist the embedding cache across restarts; optional cross-encoder rerank.
- [~] Better coding/execution: disposable worktrees delivered (2026-07-16) —
  `git_worktree` tool (create/list/adopt/discard) gives isolated
  adopt-or-discard scratch space under `<workspace>/.worktrees/` so risky
  refactors never leave the operator's checkout broken. Paired with
  `code_run` and the run ledger. The workspace must be a git repo for worktrees to apply (unit tests cover the git mechanics; deployed tool is registered and invocable). Remaining: an operator workspace-repo-init affordance, and wire the existing
  `RepoWorkflowAgent` (explore→patch→build→verify) to a chat entry point so
  the full loop runs end-to-end from a single request.
- [ ] More autonomy: default plan-execute-verify for multi-step webchat
  tasks, auto-continue across segments, chaos-style recovery on tool errors.
- [ ] Throughput: keep_alive=-1 pinned chat model, byte-stable system-prompt
  prefix for Ollama prefix-cache hits, measure with the live ttft/tps
  metrics now in the UI.

## Global access + friendly settings (operator request, 2026-07-16)

### Internal VPN — connect to your nodes from anywhere

Recommended: **Tailscale sidecar** (WireGuard under the hood, free for
personal use, NAT/CGNAT traversal with no port-forwarding, no public
exposure). LlamaFarm already has a `[tunnel] provider = "tailscale"` config
hook and refuses public bind without one, so this fits the existing design.

- [x] Optional `tailscale` service in `docker-compose.bundle.yml`
  (2026-07-16, `--profile vpn`, verified absent from default deploys)
  (image `tailscale/tailscale`, `TS_AUTHKEY` from the node env file,
  `TS_STATE_DIR` on a persistent volume, `--advertise-tags`), with the
  LlamaFarm container joining it via `network_mode: service:tailscale`.
  Gated behind a compose profile so it stays strictly opt-in.
- [ ] Both nodes then get stable `100.x` tailnet IPs reachable worldwide:
  federation peers use tailnet IPs instead of `192.168.1.x`, so the two-node
  setup keeps working off-LAN (this also unblocks the gpu box redeploy from
  anywhere).
- [ ] Enable Tailscale Serve/Funnel optionally for HTTPS access from a phone
  without exposing the LAN; keep `allow_public_bind = false`.
- [ ] Alternative documented for reference: a self-hosted WireGuard
  container (full control, but requires a port-forward + static endpoint —
  worse for laptops that roam). Do NOT expose 42617 to the internet directly.
- [ ] Security: tailnet ACLs restricting who can reach 42617; keep the
  federation shared token as defence-in-depth.

### Friendly settings UI (Copilot-style connections)

- [x] Connections cards on the Settings page (2026-07-16): live state for
  GitHub / Ollama / Memory above the raw TOML. Remaining cards: Qdrant,
  Federation peers, Tailscale.
- [x] GitHub connection card (2026-07-16): "Connect GitHub" runs the
  **OAuth device flow** — the UI shows a user code and an
  `https://github.com/login/device` link to click, polls for completion, then
  stores the token owner-only in the node volume and shows the connected
  account + scopes. No token pasting.  (`src/auth/` already implements this
  pattern for other providers — reuse `oauth_common`.)
- [x] Token stored owner-only (0600) and brokered via
  `github_device::brokered_token`; never in model context, tool output,
  traces, memory, or browser storage (unit-tested). Remaining: wire the
  broker into the git_operations/git_worktree push paths.
- [ ] Surface repo capability state (can clone / push / open PRs) as
  evidence, so the agent never claims git powers it lacks.
- [ ] Move the common knobs (model, provider, autonomy level) onto the
  Dashboard as inline controls; keep advanced TOML behind a disclosure.

## Next-gen RAG and speed ideas (operator request, 2026-07-16)

Storage decision: no MongoDB. Local-first stack stays files (source of truth)
+ Qdrant (vectors) + SQLite (metadata/sessions). Adding Mongo would add an
extra always-on service with no capability the current stack lacks.

- [x] **Cross-session chat memory** (delivered 2026-07-16, verified E2E:
  a fact taught in one session was recalled in a brand-new session).
  Webchat turns recall relevant memories across all sessions via the
  configured backend and inject them as cited context; completed turns
  persist compact Q/A exchange records. Currently keyword-based on the
  node's sqlite backend — switch `[memory] backend = "qdrant"` in the node
  config for semantic recall (same code path). Remaining upgrade: also
  embed tool-result summaries and add "solved on <date> in session X"
  citations.
- [x] **Drop-a-document RAG inbox** (delivered 2026-07-16, verified E2E: a
  fact from a dropped file was retrieved and cited by source+section).
  `workspace_rag` tool over `<workspace>/rag/inbox/`: text/markdown/code/log
  files are indexed automatically (lazy fingerprint-based rebuild; deletions
  drop out), BM25 retrieval with `[Source N]` citations. Follow-ups: Files
  page upload button targeting the inbox, PDF parsing, and vector fusion via
  local Ollama embeddings for semantic matching.
- [x] **Run-ledger RAG** (initial, 2026-07-16). When a webchat run finishes
  with a fully verified plan, a compact playbook record (task, steps, tools
  used per step) is stored to long-term memory and surfaces through
  cross-session recall on similar future tasks. Follow-up: same hook for
  autonomous (non-webchat) runs, and semantic matching once the memory
  backend is switched to Qdrant.
- [ ] **Embedding cache.** Key embeddings by content SHA-256; never re-embed
  unchanged chunks on reindex. Makes the inbox/reindex path near-instant.
- [ ] **Semantic tool-result reuse.** Before expensive read-only tools
  (web_fetch, large file_read), check Qdrant for a semantically-equivalent
  cached result within TTL — complements the exact-hash tool cache.
- [ ] **Prefix-cache-friendly prompts.** Keep the system prompt byte-stable
  across turns (stable tool ordering, no timestamps) so Ollama's prefix cache
  hits; pin the chat model with `keep_alive=-1`. Measure TTFT before/after
  with the existing `InferenceMetrics` and surface `generation_tps`/`ttft_ms`
  in the chat UI (already a 10.3 TODO).
- [ ] **Small-model fast lane.** Route trivial turns (greetings, status
  questions, plan updates) to a small always-loaded model on the 4070 and
  escalate to the big model only when the router says so — perceived latency
  drops without losing depth.

### More next-gen agent ideas (2026-07-16)

- [ ] **Self-authored skills (procedural memory).** After a successful novel
  multi-step run, have the agent distill the workflow into a workspace skill
  markdown file (same loader as SOUL.md/AGENT.md) with a link back to the
  source run ledger as evidence. The agent literally gets better at tasks it
  has done once — the most "next-gen" capability on this list.
- [ ] **Memory distillation.** Periodic job that summarizes chat + tool
  history into durable long-term facts (markdown in the workspace, embedded
  into Qdrant), each fact carrying provenance links to run ledgers. Old raw
  history can then be aggressively compacted without losing knowledge.
- [ ] **Nightly self-maintenance cron.** Refresh RAG indexes, re-run the
  model bakeoff, run the eval suite in a disposable repo, and post a morning
  report to the dashboard and conversation memory. The platform maintains
  itself while idle.
- [ ] **Independent verifier model.** A small local model cross-examines the
  executor's completion claims against ledger evidence before a run may
  complete — an LLM judge layered on top of the deterministic evidence gate,
  cheap to run on the 4070.
- [ ] **Patch review lane.** Proposed diffs surfaced in the Workspace IDE
  with apply/rollback buttons, executed in disposable git worktrees per run
  so the operator can adopt or discard agent changes atomically.
- [x] **Auto-rollback deploys** (2026-07-16). `up-bundle.sh` now snapshots
  the running image as `llamafarm-local:last-green` before any healthy-node
  `up`, health-gates the new deploy (`LLAMAFARM_HEALTH_TIMEOUT`, default
  180s), and automatically retags + recreates from last-green when the new
  build never becomes healthy (exit 2 = rolled back, exit 1 = manual).
  Applies to both nodes since they share the deploy scripts.
- [ ] **Run cost accounting.** Per-run token counts, GPU seconds, TTFT and
  generation TPS aggregated into the run inspector (extends the existing
  `InferenceMetrics`), so routing decisions can be justified with data.
- [ ] **Cross-node knowledge sync.** Replicate `rag/inbox` documents,
  playbook memories, and long-term facts between the two nodes over the
  existing federation channel (idempotency-keyed, newest-wins), so a fact
  taught to the laptop is recallable on the 5070 Ti box and vice versa.
  Highest-value two-node feature on this list.
- [ ] **Local SRE watch mode.** A cron-driven watcher tails journald/docker
  events for crashloops, OOM kills, and disk pressure, and opens an agent
  session with the evidence attached — the operator wakes up to a diagnosed
  incident, not a dead service. Uses existing service_control/process tools.
- [ ] **Model auto-promotion.** Nightly check for newer tags of installed
  Ollama models, run the eval suite against the deployed baseline in a
  disposable workspace, and promote only on measured wins — with the
  last-green rollback pattern applied to model routing.
- [ ] **Voice lane.** Local whisper.cpp STT + Piper TTS behind the gateway
  for hands-free operator chat on LAN devices; push-to-talk in the web UI.
- [ ] **Fork/replay in the run inspector.** `AutonomousLoop::fork()` exists;
  expose fork-from-checkpoint and step replay in the Runs page so chaos
  experiments can branch A/B recovery strategies visually.

- [ ] **Local browser automation lane.** CDP/Playwright-driven browser tool
  with screenshots streamed into the chat work panel — real web operation,
  not just fetch-and-parse.

## Local IDE, data, and model capabilities

- [ ] Build workspace RAG: local Ollama embeddings, per-workspace Qdrant
  collections, incremental git-aware indexing, lexical+vector fusion,
  optional local reranking, and exact file/line citations.
- [ ] Add a versioned local eval suite in disposable repos: tool calls,
  code changes plus tests, recovery, web citations, RAG grounding, memory
  resume, federation retry, and long-run completion.  Promote models/prompts
  only when they beat the deployed baseline.
- [ ] Benchmark an optional llama.cpp server lane for models too large for one
  GPU.  Prefer it for explicit unequal-VRAM tensor splitting; only benchmark
  vLLM/SGLang on the 5070 Ti when throughput/prefix-cache measurements justify
  their additional complexity.
- [ ] Evaluate local model roles with the test suite: small fast tool router on
  the 4070, Qwen 9B main agent on the 5070 Ti, and optional larger coding or
  reasoning models only when they leave practical KV-cache headroom.

## Operator experience and safety boundaries

- [ ] Add an **Operator Integrations** page for Git identity and credentials.
  Support local SSH-key selection and scoped GitHub/GitLab access tokens stored
  owner-only in the persistent node volume.  Broker credentials only to Git
  operations; never expose a raw secret in model context, tool output, traces,
  memory, or browser storage.  Surface authenticated/repository capability
  state so the agent can clone, branch, commit, push, and create reviews using
  the operator's configured scopes.
- [ ] Preserve unrestricted operator capability while running routine browser,
  RAG, and coding jobs in isolated disposable worktrees/containers with
  explicit resource limits.  Expose privileged actions as deliberate operator
  runs rather than pretending every registered tool is usable everywhere.
- [ ] Make capability reports distinguish registered, configured, verified,
  unavailable, and intentionally non-destructive tools so the agent never
  falsely claims Docker, git, systemd, Pushover, cron, or similar integrations
  are functional without evidence.

## Baseline implementation backlog (preserved)

## Mission

Turn LlamaFarm into an Ollama-only, local-first, high-autonomy agentic RAG and machine-operator system for disposable lab environments.

This build is for:
- sandbox VMs
- disposable containers
- throwaway lab machines
- reimageable test hosts
- isolated hardware boxes intended for destructive experimentation

This build is NOT for:
- shared systems
- production machines
- third-party infrastructure
- anything requiring human-safe guardrails

LlamaFarm must:
- use Ollama only
- be model-agnostic across Ollama pulls/tags/families
- run fully local
- act autonomously without per-step approval
- plan, retrieve, edit, execute, break, recover, retry, and iterate on its own
- operate as a true machine operator, not a passive chatbot
- support destructive experimentation inside the disposable target environment

Non-goals:
- no cloud model providers
- no hosted inference
- no human-in-the-loop confirmations
- no “assistant mode” safety friction
- no fake agent loops
- no fake RAG
- no provider abstraction beyond Ollama

## Product Definition

LlamaFarm is:
- an Ollama-native autonomous operator
- an agentic RAG runtime
- a local tool execution engine
- a local indexing and memory system
- a lab automation and chaos-testing system
- a self-directed repo and machine manipulation agent

The system should be able to:
- inspect a machine
- reason about the machine
- search local knowledge
- edit files
- run commands
- install/remove packages
- restart/kill services
- mutate configs
- benchmark models
- break the environment
- attempt recovery
- keep trying until timeout, success, or hard stop

## Core Rules

### 1. Ollama only
All inference must go through Ollama.
No other providers.
No cloud fallback.
No external dependency for core operation.

### 2. Model-agnostic within Ollama
Do not hardcode for one model.
Support any locally pulled Ollama model.
Route by measured capability, not by assumptions.

### 3. Autonomy first
No per-step confirmation prompts.
The agent may:
- inspect
- decide
- execute
- revise plan
- retry
- chain tools
- modify state
- continue until objective complete or runtime stops

### 4. Disposable target assumption
The target environment is assumed expendable.
The agent may intentionally or unintentionally damage the target state.
This is acceptable by design.

### 5. Full observability
Every run must record:
- selected model
- prompts
- retrieval hits
- tool decisions
- commands executed
- files changed
- packages changed
- services changed
- logs captured
- errors
- retries
- final status

## Priority 1 — Ollama Runtime

### 1.1 Build a strict Ollama runtime layer
Implement:
- chat
- streaming
- embeddings
- model list
- model pull
- tag/alias support
- context size metadata
- JSON/structured output handling
- vision capability detection where available

### 1.2 Build an Ollama capability registry
Track per installed model:
- name
- tag
- family
- context window
- tool-use reliability
- JSON compliance
- coding ability
- RAG synthesis quality
- long-context behavior
- latency
- RAM/VRAM footprint
- failure patterns

### 1.3 Multi-model routing
Allow separate models for:
- planner
- executor
- coder
- summarizer
- embeddings
- fallback fast model
- long-context synthesis model

### 1.4 Automatic model bakeoff
Benchmark all installed Ollama models for:
- shell reliability
- file editing reliability
- JSON discipline
- repo understanding
- RAG answer quality
- citation faithfulness
- autonomous task completion
- recovery after failure

Use scores to auto-pick the best model for each task type.

## Priority 2 — Tooling That Works Even When The Model Is Sloppy

### 2.1 Tool intent normalization
Implement a parser pipeline:
1. native tool-call parse
2. JSON parse
3. structured pseudo-tool parse
4. fenced shell/code parse
5. command-intent recovery
6. prose-intent recovery
7. plain-answer fallback

### 2.2 Deterministic command routing
If the model outputs:
- a shell command
- structured JSON
- a pseudo-tool block
- a code fence clearly containing executable intent

then LlamaFarm should route it into the correct tool automatically.

### 2.3 Tool executor
Implement:
- shell
- file read/write/edit
- search/grep
- git
- docker/podman
- package manager control
- process inspection/control
- service control
- local Python runner
- document parser
- indexer
- scheduler
- browser/web fetch if enabled

### 2.4 Tool chaining
Support long autonomous chains:
- observe
- retrieve
- plan
- act
- verify
- retry
- recover
- summarize

### 2.5 Runtime controls
Do not ask permission mid-run.
Do implement:
- timeout
- retry budget
- loop detection
- process kill on stop
- task boundary
- workspace boundary
- optional wall-clock cap

## Priority 3 — Real Agentic RAG

### 3.1 Build a full retrieval pipeline
Stages:
1. ingest
2. normalize
3. chunk
4. embed
5. index
6. lexical retrieval
7. vector retrieval
8. hybrid merge
9. rerank or score refinement
10. context pack
11. answer with citations

### 3.2 Supported corpora
Index:
- repos
- markdown docs
- PDFs
- text notes
- logs
- configs
- saved outputs
- prior run history
- local knowledge bases

### 3.3 Hybrid retrieval
Use:
- keyword/BM25
- vector search
- metadata filters
- recency weighting
- source weighting

### 3.4 Context composers
Build specialized context packaging for:
- coding
- debugging
- system diagnosis
- repo synthesis
- log forensics
- long-form writeups
- timeline reconstruction

### 3.5 Persistent memory
Create:
- task memory
- session memory
- long-term project memory
- machine memory
- retrieval cache
- tool-result cache

## Priority 4 — Chaos / Break / Recover Mode

### 4.1 Add execution modes
Implement:
- chat
- operator
- autonomous_operator
- chaos_lab

### 4.2 chaos_lab mode behavior
In chaos_lab mode the agent may:
- install or remove packages
- rewrite configs
- mutate repo state
- kill or restart services
- fill disk up to configured quota
- create and delete files recursively
- downgrade/upgrade packages
- intentionally stress the environment
- attempt self-healing
- repeatedly try alternate fixes

### 4.3 Recovery loops
The agent should be able to:
- detect failure
- inspect logs
- form hypotheses
- try fixes
- compare outcomes
- rollback if possible
- continue until resolved or timed out

### 4.4 Failure experiments
Support tasks like:
- break service then repair it
- mutate config until failure then isolate root cause
- upgrade dependencies until app dies then recover
- hammer a repo with edits then restore it
- stress disk, memory, process, and service behavior
- benchmark self-repair strategies

## Priority 5 — Machine Operator Experience

### 5.1 It must operate like a real local operator
The agent should:
- inspect machine state
- read logs
- analyze repos
- patch code
- run tests
- fix services
- manipulate docker
- rebuild systems
- summarize everything it did

### 5.2 Patch-first repo workflow
For coding tasks:
- inspect repo
- identify files
- plan edits
- patch files
- run tests/lint/build
- inspect failures
- retry intelligently
- summarize final diff and status

### 5.3 Autonomous run loop
Every autonomous task should follow:
- objective parse
- context retrieval
- plan
- action
- verification
- correction
- repeat

## Priority 6 — UX

### 6.1 Workspace-centric design
Each workspace should contain:
- chats
- files
- indexes
- memories
- model profiles
- run logs
- tasks
- artifacts

### 6.2 Run trace viewer
Expose:
- prompt summary
- selected model
- retrieval sources
- commands run
- files changed
- outputs
- failures
- retries
- elapsed time

### 6.3 Artifact generation
Generate:
- docs
- reports
- code patches
- scripts
- spreadsheets
- summaries
- timelines

## Priority 7 — Evals And Benchmarks

### 7.1 Mandatory evals
Every release must test:
- tool routing
- shell execution quality
- repo QA
- RAG QA
- citation accuracy
- edit correctness
- autonomous task completion
- failure recovery
- long-run stability

### 7.2 Ollama model comparison
Continuously compare installed models for:
- coding
- planning
- command reliability
- debugging
- RAG synthesis
- long context
- speed

### 7.3 Regression protection
Do not merge changes that reduce:
- tool-call success
- retrieval quality
- patch success
- run trace visibility
- autonomy stability

## Priority 8 — Packaging

## Priority 9 — Claw Ecosystem Patterns

Research from the Claw agent ecosystem (OpenClaw, ZeroClaw, NemoClaw) and Claude Code
reveals concrete patterns LlamaFarm should adopt. Sources:
- **OpenClaw** (2025–2026): TypeScript autonomous agent, 247k GitHub stars. Popularized
  the SOUL.md behavioral rules file pattern. Demonstrates messaging-channel-first design.
- **ZeroClaw**: Rust-based ultra-lightweight agent runtime, 3.4MB binary, <10ms boot.
  Proves the same pattern LlamaFarm uses (Tokio, trait-based Memory, Ollama as provider,
  SQLite persistence) is production-viable at edge scale. Key contributions:
  hybrid search weights (0.7 vector / 0.3 keyword), SQLite context rebuild (30–50%
  token reduction), aggressive Cargo profiles, Landlock confinement per tool call.
- **NemoClaw**: NVIDIA's enterprise OpenClaw overlay. Introduces agent-fleet orchestration
  and policy-based governance. Points toward LlamaFarm's multi-agent chaos scenarios.
- **Claude Code Agent SDK**: Formalizes the three-phase loop (gather → act → verify),
  PreToolUse/PostToolUse hooks, subagent isolation, context compaction with focus,
  and session fork/resume as first-class primitives.

### 9.1 AGENT.md persona file (OpenClaw SOUL.md pattern)
Support a per-workspace `AGENT.md` file (analogous to OpenClaw's `SOUL.md`) that:
- Defines agent personality, behavioral rules, capability scope
- Is injected verbatim into the system prompt at session start
- Allows per-workspace behavioral customization without code changes
- Example: a chaos_lab workspace's `AGENT.md` says "you are a destructive lab operator;
  destroy, measure, recover" while a coding workspace says "patch only, test first"

### 9.2 Context compaction with focus (Claude Code pattern)
When the context window fills during a long autonomous run:
- Summarize accumulated history while preserving a caller-specified focus
- Example: `compact(focus="service restart failures")` keeps all failure events,
  drops routine shell output
- Feed compressed summary + focus back as injected system context
- Critical for 100+ step autonomous runs where full history exceeds context limits
- Prevent the agent from losing the thread after a context overflow

### 9.3 Session fork for autonomous runs (Claude Code pattern)
Allow branching an autonomous run at any checkpoint:
- `--fork-run <run-id>` creates a sibling run with the same history up to that point
- Essential for chaos_lab: snapshot state before a destructive experiment, then fork
  to try two different recovery strategies in parallel without affecting each other
- Parent run is untouched; fork gets a new run ID and independent retry budget

### 9.4 SQLite session resume / context rebuild (ZeroClaw pattern)
ZeroClaw stores compressed run summaries in SQLite, enabling resume from checkpoints
with 30–50% token reduction vs. re-sending full history. LlamaFarm should:
- After each completed step in an autonomous run, persist a compact step summary to SQLite
- On resume (`--resume-run <run-id>`), reconstruct context from step summaries + tool
  results rather than the raw conversation log
- Store tool-result cache keyed by (tool_name, args_hash) to skip redundant reads

### 9.5 Hybrid recall weight config (ZeroClaw: 0.7/0.3)
ZeroClaw's memory layer uses a validated 0.7 vector / 0.3 keyword hybrid ratio.
LlamaFarm's RAG retrieval should:
- Expose `rag.vector_weight` and `rag.keyword_weight` in config schema
- Default to 0.7/0.3
- Allow per-workspace override via `AGENT.md` or workspace config
- Log which ratio was used in the run trace for post-run analysis

### 9.6 Agent fleet orchestration (NemoClaw pattern)
NemoClaw's key enterprise feature: spawn and coordinate a fleet of autonomous agents
targeting different aspects of the same machine. For chaos_lab:
- Fleet manager spawns N sub-agents with distinct sub-objectives (e.g., one breaks
  services, one monitors logs, one attempts recovery)
- Coordinator tracks agent states, detects conflicts, merges findings into a single
  run report
- Uses the existing `subagent_spawn` / `subagent_registry` infrastructure but adds
  a fleet-level coordinator with shared memory and cross-agent messaging

### 9.7 Landlock tool confinement (ZeroClaw pattern)
LlamaFarm already has `src/security/landlock.rs`. ZeroClaw wraps every tool's child
process in Landlock syscall-level sandboxing to confine it to workspace + allowed paths.
Wire Landlock into the tool executor dispatch path so shell, file, docker, and process
tools automatically scope their child processes to the configured workspace boundary.

### 9.8 Package manager tool
Dedicated tool (not just `cli_discovery`) for:
- apt/apt-get (Debian/Ubuntu)
- dnf/yum (Fedora/RHEL)
- pip/uv (Python)
- cargo (Rust crates in the target workspace)
Operations: install, remove, upgrade, list, search, hold
Blocked in non-chaos modes. Fully unlocked in chaos_lab.

### 9.9 Service control tool
Dedicated tool for:
- systemctl (start, stop, restart, enable, disable, status, journal tail)
- service (SysV fallback)
Blocked in non-chaos modes. Fully unlocked in chaos_lab.
Outputs structured status including exit code, active state, recent log lines.



### 8.1 Deploy targets
Support:
- single local dev run
- docker compose
- headless server mode
- lab box mode

### 8.2 Resource-aware behavior
Adapt automatically to machine limits:
- chunk size
- concurrency
- embedding batch size
- selected model
- context packing depth
- retry budget

## Non-Negotiable Acceptance Criteria

LlamaFarm is not done until it can:
- run fully local with Ollama only
- switch between Ollama models without code changes
- select different Ollama models for different tasks
- ingest and query a repo/docs corpus with citations
- recover tool intent from messy model output
- autonomously inspect, edit, and execute on a disposable machine
- run long multi-step tasks without constant user approval
- break and attempt to recover a sandboxed environment
- benchmark installed Ollama models and pick winners by task
- expose a full trace of what it did

## Codex Execution Rules

1. Ollama is the only model runtime.
2. Do not build cloud adapters.
3. Prefer typed modules and schemas.
4. Prefer real working end-to-end slices over scaffolding.
5. Add tests for parser recovery and tool routing.
6. Add integration tests for RAG and autonomous task runs.
7. Add benchmarks for installed Ollama models.
8. Expose traces for every major subsystem.
9. Optimize for actual operator usefulness, not demo polish.
10. Build for autonomy, long runs, and stateful recovery.

## First Concrete Tasks

- [x] 1. Create `docs/ARCHITECTURE.md`. → `docs/ARCHITECTURE.md`
- [x] 2. Build `core/ollama/` as the single model runtime. → `src/providers/ollama.rs` extended with `embed()`, `pull_model()`, `show_model()`, `list_models()`, `OllamaModelInfo`, `OllamaModelEntry`
- [x] 3. Build an Ollama model registry with capability scores. → `src/capability/mod.rs` — per-model scoring, smart routing hints, JSON persistence
- [x] 4. `ToolIntentNormalizer` — all 7 fallback stages confirmed implemented in `src/agent/loop_/parsing.rs` + `src/agent/loop_.rs`: (1) native tool-call, (2) XML tags (`<tool_call>`, `<function_calls>`), (3) JSON objects, (4) structured pseudo-format (`TOOL: name\nARGS:`), (5) fenced code blocks, (6) command-intent recovery (implicit shell), (7) plain-answer fallback. 67+ tests cover parsing, deduplication, and semantic grounding.
- [x] 5. Complete `ToolExecutor` — package manager + service control. → Docker ✓ (`src/tools/docker.rs`); shell ✓; git ✓; process ✓; `PackageManagerTool` ✓ (`src/tools/package_manager.rs` — apt/dnf/pip/uv/cargo); `ServiceControlTool` ✓ (`src/tools/service_control.rs` — systemctl + SysV + journalctl). All wired into `all_tools_with_runtime`. Mutating ops blocked outside chaos modes.
- [x] 6. Per-run trace file + replay viewer. → `src/observability/runtime_trace.rs` — `RunTracer::open(dir, run_id)` writes `<workspace>/state/runs/<run_id>.jsonl`; `AutonomousLoop` creates/closes tracer automatically; `format_run_trace()` + `list_run_traces()` for replay. CLI wired in `src/main.rs` as `llamafarm trace list` and `llamafarm trace replay <run-id>|--latest`.
- [x] 7. Build document/repo ingest pipeline. → `src/rag/doc_rag.rs` — ingest text/files/directories, chunking, BM25 indexing
- [x] 8. Build hybrid retrieval and citation rendering. → `src/rag/doc_rag.rs` — BM25 + vector RRF fusion, `[Source N: path § heading]` citations
- [x] 9. Tool-result cache. → `src/agent/tool_cache.rs` — SHA-256 keyed, TTL-based (5 min default), read-only tools only (file_read, glob_search, web_fetch, etc.); `TOOL_CACHE` task-local in `loop_.rs`; `AutonomousLoop` scopes cache per attempt + `invalidate_all()` after each attempt. Step-summary persistence + `--resume-run` still TODO.
- [x] 10. Add autonomous run loop with retries and verification. → `src/agent/autonomous.rs` — retry budget, wall-clock cap, chaos/operator recovery prompts
- [x] 11. Add `chaos_lab` mode. → `AgentExecutionMode::ChaosLab` in config schema + god-mode config template
- [x] 12. Add model bakeoff harness. → `src/bakeoff/mod.rs` — benchmarks all installed Ollama models, writes to capability registry
- [x] 13. Repo patch/test/retry workflow. → `src/agent/repo_workflow.rs` — `RepoWorkflowAgent` wraps `AutonomousLoop` with explore→plan→patch→build→verify system prompt; configurable `build_cmd`, retry budget, wall-clock cap; `RepoWorkflowOutcome` with `is_success()` and `summary()`.
- [x] 14. Wire everything into a working vertical slice. → `tests/autonomous_vertical_slice.rs` covers objective → RAG retrieval (`DocRag`) → tool chain (`rag_lookup` + `verify_rag`) → autonomous completion → per-run trace file + replay formatting checks.
- [x] 15. Update README to reflect the new vision. → `README.md` now documents Ollama-first autonomous operator scope, disposable-target contract, `chaos_lab` posture, model-routing/bakeoff role, and agentic online-research flow.
- [x] 16. AGENT.md persona file support. → see §9.1; added to `IdentitySection` workspace file list in `src/agent/prompt.rs` alongside `SOUL.md`, `AGENTS.md`. Falls back gracefully if absent.
- [x] 17. Context compaction with focus. → `src/agent/loop_/history.rs` — `auto_compact_history_focused(focus: Option<&str>)` amends summariser prompt with current objective; `compact_history_with_focus` re-exported from `loop_.rs`; `AutonomousLoop` calls it between retries using first user message as objective.
- [x] 18. Session fork for autonomous runs. → `AutonomousLoop::fork()` returns `AutonomousLoopFork{run_id, parent_run_id}` for branching chaos experiments. `--fork` CLI subcommand still TODO.
- [x] 19. Hybrid recall weight config. → Already done: `MemoryConfig.vector_weight` = 0.7, `MemoryConfig.keyword_weight` = 0.3 in `src/config/schema.rs`. `doc_rag.rs` uses RRF fusion (better than weighted sum). No action needed.
- [x] 20. Agent fleet orchestration. → `subagent_spawn` + `subagent_registry` + `subagent_list` + `subagent_manage` provide concurrent sub-agent fleet lifecycle control; `delegate` coordination bus and `agents_ipc` shared state/message tools provide cross-agent coordination surfaces.
- [x] 21. Landlock confinement per tool call. → command-producing tools now accept a sandbox backend and apply `wrap_command` before spawn (`shell`, `process`, `docker`, `package_manager`, `service_control`); `LandlockSandbox` now installs child-process restrictions via `pre_exec` with workspace + explicit filesystem allow-list.
- [x] 22. PackageManagerTool. → `src/tools/package_manager.rs` — apt/apt-get/dnf/yum/pip/uv/cargo; install/remove/upgrade/list/search/hold; mutating ops blocked outside chaos modes.
- [x] 23. ServiceControlTool. → `src/tools/service_control.rs` — systemctl + SysV service fallback + journalctl logs; start/stop/restart/enable/disable/status/reload/daemon-reload/logs/list-units; read-only ops always permitted; mutating ops blocked outside chaos modes.

## Priority 10 — Inference Quality: GPU, Context, and Metrics

### 10.1 GPU-first loading (fill GPU, spill to CPU)
Ollama's `num_gpu` option controls layer offloading.
- `num_gpu = 999` → fill all GPU layer slots; layers that don't fit overflow to CPU RAM
- Set via `provider.ollama_gpu_layers = 999` in config.toml
- Or export `OLLAMA_GPU_LAYERS=999` (or `OLLAMA_NUM_GPU=999`) in the environment
- LlamaFarm reads either env var at startup if config is absent
- `provider.ollama_main_gpu` selects the GPU index for largest tensors (multi-GPU boxes)
- Status: ✓ implemented in `src/providers/ollama.rs` + `src/config/schema.rs`

### 10.2 Extended context via KV cache compression (turboquant insight)
TurboQuant's KV cache quantization achieves 2x+ context windows in the same VRAM.
Ollama's built-in equivalent: `OLLAMA_KV_CACHE_TYPE=q8_0` (server env var, global).
LlamaFarm implementation:
- `provider.ollama_num_ctx` → override context window size per-request (e.g. 32768, 65536)
- Env var `OLLAMA_NUM_CTX` also read as fallback
- **To activate KV quantization**: set `OLLAMA_KV_CACHE_TYPE=q8_0` in Ollama's service
  environment (requires Flash Attention, which Ollama enables automatically on CUDA/Metal)
- q8_0 = ~half KV cache VRAM; q4_0 = ~quarter KV cache VRAM (some quality loss)
- Recommended for long autonomous runs: `OLLAMA_KV_CACHE_TYPE=q8_0` + `ollama_num_ctx = 65536`
- Status: ✓ `num_ctx` wired into per-request Options; env docs added to schema

### 10.3 Accurate inference metrics (replace wall-clock TPS)
Old TPS: wall-clock time / output tokens (includes HTTP, prefill, load — inaccurate).
New metrics from Ollama's nanosecond timing fields:
- `generation_tps`: output tokens / eval_duration — the real decode throughput
- `prefill_tps`: input tokens / prompt_eval_duration — prefill speed
- `ttft_ms`: (load_duration + prompt_eval_duration) / 1e6 — time to first token
- `total_ms`: total_duration / 1e6 — full wall-clock request time
- Exposed as `InferenceMetrics` in `ChatResponse.metrics` (Ollama only; other providers = None)
- Status: ✓ implemented in `src/providers/traits.rs` + `src/providers/ollama.rs`
- [x] `generation_tps` + `ttft_ms` now stream over the chat websocket as `metrics` events and replace the wall-clock TPS estimate in the UI (2026-07-16)

### 10.4 NPU hardware detection
AMD XDNA NPU driver landed in Linux kernel 6.14. Intel NPU driver also available.
Ollama does not yet route to NPU for inference. Actions:
- Detect NPU in `src/hardware/discover.rs` (check `/dev/accel/`, `amdxdna` kernel module,
  `/dev/intel_npu`)
- Report NPU presence in hardware introspection output
- Add NPU entry to capability registry (model routing: small models on NPU if available)
- Watch Ollama releases for NPU backend support; add routing when available
- Status: detection ✓ (doctor reports /dev/accel nodes + amdxdna/intel_vpu modules, 2026-07-16); inference routing still blocked on Ollama NPU support

## Final Standard

LlamaFarm should be the thing you run when you want:
- your own models
- your own data
- your own machine
- your own tools
- your own autonomy
- your own lab
- your own chaos
- your own recovery loop

No cloud.
No fake agency.
No weak sauce.
Build the real local operator.
