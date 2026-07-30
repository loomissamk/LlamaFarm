import assert from 'node:assert/strict';
import { test } from 'node:test';

import { databaseResultToCsv } from '../src/lib/databaseResults.ts';

test('database results export valid CSV for punctuation, objects, and nulls', () => {
  const csv = databaseResultToCsv({
    columns: ['name', 'metadata', 'empty'],
    rows: [['alpha, beta', { enabled: true }, null]],
    row_count: 1,
    truncated: false,
  });

  assert.equal(
    csv,
    'name,metadata,empty\n"alpha, beta","{""enabled"":true}",\n',
  );
});
