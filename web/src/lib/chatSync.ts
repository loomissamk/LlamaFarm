import type { ChatSessionSummary, StoredChatMessage } from '../types/api';

export interface LocalTranscriptMessage {
  role: 'user' | 'agent';
  content: string;
  seedRole?: StoredChatMessage['role'];
  seedContent?: string;
}

function normalizeStoredRole(role: StoredChatMessage['role']): string {
  return role === 'agent' ? 'assistant' : role;
}

function storedMessageKey(message: StoredChatMessage): string {
  return `${normalizeStoredRole(message.role)}\u0000${message.content.trim()}`;
}

function localMessageKey(message: LocalTranscriptMessage): string {
  const role = message.seedRole ?? (message.role === 'agent' ? 'assistant' : 'user');
  const content = (message.seedContent ?? message.content).trim();
  return `${normalizeStoredRole(role)}\u0000${content}`;
}

/** Drop only adjacent wire duplicates, which can be emitted when both the
 * agent loop and the gateway checkpoint the same terminal assistant reply. */
export function collapseAdjacentStoredMessages(
  messages: StoredChatMessage[],
): StoredChatMessage[] {
  return messages.filter(
    (message, index) =>
      index === 0 || storedMessageKey(message) !== storedMessageKey(messages[index - 1]!),
  );
}

/** Return authoritative server messages that are absent from the browser
 * transcript. Counts are occurrence-aware, so repeated real turns survive,
 * while local tool/status bubbles cannot hide a newly completed final reply. */
export function missingStoredMessages(
  local: LocalTranscriptMessage[],
  stored: StoredChatMessage[],
): StoredChatMessage[] {
  const remainingLocal = new Map<string, number>();
  for (const message of local) {
    const key = localMessageKey(message);
    remainingLocal.set(key, (remainingLocal.get(key) ?? 0) + 1);
  }

  const missing: StoredChatMessage[] = [];
  for (const message of collapseAdjacentStoredMessages(stored)) {
    const key = storedMessageKey(message);
    const remaining = remainingLocal.get(key) ?? 0;
    if (remaining > 0) {
      remainingLocal.set(key, remaining - 1);
    } else {
      missing.push(message);
    }
  }
  return missing;
}

export function chatSessionUpdatedAtMs(
  session: Pick<ChatSessionSummary, 'updated_at_unix' | 'updated_at_unix_ms'>,
): number {
  return session.updated_at_unix_ms ?? session.updated_at_unix * 1000;
}

export function chatSessionRevision(
  session: Pick<
    ChatSessionSummary,
    'revision' | 'updated_at_unix' | 'updated_at_unix_ms'
  >,
): number | undefined {
  return session.revision;
}

export function shouldHydrateServerSession(
  summary: ChatSessionSummary,
  localRevision: number | undefined,
  localUpdatedAtMs: number | undefined,
  localTranscriptTruncated: boolean,
  browserOwnsLiveSocket: boolean,
): boolean {
  if (browserOwnsLiveSocket) return false;
  if (localTranscriptTruncated) return true;

  // Explicit revisions are authoritative and equality-based. If the server's
  // persisted store was restored or replaced, a lower-but-different revision
  // still needs reconciliation rather than being silently ignored.
  if (summary.revision !== undefined) {
    return summary.revision !== localRevision;
  }

  // Older nodes have no revision field. Keep their timestamp watermark
  // separate from the monotonic revision so rolling upgrades never compare
  // an epoch-millisecond value to a small revision counter.
  return chatSessionUpdatedAtMs(summary) > (localUpdatedAtMs ?? 0);
}
