import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { createBackupFilename, summarizeTextChange } from '../src/lib/editorDraft.ts';

test('text change summary isolates the changed middle block', () => {
  const summary = summarizeTextChange(
    'shared first\nold one\nold two\nshared last',
    'shared first\nnew one\nnew two\nnew three\nshared last',
  );

  assert.equal(summary.changed, true);
  assert.equal(summary.firstChangedLine, 2);
  assert.equal(summary.originalChangedLines, 2);
  assert.equal(summary.draftChangedLines, 3);
  assert.equal(summary.addedLines, 1);
  assert.deepEqual(summary.originalPreview, ['old one', 'old two']);
  assert.deepEqual(summary.draftPreview, ['new one', 'new two', 'new three']);
});

test('text change summary reports an unchanged draft', () => {
  const summary = summarizeTextChange('same', 'same');
  assert.equal(summary.changed, false);
  assert.equal(summary.description, 'No draft changes.');
});

test('backup filename is deterministic and filesystem friendly', () => {
  assert.equal(
    createBackupFilename('AGENTS.md live', new Date('2026-07-30T18:30:00.000Z')),
    'AGENTS.md-live.2026-07-30T18-30-00-000Z.bak',
  );
});

test('dirty draft guard covers browser history navigation', () => {
  const guard = readFileSync(
    fileURLToPath(new URL('../src/hooks/useDirtyDraftGuard.ts', import.meta.url)),
    'utf8',
  );

  assert.match(guard, /window\.addEventListener\('popstate', handlePopState\)/);
  assert.match(guard, /currentHistoryIndex - nextHistoryIndex/);
  assert.match(guard, /window\.history\.go\(restoreDelta\)/);
});
