# CI Workflow Map

This page is the current map of executable GitHub Actions. Historical workflow
names in older project notes are not active unless a matching file exists under
`.github/workflows/`.

For event-by-event behavior, see
[`.github/workflows/main-branch-flow.md`](../.github/workflows/main-branch-flow.md).

## Executable Workflow Baseline

| Workflow | Trigger | Primary result |
| --- | --- | --- |
| `ci-run.yml` | push/PR/merge queue for `main` and `dev`; manual | core Rust and web validation |
| `docs-deploy.yml` | docs/site PR to `main`; matching `main` push; manual | Pages build and `main` deployment |

## Core CI Contract

`.github/workflows/ci-run.yml` runs:

- changed-Rust-file formatting with Rust 1.92.0;
- `cargo clippy --locked --all-targets -- -D clippy::correctness`;
- `cargo test --locked`;
- `npm ci`, `npm test`, and `npm run build` in `web/`.

The stable aggregate check is `CI Required Gate`. It fails when any Rust lint,
Rust test, or web job fails.

Formatting is incremental because the existing tree has repository-wide
rustfmt drift. New and modified Rust files remain gated without turning baseline
cleanup into an unrelated blocker.

## Docs Pages Contract

`.github/workflows/docs-deploy.yml`:

- builds `site/` with Node.js 22;
- derives the Vite base path from the repository name;
- verifies the generated docs manifest is committed;
- builds but does not deploy on pull requests;
- uploads and deploys `gh-pages/` only from `main`.

The repository Pages source must be configured as **GitHub Actions**. See the
[docs deploy runbook](operations/docs-deploy-runbook.md) for setup, diagnosis,
and rollback steps.

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
git diff --exit-code -- src/generated/docs-manifest.json
```

To reproduce the incremental format lane, run `rustfmt --edition 2021 --check`
against only the changed `.rs` paths.

## Fast Triage

1. For `CI Required Gate`, open `ci-run.yml` and inspect the first failed
   dependency job.
2. For `Build Docs Site`, reproduce the `site/` build and check whether the docs
   manifest changed.
3. For Pages deployment, verify the run is on `main`, the build job succeeded,
   and the Pages source is set to **GitHub Actions**.
4. For asset paths, inspect the generated HTML under `gh-pages/` and confirm its
   URLs include the configured repository base path.

## Maintenance Rules

- Keep `CI Required Gate` stable or coordinate a branch-rules update.
- Keep Rust and Node versions explicit and consistent with local tooling.
- Pin GitHub Actions to immutable revisions.
- Keep the docs manifest generated and committed with source-doc changes.
- Update this map, its localized counterparts, and the required-check mapping
  whenever the executable workflow inventory changes.
