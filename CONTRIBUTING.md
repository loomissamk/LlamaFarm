# Contributing to LlamaFarm

LlamaFarm welcomes focused fixes, tests, documentation, and trait-based extensions.

## Development setup

1. Fork and clone the repository.
2. Create a dedicated worktree on a non-`main` branch for one concern.
3. Read the relevant trait, factory wiring, adjacent tests, and `AGENTS.md`.
4. Implement the smallest coherent change.

Open a PR against `main` using the PR template (`dev` is used only when maintainers explicitly request integration batching).

## Required validation

Run the checks that match the changed surface:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D clippy::correctness
cargo test --locked
```

For dashboard changes:

```bash
cd web
npm ci
npm test
npm run build
```

For documentation-site changes:

```bash
cd site
npm ci
npm run build
```

The `CI Required Gate` checks formatting for changed Rust files, runs Clippy and
tests, and enforces the dashboard test/build contract. Documentation-site builds
are local-only; the repository has no automatic documentation publishing or
deployment workflow.

## Pull request checklist

- Keep one concern per branch and commit.
- Use a Conventional Commit title.
- Describe the problem, implementation, non-goals, validation, side effects, and rollback.
- Update user-facing references when CLI, config, provider, channel, or runtime behavior changes.
- Follow `docs/i18n-guide.md` when changing shared documentation or navigation.
- Link resolved issues with `Closes #...` in the PR body.
- Preserve attribution when superseding another contributor's work.

See [PR workflow](docs/pr-workflow.md), [CI map](docs/ci-map.md), and [reviewer playbook](docs/reviewer-playbook.md) for the complete collaboration contract.
