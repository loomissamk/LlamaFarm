# LlamaFarm Operations Runbook

This runbook is for operators who maintain availability, security posture, and incident response.

Last verified: **February 18, 2026**.

## Scope

Use this document for day-2 operations:

- starting and supervising runtime
- health checks and diagnostics
- safe rollout and rollback
- incident triage and recovery

For first-time installation, start from [one-click-bootstrap.md](one-click-bootstrap.md).
For opt-in container-to-host lifecycle control, see
[host-runner.md](host-runner.md).

## Runtime Modes

| Mode | Command | When to use |
|---|---|---|
| Foreground runtime | `llamafarm daemon` | local debugging, short-lived sessions |
| Foreground gateway only | `llamafarm gateway` | webhook endpoint testing |
| User service | `llamafarm service install && llamafarm service start` | persistent operator-managed runtime |

## Baseline Operator Checklist

1. Validate configuration:

```bash
llamafarm status
```

2. Verify diagnostics:

```bash
llamafarm doctor
llamafarm channel doctor
```

3. Start runtime:

```bash
llamafarm daemon
```

4. For persistent user session service:

```bash
llamafarm service install
llamafarm service start
llamafarm service status
```

## Health and State Signals

| Signal | Command / File | Expected |
|---|---|---|
| Config validity | `llamafarm doctor` | no critical errors |
| Channel connectivity | `llamafarm channel doctor` | configured channels healthy |
| Runtime summary | `llamafarm status` | expected provider/model/channels |
| Daemon heartbeat/state | `~/.llamafarm/daemon_state.json` | file updates periodically |

## Logs and Diagnostics

### macOS / Windows (service wrapper logs)

- `~/.llamafarm/logs/daemon.stdout.log`
- `~/.llamafarm/logs/daemon.stderr.log`

### Linux (systemd user service)

```bash
journalctl --user -u llamafarm.service -f
```

## Incident Triage Flow (Fast Path)

1. Snapshot system state:

```bash
llamafarm status
llamafarm doctor
llamafarm channel doctor
```

2. Check service state:

```bash
llamafarm service status
```

3. If service is unhealthy, restart cleanly:

```bash
llamafarm service stop
llamafarm service start
```

4. If channels still fail, verify allowlists and credentials in `~/.llamafarm/config.toml`.

5. If gateway is involved, verify bind/auth settings (`[gateway]`) and local reachability.

## Secret Leak Incident Response (CI Gitleaks)

When `sec-audit.yml` reports a gitleaks finding or uploads SARIF alerts:

1. Confirm whether the finding is a true credential leak or a test/doc false positive:
   - review `gitleaks.sarif` + `gitleaks-summary.json` artifacts
   - inspect changed commit range in the workflow summary
2. If true positive:
   - revoke/rotate the exposed secret immediately
   - remove leaked material from reachable history when required by policy
   - open an incident record and track remediation ownership
3. If false positive:
   - prefer narrowing detection scope first
   - only add allowlist entries with explicit governance metadata (`owner`, `reason`, `ticket`, `expires_on`)
   - ensure the related governance ticket is linked in the PR
4. Re-run `Sec Audit` and confirm:
   - gitleaks lane green
   - governance guard green
   - SARIF upload succeeds

## Safe Change Procedure

Before applying config changes:

1. backup `~/.llamafarm/config.toml`
2. apply one logical change at a time
3. run `llamafarm doctor`
4. restart daemon/service
5. verify with `status` + `channel doctor`

## Rollback Procedure

If a rollout regresses behavior:

1. restore previous `config.toml`
2. restart runtime (`daemon` or `service`)
3. confirm recovery via `doctor` and channel health checks
4. document incident root cause and mitigation

## Related Docs

- [one-click-bootstrap.md](one-click-bootstrap.md)
- [troubleshooting.md](troubleshooting.md)
- [config-reference.md](config-reference.md)
- [commands-reference.md](commands-reference.md)
