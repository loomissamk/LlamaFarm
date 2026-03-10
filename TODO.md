# TODO

## Goal

Turn this repo into a local-first, Ollama-only personal project named `LlamaFarm`.

## Current Pause Point

Current in-progress work:

- Finish the Ollama-only cleanup on the remaining web pages (`/integrations`, `/cron`, `/memory`, `/tools`).
- Rebuild and redeploy locally after the UI/API cleanup pass.

## Progress Update (2026-03-10)

Completed or effectively completed:

- Live gateway runtime snapshots now hot-reload provider/model/tools after config and integration saves.
- `/api/status` and the dashboard now expose Ollama-local model/runtime information instead of provider marketing/cost emphasis.
- The browser title, header, sidebar, and visible app branding now present `LlamaFarm`.
- The visible language selector has been removed and the current shell is effectively English-only.
- Pairing UI and pairing status are gone from the current web shell; local runtime config already runs with `require_pairing = false`.

Still open in this chat:

- Simplify the Ollama settings page copy and controls.
- Finish scheduled job create/edit/run flows with the `/home/bat/test.sh` starter command.
- Add memory clear-history actions with confirmation.
- Improve the tools page so local command/runtime availability is obvious.
- Redeploy locally and do a live smoke pass on localhost.
- Add follow-up cleanup work to remove GitHub CI/workflow overhead for this local-only deployment repo.

## Guardrails For Next Chat

- Do not spend time on CI, runners, or cloud-provider workflows.
- Focus on the local Docker/dev stack and the actual web app behavior.
- Preserve unrelated user worktree changes unless explicitly told to remove them.
- Prefer fixing runtime behavior end-to-end and verifying in the live app.

## Ordered Worklist

- [ ] 1. Make the app Ollama-only in runtime and UI.
  - Remove or hide non-Ollama provider/integration cards in the web UI.
  - Remove dashboard/provider wording that implies ChatGPT, Codex, Grok, Gemini, Claude, OpenRouter, etc. are part of the intended product.
  - Keep external API-compat endpoints only if they are still needed technically, but remove their branding from the product UI.
  - Progress:
    - Backend status + integrations API are Ollama-only.
    - Dashboard and Models pages are already Ollama-only.
    - Remaining cleanup is centered on the Integrations and Tools screens.
  - Acceptance:
    - The visible app experience is clearly Ollama-only.
    - No top-level UI path suggests cloud/provider setup.

- [ ] 2. Finish the paused Ollama model-switch work.
  - Backend must hot-reload provider/model after Integrations save instead of requiring restart.
  - The Ollama card must list real installed models from the configured Ollama endpoint.
  - The current model label must update after switching.
  - Progress:
    - Live runtime hot-reload support is in place.
    - Installed model discovery already comes from the active Ollama endpoint.
    - Remaining work is the final Integrations page wiring + live validation.
  - Acceptance:
    - Changing models from the UI changes `/api/status`.
    - A fresh chat turn uses the newly selected model without restarting the container.

- [x] 3. Replace cost-focused dashboard content with Ollama-local model info.
  - Remove or repurpose the `Cost Overview` card.
  - Show useful local Ollama information instead, for example:
    - installed models
    - current active model
    - Ollama endpoint
    - whether the current model is loaded
  - Acceptance:
    - Dashboard reflects local Ollama usage rather than cloud cost tracking.

- [ ] 4. Make the Tools page actually usable and verify every listed tool path.
  - Audit all tools shown in `/tools`.
  - Confirm which tools are really callable from the agent.
  - Fix broken schemas, broken invocations, or misleading entries.
  - Add a practical validation pass for shell, browser/chromium, file, memory, scheduler, and process-related tools.
  - Acceptance:
    - The key tools work from the live app, not just from code inspection.
    - Broken tools are either fixed or removed from the page.

