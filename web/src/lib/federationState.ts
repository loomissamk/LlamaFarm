import type { FederationRole } from '@/types/api';

export interface FederationTaskState {
  taskId: string;
  peerId: string;
  peerName: string;
  delegateAgent: string;
  status: 'status' | 'streaming' | 'done' | 'error';
  content: string;
  message: string;
  lastToolName?: string;
  lastToolOutput?: string;
  lastToolSuccess?: boolean;
  lastToolDurationSecs?: number;
  generationTps?: number;
  ttftMs?: number;
  promptTokens?: number;
  updatedAt: Date;
}

export interface StoredChatSessionSummary {
  id: string;
  title: string;
  temporary: boolean;
  updatedAt: Date;
}

interface PersistedFederationTaskState {
  taskId: string;
  peerId: string;
  peerName: string;
  delegateAgent: string;
  status: FederationTaskState['status'];
  content: string;
  message: string;
  lastToolName?: string;
  lastToolOutput?: string;
  lastToolSuccess?: boolean;
  lastToolDurationSecs?: number;
  generationTps?: number;
  ttftMs?: number;
  promptTokens?: number;
  updatedAt: string;
}

interface PersistedChatSessionSummary {
  id: string;
  title: string;
  temporary: boolean;
  updatedAt: string;
}

const ACTIVE_CHAT_STORAGE_KEY = 'llamafarm.agent_chat.active_session.v2';
const CHAT_SESSIONS_STORAGE_KEY = 'llamafarm.agent_chat.sessions.v2';
const FEDERATION_SELECTED_PEERS_STORAGE_KEY =
  'llamafarm.federation.selected_peers.v1';
const FEDERATION_TASKS_STORAGE_KEY = 'llamafarm.federation.tasks.v1';
const MAX_PERSISTED_TASKS_PER_SESSION = 8;

export function loadFederationPeerSelections(): Record<string, string[]> {
  if (typeof window === 'undefined') {
    return {};
  }

  try {
    const raw = localStorage.getItem(FEDERATION_SELECTED_PEERS_STORAGE_KEY);
    if (!raw) {
      return {};
    }

    const parsed = JSON.parse(raw) as Record<string, unknown>;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return {};
    }

    return Object.fromEntries(
      Object.entries(parsed).flatMap(([sessionId, peerIds]) => {
        if (
          typeof sessionId !== 'string' ||
          !Array.isArray(peerIds) ||
          peerIds.some((peerId) => typeof peerId !== 'string')
        ) {
          return [];
        }
        return [[sessionId, Array.from(new Set(peerIds as string[]))]];
      }),
    );
  } catch {
    return {};
  }
}

export function persistFederationPeerSelections(
  selections: Record<string, string[]>,
): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    const sanitizedEntries: Array<[string, string[]]> = Object.entries(selections).flatMap(
      ([sessionId, peerIds]) => {
        const sanitizedPeerIds = Array.from(
          new Set(peerIds.filter((peerId) => typeof peerId === 'string' && peerId.trim())),
        );
        return sanitizedPeerIds.length > 0 ? [[sessionId, sanitizedPeerIds]] : [];
      },
    );
    const sanitized = Object.fromEntries(sanitizedEntries);
    localStorage.setItem(
      FEDERATION_SELECTED_PEERS_STORAGE_KEY,
      JSON.stringify(sanitized),
    );
  } catch {
    // localStorage may be unavailable in some browser modes; fail silently.
  }
}

