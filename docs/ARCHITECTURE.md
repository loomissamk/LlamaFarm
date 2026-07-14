# LlamaFarm Architecture

## Mission

LlamaFarm is an Ollama-native autonomous operator, agentic RAG runtime, and local tool execution engine for disposable lab environments. It runs fully local, acts without per-step confirmation, and is designed to break things, recover from them, and iterate until objectives are complete.

## System Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                         LlamaFarm                               │
│                                                                 │
│  ┌─────────────┐   ┌──────────────┐   ┌────────────────────┐   │
│  │  Channels   │   │  Agent Loop  │   │   Tool Executor    │   │
│  │  (CLI/Web/  │──▶│  plan→act→   │──▶│  shell, file, git, │   │
│  │  Telegram/  │   │  verify→     │   │  docker, process,  │   │
│  │  Discord)   │   │  retry→done  │   │  http, rag, memory │   │
│  └─────────────┘   └──────┬───────┘   └────────────────────┘   │
│                            │                                    │
│              ┌─────────────┼──────────────┐                    │
│              ▼             ▼              ▼                     │
│  ┌───────────────┐ ┌──────────────┐ ┌─────────────────────┐    │
│  │ Ollama Runtime│ │  RAG Engine  │ │  Capability Registry │    │
│  │ chat/embed/   │ │ ingest/chunk/│ │  model scores, ctx   │    │
│  │ pull/stream   │ │ embed/hybrid │ │  window, tool-use    │    │
│  │ model routing │ │ retrieve/cite│ │  reliability scores  │    │
│  └───────────────┘ └──────────────┘ └─────────────────────┘    │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                  Security & Autonomy                      │   │
│  │  god_mode / full autonomy / supervised / read_only        │   │
│  │  policy engine · audit log · estop · OTP gating          │   │
│  └──────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────┘
```

## Core Modules

### 1. Ollama Runtime (`src/providers/ollama.rs`)

The single model inference backend. All inference goes through Ollama — no cloud, no other providers.

- **Chat / Streaming**: `POST /api/chat` with SSE streaming
- **Embeddings**: `POST /api/embeddings` — used by RAG and capability probing
- **Model List**: `GET /api/tags` — enumerates installed models
- **Model Pull**: `POST /api/pull` — downloads a model from Ollama registry
- **Model Show**: `POST /api/show` — metadata: context window, family, parameters
- **Vision Detection**: checks `details.families` for multimodal capability
- **Tool Calling**: native Ollama tool_calls format + XML/JSON/prose fallback pipeline

### 2. Agent Loop (`src/agent/loop_.rs`)

The central execution engine. Runs a `plan → act → verify → retry → done` cycle.

**Execution Modes:**
| Mode | Description |
|------|-------------|
| `chat` | Interactive assistant, supervised approvals |
| `operator` | Machine operator, no per-step confirmation |
| `autonomous_operator` | Full autonomy, long multi-step tasks |
| `chaos_lab` | Intentional break/recover experimentation |

**Autonomy Levels** (`src/security/policy.rs`):
- `ReadOnly` — observe only, no state changes
- `Supervised` — acts but prompts before risky operations
- `Full` — no prompts, `needs_approval()` always returns `false`

**Tool loop:**
1. Send system prompt + history to Ollama
2. Parse tool calls (XML → JSON → prose → shell → fallback)
3. Check approval policy (Full = never asks)
4. Execute tool via `ToolExecutor`
5. Append result to history
6. Repeat until no more tool calls or `max_tool_iterations`

### 3. Tool Intent Normalizer (`src/agent/loop_/parsing.rs`)

Seven-stage recovery pipeline for extracting tool calls from messy model output:

```
1. Native tool-call parse       (Ollama native format)
2. XML parse                    (<tool_call>, <function_calls>)
3. JSON parse                   ({"tool": ..., "args": ...})
4. Structured pseudo-tool parse (TOOL: name\nARGS: ...)
5. Fenced shell/code parse      (```bash ... ```)
6. Command-intent recovery      (implicit shell commands)
7. Plain-answer fallback        (no tool call detected)
```

### 4. Tool Executor (`src/tools/`)

67 tools organized by category:

| Category | Tools |
|----------|-------|
| File ops | `file_read`, `file_write`, `file_edit`, `apply_patch`, `glob_search`, `content_search` |
| Shell | `shell` (full sandbox with allowlist + path validation) |
| Process | `process` (list, kill, signal) |
| Git | `git_operations` (clone, commit, push, diff, log, blame) |
| Docker | `docker` (run, exec, build, ps, logs, stop, rm) |
| Web | `http_request`, `web_fetch`, `web_search_tool`, `browser` |
| Memory | `memory_store`, `memory_recall`, `memory_forget` |
| Ollama | `ollama_model` (list, pull, delete, show, running) |
| RAG | `content_search` |
| Scheduling | `cron_add/remove/update/run/list`, `schedule` |
| Agents | `delegate`, `agents_send/list/inbox` |

### 5. RAG Engine (`src/rag/`)

Full pipeline from raw documents to cited answers:

```
Ingest → Normalize → Chunk → Embed → Index
                                        ↓
Answer ← Context Pack ← Rerank ← Hybrid Retrieve
           ↑ citations                  ↑
                               BM25 + Vector merge
