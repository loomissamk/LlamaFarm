# GPU0/GPU1 model profiles -- measured, not assumed

Single user, two V100 32GB GPUs. No multi-user concurrency reservations --
`OLLAMA_NUM_PARALLEL` is set to 1 everywhere except the one deliberate
exception (Rana), because reserving multiple simultaneous inference slots
for a single-user workstation just burns VRAM that could otherwise go to
quality or context.

## GPU0 -- fixed, no profiles

`qwen3.8-unqwen:27b-256k`, Q4_K_M, 262144 context, 1 slot, served from the
main `LlamaFarm` container (port 11434, shared by Kilo and OpenCode's
primary agent). This is the dependable baseline and does not change.
26.8GB, confirmed fully GPU-resident (66/66 layers).

## GPU1 -- selectable profiles, one at a time

`OLLAMA_NUM_PARALLEL` is a container-level setting, not per-model, so
switching profiles means recreating the container (`scripts/kilo/gpu1-profile.sh`),
not just swapping the loaded model. Only one profile is ever active.

All numbers below are **measured** (`size_vram == size` in `/api/ps`, plus
the explicit `load_tensors: offloaded N/66 layers to GPU` log line) on this
exact box, not estimated.

| Profile | Model | Quant | Context | Slots | VRAM | Fully GPU-resident | Headroom |
|---|---|---|---|---|---|---|---|
| `q8-32k` | UnQwen 27B | Q8_0 | 32,768 | 1 | 28.96GB | yes (66/66) | ~3.8GB |
| `q8-64k` **(default)** | UnQwen 27B | Q8_0 | 65,536 | 1 | 30.2GB | yes (66/66) | ~2.5GB |
| `q8-80k` | UnQwen 27B | Q8_0 | 81,920 | 1 | 31.03GB | yes (66/66) | ~1.9GB |
| `q8-98k` | UnQwen 27B | Q8_0 | 98,304 | 1 | 31.69GB | yes (66/66) | ~1.0GB (tight) |
| `q8-131k` *(removed)* | UnQwen 27B | Q8_0 | 131,072 | 1 | 31.85GB used of 34.6GB needed | **no** -- ~2.8GB spilled to CPU | n/a |
| `rana` | Rana 9B | Q8_0 | 32,768 | **3** | 11.17GB | yes | ~21.6GB |

**The real ceiling is between 98K and 131K.** 98K is the largest context that
stays 100% GPU-resident; 131K measurably spills to CPU (`size` and
`size_vram` stop matching). `q8-64k` is the recommended default: real
quality upgrade over Q4, strong headroom, no edge-case risk.

**Q4 at 32K context was removed as a profile.** It used *more* VRAM
disadvantage for *less* quality than Q8 at 64K -- there was no scenario
where it beat Q8-64K on any axis, so it isn't offered.

**Rana is the one deliberate concurrency exception.** At 9B/Q8 it's cheap
enough (11.17GB) that 3 simultaneous slots cost far less than reserving
multiple slots ever did for the 27B, and genuinely enables fast concurrent
bounded subagent lookups. This is a conscious choice, not an oversight --
single-user concurrency for the 27B was removed everywhere else because it
reserved capacity nothing would ever use.

## Switching profiles

```
./scripts/kilo/gpu1-profile.sh q8-64k   # default
./scripts/kilo/gpu1-profile.sh q8-98k   # max context that's still 100% GPU
./scripts/kilo/gpu1-profile.sh rana     # cheap 3-slot subagent pool
./scripts/kilo/gpu1-profile.sh status   # what's loaded right now, no change
```

Each switch recreates the container and verifies residency itself (checks
`size == size_vram` in `/api/ps`), printing the real measured result --
it doesn't just claim success.

## Kilo and OpenCode both point at this

- **Kilo** (`~/.config/kilo/kilo.jsonc`): `ollamav100` = GPU0 (port 11434,
  fixed). `ollamav100gpu1` = GPU1 (port 11436), with all four Q8 context
  variants listed as selectable models -- pick whichever profile is
  currently loaded.
- **OpenCode** (`~/.config/opencode/opencode.json`): `gpu0/unqwen` = primary
  agent, fixed. `gpu1/specialist` (+ `q8-32k`/`q8-80k`/`q8-98k` aliases) =
  the subagent model, same four variants.

Both tools read whatever model is actually resident on GPU1 at request
time -- if you switch profiles with the script, both Kilo and OpenCode's
GPU1 option work against whatever's currently loaded without needing their
own config changed, *as long as you ask for the model tag matching the
active profile*. If you ask for a tag that isn't currently loaded, Ollama
auto-swaps to it (since `OLLAMA_MAX_LOADED_MODELS=1`) -- a few seconds,
not a manual unload step.

## OpenCode subagent delegation -- confirmed working, with real numbers

Real end-to-end proof, not a benchmark claim: asked the GPU0 primary to
delegate three genuinely independent, unrelated checks (list `/tmp`, get
the date, count `.json` files) to the GPU1 specialist "in parallel, in the
same turn." Transcript confirms all three were dispatched and tracked as
"running" simultaneously *before any of them completed* -- genuine
parallel fan-out, not one-at-a-time sequencing. All three results were
correctly collected and synthesized into one final answer.

**Total time: 1296 seconds (~21.6 minutes).** That's slow in absolute
terms -- this hardware/model combination is not fast for multi-step
agentic work -- but it is correct and it is genuinely parallel. Don't
expect fast; expect correct.

Getting here required one real system-prompt fix: telling the primary
agent explicitly that "redundant" means duplicate work on the same thing,
not "more than one subagent" -- an earlier, more conservative wording
("don't launch overlapping or redundant subagents") was plausibly
suppressing legitimate parallel delegation of genuinely different tasks.

## What was tried and reverted, and why

- **Running the 27B's own internal Ollama inside the main "LlamaFarm"
  bundle container** was abandoned. Its scheduler couldn't be reliably
  pinned to a specific GPU (identical `CUDA_VISIBLE_DEVICES` settings
  produced different physical placement across attempts), and a specific
  invocation path inside that container consistently launched with
  `-b 2048 -ub 2048 --load-mode none` and never became GPU-resident
  (`size_vram: 0`, 400-1000%+ CPU, for 30+ minutes) for reasons never
  fully root-caused. Plain standalone `ollama/ollama` containers pinned
  via Docker's own `--gpus device=N` flag worked reliably every single
  time, so that's the pattern both GPU0 and GPU1 now use. Port 11434 was
  freed from the bundle container (`OLLAMA_PORT=19999` in `.env`) and
  reclaimed by the real, working GPU0 container.
- **A concurrency-limiter plugin + "launch multiple subagents" system
  prompt was briefly removed**, mistaking "needs to work correctly" for
  "should be simplified away." Restored -- the plugin and delegation
  behavior were the actual feature being asked for, not a mistake to
  revert.
