# Host Runner

The host runner is an explicit opt-in bridge for a containerized LlamaFarm
agent that must manage host Docker, user services, or host files. It is a
systemd user service outside Docker and listens on an owner-only Unix socket
under the host home directory. It does not use `sudo`.

Normal tools still execute in the configured runtime. `host_exec` appears only
when `[host_runner].enabled = true` or
`LLAMAFARM_HOST_RUNNER_ENABLED=true`.

## Install

Run this from the host checkout that should be used for self-redeploys:

```bash
./scripts/install-host-runner.sh
```

The installer:

1. builds and installs a dedicated binary at
   `~/.local/libexec/llamafarm/host-runner`;
2. installs and starts `llamafarm-host-runner.service` with `systemctl --user`;
3. writes service paths and the trusted redeploy checkout to
   `~/.config/llamafarm/host-runner.env`;
4. opts the Docker bundle in through the checkout's ignored `.env`.

Recreate the bundle once after first installation:

```bash
./scripts/docker/up-bundle.sh up -d --build
```

Verify either side:

```bash
systemctl --user status llamafarm-host-runner.service
llamafarm host-runner health
journalctl --user -u llamafarm-host-runner.service -f
```

The service starts with the user's login. Starting before any login requires
administrator-managed user lingering; the installer intentionally does not
request elevated privileges.

## Tool operations

`host_exec` supports:

| Action | Purpose | Required fields |
|---|---|---|
| `health` | Check service and capability state | none |
| `exec` | Bounded foreground host command | `command`, absolute `cwd` |
| `spawn` | Durable background host command | `command`, absolute `cwd` |
| `status` | Read a durable job's state and output tails | `job_id` |
| `redeploy` | Run the configured checkout's health-gated bundle rebuild | none |

Examples:

```json
{"action":"exec","command":"docker ps","cwd":"/home/operator"}
{"action":"spawn","command":"systemctl --user restart example.service","cwd":"/home/operator"}
{"action":"status","job_id":"<id returned by spawn>"}
{"action":"redeploy"}
```

Use `redeploy` for LlamaFarm itself. The host service starts the existing
`scripts/docker/up-bundle.sh` health-gated deployment and immediately returns a
job id. The command and its result files live outside the container, so the
current LlamaFarm container can be replaced without terminating or forgetting
the job.

## Security and audit model

- Both tool registration and service installation are opt-in.
- Server socket and state paths must remain below the configured host `HOME`.
- The socket directory is mode `0700`; the socket and audit files are mode
  `0600`.
- Arbitrary `exec`/`spawn` is disabled by the server unless it starts with
  `--allow-exec`. The supplied service unit includes that flag because running
  the installer is the operator's explicit authorization.
- `host_exec` applies the same command allowlist, hard-denial, path, autonomy,
  approval, and action-rate policy as `shell`. Full autonomy does not create an
  interactive approval dependency, but hard denials still apply.
- Requests are size-bounded, identifiers reject traversal, foreground
  execution is time-bounded, and job output returned to the model is capped.
- `~/.local/state/llamafarm/host-runner/audit.jsonl` records timestamps,
  operations, outcomes, job ids, and SHA-256 command digests. It does not store
  command text or command output.

To revoke access immediately:

```bash
systemctl --user disable --now llamafarm-host-runner.service
```

Then set `LLAMAFARM_HOST_RUNNER_ENABLED=false` (or remove it) and recreate the
container.
