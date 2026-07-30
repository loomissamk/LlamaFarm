import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const readSource = (path: string) =>
  readFileSync(fileURLToPath(new URL(path, import.meta.url)), 'utf8');

test('application routes and agent chat load on demand behind shell-local fallbacks', () => {
  const app = readSource('../src/App.tsx');
  const layout = readSource('../src/components/layout/Layout.tsx');

  assert.match(app, /lazy\(\(\) => import\('\.\/pages\/Dashboard'\)\)/);
  assert.match(app, /lazy\(\(\) => import\('\.\/pages\/Database'\)\)/);
  assert.match(layout, /lazy\(\(\) => import\('@\/pages\/AgentChat'\)\)/);
  assert.ok(layout.includes('<Suspense fallback={<PageLoading />}>'));
});
