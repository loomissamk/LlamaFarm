# LlamaFarm client profiles

These profiles select which model the MCP stdio client asks the remote
LlamaFarm gateway to use. They do not start a second Ollama server.

| Profile | Model | Intended use |
|---|---|---|
| `fast` | `qwen3.5:9b` | short delegation, planning, code inspection, quick second opinions |
| `heavy` | `qwen3.8:27b` | harder reasoning, longer synthesis, final review |

The V100 deployment currently keeps one model resident. Its saved default is
Qwen 3.5 9B; the 27B profile remains available but using it reloads 27B and
evicts 9B. Model switching can therefore incur a cold-load pause.

`llamafarm_delegate_batch` sends up to four independent text requests.
`llamafarm_agent_batch` sends up to four independent tool-capable `/agent`
requests. Both dispatch requests concurrently, but Ollama 0.40.8 reports that
its `qwen35` runner does not support parallel requests, so model decoding
queues on the V100. Each agent result must be checked separately.

The gateway currently controls reasoning globally. The MCP client's
`max_tokens` argument is sent for forward compatibility, but the deployed
OpenAI-compatible gateway must explicitly propagate that field before it can
bound generation per MCP call.
