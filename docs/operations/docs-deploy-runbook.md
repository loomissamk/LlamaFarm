# Local Docs Build Runbook

The documentation frontend is built and previewed locally. There is no
automatic publishing or deployment workflow.

## Local Build

```bash
cd site
npm ci
npm run build
git diff --exit-code -- src/generated/docs-manifest.json
```

The build regenerates the documentation manifest and writes output to
`site/dist/`.

```bash
npm run preview
```

Open the local URL printed by Vite and check top-level navigation, a nested docs
route, and static assets. This preview does not publish or deploy the site.

## Failure Handling

### Manifest verification failed

Run the local build, review
`site/src/generated/docs-manifest.json`, and commit the regenerated file with
the source-doc changes.

### Local preview returns 404

Confirm `npm run build` completed, then restart `npm run preview` and use the URL
printed by Vite. The build uses `/` as its base path.

### Build output is stale

Remove `site/dist/` or rerun `npm run build`; Vite empties the output directory
before rebuilding.

## Delivery Boundary

The generated output remains local. Do not publish or deploy it without a
separate, explicit maintainer request.
