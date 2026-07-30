import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const readPage = (name: string) =>
  readFileSync(
    fileURLToPath(new URL(`../src/pages/${name}.tsx`, import.meta.url)),
    'utf8',
  );

test('connections refresh only while visible and report freshness and scoped errors', () => {
  const connections = readPage('Connections');

  assert.match(connections, /document\.visibilityState !== 'visible'/);
  assert.match(connections, /document\.addEventListener\('visibilitychange'/);
  assert.match(connections, /setLastUpdated\(new Date\(\)\)/);
  assert.match(connections, /Updated \$\{lastUpdated\.toLocaleTimeString\(\)\}/);
  assert.match(connections, /loadError, actionError, contextError/);
});

test('connections make context settings keyboard-safe and destructive actions explicit', () => {
  const connections = readPage('Connections');

  assert.match(connections, /onPointerUp=\{\(\) => void saveContext\(ctxDraft\)\}/);
  assert.match(connections, /onKeyUp=\{saveRangeFromKeyboard\}/);
  assert.match(connections, /onBlur=\{\(\) => void saveContext\(ctxDraft\)\}/);
  assert.match(connections, /Apply context window/);
  assert.match(connections, /role="alertdialog"/);
  assert.match(connections, /aria-labelledby="disconnect-github-title"/);
  assert.doesNotMatch(connections, /window\.confirm/);
});

test('integrations keep refresh failures retryable without discarding loaded values', () => {
  const integrations = readPage('Integrations');

  assert.match(integrations, /if \(loading && !integration && !status\)/);
  assert.match(integrations, /Showing the last loaded values/);
  assert.match(integrations, /onClick=\{requestRefresh\}/);
  assert.match(integrations, /Effective model/);
  assert.match(integrations, /Effective endpoint/);
  assert.match(integrations, /Configured: \{configuredModel/);
  assert.match(integrations, /model_environment_override/);
});

test('integrations protect dirty editor state and expose accessible dialog semantics', () => {
  const integrations = readPage('Integrations');

  assert.match(integrations, /const editorDirty =/);
  assert.match(integrations, /setDiscardAction\('close'\)/);
  assert.match(integrations, /setDiscardAction\('refresh'\)/);
  assert.match(integrations, /window\.addEventListener\('beforeunload'/);
  assert.match(integrations, /role="dialog"/);
  assert.match(integrations, /aria-labelledby="ollama-editor-title"/);
  assert.match(integrations, /htmlFor="ollama-model-choice"/);
  assert.match(integrations, /htmlFor="ollama-endpoint"/);
  assert.match(integrations, /Discard unsaved changes/);
});

test('workspace browser preserves its location across refresh failures and stale requests', () => {
  const workspaceFiles = readPage('WorkspaceFiles');

  assert.match(workspaceFiles, /const currentPathRef = useRef\(''\)/);
  assert.match(workspaceFiles, /const browserRequestRef = useRef\(0\)/);
  assert.match(workspaceFiles, /path = currentPathRef\.current/);
  assert.match(workspaceFiles, /browserRequestRef\.current !== requestId/);
  assert.match(workspaceFiles, /currentPathRef\.current = nextBrowser\.current_path/);
  assert.match(workspaceFiles, /currentPathRef\.current === destinationPath/);
  assert.match(workspaceFiles, /const workspaceMutationInFlight =/);
  assert.match(workspaceFiles, /disabled=\{workspaceMutationInFlight\}/);
});

test('workspace uploads are confirmed, counted, and announced', () => {
  const workspaceFiles = readPage('WorkspaceFiles');

  assert.match(workspaceFiles, /const \[pendingUploads, setPendingUploads\]/);
  assert.match(workspaceFiles, /const \[uploadProgress, setUploadProgress\]/);
  assert.match(workspaceFiles, /Confirm file upload/);
  assert.match(workspaceFiles, /existing file/);
  assert.match(workspaceFiles, /Uploaded \{uploadProgress\.completed\} of/);
  assert.match(workspaceFiles, /aria-live="polite"/);
});

test('workspace dialogs and narrow file lists preserve labels and actions', () => {
  const workspaceFiles = readPage('WorkspaceFiles');

  assert.match(workspaceFiles, /role="alertdialog"/);
  assert.match(workspaceFiles, /aria-labelledby="delete-workspace-entry-title"/);
  assert.match(workspaceFiles, /htmlFor="new-workspace-folder-name"/);
  assert.match(workspaceFiles, /aria-labelledby="upload-workspace-files-title"/);
  assert.match(workspaceFiles, /hidden px-4 py-3 lg:table-cell/);
  assert.match(workspaceFiles, /hidden px-4 py-3 sm:table-cell/);
  assert.match(workspaceFiles, /flex flex-wrap justify-end gap-2/);
});
