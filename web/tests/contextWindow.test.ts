import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  automaticContextLabel,
  CONTEXT_PRESETS,
  contextDraftLabel,
  contextSourceLabel,
  formatContextTokens,
  hasAdaptiveContextPolicy,
  isAdaptiveContextActive,
} from '../src/lib/contextWindow.ts';

test('context presets expose automatic and the full 256K window', () => {
  assert.deepEqual(
    CONTEXT_PRESETS.map(({ label, value }) => [label, value]),
    [
      ['Auto', 0],
      ['64K', 65_536],
      ['128K', 131_072],
      ['256K', 262_144],
    ],
  );
});

test('automatic context describes the adaptive range truthfully', () => {
  const info = {
    num_ctx: null,
    effective_default_num_ctx: 65_536,
    source: 'environment' as const,
    adaptive: { enabled: true, baseline: 65_536, max: 262_144 },
  };

  assert.equal(automaticContextLabel(info), 'Adaptive 64K → 256K');
  assert.equal(contextDraftLabel(info, 0), 'Adaptive 64K → 256K');
  assert.equal(contextDraftLabel(info, 131_072), '128K tokens');
});

test('automatic context distinguishes node profile and model-native sources', () => {
  assert.equal(
    automaticContextLabel({
      num_ctx: null,
      effective_default_num_ctx: 65_536,
      source: 'environment',
    }),
    'Node profile · 64K',
  );
  assert.equal(automaticContextLabel({ num_ctx: null }), 'Model native');
  assert.equal(contextSourceLabel('config'), 'Saved override');
  assert.equal(formatContextTokens(262_144), '256K');
});

test('a manual override is inactive while Auto previews the policy it will restore', () => {
  const info = {
    num_ctx: 131_072,
    effective_default_num_ctx: 131_072,
    source: 'config' as const,
    adaptive: { enabled: true, active: false, baseline: 65_536, max: 262_144 },
  };

  assert.equal(isAdaptiveContextActive(info), false);
  assert.equal(hasAdaptiveContextPolicy(info), true);
  assert.equal(automaticContextLabel(info), 'Adaptive 64K → 256K');
});
