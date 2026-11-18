# GPU1 bundle capacity benchmark — 2026-11-17

The Mac sent distinct coding-agent prompts over Tailscale to the Ollama server bundled in `LlamaFarm-GPU1`. Each prompt contained 11,809 input tokens, a Python source listing, a specified `divide_*` defect, and a `report_patch` function tool. A request counted as complete only when it made the correct structured tool call and described the zero-count fix. `bench-gpu1.py` recorded elapsed time, request latency, token counts, prompt and output throughput, GPU utilization, and peak GPU memory. The profile switcher separately required `size_vram == size`, the requested context, and the selected quantization before each run.

| Profile | Requests | Valid tool calls | Wall s | Latency s | Tasks/min | Output tok/s | Peak GPU1 MiB | GPU KV MiB |
| --- | ---: | ---: | ---: | --- | ---: | ---: | ---: | ---: |
| A UnQwen Q8 64K | 1 | 1/1 | 17.26 | 17.26 | 3.475 | 3.59 | 30,410 | 2,176 |
| D Rana Q8 32K | 1 | 1/1 | 4.98 | 4.97 | 12.058 | 10.45 | 10,556 | 544 |
| B Rana Q8 32K | 2 | 2/2 | 8.91 | 8.91, 8.84 | 13.461 | 11.89 | 11,494 | 1,088 |
| C Rana Q8 32K | 3 | 3/3 | 14.03 | 13.63, 14.02, 13.86 | 12.831 | 11.55 | 12,000 | 1,632 |

The same Rana model weights were loaded once per profile; `OLLAMA_NUM_PARALLEL` changed from 1 to 2 to 3. GPU1's Ollama logs reported 33/33 layers offloaded for all Rana profiles. The Q8 27B log reported 66/66 layers and a 2,176 MiB GPU KV buffer. No profile spilled weights or KV to CPU in these runs. GPU0 stayed occupied by a separate OpenCode inference workload during the GPU1 tests.

Three Rana requests fit but completed fewer tasks per minute than two. `scripts/kilo/gpu1-capacity.json` therefore sets `max_stable_parallel` to 2. The remote switcher and Mac OpenCode/Kilo selector automatically lower a request for profile C to two Rana slots. The raw JSON files preserve the token counts, per-request latency, and GPU samples. `D-1-cold.json` records the initial cold-start run; the table uses a warm run for comparison.

These direct model calls validate GPU capacity and tool-call syntax. The acceptance gate for a profile also requires a real OpenCode delegation and parent review with the selected profile. Do not treat the direct benchmark alone as agent completion.

## Q4 256K concurrency probe

Two simultaneous requests to the 27B Q4 model at 262,144 context each completed in 10.12 seconds, but Ollama reported 39,379,040,987 bytes total and only 31,968,544,684 bytes in VRAM. The 7,410,496,303-byte CPU spill fails the full-GPU requirement. The probe restored GPU 1 to one slot and verified `size_vram == size` at 262,144 context. Three full-size slots were not attempted because they require more KV memory than two. See `Q4-2x256k-failed-residency.json`.
