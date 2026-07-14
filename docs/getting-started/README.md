# Getting Started Docs

For first-time setup and quick orientation.

## Start Path

1. Main overview and quick start: [../../README.md](../../README.md)
2. One-click setup and dual bootstrap mode: [../one-click-bootstrap.md](../one-click-bootstrap.md)
3. Update or uninstall on macOS: [macos-update-uninstall.md](macos-update-uninstall.md)
4. Set up on Android (Termux/ADB): [../android-setup.md](../android-setup.md)
5. Find commands by tasks: [../commands-reference.md](../commands-reference.md)

## Choose Your Path

| Scenario | Command |
|----------|---------|
| I have an API key, want fastest setup | `llamafarm onboard --api-key sk-... --provider openrouter` |
| I want the full local bundle (UI + Ollama + Chromium) | `./scripts/docker/up-bundle.sh` |
| I want the full local bundle on an NVIDIA gaming box | `./scripts/docker/up-bundle-nvidia.sh` |
| I want guided prompts | `llamafarm onboard --interactive` |
| Config exists, just fix channels | `llamafarm onboard --channels-only` |
| Config exists, I intentionally want full overwrite | `llamafarm onboard --force` |
| Using subscription auth | See [Subscription Auth](../../README.md#subscription-auth-openai-codex--claude-code) |

## Onboarding and Validation

- Quick onboarding: `llamafarm onboard --api-key "sk-..." --provider openrouter`
- Interactive onboarding: `llamafarm onboard --interactive`
- Existing config protection: reruns require explicit confirmation (or `--force` in non-interactive flows)
- Ollama cloud models (`:cloud`) work with a local signed-in Ollama runtime (`ollama signin`) or a remote `api_url`; direct remote endpoints still require an API key.
- Validate environment: `llamafarm status` + `llamafarm doctor`

## Bundled Local Runtime

For the single-container local stack with bundled LlamaFarm, Ollama, and
Chromium:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle.sh
```

For gaming systems where you want the NVIDIA path explicitly:

```bash
git clone https://github.com/llamafarm-labs/llamafarm.git
cd llamafarm
./scripts/docker/up-bundle-nvidia.sh
```

Then open `http://127.0.0.1:42617`.

## Next

- Runtime operations: [../operations/README.md](../operations/README.md)
- Reference catalogs: [../reference/README.md](../reference/README.md)
- macOS lifecycle tasks: [macos-update-uninstall.md](macos-update-uninstall.md)
- Android setup path: [../android-setup.md](../android-setup.md)
