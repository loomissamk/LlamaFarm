# Local Docs Build Policy

The documentation frontend is a local development and validation surface. The
repository does not automatically publish or deploy it.

## Build Policy

- Documentation and site changes should build successfully before review.
- The generated docs manifest must match the committed source tree.
- Builds use a root-relative base path and write to `site/dist/`.
- Validation is run locally with the commands in the build runbook.

## Publishing Policy

- There is no GitHub Pages workflow or other automatic publishing path.
- Builds do not upload artifacts or modify an external hosting service.
- Publishing requires a separate, explicit maintainer decision and is outside
  this repository's current delivery contract.

The legacy `.github/release/docs-deploy-policy.json` and
`scripts/ci/docs_deploy_guard.py` are not invoked by an executable workflow.
They are not part of the current build contract.

## Change Checklist

When documentation-site build behavior changes:

1. update this policy and the local build runbook together;
2. run the site build and verify a clean generated manifest;
3. preview the output locally;
4. confirm no automatic publishing or deployment workflow was introduced.
