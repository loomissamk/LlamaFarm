# TODO

## Goal

Finish turning this repo into a local-first, Ollama-only `LlamaFarm` deployment.

## Current State (2026-03-11)

- Local Docker deployment is up as container `LlamaFarm`.
- The live stack now runs from the current repo compose file with stable local names:
  `llamafarm-local` project, `llamafarm-local:dev` image, and `llamafarm-data`
  persistent volume.
- The old `LlamaFarm` container from the legacy `llamafarm-local-todo` worktree
  was removed after cutover. The old `llamafarm-local-todo_data` volume is still
  present as rollback data.
- Local web UI is reachable on `http://127.0.0.1:42617`.
- Smoke checks passed for `/health`, `/api/status`, `/api/tools`,
  `/api/cli-tools`, `/api/doctor`, and the root HTML shell title.
- Current locally smoke-tested Ollama models: `devstral-small-2:latest` and
  `qwen3.5:9b`.
- Visible app branding is now `LlamaFarm`.
- The current pause point is tool/runtime debugging in the live local app.

## Done

- Pushed a checkpoint of the current branch/worktree to GitHub.
- Renamed the visible web shell branding from `ZeroClaw` to `LlamaFarm`.
- Updated the local Docker/dev config for a local Ollama-first setup.
- Set the compose container name to `LlamaFarm`.
- Rebuilt and redeployed the local container successfully.
- Switched the local compose deployment to stable project/image/volume naming and
  migrated the live workspace into `llamafarm-data`.
- Fixed the containerized config bootstrap so
  `/zeroclaw-data/.zeroclaw/config.toml` is created with mode `600`.
- Documented the currently smoke-tested Ollama models in `dev/README.md`.
- Shifted the dashboard/models experience toward local Ollama runtime status.
- Removed the visible language selector and pairing-focused shell UI from the current web app.
- Hardened Ollama/Qwen tool-call recovery for malformed local-tool outputs:
  raw JSON tool objects, `shell("...")`, bare commands like `lsusb`,
  narrative shell fences, `json{shell(...)}`, and `'''bash` shell fences.
- Rebuilt and redeployed the local `LlamaFarm` container with the widened tool parser.
- Confirmed the ugly `lsusb` prompt now reaches real `shell` execution in the live local app instead of stopping at fake JSON/plain-text tool output.

## Resume Later
- [ ] Rename all zeroclaw to llamafarm
  - rename all legacy code with zeroclaw to be LlamaFarm
  - if ZEROCLAW then LLAMAFARM, elif zeroclaw llamafarm, else Zero_Claw then Llama_Farm and so on

- [ ] Fix shell/runtime diagnostics for the local deployment.
  - Set a sane default `SHELL` inside the container/runtime add users shell by defualt. add space to add other in text box that will run.
  - Stop showing daemon-state errors in gateway-only mode.
  - Fix CLI discovery so `sh --version` does not show `Illegal option --` as a fake version.

- [ ] Fix the Tools page health cards.
  - Remove or reword misleading `missing locally` checks.
  - Treat Ollama runtime health separately from Ollama CLI presence.
  - Decide whether `sqlite3` and browser binaries should be installed or shown as optional.

- [ ] Fix web agent tool execution.
  - Finish the final-answer follow-through after successful live tool execution, especially on the web/WebSocket chat path.
  - Revisit the local autonomy/tool allowlist defaults. Allow everything!!! or just most linux, mac, windows 
  - Retest `lsusb`, shell, scheduler, and file tools end-to-end in the live app. ensure real outputs

- [ ] Replace the stale local config/TOML with a clean local-operator profile.
  - Remove foreign/non-ASCII characters from the live TOML path and keep the local config ASCII-only.
  - Prune outdated config sections that no longer help the local-first deployment.
  - Expand the practical tool/autonomy defaults toward the intended full local "god mode" agent profile.
  - Verify the refreshed config is what the running app actually uses after reload/redeploy.

- [ ] Add a simple UI editor for `SOUL.md` and `AGENTS.md`.
  - Add a dedicated page with tabs for both files. More if it makes sense....
  - Load and save the workspace copies live from the UI. These need to work instantly on save
  - Keep the scope limited to those two files. Unless more make sense

- [ ] Add basic chat/session management in the web UI.
  - Show prior conversations or saved sessions instead of a single transient chat thread.
  - Add explicit new chat / clear chat controls.
  - Keep tool-call and tool-result messages visible and readable in the transcript.

- [ ] Decide and implement Ollama model unload behavior on model switch.
  - Verify the actual Ollama unload/stop path first. Literally tell ollamas to stop old model if able...
  - Make switching prefer the selected model being the only loaded one if the runtime supports it cleanly.

- [ ] Remove GitHub CI/workflow overhead that is not needed for this local-only deployment.
  - Drop hosted/cloud-oriented workflow noise. Reduce noise in ui about local as well, just Ollama.
  - Keep local Docker build/run/smoke flow as the primary path.

- [ ] Run a final local smoke pass after the above fixes.
  - Rebuild `LlamaFarm`.
  - Verify dashboard, tools, doctor, and agent pages.
  - Confirm container name, shell tooling, model switching, and docs-editor behavior.

## Notes

- Rust test/CI cleanup is intentionally deferred until the local runtime/tooling path is stable. We just want to remove CI if able as it is no longer truly needed for local deploymenet we just want all tools not gitlab junk calls and dumb emails about github runners failing ci since we are never going to use that (for now)
- Do not reopen older branding/pairing cleanup tasks unless a live page still shows them.
- ensure all functionality works with smoke tests for accuracy, ollama -> LlamaFarm tool -> expected accurate result
