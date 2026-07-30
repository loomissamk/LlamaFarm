# Docs Deploy Policy

This document describes the policy enforced by the current
`.github/workflows/docs-deploy.yml`.

## Build Policy

- Docs and site pull requests to `main` must build successfully.
- The generated docs manifest must match the committed source tree.
- The Pages base path is derived from the repository name in CI.
- Pull-request runs never upload or deploy a Pages artifact.

## Production Policy

- Production deployment is allowed only from `refs/heads/main`.
- The build job must succeed before deployment.
- The deployed artifact is the repository-root `gh-pages/` directory.
- GitHub Pages must use **GitHub Actions** as its source.

## Manual Dispatch

A manual run is validation-only unless `main` is selected as the workflow ref.
Selecting `main` runs the same build and deployment path as a matching push.

## Rollback Policy

Rollback is performed by reverting the regression through the normal branch
workflow and producing a new deployment from `main`. This keeps the deployed
state tied to a repository commit.

The legacy `.github/release/docs-deploy-policy.json` and
`scripts/ci/docs_deploy_guard.py` are not invoked by the current workflow. They
are not part of the executable deployment contract unless a future change wires
them into `docs-deploy.yml` and updates this document.

## Change Checklist

When deployment behavior changes:

1. update the workflow, this policy, and the runbook together;
2. run `actionlint`;
3. run the site production build and verify a clean generated manifest;
4. confirm pull-request runs remain build-only;
5. confirm production deploys remain limited to `main`.
