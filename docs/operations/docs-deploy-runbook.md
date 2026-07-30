# Docs Deploy Runbook

Workflow: `.github/workflows/docs-deploy.yml`

## Current Lanes

- `Build Docs Site`: installs dependencies, generates the docs manifest, builds
  the Vite site, and confirms the committed manifest is current.
- `Deploy Docs to GitHub Pages`: deploys the uploaded Pages artifact from
  `main`.

Pull requests run the build lane only. Pushes and manual runs deploy only when
the workflow ref is `refs/heads/main`.

## One-Time Repository Setup

1. Open **Settings > Pages**.
2. Under **Build and deployment**, set **Source** to **GitHub Actions**.
3. Confirm the workflow may create/use the `github-pages` environment.
4. Merge the workflow to `main` before expecting manual dispatch to be
   discoverable.

## Local Build

```bash
cd site
npm ci
VITE_BASE_PATH=/llamafarm/ npm run build
git diff --exit-code -- src/generated/docs-manifest.json
```

The build output is written to the repository-root `gh-pages/` directory. Use a
different base path when the repository slug differs:

```bash
VITE_BASE_PATH=/repository-name/ npm run build
```

## Normal Deployment

1. Merge a docs/site change to `main`, or manually dispatch `Docs Pages` with
   `main` selected as the ref.
2. Confirm `Build Docs Site` succeeds.
3. Confirm `Deploy Docs to GitHub Pages` reports a Pages URL.
4. Open that URL and check top-level navigation, a nested docs route, and static
   assets.

## Failure Handling

### Manifest verification failed

Run the local build, review
`site/src/generated/docs-manifest.json`, and commit the regenerated file with
the source-doc changes.

### Pages artifact was not uploaded

Confirm the event is not a pull request and the selected workflow ref is
`main`. Pull-request runs intentionally stop after build verification.

### Deployment job was skipped

Confirm `github.ref` is `refs/heads/main`. A manual run from another branch is
validation-only.

### Published paths return 404

Confirm the Pages source is **GitHub Actions** and the build log shows
`VITE_BASE_PATH` matching `/${repository-name}/`.

## Rollback

Revert the docs/site regression on `main`, then rerun `Docs Pages` from `main`.
For an urgent return to a known-good version, revert to the known-good commit
through the normal branch workflow and let that commit produce a fresh Pages
deployment. Each deployment remains visible in the `github-pages` environment
history.
