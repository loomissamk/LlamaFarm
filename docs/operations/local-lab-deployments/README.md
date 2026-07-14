# Local Lab Deployment Runbook

This runbook is for the operator-owned lab environments used to validate LlamaFarm against real Docker hosts, preserved Ollama stores, and mixed GPU/CPU nodes.

## Goals

- redeploy without losing local operator state
- reuse existing Ollama model storage whenever possible
- use NVIDIA only on hosts that actually expose a working GPU stack
- keep one repeatable acceptance smoke that proves task-plan execution works end-to-end

## Deployment Modes

### 1. Bundled runtime on NVIDIA hosts

Use this on the primary GPU boxes where LlamaFarm and Ollama run in the same container.

```bash
./scripts/docker/up-bundle-nvidia.sh
```

What this uses:

- compose file: `docker-compose.bundle.yml`
- persistent volume: `llamafarm-bundle-data`
- data mount inside container: `/llamafarm-data`
- default UI/API port: `42617`
- companion UI container: `OpenWebUI`

What not to do:

- do not run `docker compose down -v`
- do not delete `llamafarm-bundle-data` unless the goal is a full reset

Why:

- the bundled container keeps Ollama models under `/llamafarm-data/.ollama`
- preserving the named volume avoids re-pulling large models such as `gpt-oss:120b`

### 2. CPU-only runtime with host Ollama

Use this on low-power nodes where LlamaFarm should stay containerized but Ollama is already managed outside the bundle.

```bash
docker compose up -d --build
```

What this uses:

- compose file: `docker-compose.yml`
- persistent volume: `llamafarm-data`
- container name: `LlamaFarm`

Requirements:

- host Ollama must already be reachable at `http://127.0.0.1:11434`
- do not wipe the host Ollama volume/store during LlamaFarm redeploys

## Verification

### Health

```bash
curl -fsS http://127.0.0.1:42617/health
docker ps --filter name=LlamaFarm
```

### Volume reuse

Bundled mode:

```bash
docker inspect LlamaFarm --format '{{json .Mounts}}'
docker volume inspect llamafarm-bundle-data
docker exec LlamaFarm ollama list
```

CPU/host-Ollama mode:

```bash
docker inspect LlamaFarm --format '{{json .Mounts}}'
docker volume inspect llamafarm-data
```

### NVIDIA confirmation

Run only on GPU hosts:

```bash
docker exec LlamaFarm nvidia-smi -L
docker inspect LlamaFarm --format '{{json .HostConfig.DeviceRequests}}'
```

Expected outcome:

- the container sees the intended NVIDIA GPU
- Docker device requests are present

### Remote GPU host via SSH

Use this when the target box is a separate operator-managed GPU host on the
same trusted LAN/WLAN, for example `192.168.1.154`.

Prefer a full IP or a real SSH host alias. Do not rely on shorthand aliases
such as `154` unless they are already known-good in the local SSH config.

```bash
export HOST=192.168.1.154
ssh "$HOST" "hostname"
ssh "$HOST" "docker ps --format 'table {{.Names}}\t{{.Status}}\t{{.Image}}\t{{.Ports}}' | sed -n '1,8p'"
ssh "$HOST" "curl -fsS http://127.0.0.1:42617/health"
ssh "$HOST" "docker inspect LlamaFarm --format '{{json .Mounts}}'"
ssh "$HOST" "docker exec LlamaFarm nvidia-smi -L"
ssh "$HOST" "docker exec LlamaFarm grep -n 'request_timeout_secs' /llamafarm-data/.llamafarm/config.toml"
```

Expected outcome:

- `LlamaFarm` is up and healthy on the remote host
- the remote bundle still mounts `llamafarm-bundle-data` at `/llamafarm-data`
- `nvidia-smi -L` works inside the container
- the remote gateway timeout matches the workload the box is expected to handle

## Acceptance Smoke

Use this prompt against `/agent` after each GPU-host redeploy. The runtime must create a task plan first, then actually execute the plan instead of stopping at the plan summary or asking for a `plan_id`.

Prompt:

```text
Use task_plan first because this request is multi-step. Then complete the plan end-to-end using real tools: create a directory named rust_kernel in the workspace, run pwd and ls -d rust_kernel to verify it exists, delete the directory, and answer with the verification output plus whether cleanup succeeded. Do not stop after planning.
```

One simple way to run it:

```bash
curl -sS http://127.0.0.1:42617/agent \
  -H 'content-type: application/json' \
  -d @- <<'JSON'
{"message":"Use task_plan first because this request is multi-step. Then complete the plan end-to-end using real tools: create a directory named rust_kernel in the workspace, run pwd and ls -d rust_kernel to verify it exists, delete the directory, and answer with the verification output plus whether cleanup succeeded. Do not stop after planning."}
JSON
```

Pass criteria:

- first turn creates a `task_plan`
- later turns emit real tool calls
- final answer includes verification output and cleanup status
- the model does not loop on the same tool and does not ask for a `plan_id`

### Model capability note

This smoke is intentionally stricter than a plain health check. A healthy Docker
deployment can still fail this acceptance smoke if the active default model is
too weak at multi-step tool follow-through.

One concrete failure mode is:

```text
Provider error: Model repeated intent text without a tool call after 1 retries
```

If that happens:

- treat the Docker deployment itself as healthy if `/health`, mounts, and
  `docker ps` checks already passed
- switch the box to a stronger default local model for agentic work
- rerun the same acceptance smoke before calling the host good

Recommended stronger local defaults for this smoke:

- `qwen3.5:9b`
- `gpt-oss:20b`

### Long-running local-model note

On slower local models, long `/agent` requests can legitimately run for several
minutes. The acceptance smoke above is only valid if the gateway timeout is high
enough for the active model.

Check the effective timeout:

```bash
docker exec LlamaFarm grep -n "request_timeout_secs" /llamafarm-data/.llamafarm/config.toml
```

If the box is expected to handle longer coding/research turns, raise
`[gateway].request_timeout_secs` before restarting the container. For
interactive operator sessions, prefer the web UI or `/ws/chat` so progress is
visible while the tool loop runs.

## Rollback

If a redeploy regresses runtime behavior:

1. return to the previous known-good commit
2. redeploy with the same mode as before
3. keep the existing named volume in place
4. rerun the acceptance smoke before declaring recovery complete
