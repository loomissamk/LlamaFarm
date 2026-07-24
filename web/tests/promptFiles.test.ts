import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const promptPage = readFileSync(
  fileURLToPath(new URL('../src/pages/WorkspacePrompts.tsx', import.meta.url)),
  'utf8',
);

test('prompt editor loads only the supported AGENTS.md file', () => {
  assert.match(promptPage, /const WORKSPACE_FILES: WorkspaceFileName\[\] = \['AGENTS\.md'\]/);
  assert.match(promptPage, /Edit the live workspace copy of/);
  assert.doesNotMatch(promptPage, /SOUL\.md/);
});
