# One-Click Bootstrap

This page defines the fastest supported path to install and initialize LlamaFarm.

Last verified: **March 13, 2026**.

## Option 0: Homebrew (macOS/Linuxbrew)

```bash
brew install llamafarm
```

## Option A (Recommended): Clone + local script

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./bootstrap.sh
```

Windows PowerShell equivalent:

```powershell
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
.\bootstrap.ps1
```

What it does by default:

1. `cargo build --release --locked`
2. `cargo install --path . --force --locked`

### Resource preflight and pre-built flow

Source builds typically require at least:

- **2 GB RAM + swap**
- **6 GB free disk**

When resources are constrained, bootstrap now attempts a pre-built binary first.

```bash
./bootstrap.sh --prefer-prebuilt
```

To require binary-only installation and fail if no compatible release asset exists:

```bash
./bootstrap.sh --prebuilt-only
```

To bypass pre-built flow and force source compilation:

```bash
./bootstrap.sh --force-source-build
```

## Dual-mode bootstrap

Default behavior is **app-only** (build/install LlamaFarm) and expects existing Rust toolchain.

For fresh machines, enable environment bootstrap explicitly:

```bash
./bootstrap.sh --install-system-deps --install-rust
```

Notes:

- `--install-system-deps` installs compiler/build prerequisites (may require `sudo`).
- `--install-rust` installs Rust via `rustup` when missing.
- `--prefer-prebuilt` tries release binary download first, then falls back to source build.
- `--prebuilt-only` disables source fallback.
- `--force-source-build` disables pre-built flow entirely.
- On Windows, use `bootstrap.ps1` (`-InstallRust`, `-PreferPrebuilt`, `-PrebuiltOnly`, `-ForceSourceBuild`).

## Option B: Remote one-liner

```bash
curl -fsSL https://raw.githubusercontent.com/llamafarm-labs/llamafarm/main/scripts/bootstrap.sh | bash
```

For high-security environments, prefer Option A so you can review the script before execution.

Legacy compatibility:

```bash
curl -fsSL https://raw.githubusercontent.com/llamafarm-labs/llamafarm/main/scripts/install.sh | bash
```

This legacy endpoint prefers forwarding to `scripts/bootstrap.sh` and falls back to legacy source install if unavailable in that revision.

If you run Option B outside a repository checkout, the bootstrap script automatically clones a temporary workspace, builds, installs, and then cleans it up.

## Option C: Bundled local runtime (LlamaFarm + Ollama + Chromium)

Use this when you want the full local stack in one Docker container with the
web UI, internal Ollama runtime, bundled Chromium/chromedriver, and automatic
model pulls.

Clone the repo, then run:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle.sh
```

The launcher auto-detects the best backend in this order:

1. NVIDIA / CUDA
2. AMD ROCm
3. Vulkan (`/dev/dri`)
4. CPU

For a gaming laptop or desktop where you explicitly want the NVIDIA path:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle-nvidia.sh
```

What this bundled stack does:

1. builds the local `llamafarm-local:bundle` image
2. starts `LlamaFarm` on `http://127.0.0.1:42617`
3. starts internal Ollama on `127.0.0.1:11434`
4. starts internal Chromium WebDriver on `127.0.0.1:9515`
5. starts a virtual X display and verifies it with a real screenshot capture
6. builds the advertised PDF reader with `rag-pdf` support
7. reports the source commit and UTC build time through the status/federation APIs
8. optionally pulls every model listed in `OLLAMA_PULL_MODELS`

Useful follow-ups:

```bash
docker logs -f LlamaFarm
docker exec LlamaFarm ollama list
docker exec LlamaFarm sh -lc 'tail -n 80 /tmp/ollama.log'
```

## Optional onboarding modes

### Containerized onboarding (Docker)

```bash
./bootstrap.sh --docker
```

This builds a local LlamaFarm image and launches onboarding inside a container while
persisting config/workspace to `./.llamafarm-docker`.

Container CLI defaults to `docker`. If Docker CLI is unavailable and `podman` exists,
bootstrap auto-falls back to `podman`. You can also set `LLAMAFARM_CONTAINER_CLI`
explicitly (for example: `LLAMAFARM_CONTAINER_CLI=podman ./bootstrap.sh --docker`).

For Podman, bootstrap runs with `--userns keep-id` and `:Z` volume labels so
workspace/config mounts remain writable inside the container.

If you add `--skip-build`, bootstrap skips local image build. It first tries the local
Docker tag (`LLAMAFARM_DOCKER_IMAGE`, default: `llamafarm-bootstrap:local`); if missing,
it pulls `ghcr.io/llamafarm-labs/llamafarm:latest` and tags it locally before running.

### Quick onboarding (non-interactive)

```bash
./bootstrap.sh --onboard --api-key "sk-..." --provider openrouter
```

Or with environment variables:

```bash
LLAMAFARM_API_KEY="sk-..." LLAMAFARM_PROVIDER="openrouter" ./bootstrap.sh --onboard
```

### Interactive onboarding

```bash
./bootstrap.sh --interactive-onboard
```

## Useful flags

- `--install-system-deps`
- `--install-rust`
- `--skip-build` (in `--docker` mode: use local image if present, otherwise pull `ghcr.io/llamafarm-labs/llamafarm:latest`)
- `--skip-install`
- `--provider <id>`

See all options:

```bash
./bootstrap.sh --help
```

## Related docs

- [README.md](../README.md)
- [commands-reference.md](commands-reference.md)
- [providers-reference.md](providers-reference.md)
- [channels-reference.md](channels-reference.md)
