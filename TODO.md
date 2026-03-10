# TODO

## Goal

Finish turning this repo into a local-first, Ollama-only `LlamaFarm` deployment.

## Current State (2026-03-10)

- Local Docker deployment is up as container `LlamaFarm`.
- Local web UI is reachable on `http://127.0.0.1:42617`.
- Visible app branding is now `LlamaFarm`.
- The current pause point is tool/runtime debugging in the live local app.

## Done

- Pushed a checkpoint of the current branch/worktree to GitHub.
- Renamed the visible web shell branding from `ZeroClaw` to `LlamaFarm`.
- Updated the local Docker/dev config for a local Ollama-first setup.
- Set the compose container name to `LlamaFarm`.
- Rebuilt and redeployed the local container successfully.
- Shifted the dashboard/models experience toward local Ollama runtime status.
- Removed the visible language selector and pairing-focused shell UI from the current web app.

## Resume Later
- [ ] Rename all zeroclaw to llamafarm
  - rename all legacy code with zeroclaw to be LlamaFarm
  - if ZEROCLAW then LLAMAFARM, elif zeroclaw llamafarm, else Zero_Claw then Llama_Farm and so on

- [ ] Fix shell/runtime diagnostics for the local deployment.
  - Set a sane default `SHELL` inside the container/runtime.
  - Stop showing daemon-state errors in gateway-only mode.
  - Fix CLI discovery so `sh --version` does not show `Illegal option --` as a fake version.

- [ ] Fix the Tools page health cards.
  - Remove or reword misleading `missing locally` checks.
  - Treat Ollama runtime health separately from Ollama CLI presence.
  - Decide whether `sqlite3` and browser binaries should be installed or shown as optional.

- [ ] Fix web agent tool execution.
  - The live agent should execute shell/tool calls instead of only describing JSON calls.
  - Revisit the local autonomy/tool allowlist defaults.
  - Retest `lsusb`, shell, scheduler, and file tools end-to-end in the live app.

- [ ] Add a simple UI editor for `SOUL.md` and `AGENTS.md`.
  - Add a dedicated page with tabs for both files.
  - Load and save the workspace copies live from the UI.
  - Keep the scope limited to those two files.

- [ ] Decide and implement Ollama model unload behavior on model switch.
  - Verify the actual Ollama unload/stop path first.
  - Make switching prefer the selected model being the only loaded one if the runtime supports it cleanly.

- [ ] Remove GitHub CI/workflow overhead that is not needed for this local-only deployment.
  - Drop hosted/cloud-oriented workflow noise.
  - Keep local Docker build/run/smoke flow as the primary path.

- [ ] Run a final local smoke pass after the above fixes.
  - Rebuild `LlamaFarm`.
  - Verify dashboard, tools, doctor, and agent pages.
  - Confirm container name, shell tooling, model switching, and docs-editor behavior.

## Notes

- Rust test/CI cleanup is intentionally deferred until the local runtime/tooling path is stable.
- Do not reopen older branding/pairing cleanup tasks unless a live page still shows them.
