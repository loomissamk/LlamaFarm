# TODO

## Goal

Finish the repo cleanup for a local-first LlamaFarm deployment built around Ollama.

## Code-Side Status (2026-03-11)

- Local runtime/tool parsing is already landing real shell execution for the malformed Ollama/Qwen cases we documented.
- Gateway-only doctor output already avoids daemon-state false errors.
- Tools page already treats Ollama runtime separately and marks browser/sqlite as optional.
- Workspace `AGENTS.md`/`SOUL.md` editing already exists in the web UI.
- Chat/session persistence and visible tool-call/tool-result transcript rendering already exist in the web UI.
- Live model switching now has an Ollama unload/rebalance path instead of just flipping the config.
- Shell/runtime cleanup is in tree: sane shell fallback logic and `sh` version probing no longer degrades into fake `Illegal option --` noise.
- Local operator profiles were widened for bigger autonomous runs:
  - `max_tool_iterations = 120`
  - `max_actions_per_hour = 10000`
  - `max_cost_per_day_cents = 100000`
- Local/dev naming cleanup is now LlamaFarm-first across the active local stack files.
- Hosted GitHub CI/release workflow files and workflow scripts were removed from the branch so the repo matches the local-only deployment target.

## Manual Follow-Through

- Redeploy locally from this branch.
- Confirm the running config inside `/llamafarm-data/.llamafarm/config.toml` reflects the widened local profile.
- Recheck dashboard, tools, doctor, agent chat, model switch, and workspace editor in the live app.

## Notes

- Stable runtime identifiers that still control the binary, env vars, config path resolution, or filesystem layout were not blindly renamed, because breaking those would make the local stack stop booting.
- The remaining validation is operational, not code-authoring.
