# macOS Update and Uninstall Guide

This page documents supported update and uninstall procedures for LlamaFarm on macOS (OS X).

Last verified: **February 22, 2026**.

## 1) Check current install method

```bash
which llamafarm
llamafarm --version
```

Typical locations:

- Homebrew: `/opt/homebrew/bin/llamafarm` (Apple Silicon) or `/usr/local/bin/llamafarm` (Intel)
- Cargo/bootstrap/manual: `~/.cargo/bin/llamafarm`

If both exist, your shell `PATH` order decides which one runs.

## 2) Update on macOS

### A) Homebrew install

```bash
brew update
brew upgrade llamafarm
llamafarm --version
```

### B) Clone + bootstrap install

From your local repository checkout:

```bash
git pull --ff-only
./bootstrap.sh --prefer-prebuilt
llamafarm --version
```

If you want source-only update:

```bash
git pull --ff-only
cargo install --path . --force --locked
llamafarm --version
```

### C) Manual prebuilt binary install

Re-run your download/install flow with the latest release asset, then verify:

```bash
llamafarm --version
```

## 3) Uninstall on macOS

### A) Stop and remove background service first

This prevents the daemon from continuing to run after binary removal.

```bash
llamafarm service stop || true
llamafarm service uninstall || true
```

Service artifacts removed by `service uninstall`:

- `~/Library/LaunchAgents/com.llamafarm.daemon.plist`

### B) Remove the binary by install method

Homebrew:

```bash
brew uninstall llamafarm
```

Cargo/bootstrap/manual (`~/.cargo/bin/llamafarm`):

```bash
cargo uninstall llamafarm || true
rm -f ~/.cargo/bin/llamafarm
```

### C) Optional: remove local runtime data

Only run this if you want a full cleanup of config, auth profiles, logs, and workspace state.

```bash
rm -rf ~/.llamafarm
```

## 4) Verify uninstall completed

```bash
command -v llamafarm || echo "llamafarm binary not found"
pgrep -fl llamafarm || echo "No running llamafarm process"
```

If `pgrep` still finds a process, stop it manually and re-check:

```bash
pkill -f llamafarm
```

## Related docs

- [One-Click Bootstrap](../one-click-bootstrap.md)
- [Commands Reference](../commands-reference.md)
- [Troubleshooting](../troubleshooting.md)
