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

4. Harden the local-box agent loop for incomplete and resumed tasks.
   Keep this pass model-agnostic first; use provider-specific switches only if the generic loop still fails.
   Add a stricter recovery path when the model emits incomplete internal fallback text instead of a real action.
   If the first corrective retry still produces no real tool call, escalate cleanly instead of hanging the request.
   Add a fast-fail path for long stalled follow-up turns so `/agent` does not sit on an empty response for 60-90 seconds.
   Preserve the last grounded tool result and the next expected action so resumed chats continue instead of restarting.

5. Stop leaking provider fallback text to the user.
   In `src/providers/ollama.rs`, stop turning thinking-only / empty-content responses into user-visible placeholder text.
   Return a structured incomplete-response signal, or a retryable provider error, so the loop can recover intentionally.
   Keep the runtime trace explicit about whether the failure was parse-related, tool-followthrough-related, or provider-stall-related.

6. Reduce local Ollama instability under real tool-use load.
   Add per-model or per-provider concurrency limits so multiple local `/agent` requests do not trigger `500 EOF` churn.
   Add server-side turn timeout and cancellation propagation for stalled Ollama calls.
   Surface clear retry/backoff telemetry for local models in logs and traces.
   Consider a local-Ollama-only no-thinking / reasoning-off tool-turn mode as a last-resort experiment, not the default path.

7. Add a focused local-agent smoke suite.
   Cover the basic create-file / compute / delete-file task flow.
   Cover a simple real shell task like `lsusb`.
   Cover `rust_kernel/src` inspection so raw pseudo-tool markup is never surfaced again.
   Cover resumed-chat continuation so a saved local chat can continue an unfinished coding task.
   Cover a multi-step coding prompt that should start with a task list or planning step when appropriate.

8. Keep extending parser and tool-dispatch compatibility.
   Add more regression cases for direct tool tags with attributes, malformed closers, and mixed inner-text payloads.
   Add regression coverage for incomplete internal fallback text plus followthrough retry.
   Keep native-tool, XML-tool, and pseudo-tool recovery behavior aligned across the same turn loop.

9. Improve local efficiency and observability.
   Record which dispatcher path was used, why a retry happened, and how many iterations were spent before success/failure.
   Add visibility for in-flight local model requests, Ollama runner health, and recent `EOF` failures in the dashboard/log flow.
   Add adaptive retry/iteration policy so simple tasks complete quickly while longer tasks still get enough loop budget.
