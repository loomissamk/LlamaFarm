# Kilo Code integration backlog

- Deploy the tested gateway changes from the Git branch through the existing V100 bundle health gate after review. Keep the previous image for rollback; do not restart active training.
- Verify native non-streaming and streaming gateway tool calls against Qwen3.8 27B, including tool result round trips, IDs, usage, limits, and errors. The new gateway streaming tool response is buffered before SSE emission; add true incremental Ollama tool streaming only after backend chunk behavior is measured.
- In Kilo, verify a real file read, controlled multi-file edit, failing test diagnosis, MCP `llamafarm_agent` run ID, and independent read-only review worktree. The 2026-10-28 pass proved only model text inference and MCP discovery.
- Benchmark 8K, 16K, 32K, and 64K contexts, prefill, decode, cold load, and concurrent requests with raw Ollama and NVIDIA counters. Preserve the current config as baseline.
- Add a second GPU only after hardware is present: separate pinned workers and routing/health tests first, combined mode after both are idle.
- Test an optional Qwen3.8-Flash-Next IQ4/Q4 split n-gram GGUF on a pinned llama.cpp build with Volta CUDA support, recording RAM, VRAM, prefill, decode, and tool accuracy before offering a profile.
