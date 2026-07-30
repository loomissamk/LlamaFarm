# Required Check Mapping

This document maps the current executable workflows to stable check names.

## Merge to `main` or `dev`

| Required check name | Source workflow | Scope |
| --- | --- | --- |
| `CI Required Gate` | `.github/workflows/ci-run.yml` | aggregate Rust lint/test and web test/build gate |

Dependency jobs that feed the aggregate check:

- `Rust Format and Clippy`
- `Rust Tests`
- `Web Tests and Build`

Configure branch protection or rulesets against `CI Required Gate` rather than
the dependency jobs so the required-check surface stays stable.

## Documentation Site

There is no documentation-site publishing, deployment, or required-check
workflow. Build and manifest validation are local-only.

## Verification Procedure

1. Resolve the latest workflow runs:

   ```bash
   gh run list --workflow ci-run.yml --limit 1
   ```

2. Enumerate job names:

   ```bash
   gh run view <run_id> --json jobs --jq '.jobs[].name'
   ```

3. Compare the names with this mapping.
4. If a required name changes, update this file and repository branch rules in
   the same delivery change.

GitHub discovers manual workflows from the default branch. Merge a new workflow
before expecting it to appear in the manual-run interface.
