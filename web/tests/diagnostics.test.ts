import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { DiagResult } from '../src/types/api.ts';
import {
  diagnosticCategories,
  diagnosticCounts,
  filterDiagnostics,
} from '../src/lib/diagnostics.ts';

const results: DiagResult[] = [
  { severity: 'ok', category: 'runtime', message: 'Runtime ready' },
  { severity: 'warn', category: 'models', message: 'Model cold' },
  { severity: 'error', category: 'runtime', message: 'Worker unavailable' },
];

test('diagnostic helpers provide stable counts and category choices', () => {
  assert.deepEqual(diagnosticCounts(results), { ok: 1, warn: 1, error: 1 });
  assert.deepEqual(diagnosticCategories(results), ['models', 'runtime']);
});

test('diagnostic filters combine category and severity', () => {
  assert.deepEqual(filterDiagnostics(results, 'error', 'runtime'), [results[2]]);
  assert.deepEqual(filterDiagnostics(results, 'all', 'models'), [results[1]]);
});
