# Kilo Code → direct Ollama (V100) integration

Kilo (the VS Code extension) talks **directly to Ollama** on the dual-V100
box (`pop-os-1`, Tailscale IP `100.107.226.49`) using the OpenAI-compatible
`@ai-sdk/openai-compatible` provider. **LlamaFarm's gateway (port 42617) is
deliberately not in this path.** Its own internal GPU-routing layer
(`gpu_router`, part of the dynamic dual-V100 placement scheme in
`deploy/host-profiles/dynamic-placement/`) was unreliable for an
interactive coding agent and added a layer of indirection with no benefit
for this use case. LlamaFarm itself still uses the gateway for its own
webchat/cron/agent_api features — just not Kilo.

## One-time server-side setup (pop-os-1) — already done, not needed per client

1. Ollama's port (11434) is published by the `LlamaFarm` container only to
   `127.0.0.1` on the host (deliberate: the native Ollama API has no auth).
   It's exposed to the tailnet via Tailscale Serve instead of changing the
   container's port binding:

   ```
   sudo tailscale serve --bg --tcp=11434 tcp://127.0.0.1:11434
   ```

   This is stored in tailscaled's own state and persists across reboots.
   Check current state with `tailscale serve status`.

2. Two model tags, both with `num_ctx` explicitly baked in via Modelfile
   (Ollama does **not** default to a model's architectural max context —
   `qwen3.5-rana:9b-q8_0` was silently running on Ollama's small default
   until this was added):

   ```
   # qwen3.8-unqwen:27b-256k already had num_ctx=262144 baked in.
   # qwen3.5-rana:9b-256k was created with:
   cat > rana-256k.Modelfile <<'EOF'
   FROM abliterant/qwen3.5-rana:9b-q8_0
   PARAMETER num_ctx 262144
   EOF
   ollama create qwen3.5-rana:9b-256k -f rana-256k.Modelfile
   ```

   Note: on this box, "Qwen" and "UnQwen" are **not** separate
   censored/uncensored weights — `scripts/kilo/orcarouter-qwen3.8-256k.Modelfile`
   builds from the identical `orcarouter/Qwen3.8-27B-Uncensored` base as the
   UnQwen tag. The two models actually in use (`qwen3.8-unqwen:27b-256k`,
   `qwen3.5-rana:9b-256k`) differ in size/speed, not in censorship.

## Per-laptop setup (do this on each new machine)

1. Install Tailscale, log into the same tailnet (must be able to reach
   `100.107.226.49`).
2. Install the Kilo Code VS Code extension.
3. Create `~/.config/kilo/kilo.jsonc` with:

   ```jsonc
   {
     "$schema": "https://app.kilo.ai/config.json",
     "enabled_providers": ["ollamav100"],
     "model": "ollamav100/qwen3.8-unqwen:27b-256k",
     "small_model": "ollamav100/qwen3.5-rana:9b-256k",
     "provider": {
       "ollamav100": {
         "npm": "@ai-sdk/openai-compatible",
         "options": {
           "baseURL": "http://100.107.226.49:11434/v1",
           "apiKey": "none",
           "includeUsage": true,
           "timeout": false
         },
         "models": {
           "qwen3.8-unqwen:27b-256k": {
             "name": "UnQwen 3.8 27B | 256K | V100 direct Ollama",
             "tool_call": true,
             "limit": { "context": 262144, "output": 131072 }
           },
           "qwen3.5-rana:9b-256k": {
             "name": "Rana 3.5 9B | 256K | V100 direct Ollama",
             "tool_call": true,
             "limit": { "context": 262144, "output": 131072 }
           }
         }
       }
     },
     "agent": {
       "code": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
       "plan": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
       "debug": { "model": "ollamav100/qwen3.8-unqwen:27b-256k", "temperature": 0.2 },
       "review": {
         "description": "Read-only code reviewer for independent worktrees",
         "mode": "all",
         "model": "ollamav100/qwen3.8-unqwen:27b-256k",
         "temperature": 0.1,
         "prompt": "Review changes using repository files and diffs. Report findings with file paths and evidence. Do not edit files or run commands.",
         "permission": { "edit": "deny", "bash": "deny" }
       }
     },
     "permission": { "*": "allow" },
     "reasoning_display": "preview",
     "terminal_command_display": "collapsed",
     "code_edit_display": "collapsed",
     "mcp_tool_display": "collapsed",
     "snapshot": true
   }
   ```

4. Reload VS Code fully (quit + relaunch, not just "Reload Window" — Kilo
   caches provider config in the extension host's memory for the life of
   the process either way, but a full quit guarantees no stale state).

## Verifying it actually works (don't trust a single tool call)

Replay Kilo's exact request shape directly against the endpoint and chain
multiple tool-call rounds — a single successful call proves nothing, since
the failure mode this setup replaces was "stalls after N tool calls, needs
manual continue":

```python
import json, urllib.request
BASE = "http://100.107.226.49:11434/v1/chat/completions"
# POST with stream=true, stream_options.include_usage=true, a tools array,
# and tool_choice="auto". After each tool_calls response, append the
# assistant message + a role="tool" result and re-POST with the same
# tools array. A healthy setup chains 3+ rounds and ends with
# finish_reason="stop" only once the task is actually done.
```

Known-bad signal: any error referencing `gpu_router` or port `42617` means
a client somewhere is still hitting LlamaFarm's gateway instead of Ollama
directly — check for a stale cached provider config.

