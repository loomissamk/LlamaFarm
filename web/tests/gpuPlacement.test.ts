import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  canonicalOllamaModel,
  GPU_LAYER_PRESETS,
  placementWorkerId,
  primaryRuntimeLabel,
  withModelPlacement,
} from '../src/lib/gpuPlacement.ts';

test('GPU layer Auto serializes as null rather than a CPU-only sentinel', () => {
  assert.deepEqual(
    GPU_LAYER_PRESETS.map(({ label, value }) => [label, value]),
    [
      ['All GPU', 999],
      ['Auto', null],
      ['CPU only', 0],
    ],
  );
  assert.equal(GPU_LAYER_PRESETS.some(({ value }) => value === -1), false);
});

test('current-model runtime preference adds, replaces, and removes one canonical route', () => {
  const initial = [
    { model: 'embedding-model:latest', worker_id: 'embedding' },
    { model: 'qwen3.5', worker_id: 'old-worker' },
  ];

  const replaced = withModelPlacement(initial, 'QWEN3.5:latest', 'gpu-runtime');
  assert.equal(canonicalOllamaModel('QWEN3.5'), 'qwen3.5:latest');
  assert.equal(placementWorkerId(replaced, 'qwen3.5'), 'gpu-runtime');
  assert.deepEqual(replaced, [
    { model: 'embedding-model:latest', worker_id: 'embedding' },
    { model: 'QWEN3.5:latest', worker_id: 'gpu-runtime' },
  ]);

  assert.deepEqual(withModelPlacement(replaced, 'qwen3.5', ''), [
    { model: 'embedding-model:latest', worker_id: 'embedding' },
  ]);
});

test('primary runtime label identifies a single detected GPU without hard-coding hardware', () => {
  assert.equal(primaryRuntimeLabel([]), 'Primary / automatic');
  assert.equal(
    primaryRuntimeLabel([
      {
        index: 0,
        uuid: 'GPU-test',
        name: 'NVIDIA Test GPU',
        memory_total_mb: 32_768,
      },
    ]),
    'Primary · NVIDIA Test GPU · 32 GB',
  );
  assert.equal(
    primaryRuntimeLabel([
      { index: 0, uuid: 'GPU-a', name: 'GPU A', memory_total_mb: 32_768 },
      { index: 1, uuid: 'GPU-b', name: 'GPU B', memory_total_mb: 32_768 },
    ]),
    'Primary · 2 visible GPUs',
  );
});

test('GPU placement UI keeps the full editor advanced and supports external runtimes', () => {
  const component = readFileSync(
    fileURLToPath(new URL('../src/components/OllamaGpuPlacement.tsx', import.meta.url)),
    'utf8',
  );
  const integrations = readFileSync(
    fileURLToPath(new URL('../src/pages/Integrations.tsx', import.meta.url)),
    'utf8',
  );
  const connections = readFileSync(
    fileURLToPath(new URL('../src/pages/Connections.tsx', import.meta.url)),
    'utf8',
  );

  assert.match(component, /id="ollama-preferred-runtime"/);
  assert.match(component, /<details className=/);
  assert.match(component, /Advanced runtime pools and per-model routes/);
  assert.match(component, /<option value="external">External Ollama-compatible runtime<\/option>/);
  assert.match(component, /disabled=\{worker\.managed\}/);
  assert.doesNotMatch(component, /V100|Vulkan|5070/);
  assert.match(integrations, /currentModel=\{currentModel\}/);
  assert.match(connections, /gpu_layers: value/);
  assert.doesNotMatch(connections, /\['Auto', -1\]/);
});