- [ ] 5. Make the Scheduled Jobs page work end-to-end.
  - Verify create/list/run/update/remove flows.
  - Add a simple starter/template job in the UI:
    - run `/home/bat/test.sh`
    - at a user-chosen time or schedule
  - Make sure the scheduler respects the configured security policy and uses clear errors.
  - Progress:
    - Backend list/add/update/run endpoints are in place.
    - The remaining work is the page wiring and local smoke validation.
  - Acceptance:
    - A job can be created from the UI, listed, and run successfully.
    - The template/example is visible and understandable.

- [x] 6. Simplify the top-right language selector to English only.
  - Remove the visible language switcher if English is the only supported UI language.
  - Remove extra language choices from the current web UI.
  - Acceptance:
    - The dashboard no longer shows `TR` or other language toggles.

- [ ] 7. Add a clear-history action on the Memory page.
  - Add a button to clear memory/history from the UI.
  - Decide whether this should clear:
    - conversation memory only
    - all memory entries
    - or offer both
  - Require a confirmation step.
  - Acceptance:
    - User can wipe history from `/memory`.
    - The result is reflected immediately in the table.

- [ ] 8. Remove pairing completely.
  - Remove pairing UI, pairing checks, pairing text, and pairing-related dashboard status.
  - Default local access should not require pairing tokens or codes.
  - Progress:
    - Local runtime config already disables pairing.
    - Pairing UI/status were removed from the current web shell.
    - Remaining work is cleanup of backend/auth leftovers only if they still affect local deployment behavior.
  - Acceptance:
    - No pairing screen or pairing status appears anywhere in the product.
    - Relevant endpoints and websocket flows work without pairing logic.

- [x] 9. Rename visible product surfaces from `ZeroClaw` to `LlamaFarm`.
  - Update app title, sidebar/header branding, browser title, and visible product text.
  - Rename docs and screenshots only if they are still part of the intended local product.
  - Keep internal crate/binary names as `zeroclaw` only where changing them would cause unnecessary breakage; evaluate separately.
  - Acceptance:
    - The web UI presents itself as `LlamaFarm`.
    - No visible `ZeroClaw` branding remains in the local app.

- [ ] 10. Do a cleanup sweep for stray non-Ollama references.
  - Search for visible references to cloud models/providers in the web UI and dashboard text.
  - Search for visible `ZeroClaw` branding in the web UI.
  - Search for pairing-related labels/messages.
  - Progress:
    - The obvious dashboard/header/sidebar branding cleanup is done.
    - Remaining stray product text is concentrated in legacy page copy and internal comments/helpers.
  - Acceptance:
    - Product-facing UI text matches the new local-only LlamaFarm direction.

- [ ] 11. Remove GitHub CI/workflow overhead that is no longer needed for this repo.
  - Remove GitHub CI/jobs/runners that are only useful for hosted/cloud delivery.
  - Keep local build/run instructions and local smoke validation as the primary path.
  - Acceptance:
    - The repo no longer carries unnecessary GitHub CI workflow noise for this local-only deployment setup.

## Suggested Execution Order

1. Finish the paused model-switch/runtime-reload fix.
2. Lock the product to Ollama-only.
3. Remove pairing.
4. Rename visible branding to `LlamaFarm`.
5. Replace dashboard cost content with Ollama model info.
6. Fix Tools page behavior and verify shell/browser/process/file/memory tools.
7. Fix Scheduled Jobs page and add the `/home/bat/test.sh` example/template.
8. Add Memory clear-history action.
9. Remove language switcher and keep English only.
10. Run a full live UI validation pass.
11. Remove GitHub CI/workflow overhead that is no longer relevant to local-only deployment.

## Final Validation Checklist

- [ ] Ollama model can be changed from the UI and takes effect live.
- [ ] Dashboard shows Ollama-local information instead of cloud-cost emphasis.
- [ ] Tools page only shows working or intentionally supported tools.
- [ ] Shell tooling works.
- [ ] Chromium/browser tooling works.
- [ ] Scheduled job creation/list/run/remove works.
- [ ] Memory clear-history works.
- [ ] English-only UI is enforced.
- [ ] Pairing is gone.
- [ ] Visible branding says `LlamaFarm`.
- [ ] No obvious cloud-provider branding remains in the normal product UI.
- [ ] Local deployment path works on localhost after rebuild.
