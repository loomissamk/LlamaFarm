import assert from 'node:assert/strict';
import { test } from 'node:test';

import { formatInterval, intervalFromMs, intervalToMs } from '../src/lib/cron.ts';

test('interval helpers preserve readable scheduler units', () => {
  assert.deepEqual(intervalFromMs(7_200_000), { value: 2, unit: 'hours' });
  assert.deepEqual(intervalFromMs(90_000), { value: 90, unit: 'seconds' });
  assert.equal(intervalToMs(2.5, 'minutes'), 150_000);
  assert.equal(formatInterval(60_000), '1 minute');
  assert.equal(formatInterval(5_000), '5 seconds');
});

test('invalid intervals are rejected before reaching the API', () => {
  assert.equal(intervalToMs(0, 'minutes'), 0);
  assert.equal(intervalToMs(Number.NaN, 'hours'), 0);
});
