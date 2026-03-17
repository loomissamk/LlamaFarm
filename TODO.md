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