export function loadFederationTasksBySession(): Record<string, FederationTaskState[]> {
  if (typeof window === 'undefined') {
    return {};
  }

  try {
    const raw = localStorage.getItem(FEDERATION_TASKS_STORAGE_KEY);
    if (!raw) {
      return {};
    }

    const parsed = JSON.parse(raw) as Record<string, PersistedFederationTaskState[]>;
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return {};
    }

    return Object.fromEntries(
      Object.entries(parsed).flatMap(([sessionId, tasks]) => {
        if (typeof sessionId !== 'string' || !Array.isArray(tasks)) {
          return [];
        }

        const normalized = tasks
          .map((task): FederationTaskState | null => {
            if (!task || typeof task !== 'object') {
              return null;
            }

            const updatedAt = new Date(task.updatedAt);
            if (
              typeof task.taskId !== 'string' ||
              typeof task.peerId !== 'string' ||
              typeof task.peerName !== 'string' ||
              typeof task.delegateAgent !== 'string' ||
              !['status', 'streaming', 'done', 'error'].includes(task.status) ||
              typeof task.content !== 'string' ||
              typeof task.message !== 'string' ||
              Number.isNaN(updatedAt.getTime())
            ) {
              return null;
            }

            return {
              taskId: task.taskId,
              peerId: task.peerId,
              peerName: task.peerName,
              delegateAgent: task.delegateAgent,
              status: task.status,
              content: task.content,
              message: task.message,
              lastToolName:
                typeof task.lastToolName === 'string' ? task.lastToolName : undefined,
              lastToolOutput:
                typeof task.lastToolOutput === 'string' ? task.lastToolOutput : undefined,
              lastToolSuccess:
                typeof task.lastToolSuccess === 'boolean'
                  ? task.lastToolSuccess
                  : undefined,
              lastToolDurationSecs:
                typeof task.lastToolDurationSecs === 'number' &&
                Number.isFinite(task.lastToolDurationSecs)
                  ? task.lastToolDurationSecs
                  : undefined,
              generationTps:
                typeof task.generationTps === 'number' && Number.isFinite(task.generationTps)
                  ? task.generationTps
                  : undefined,
              ttftMs:
                typeof task.ttftMs === 'number' && Number.isFinite(task.ttftMs)
                  ? task.ttftMs
                  : undefined,
              promptTokens:
                typeof task.promptTokens === 'number' && Number.isFinite(task.promptTokens)
                  ? task.promptTokens
                  : undefined,
              updatedAt,
            };
          })
          .filter((task): task is FederationTaskState => task !== null)
          .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
          .slice(0, MAX_PERSISTED_TASKS_PER_SESSION);

        return normalized.length > 0 ? [[sessionId, normalized]] : [];
      }),
    );
  } catch {
    return {};
  }
}

export function persistFederationTasksBySession(
  tasksBySession: Record<string, FederationTaskState[]>,
): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    const payloadEntries: Array<[string, PersistedFederationTaskState[]]> = Object.entries(
      tasksBySession,
    ).flatMap(([sessionId, tasks]) => {
      const normalizedTasks = tasks
        .slice()
        .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
        .slice(0, MAX_PERSISTED_TASKS_PER_SESSION)
        .map((task): PersistedFederationTaskState => ({
          taskId: task.taskId,
          peerId: task.peerId,
          peerName: task.peerName,
          delegateAgent: task.delegateAgent,
          status: task.status,
          content: task.content,
          message: task.message,
          lastToolName: task.lastToolName,
          lastToolOutput: task.lastToolOutput,
          lastToolSuccess: task.lastToolSuccess,
          lastToolDurationSecs: task.lastToolDurationSecs,
          generationTps: task.generationTps,
          ttftMs: task.ttftMs,
          promptTokens: task.promptTokens,
          updatedAt: task.updatedAt.toISOString(),
        }));
      return normalizedTasks.length > 0 ? [[sessionId, normalizedTasks]] : [];
    });
    const payload = Object.fromEntries(payloadEntries);

    localStorage.setItem(FEDERATION_TASKS_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // localStorage may be unavailable in some browser modes; fail silently.
  }
}

export function loadPersistedChatSessionSummaries(): StoredChatSessionSummary[] {
  if (typeof window === 'undefined') {
    return [];
  }

  try {
    const raw = localStorage.getItem(CHAT_SESSIONS_STORAGE_KEY);
    if (!raw) {
      return [];
    }

    const parsed = JSON.parse(raw) as PersistedChatSessionSummary[];
    if (!Array.isArray(parsed)) {
      return [];
    }

    return parsed
      .map((session): StoredChatSessionSummary | null => {
        if (!session || typeof session !== 'object') {
          return null;
        }

        const updatedAt = new Date(session.updatedAt);
        if (
          typeof session.id !== 'string' ||
          typeof session.title !== 'string' ||
          typeof session.temporary !== 'boolean' ||
          Number.isNaN(updatedAt.getTime())
        ) {
          return null;
        }

        return {
          id: session.id,
          title: session.title,
          temporary: session.temporary,
          updatedAt,
        };
      })
      .filter((session): session is StoredChatSessionSummary => session !== null)
      .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime());
  } catch {
    return [];
  }
}

export function loadPersistedActiveChatSessionId(): string | null {
  if (typeof window === 'undefined') {
    return null;
  }

  try {
    return localStorage.getItem(ACTIVE_CHAT_STORAGE_KEY);
  } catch {
    return null;
  }
}

export function persistActiveChatSessionId(sessionId: string | null): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    if (!sessionId) {
      localStorage.removeItem(ACTIVE_CHAT_STORAGE_KEY);
      return;
    }

    localStorage.setItem(ACTIVE_CHAT_STORAGE_KEY, sessionId);
  } catch {
    // localStorage may be unavailable in some browser modes; fail silently.
  }
}

export function summarizeFederationRole(role: FederationRole): string {
  switch (role) {
    case 'master':
      return 'master';
    case 'worker':
      return 'worker';
    case 'both':
      return 'both';
    case 'disabled':
      return 'disabled';
    default:
      return 'unknown';
  }
}
