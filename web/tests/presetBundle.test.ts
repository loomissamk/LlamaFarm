import assert from 'node:assert/strict';
import { test } from 'node:test';
import type {
  ConfigPresetEntry,
  WorkspaceFileResponse,
} from '../src/types/api.ts';
import {
  applyPresetBundleWithRollback,
  PresetBundleApplyError,
  type PresetBundleOperations,
} from '../src/lib/presetBundle.ts';

const preset: ConfigPresetEntry = {
  id: 'god',
  label: 'God',
  summary: 'Test bundle',
  highlights: [],
  content: 'new config',
  workspace_files: [
    { name: 'AGENTS.md', content: 'new agents' },
    { name: 'RUNTIME.md', content: 'new runtime' },
  ],
};

function workspaceFile(
  name: string,
  content: string,
  exists = true,
): WorkspaceFileResponse {
  return { name, content, exists };
}

test('preset bundle captures every live value before applying writes', async () => {
  const events: string[] = [];
  const operations: PresetBundleOperations = {
    async getConfig() {
      events.push('get-config');
      return 'old config';
    },
    async getWorkspaceFile(name) {
      events.push(`get-file:${name}`);
      return workspaceFile(name, `old ${name}`);
    },
    async putConfig(content) {
      events.push(`put-config:${content}`);
    },
    async putWorkspaceFile(name, content) {
      events.push(`put-file:${name}:${content}`);
    },
  };

  await applyPresetBundleWithRollback(preset, operations);

  assert.deepEqual(events, [
    'get-config',
    'get-file:AGENTS.md',
    'get-file:RUNTIME.md',
    'put-config:new config',
    'put-file:AGENTS.md:new agents',
    'put-file:RUNTIME.md:new runtime',
  ]);
});

test('preset bundle restores completed writes after a later file fails', async () => {
  const events: string[] = [];
  const operations: PresetBundleOperations = {
    async getConfig() {
      events.push('get-config');
      return 'old config';
    },
    async getWorkspaceFile(name) {
      events.push(`get-file:${name}`);
      return workspaceFile(name, `old ${name}`);
    },
    async putConfig(content) {
      events.push(`put-config:${content}`);
    },
    async putWorkspaceFile(name, content) {
      events.push(`put-file:${name}:${content}`);
      if (name === 'RUNTIME.md' && content === 'new runtime') {
        throw new Error('second file rejected');
      }
    },
  };

  await assert.rejects(
    () => applyPresetBundleWithRollback(preset, operations),
    (error: unknown) => {
      assert.ok(error instanceof PresetBundleApplyError);
      assert.equal(error.mutationsStarted, true);
      assert.deepEqual(error.rollbackFailures, []);
      assert.match(error.toDisplayMessage(), /rolled back to the captured server state/);
      return true;
    },
  );
  assert.deepEqual(events.slice(-2), [
    'put-file:AGENTS.md:old AGENTS.md',
    'put-config:old config',
  ]);
});

test('preset bundle performs no writes when a preflight snapshot fails', async () => {
  const writes: string[] = [];
  const operations: PresetBundleOperations = {
    async getConfig() {
      return 'old config';
    },
    async getWorkspaceFile(name) {
      if (name === 'RUNTIME.md') throw new Error('snapshot unavailable');
      return workspaceFile(name, `old ${name}`);
    },
    async putConfig(content) {
      writes.push(`config:${content}`);
    },
    async putWorkspaceFile(name, content) {
      writes.push(`${name}:${content}`);
    },
  };

  await assert.rejects(
    () => applyPresetBundleWithRollback(preset, operations),
    (error: unknown) => {
      assert.ok(error instanceof PresetBundleApplyError);
      assert.equal(error.mutationsStarted, false);
      assert.match(error.toDisplayMessage(), /No live bundle steps completed/);
      return true;
    },
  );
  assert.deepEqual(writes, []);
});
