import assert from 'node:assert/strict';
import test from 'node:test';
import type { DbDiscoveryResult } from '../src/types/api.ts';
import { pickDiscoveredConnection } from '../src/lib/databaseDiscovery.ts';

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