## Why LlamaFarm's gateway was cut out, concretely

`/llamafarm-data/.llamafarm/config.toml` (inside the `LlamaFarm` container's
data volume — **not** `~/.llamafarm/config.toml` on the host, which is a
stale, unrelated file labeled "Alfred node") had:

```
api_url = "http://gpu_router:11434"
```

`gpu_router` was a now-removed helper container that the dynamic dual-GPU
placement daemon used to shuffle which physical GPU served which model tag
under load. When that container went away, every LlamaFarm-gateway-routed
model call failed with a retryable connection error, surfaced through
Kilo's strict OpenAI-schema validation as an opaque `UnknownError: Type
validation failed`. Fixed by pointing `api_url` straight at the real
Ollama instance in the same container (`http://127.0.0.1:11434`) and
restarting the gateway — but Kilo was already bypassing this file entirely
once pointed at Ollama directly, which is why removing LlamaFarm from
Kilo's path is the actual fix, not the config patch.

## Running Kilo on pop-os-1 itself instead of over the network

Ping Mac → pop-os-1 over Tailscale is ~2-5ms, direct (not DERP-relayed).
That's negligible next to GPU generation time per token, so colocating
Kilo and Ollama on the same box buys essentially no latency. The real
reason to do it is **native file access** for any coding task whose actual
files live on pop-os-1 (this repo, training scripts, etc.) — a Mac-side
Kilo working on those would need some remote-filesystem bridge, whereas a
Kilo running on pop-os-1 itself just reads local disk.

Recommended mechanism: VS Code **Remote-SSH** into pop-os-1 from the same
local VS Code app, rather than installing/running a second desktop VS Code
on the headless box. Kilo then runs in the remote extension host with
`baseURL` simplified to `http://127.0.0.1:11434/v1` (no Tailscale Serve
indirection needed at all, since client and server are the same machine).

(Decided against setting this up for now — Tailscale latency measured at
2-5ms made the main justification moot. Revisit if a coding task's actual
files start living on pop-os-1.)

## Status + self-repair scripts

Two companion scripts, one per side, each ending in a clean status table:

- **`scripts/kilo/moab.sh`** — run on pop-os-1 (or via
  `ssh bat@100.107.226.49 'bash ~/LlamaFarm-dual-v100-deploy/scripts/kilo/moab.sh'`).
  Checks and auto-repairs: the LlamaFarm container, debris containers
  matching known junk patterns, gateway health, `api_url` sanity (the
  `gpu_router` failure mode specifically), Tailscale Serve exposure for
  port 11434, both model tags' `num_ctx`, and reports both GPUs'
  free/busy state.

  Subcommands:
  - `./moab.sh` — status + repair (default, safe to run anytime)
  - `./moab.sh enable-gpu1` — starts a second Ollama worker pinned to
    GPU1 (`llamafarm-ollama-worker-gpu1`, port 11436) and exposes it over
    Tailscale. Refuses loudly if GPU1 is still busy (>20% util) — will
    not silently queue or interfere with a running training job.
  - `./moab.sh disable-gpu1` — tears it back down.

- **`scripts/kilo/moabKilo.sh`** — run locally on any laptop
  (`bash ~/.config/kilo/moabKilo.sh`). Checks `kilo.jsonc` exists and
  points at direct Ollama (not the gateway's port 42617 — that specific
  regression is checked for explicitly), confirms Tailscale sees
  pop-os-1, confirms Ollama is actually reachable, confirms the Kilo
  extension is installed. Auto-repairs `kilo.jsonc` from a canonical
  template (backing up whatever was there first) if anything's missing
  or drifted.

Run `moabKilo.sh` first on a new laptop (it writes a working `kilo.jsonc`
from scratch if none exists), then `moab.sh` on pop-os-1 to confirm the
server side is healthy too.

## Using two GPUs (when GPU1 isn't busy with training)

A real GPU-placement dashboard already exists — no new infrastructure
needed. It's embedded directly in the LlamaFarm gateway binary and is
live right now at **`http://100.107.226.49:42617/`**. It talks to a real
backend (`GET`/`PUT /api/ollama/gpu-placement`), not a mockup.

Current state (deliberately minimal and safe): one worker, `gpu0`
(the existing primary instance, `managed = false`), `ollama_auto_workers
= false`, no model placements configured. The containers that broke
(`gpu_router`, `llamafarm-ollama-worker-gpu1`) were from an earlier,
separate hand-rolled attempt — not from this mechanism, which was never
actually exercised.

To bring GPU1 online once it's free:

1. Check it's actually free: `nvidia-smi` on pop-os-1, or `moab.sh`'s
   status table (GPU1 UUID is `GPU-4dda4033-eb45-c5a7-fe70-16979f13147c`).
2. Either run `moab.sh enable-gpu1`, or add a `gpu1` worker through the
   dashboard at the URL above.
3. Point a **second** provider entry in `kilo.jsonc` directly at that
   worker's own endpoint (e.g. `http://100.107.226.49:11436/v1`) — same
   principle as the primary setup: Kilo talks straight to the Ollama
   instance, never through the gateway's routing layer. This keeps GPU1
   usable for a second Kilo window, or a dedicated `review` agent role
   running on separate hardware from the main `code` agent.

Deliberately **not** automated (no watcher polling GPU state and flipping
routing on its own) — that reintroduces the exact category of fragility
that broke the setup before, just with better process supervision. Manual
is simple, visible, and nothing can drift silently.
