import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

function source(relativePath: string): string {
  return readFileSync(
    fileURLToPath(new URL(relativePath, import.meta.url)),
    'utf8',
  );
}

test('dashboard renders reported capacity without opening duplicate operational panels', () => {
  const dashboard = source('../src/pages/Dashboard.tsx');
  assert.match(dashboard, /status\.capacity/);
  assert.match(dashboard, /Queue depth unavailable/);
  assert.match(dashboard, /restart required to apply listener changes/);
  assert.doesNotMatch(dashboard, /<LogsPanel/);
  assert.doesNotMatch(dashboard, /<DiagnosticsPanel/);
});

test('federation fleet view renders local and peer runtime facts with restrained polling', () => {
  const federation = source('../src/pages/Federation.tsx');
  assert.match(federation, /local_capabilities/);
  assert.match(federation, /peer\.capacity/);
  assert.match(federation, /document\.hidden/);
  assert.doesNotMatch(federation, /setInterval/);
});