```

**Supported corpora:** repos, markdown, PDF, text, logs, configs, prior run history

**Retrieval:** keyword/BM25 + Ollama vector embeddings → merged + reranked by score

### 6. Capability Registry (`src/capability/`)

Per-model scoring registry, updated by the bakeoff harness:

| Field | Description |
|-------|-------------|
| `context_window` | Max tokens (from `ollama show`) |
| `tool_use_score` | 0.0–1.0, measured by bakeoff |
| `json_compliance` | 0.0–1.0, structured output reliability |
| `coding_score` | 0.0–1.0, code generation quality |
| `rag_score` | 0.0–1.0, RAG synthesis quality |
| `latency_ms_p50` | Median TTFT in milliseconds |
| `vram_mb` | Estimated VRAM footprint |
| `supports_vision` | Bool, from model family metadata |
| `supports_tools_native` | Bool, native tool-call format |

**Routing:** the model router uses capability scores to auto-select the best model per task hint (`planner`, `coder`, `embedder`, `fast`, etc.)

### 7. Chaos Lab Mode (`src/agent/loop_.rs` + `src/config/schema.rs`)

Enabled via `execution_mode = "chaos_lab"` or `--mode chaos_lab`.

Unlocks:
- Package install/remove without confirmation
- Config mutation and service restart
- Disk stress up to configured quota
- Self-healing retry loops with hypothesis generation
- Failure injection → root cause isolation → recovery

**Safety:** still respects `forbidden_paths`, `max_actions_per_hour`, and the emergency stop system.

### 8. Model Bakeoff Harness (`src/bakeoff/`)

Benchmarks all installed Ollama models across task categories:

```
shell_reliability  → run 10 shell tasks, score by success rate
file_editing       → apply patches, score by diff correctness
json_discipline    → structured output tasks, score by parse success
repo_understanding → ask questions about a test repo, score by accuracy
rag_synthesis      → answer questions from retrieved context, score citations
autonomous_tasks   → multi-step tasks, score by completion rate
recovery_score     → break + repair tasks, score by recovery success
latency            → TTFT and tokens/sec
```

Results written to `~/.llamafarm/capability-registry.json` and loaded at startup.

## Security Architecture

```
┌──────────────────────────────────────┐
│           Security Stack             │
├──────────────────────────────────────┤
│ Emergency Stop (estop.rs)            │  ← KillAll, NetworkKill, ToolFreeze
│ OTP Gating (otp.rs)                  │  ← TOTP for sensitive domains
│ Policy Engine (policy.rs)            │  ← command risk classification
│ Approval Manager (approval/mod.rs)   │  ← Full/Supervised/ReadOnly
│ Audit Logger (audit.rs)              │  ← every command, every change
│ Leak Detector (leak_detector.rs)     │  ← API key pattern scanning
│ Syscall Anomaly (syscall_anomaly.rs) │  ← kernel exploit detection
└──────────────────────────────────────┘
```

**God Mode** (`dev/config.template.toml`): `autonomy.level = "full"`, all high-risk commands allowed, no approval prompts, no path restrictions beyond hardware paths. Designed for intentionally disposable sandbox environments.

## Data Flow: Autonomous Task

```
User: "break nginx config then fix it"
        ↓
Agent Loop: parse objective
        ↓
RAG: retrieve relevant context (nginx docs, config examples)
        ↓
Planner model: generate step-by-step plan
        ↓
For each step:
  Executor model → tool calls → shell/file/process
  Verifier model → check outcome → pass/fail
  On fail: generate hypothesis → retry with new approach
        ↓
Summary: diff of all changes, final state, errors encountered
```

## Directory Structure

```
LlamaFarm/
├── src/
│   ├── agent/          # Agent loop, tool dispatch, parsing
│   ├── approval/       # Interactive approval workflow
│   ├── bakeoff/        # Model benchmark harness
│   ├── capability/     # Per-model capability registry
│   ├── channels/       # CLI, Telegram, Discord, Slack, etc.
│   ├── config/         # Config schema + loader
│   ├── goals/          # Autonomous goal loop engine
│   ├── memory/         # SQLite, Qdrant, Markdown, PostgreSQL backends
│   ├── providers/      # Ollama provider + router
│   ├── rag/            # Document ingest, chunking, hybrid retrieval
│   ├── security/       # Policy engine, estop, audit, OTP
│   ├── sop/            # Standard Operating Procedure engine
│   ├── tools/          # 67 tool implementations
│   └── workspace/      # Multi-workspace registry
├── dev/
│   └── config.template.toml   # God mode config for sandbox use
├── docs/
│   ├── ARCHITECTURE.md         # This file
│   ├── config-reference.md     # All config options
│   └── ...
└── docker-compose.yml          # Bundled stack (Ollama + Chromium + LlamaFarm)
```

## Observability

Every run records:
- Selected model and provider
- All prompts and completions
- Tool calls and results
- Files changed (via audit log)
- Commands executed (via audit log)
- Errors and retries
- Token usage
- Elapsed time

Outputs:
- `audit.log` — structured JSONL audit events
- `state/runtime-trace.jsonl` — rolling trace of agent turns
- Runtime log broadcast channel for live streaming to web UI

## Non-Negotiable Acceptance Criteria

LlamaFarm is not done until it can:

- [ ] Run fully local with Ollama only
- [ ] Switch between Ollama models without code changes
- [ ] Select different Ollama models for different tasks
- [ ] Ingest and query a repo/docs corpus with citations
- [ ] Recover tool intent from messy model output
- [ ] Autonomously inspect, edit, and execute on a disposable machine
- [ ] Run long multi-step tasks without constant user approval
- [ ] Break and attempt to recover a sandboxed environment
- [ ] Benchmark installed Ollama models and pick winners by task
- [ ] Expose a full trace of what it did
