import assert from 'node:assert/strict';
import { test } from 'node:test';

import {
  chatSessionRevision,
  collapseAdjacentStoredMessages,
  missingStoredMessages,
  shouldHydrateServerSession,
} from '../src/lib/chatSync.ts';

const completedSummary = {
  session_id: 'chat-a',
  title: 'durable task',
  updated_at_unix: 10,
  updated_at_unix_ms: 10_500,
  revision: 4,
  message_count: 2,
  active: false,
};

test('new server revision hydrates even when local tool bubbles outnumber stored messages', () => {
  assert.equal(shouldHydrateServerSession(completedSummary, 3, 10_500, false, false), true);
  assert.equal(shouldHydrateServerSession(completedSummary, 4, 10_500, false, false), false);
  assert.equal(shouldHydrateServerSession(completedSummary, 3, 10_500, false, true), false);
});

test('a bounded browser cache rehydrates from the complete server transcript', () => {
  assert.equal(shouldHydrateServerSession(completedSummary, 4, 10_500, true, false), true);
});

test('missing final answer survives unrelated local tool and status bubbles', () => {
  const local = [
    { role: 'user' as const, content: 'do the work' },
    { role: 'agent' as const, content: '[Tool Call] shell({})' },
    { role: 'agent' as const, content: '[Tool Result] shell\ndone' },
    { role: 'agent' as const, content: 'still working' },
  ];
  const stored = [
    { role: 'user' as const, content: 'do the work' },
    { role: 'assistant' as const, content: 'finished on the server' },
  ];

  assert.deepEqual(missingStoredMessages(local, stored), [stored[1]]);
});

test('adjacent duplicate terminal replies collapse without removing real repeats', () => {
  const messages = [
    { role: 'assistant' as const, content: 'same reply' },
    { role: 'assistant' as const, content: 'same reply' },
    { role: 'user' as const, content: 'again' },
    { role: 'assistant' as const, content: 'same reply' },
  ];

  assert.deepEqual(collapseAdjacentStoredMessages(messages), [
    messages[0],
    messages[2],
    messages[3],
  ]);
});

test('legacy timestamps stay separate from revision counters during upgrades', () => {
  assert.equal(
    chatSessionRevision({ updated_at_unix: 42, updated_at_unix_ms: undefined }),
    undefined,
  );
  assert.equal(
    shouldHydrateServerSession(
      {
        ...completedSummary,
        revision: undefined,
        updated_at_unix_ms: 42_000,
      },
      undefined,
      41_000,
      false,
      false,
    ),
    true,
  );
});
