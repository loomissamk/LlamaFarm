# TODO.md — LlamaFarm Disposable Chaos Agent Build

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
- [ ] 15. Update README to reflect the new vision. → not done; describe Ollama-only autonomous operator, chaos_lab mode, model bakeoff, disposable-VM target.
- [x] 16. AGENT.md persona file support. → see §9.1; added to `IdentitySection` workspace file list in `src/agent/prompt.rs` alongside `SOUL.md`, `AGENTS.md`. Falls back gracefully if absent.
- [x] 17. Context compaction with focus. → `src/agent/loop_/history.rs` — `auto_compact_history_focused(focus: Option<&str>)` amends summariser prompt with current objective; `compact_history_with_focus` re-exported from `loop_.rs`; `AutonomousLoop` calls it between retries using first user message as objective.
- [x] 18. Session fork for autonomous runs. → `AutonomousLoop::fork()` returns `AutonomousLoopFork{run_id, parent_run_id}` for branching chaos experiments. `--fork` CLI subcommand still TODO.
- [x] 19. Hybrid recall weight config. → Already done: `MemoryConfig.vector_weight` = 0.7, `MemoryConfig.keyword_weight` = 0.3 in `src/config/schema.rs`. `doc_rag.rs` uses RRF fusion (better than weighted sum). No action needed.
- [ ] 20. Agent fleet orchestration. → see §9.6; fleet manager on top of existing `subagent_spawn`/`subagent_registry`. N agents, shared memory namespace, cross-agent messaging bus, coordinator merges run reports.
- [ ] 21. Landlock confinement per tool call. → see §9.7; `src/security/landlock.rs` exists — wire it into tool dispatch so shell/file/docker/process child processes are confined to workspace path + explicit allow-list.
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
- TODO: surface `generation_tps` and `ttft_ms` in the web UI's chat TPS indicator

### 10.4 NPU hardware detection
AMD XDNA NPU driver landed in Linux kernel 6.14. Intel NPU driver also available.
Ollama does not yet route to NPU for inference. Actions:
- Detect NPU in `src/hardware/discover.rs` (check `/dev/accel/`, `amdxdna` kernel module,
  `/dev/intel_npu`)
- Report NPU presence in hardware introspection output
- Add NPU entry to capability registry (model routing: small models on NPU if available)
- Watch Ollama releases for NPU backend support; add routing when available
- Status: TODO (detection only; inference routing blocked on Ollama NPU support)

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
