# TODO

1. Make Safe and God full runtime bundles.
   Keep the TOML, `AGENTS.md`, and `SOUL.md` presets visibly linked in the UI.
   Add bundle diff/dirty state so users can see when config and persona files are out of sync.

2. Push deeper into Ollama-native local operation.
   Keep the runtime universal across Ollama models while leaving Qwen as the local default on this machine.
   Add better model guidance, live model switching, and clearer Ollama health/load visibility in the dashboard.

3. Keep widening Ollama tool-call compatibility and post-tool follow-through.
   Accept more malformed bracket/JSON/fence/function payloads from current Ollama models.
   Keep improving quoted-heredoc, redirect, and follow-up script execution paths for local models.
   Reduce the long second-turn stall after successful tool execution so `/agent` returns faster and more reliably.

4. Lock down a core `/agent` smoke matrix for both cloud and local models.
   Keep a stable prompt set for `task_plan`, `web_search_tool`, `file_write`, `file_read`, and Python-via-`shell`.
   Turn the March 17, 2026 prompts and outputs in `docs/tool-call-examples/README.md` into a repeatable validation script.
   Latest live rerun from this chat on March 17, 2026:
   `devstral-2:123b-cloud`
   - `task_plan`: pass, turn `37a77442-910a-487f-90ff-2f8c6dc5c72b`
   - `web_search_tool`: pass, turn `5a0f3cfc-e23e-42bb-9ed4-371cff0eb5ac`
   - `file_write`: flaky fail, turn `b39fa563-50c2-4202-9967-364677730d17`
     The tool executed `file_write`, then the model replied with JSON-like success text instead of a clean final answer.
     Gateway returned `502` with `Model repeatedly deferred action without emitting a tool call`.
   - `file_read`: pass, turn `ea8ec53f-e63b-4975-a3ea-c56fe55e7642`
   - `python`: functional pass, turn `452c1e74-0ada-4c1a-8d0b-0432b1a86485`
     The task completed, but the model spammed extra `task_plan` calls and hit duplicate-tool recovery on the way.
   `qwen3.5:9b`
   - `task_plan`: fail/partial, turn `ead1beff-6673-4e7f-9fd4-ef7de3adec63`
     The model emitted `task_plan` with an empty action, got `Unknown action ''`, then asked the user to clarify instead of repairing the call.
   - `web_search_tool`: pass, turn `ce195f51-d40a-415d-a9b3-ff5ad7a02ffa`
   - `file_write`: partial fail, turn `663a4b6a-5111-4d30-bd56-86f06859dd14`
     The tool wrote the requested 15 bytes, but the final answer claimed the file content was `"Hello"`.
   - `file_read`: partial fail, turn `11f2518c-d651-49bf-b481-160a309c0c52`
     The tool returned the right file contents, but the final answer hallucinated that the result was not a valid tool.
   - `python`: functional pass, turn `33cd7c88-7324-435b-bdfa-6dfd69300f70`
     The model used `file_write` plus `shell`, but the answer stayed generic and weakly grounded in the actual tool outputs.
   Next validation expansion:
   Run the same matrix against other local Ollama options on this box, at minimum `devstral-small-2:latest` and `qwen2.5-coder:14b`, once the current local failures are stabilized.

5. Fix local `qwen3.5:9b` failures that still block "Codex-like" task completion.
   `python`: recover cleanly after shell security rejection and accept the next valid `file_write` fallback instead of stalling on malformed markup.
   Update from the latest local live rerun on March 17, 2026 after the parser + grounding patches:
   - `task_plan`: improved from hard failure to soft grounding drift.
     The model now emits a real `task_plan` call and the tool succeeds, but the final answer still embellishes the created steps instead of simply echoing the exact grounded plan summary.
   - `file_write`: now passes locally.
     Final answer correctly reported `/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt`, `tool smoke qwen postpatch`, and `25 bytes`.
   - `file_read`: now passes locally.
     Final answer correctly returned `tool smoke qwen postpatch` from `/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt`.
   - `python`: still failing locally.
     The model successfully used `file_write` and multiple `shell` calls, but the `/agent` request timed out before any final answer arrived. Cleanup did appear to happen because the temporary file was absent after timeout.
   Next fixes from this rerun:
   - add a stricter synthesized final-answer mode for successful `task_plan create` turns so local models cannot wander into embellished prose after the tool already succeeded
   - make the Python smoke path stop after verified `file_write` + `shell` execution and return a grounded summary instead of waiting for another long LLM completion

6. Track cloud validation blockers explicitly.
   `devstral-2:123b-cloud` was not rerun in this patch round because the upstream cloud route had already returned a weekly-usage-limit `429 Too Many Requests` earlier in this chat.
   Keep this as a validation blocker, not a local-runtime pass.
   The next cloud retest should rerun the same core `/agent` smoke prompts once quota is available again.

7. Make local retries progress-sensitive instead of open-ended.
   Continue spending tokens when the model is making real forward progress, but cap retries quickly when the turn is repeating intent text, malformed tool markup, or duplicate tool calls.
   Add runtime trace counters for local retry cause, retry count, and final stop reason so regressions are obvious during manual testing.
   Add a sibling rule for cloud models too:
   If a tool already succeeded and the model returns JSON-ish success prose with no usable tool call, coerce that into final-answer mode instead of escalating to `502`.
   This is now a real cloud regression on `file_write` for `devstral-2:123b-cloud`.

8. Tighten final-answer grounding after successful tool calls.
   Prefer tool-result-backed answer synthesis over freeform summarization for `file_write`, `file_read`, and simple Python smoke tasks.
   For file content questions, answer directly from the tool output instead of reinterpreting tokens like `tool smoke qwen` as a request for another tool.
   For file creation confirmations, bind the reported content to the actual write payload or read-back result.

9. Reduce unnecessary planner churn on execution prompts.
   The cloud Python smoke run succeeded but injected repeated `task_plan` calls into a direct execution task.
   Add a heuristic so direct "write, run, delete" prompts do not opportunistically reopen planning once execution has already started.

10. Keep the smoke matrix runnable across both cloud and local paths when token budget gets tight.
   Always run the core five-tool matrix against one cloud model and one local model before claiming tool reliability.
   If runtime budget is limited, run the same prompt set on fewer models rather than shortening the prompts or skipping trace capture.
