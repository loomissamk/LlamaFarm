import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import type { DbDiscoveryResult } from '../src/types/api.ts';
import {
  LatestRequestLifecycle,
  pickDiscoveredConnection,
} from '../src/lib/databaseDiscovery.ts';

function result(
  connection_name: string | undefined,
  status: DbDiscoveryResult['status'],
  newly_added = false,
): DbDiscoveryResult {
  return {
    host: '192.168.1.20',
    port: 27017,
    driver: 'mongodb',
    connection_name,
    status,
    newly_added,
  };
}

test('auto-select prioritizes a newly connected open database', () => {
  const selected = pickDiscoveredConnection([
    result('existing-open', 'connected'),
    result('new-open', 'connected', true),
  ]);

  assert.equal(selected, 'new-open');
});

test('auto-select falls back to a saved connection that needs configuration', () => {
  const selected = pickDiscoveredConnection([
    result(undefined, 'unsupported'),
    result('needs-auth', 'needs_configuration', true),
  ]);

  assert.equal(selected, 'needs-auth');
});

test('auto-select ignores unsupported services without explorer connections', () => {
  assert.equal(pickDiscoveredConnection([result(undefined, 'unsupported')]), null);
});

test('database explorer automatically runs discovery after loading saved connections', () => {
  const page = readFileSync(
    fileURLToPath(new URL('../src/pages/Database.tsx', import.meta.url)),
    'utf8',
  );

  assert.match(page, /loadConnections\(\)\.then\(handleScan\)/);
});

test('late schema completion cannot replace the latest database selection', async () => {
  const lifecycle = new LatestRequestLifecycle();
  let displayedSchema = '';
  let resolveFirst!: (schema: string) => void;
  const firstSchema = new Promise<string>((resolve) => {
    resolveFirst = resolve;
  });

  const firstRequest = lifecycle.begin();
  const firstCompletion = firstSchema.then((schema) => {
    if (firstRequest.isCurrent()) displayedSchema = schema;
  });

  const secondRequest = lifecycle.begin();
  if (secondRequest.isCurrent()) displayedSchema = 'schema-b';
  resolveFirst('schema-a');
  await firstCompletion;

  assert.equal(displayedSchema, 'schema-b');
});

test('late query completion cannot replace the latest database result', async () => {
  const lifecycle = new LatestRequestLifecycle();
  let displayedResult = '';
  let resolveFirst!: (result: string) => void;
  const firstQuery = new Promise<string>((resolve) => {
    resolveFirst = resolve;
  });

  const firstRequest = lifecycle.begin();
  const firstCompletion = firstQuery.then((result) => {
    if (firstRequest.isCurrent()) displayedResult = result;
  });

  lifecycle.invalidate();
  const secondRequest = lifecycle.begin();
  if (secondRequest.isCurrent()) displayedResult = 'result-b';
  resolveFirst('result-a');
  await firstCompletion;

  assert.equal(displayedResult, 'result-b');
});
