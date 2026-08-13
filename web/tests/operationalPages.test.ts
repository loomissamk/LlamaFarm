import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const readPage = (name: string) =>
  readFileSync(
    fileURLToPath(new URL(`../src/pages/${name}.tsx`, import.meta.url)),
    'utf8',
  );

test('agent preserves drafts and clears the composer only after a successful send', () => {
  const agent = readPage('AgentChat');

  assert.match(agent, /CHAT_DRAFT_STORAGE_KEY/);
  assert.match(agent, /useState\(\(\) => loadDraft\(\)\)/);
  assert.match(agent, /const sent = dispatchUserMessage\(activeSession, input\)/);
  assert.match(agent, /if \(!sent\)/);
  assert.match(agent, /setInput\(''\)/);
  assert.match(agent, /disabled=\{!activeSession\}/);
});

test('agent continuously discovers remote chats and audits the authoritative tool catalogue', () => {
  const agent = readPage('AgentChat');
  const ws = readFileSync(
    fileURLToPath(new URL('../src/lib/ws.ts', import.meta.url)),
    'utf8',
  );

  assert.match(agent, /REMOTE_SESSION_REFRESH_MS = 7500/);
  assert.match(agent, /globalThis\.setInterval\(\(\) => \{/);
  assert.match(agent, /if \(cancelled \|\| refreshInFlight\) return/);
  assert.match(agent, /void discoverRemoteSessions\(\)/);
  assert.match(agent, /globalThis\.clearInterval\(interval\)/);
  assert.match(agent, /const localById = new Map\(sessionsRef\.current/);
  assert.match(agent, /const detail = await getChatSession\(selectedId\)/);
  assert.match(agent, /const registeredTools = await getTools\(\)/);
  assert.match(agent, /createChatSession\(false\)/);
  assert.match(agent, /dispatchUserMessage\(session, TEST_ALL_TOOLS_PROMPT, allowedTools\)/);
  assert.match(ws, /allowed_tools: options\.allowedTools/);
});

test('agent retains measured throughput for background chats and federated workers', () => {
  const agent = readPage('AgentChat');
  const federation = readPage('Federation');
  const state = readFileSync(
    fileURLToPath(new URL('../src/lib/federationState.ts', import.meta.url)),
    'utf8',
  );

  assert.match(agent, /realMetricsBySessionRef\.current\[sessionId\] = metrics/);
  assert.match(agent, /case 'federation_metrics'/);
  assert.match(agent, /latestFederationTask\.generationTps\.toFixed\(1\)/);
  assert.match(federation, /task\.generationTps\.toFixed\(1\)/);
  assert.match(state, /generationTps: task\.generationTps/);
  assert.match(state, /promptTokens: task\.promptTokens/);
});

test('agent presents explicit connection states and accessible keyboard semantics', () => {
  const agent = readPage('AgentChat');

  assert.match(agent, /'connecting' \| 'connected' \| 'reconnecting'/);
  assert.match(agent, /event\.nativeEvent\.isComposing/);
  assert.match(agent, /Enter to send · Shift\+Enter for a new line/);
  assert.match(agent, /aria-label="Message the agent"/);
  assert.match(agent, /aria-label="Send message"/);
  assert.doesNotMatch(agent, /<p aria-live="polite" className="mt-3 whitespace-pre-wrap/);
});

test('runs poll live work with visibility awareness and bounded backoff', () => {
  const runs = readPage('Runs');

  assert.match(runs, /document\.visibilityState === 'hidden'/);
  assert.match(runs, /document\.addEventListener\('visibilitychange'/);
  assert.match(runs, /selectedIsLive/);
  assert.match(runs, /liveIdsRef\.current\.length > 0/);
  assert.match(runs, /liveIdsRef\.current\.length > 0 \? 3000 : 15000/);
  assert.match(runs, /Math\.min\(1000 \* 2 \*\*/);
  assert.doesNotMatch(runs, /setInterval/);
});

test('runs support durable selection links and useful filtering', () => {
  const runs = readPage('Runs');

  assert.match(runs, /useSearchParams\(\)/);
  assert.match(runs, /searchParams\.get\('run'\)/);
  assert.match(runs, /next\.set\('run', runId\)/);
  assert.match(runs, /statusFilter/);
  assert.match(runs, /Search ID, model, provider/);
  assert.match(runs, /navigator\.clipboard\.writeText\(window\.location\.href\)/);
});

test('logs retain paused events and expose reconnect and follow states', () => {
  const logs = readPage('Logs');

  assert.match(logs, /pausedEntriesRef/);
  assert.match(logs, /Resume\{pausedCount > 0/);
  assert.match(logs, /'connecting' \| 'connected' \| 'reconnecting'/);
  assert.match(logs, /clientRef\.current\?\.connect\(\)/);
  assert.match(logs, /autoScroll \? 'Following' : 'Follow latest'/);
});

test('logs filter by text, severity, and source and export the visible result', () => {
  const logs = readPage('Logs');

  assert.match(logs, /searchQuery/);
  assert.match(logs, /sourceFilter/);
  assert.match(logs, /levelFilter/);
  assert.match(logs, /deriveSource\(entry\.line\)/);
  assert.match(logs, /mergeLogEntries\(prev, \[entry\]\)/);
  assert.match(logs, /navigator\.clipboard\.writeText\(formatEntries\(filteredEntries\)\)/);
  assert.match(logs, /const text = formatEntries\(filteredEntries\)/);
});
