# LAN Federation

LAN federation lets one LlamaFarm node discover other LlamaFarm nodes on the same trusted LAN/WLAN and use them as remote subagents from the existing chat UI.

This is intentionally lightweight:
- no pairing workflow between nodes
- no trust prompts or approval codes
- no second subagent framework
- current single-node behavior stays local when federation is disabled

## Behavior

- `LLAMAFARM_FEDERATION_ENABLED=false`
  Current local-only behavior stays unchanged.
- `LLAMAFARM_FEDERATION_ENABLED=true` with no peers discovered
  The node still works normally and runs chats locally.
- `LLAMAFARM_FEDERATION_ENABLED=true` with peers discovered
  The chat UI shows LAN peers, their status, model/tool summaries, and role assignment controls.

Remote workers execute their own local model and their own local tools. The master node only receives streamed task events and the final result.

## Config

Environment variables:

```bash
LLAMAFARM_FEDERATION_ENABLED=true
LLAMAFARM_NODE_NAME=llamafarm-a
LLAMAFARM_API_PORT=42617
LLAMAFARM_DISCOVERY_MODE=mdns
LLAMAFARM_SERVICE_NAME=_llamafarm._tcp
LLAMAFARM_PEER_TIMEOUT_SECONDS=30
LLAMAFARM_MANUAL_PEERS=
LLAMAFARM_DEFAULT_ROLE=both
LLAMAFARM_ALLOW_REMOTE_SUBAGENTS=true
```

Notes:
- `LLAMAFARM_DISCOVERY_MODE=mdns` enables DNS-SD advertisement and discovery on the LAN.
- `LLAMAFARM_DISCOVERY_MODE=manual` disables mDNS browsing and uses `LLAMAFARM_MANUAL_PEERS`.
- `LLAMAFARM_DEFAULT_ROLE` sets the initial role shown in the federation panel.
- `LLAMAFARM_ALLOW_REMOTE_SUBAGENTS=false` keeps peer discovery visible but refuses remote task execution on that node.

## UI

The existing chat page adds a small federation section:
- discovered peers
- online or offline status
- node name
- delegate agent name
- model summary
- tool summary
- assigned role: `master`, `worker`, `both`, `disabled`
- per-chat worker selection
- streamed remote task status/output cards

Selecting zero workers keeps the chat local.

## Internal API

The worker-side federation API is private/LAN-only and is enabled only when federation is enabled:

- `GET /federation/health`
- `GET /federation/capabilities`
- `GET /federation/models`
- `GET /federation/tools`
- `POST /federation/tasks`
- `GET /federation/tasks/:id/stream`
- `POST /federation/tasks/:id/cancel`

The normal dashboard API still uses the existing bearer-token auth path. Federation worker endpoints trust only private/loopback callers on the local network.

## Two-Node Setup

Example:
- node A: `192.168.1.10`
- node B: `192.168.1.11`
- port: `42617`

### Native host setup

On node A:

```bash
export LLAMAFARM_FEDERATION_ENABLED=true
export LLAMAFARM_NODE_NAME=llamafarm-a
export LLAMAFARM_API_PORT=42617
export LLAMAFARM_DISCOVERY_MODE=mdns
export LLAMAFARM_DEFAULT_ROLE=both
export LLAMAFARM_ALLOW_REMOTE_SUBAGENTS=true
```

On node B:

```bash
export LLAMAFARM_FEDERATION_ENABLED=true
export LLAMAFARM_NODE_NAME=llamafarm-b
export LLAMAFARM_API_PORT=42617
export LLAMAFARM_DISCOVERY_MODE=mdns
export LLAMAFARM_DEFAULT_ROLE=worker
export LLAMAFARM_ALLOW_REMOTE_SUBAGENTS=true
```

Start LlamaFarm on both nodes. Open the web UI on node A. Node B should appear automatically in the federation panel.

### Manual peer fallback

Use this when mDNS is unavailable or filtered:

On node A:

```bash
export LLAMAFARM_FEDERATION_ENABLED=true
export LLAMAFARM_DISCOVERY_MODE=manual
export LLAMAFARM_MANUAL_PEERS=http://192.168.1.11:42617
```

On node B:

```bash
export LLAMAFARM_FEDERATION_ENABLED=true
export LLAMAFARM_DISCOVERY_MODE=manual
export LLAMAFARM_MANUAL_PEERS=http://192.168.1.10:42617
```

## Docker Notes

For cross-machine federation with Docker Compose:
- bind the gateway to the LAN, for example `HOST_BIND_IP=0.0.0.0`
- keep `LLAMAFARM_GATEWAY_PORT` and `LLAMAFARM_API_PORT` aligned
- if mDNS does not traverse your container/network setup, use `LLAMAFARM_DISCOVERY_MODE=manual`

Example:

```bash
export HOST_BIND_IP=0.0.0.0
export LLAMAFARM_FEDERATION_ENABLED=true
export LLAMAFARM_NODE_NAME=llamafarm-a
export LLAMAFARM_API_PORT=42617
docker compose up -d --build
```

## Failure Handling

- Offline or stale peers remain visible and are marked offline.
- If a remote worker task fails, the chat UI shows the failure in the remote task card.
- Operators can retry by re-running the chat with a different worker selection or with no workers selected for local execution.

## Files Changed

Main areas touched for this feature:
- `src/federation/`
  Discovery, peer registry, remote task adapter, and task streaming state.
- `src/gateway/mod.rs`
  Federation service startup and route wiring.
- `src/gateway/api.rs`
  Dashboard peer API and internal federation HTTP/SSE endpoints.
- `src/gateway/ws.rs`
  Chat worker selection and streamed remote-task event relay.
- `src/tools/delegate.rs`
  Remote peer delegation through the existing delegate tool.
- `src/tools/subagent_spawn.rs`
  Remote background subagent execution through the existing subagent flow.
- `web/src/pages/AgentChat.tsx`
  Minimal federation panel and remote task cards.
- `docker-compose.yml`, `docker-compose.bundle.yml`, `.env.example`
  Minimal operator config for LAN federation.
