# LlamaFarm client profiles

These profiles select the model for MCP chat calls. Agent calls use the
LlamaFarm gateway's saved default model. Profiles do not start a second
Ollama server.

| Profile | Model | Intended use |
|---|---|---|
| `fast` | `qwen3.5:9b-256k` | short delegation, planning, code inspection, quick second opinions |
| `heavy` | `qwen3.8-unqwen:27b-256k` | harder reasoning, longer synthesis, final review |

The single V100 can keep one 27B model resident. The gateway's saved default
is UnQwen 3.8 27B; switching to the fast profile can cause a cold-load pause.

`llamafarm_delegate_batch` sends up to four independent text requests.
`llamafarm_agent_batch` sends up to four independent tool-capable `/agent`
requests. Both dispatch requests concurrently, but Ollama 0.40.8 reports that
its `qwen35` runner does not support parallel requests, so model decoding
queues on the V100. Each agent result must be checked separately.

The gateway currently controls reasoning globally. The MCP client's
`max_tokens` argument is sent for forward compatibility, but the deployed
OpenAI-compatible gateway must explicitly propagate that field before it can
bound generation per MCP call.
