# Main Branch Delivery Flows

This document describes the executable GitHub Actions baseline in this repository.
It intentionally lists only workflows that exist under `.github/workflows/`.

Use this with:

- [`docs/ci-map.md`](../../docs/ci-map.md)
- [`docs/operations/required-check-mapping.md`](../../docs/operations/required-check-mapping.md)
- [`docs/operations/docs-deploy-runbook.md`](../../docs/operations/docs-deploy-runbook.md)

## Workflow Inventory

| Workflow | Events | Outcome |
| --- | --- | --- |
| `ci-run.yml` | push, pull request, merge queue, manual | Rust format/Clippy/tests and web tests/build |
| `docs-deploy.yml` | docs/site pull request, `main` push, manual | docs-site build; Pages deploy from `main` |

No release or container-publish workflow is part of this executable baseline.

## Pull Requests

### PR to `main` or `dev`

`ci-run.yml` runs three independent validation jobs:

1. `Rust Format and Clippy`
2. `Rust Tests`
3. `Web Tests and Build`

`CI Required Gate` succeeds only when all three jobs succeed. Configure branch
protection or rulesets against this stable aggregate check name.

### Docs or site PR to `main`

`docs-deploy.yml` builds the Pages bundle and verifies that
`site/src/generated/docs-manifest.json` is current. Pull requests do not upload or
deploy a Pages artifact.

## Push and Merge Queue

- A push to `main` or `dev` runs `ci-run.yml`.
- A merge-queue candidate for `main` or `dev` runs `ci-run.yml`.
- A matching docs/site push to `main` also runs `docs-deploy.yml`.
- The docs workflow deploys only when the run is on `refs/heads/main` and is not
  a pull request.

## Manual Runs

- `ci-run.yml` may be dispatched from any branch for validation.
- `docs-deploy.yml` may be dispatched manually, but its deployment job runs only
  when the selected ref is `main`.

## Pages Prerequisites

Repository **Settings > Pages > Build and deployment > Source** must be set to
**GitHub Actions**. The workflow builds with a repository-derived Vite base path,
uploads the generated `gh-pages/` directory, and deploys it through the
`github-pages` environment.

## Local Reproduction

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D clippy::correctness
cargo test --locked

cd web
npm ci
npm test
npm run build

cd ../site
npm ci
VITE_BASE_PATH=/llamafarm/ npm run build
```

The CI formatting job is incremental while the repository-wide formatting
baseline is being reconciled: it checks changed Rust files with Rust 1.92.0.

## Change Contract

When a workflow or stable job name changes:

1. update this document and [`docs/ci-map.md`](../../docs/ci-map.md);
2. update [`required-check-mapping.md`](../../docs/operations/required-check-mapping.md);
3. validate workflow syntax with `actionlint`;
4. update repository branch rules if a required check name changed.
