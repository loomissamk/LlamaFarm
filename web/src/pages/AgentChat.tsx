import { useEffect, useMemo, useRef, useState } from 'react';
import {
  Activity,
  AlertCircle,
  Bot,
  Cpu,
  History,
  Network,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Send,
  Trash2,
  User,
  Wrench,
} from 'lucide-react';
import { getFederationPeers, updateFederationPeerRole } from '@/lib/api';
import { WebSocketClient, type SeedChatMessage } from '@/lib/ws';
import type {
  FederationPeerSummary,
  FederationPeersResponse,
  FederationRole,
  WsMessage,
} from '@/types/api';

type ChatRole = 'user' | 'agent';
type ChatMessageKind = 'message' | 'tool_call' | 'tool_result' | 'error';

interface ChatMessage {
  id: string;
  role: ChatRole;
  kind: ChatMessageKind;
  content: string;
  timestamp: Date;
  stats?: ChatMessageStats;
  seedRole?: SeedChatMessage['role'];
  seedContent?: string;
}

interface ChatMessageStats {
  completionSeconds: number;
  estimatedOutputTokens: number;
  estimatedTokensPerSecond: number;
}

interface ChatSession {
  id: string;
  title: string;
  temporary: boolean;
  createdAt: Date;
  updatedAt: Date;
  messages: ChatMessage[];
}

interface PersistedChatMessage {
  id: string;
  role: ChatRole;
  kind: ChatMessageKind;
  content: string;
  timestamp: string;
  stats?: ChatMessageStats;
  seedRole?: SeedChatMessage['role'];
  seedContent?: string;
}

interface PersistedChatSession {
  id: string;
  title: string;
  temporary: boolean;
  createdAt: string;
  updatedAt: string;
  messages: PersistedChatMessage[];
}

interface InitialChatState {
  sessions: ChatSession[];
  activeSessionId: string;
}

interface PendingToolCallSeed {
  id: string;
  name: string;
}

interface AppendMessageOptions {
  stats?: ChatMessageStats;
  seed?: Pick<ChatMessage, 'seedRole' | 'seedContent'>;
}

interface FederationTaskState {
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
  updatedAt: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK =
  'Tool execution completed, but no final response text was returned.';
const CHAT_SESSIONS_STORAGE_KEY = 'llamafarm.agent_chat.sessions.v2';
const ACTIVE_CHAT_STORAGE_KEY = 'llamafarm.agent_chat.active_session.v2';
const CHAT_SIDEBAR_COLLAPSED_STORAGE_KEY = 'llamafarm.agent_chat.sidebar_collapsed.v1';
const MAX_PERSISTED_MESSAGES = 500;
const MAX_PERSISTED_SESSIONS = 40;

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;

  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 10)}`;
}

function makeSessionId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;

  fallbackMessageIdCounter += 1;
  return `chat_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}`;
}

function truncateLabel(value: string, max = 48): string {
  const trimmed = value.trim();
  if (trimmed.length <= max) {
    return trimmed;
  }
  return `${trimmed.slice(0, max - 1).trimEnd()}...`;
}

function sortSessions(sessions: ChatSession[]): ChatSession[] {
  return [...sessions].sort((a, b) => b.updatedAt.getTime() - a.updatedAt.getTime());
}

function deriveSessionTitle(messages: ChatMessage[], temporary: boolean): string {
  const firstUserMessage = messages.find((message) => message.role === 'user');
  if (firstUserMessage) {
    return truncateLabel(firstUserMessage.content.replace(/\s+/g, ' '), 44);
  }

  return temporary ? 'Temporary chat' : 'New chat';
}

function buildSessionPreview(session: ChatSession): string {
  const lastMessage = [...session.messages].reverse().find((message) => message.content.trim());
  if (!lastMessage) {
    return session.temporary ? 'Temporary session' : 'Start a new conversation';
  }

  return truncateLabel(lastMessage.content.replace(/\s+/g, ' '), 72);
}

function estimateOutputTokens(text: string): number {
  const normalized = text.trim();
  if (!normalized) {
    return 0;
  }

  return Math.max(1, Math.round(normalized.length / 4));
}

function formatCompletionSeconds(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) {
    return '0.0s';
  }

  if (seconds < 60) {
    return `${seconds.toFixed(1)}s`;
  }

  const wholeSeconds = Math.round(seconds);
  const minutes = Math.floor(wholeSeconds / 60);
  const remainderSeconds = wholeSeconds % 60;
  return `${minutes}m ${remainderSeconds}s`;
}

function formatAssistantMessageMeta(message: ChatMessage): string {
  const timestamp = message.timestamp.toLocaleTimeString();
  if (message.role !== 'agent' || message.kind !== 'message' || !message.stats) {
    return timestamp;
  }

  return [
    timestamp,
    formatCompletionSeconds(message.stats.completionSeconds),
    `~${message.stats.estimatedOutputTokens} tok`,
    `~${message.stats.estimatedTokensPerSecond.toFixed(1)} t/s`,
  ].join(' · ');
}

function createChatSession(temporary: boolean): ChatSession {
  const now = new Date();
  return {
    id: makeSessionId(),
    title: temporary ? 'Temporary chat' : 'New chat',
    temporary,
    createdAt: now,
    updatedAt: now,
    messages: [],
  };
}

function normalizeSession(session: ChatSession): ChatSession {
  const limitedMessages = session.messages.slice(-MAX_PERSISTED_MESSAGES);
  const latestTimestamp =
    limitedMessages[limitedMessages.length - 1]?.timestamp ?? session.updatedAt;

  return {
    ...session,
    messages: limitedMessages,
    updatedAt: latestTimestamp,
    title: deriveSessionTitle(limitedMessages, session.temporary),
  };
}

function loadPersistedSessions(): ChatSession[] {
  if (typeof window === 'undefined') {
    return [];
  }

  try {
    const raw = localStorage.getItem(CHAT_SESSIONS_STORAGE_KEY);
    if (!raw) {
      return [];
    }

    const parsed = JSON.parse(raw) as PersistedChatSession[];
    if (!Array.isArray(parsed)) {
      return [];
    }

    const sessions = parsed
      .map((session): ChatSession | null => {
        if (!session || typeof session !== 'object') {
          return null;
        }

        const createdAt = new Date(session.createdAt);
        const updatedAt = new Date(session.updatedAt);
        if (
          typeof session.id !== 'string' ||
          typeof session.title !== 'string' ||
          typeof session.temporary !== 'boolean' ||
          Number.isNaN(createdAt.getTime()) ||
          Number.isNaN(updatedAt.getTime()) ||
          !Array.isArray(session.messages)
        ) {
          return null;
        }

        const messages = session.messages
          .map((message): ChatMessage | null => {
            if (!message || typeof message !== 'object') {
              return null;
            }

            const timestamp = new Date(message.timestamp);
            if (
              typeof message.id !== 'string' ||
              (message.role !== 'user' && message.role !== 'agent') ||
              !['message', 'tool_call', 'tool_result', 'error'].includes(message.kind) ||
              typeof message.content !== 'string' ||
              Number.isNaN(timestamp.getTime())
            ) {
              return null;
            }

            return {
              id: message.id,
              role: message.role,
              kind: message.kind,
              content: message.content,
              timestamp,
              stats:
                message.stats &&
                typeof message.stats.completionSeconds === 'number' &&
                Number.isFinite(message.stats.completionSeconds) &&
                message.stats.completionSeconds > 0 &&
                typeof message.stats.estimatedOutputTokens === 'number' &&
                Number.isFinite(message.stats.estimatedOutputTokens) &&
                message.stats.estimatedOutputTokens >= 0 &&
                typeof message.stats.estimatedTokensPerSecond === 'number' &&
                Number.isFinite(message.stats.estimatedTokensPerSecond) &&
                message.stats.estimatedTokensPerSecond > 0
                  ? {
                      completionSeconds: message.stats.completionSeconds,
                      estimatedOutputTokens: message.stats.estimatedOutputTokens,
                      estimatedTokensPerSecond: message.stats.estimatedTokensPerSecond,
                    }
                  : undefined,
              seedRole:
                message.seedRole === 'user' ||
                message.seedRole === 'assistant' ||
                message.seedRole === 'tool' ||
                message.seedRole === 'agent'
                  ? message.seedRole
                  : undefined,
              seedContent:
                typeof message.seedContent === 'string'
                  ? message.seedContent
                  : undefined,
            };
          })
          .filter((message): message is ChatMessage => message !== null)
          .slice(-MAX_PERSISTED_MESSAGES);

        return normalizeSession({
          id: session.id,
          title: session.title,
          temporary: session.temporary,
          createdAt,
          updatedAt,
          messages,
        });
      })
      .filter((session): session is ChatSession => session !== null && !session.temporary)
      .slice(0, MAX_PERSISTED_SESSIONS);

    return sortSessions(sessions);
  } catch {
    return [];
  }
}

function persistSessions(sessions: ChatSession[]): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    const payload: PersistedChatSession[] = sortSessions(sessions)
      .filter((session) => !session.temporary)
      .slice(0, MAX_PERSISTED_SESSIONS)
      .map((session) => ({
        id: session.id,
        title: session.title,
        temporary: session.temporary,
        createdAt: session.createdAt.toISOString(),
        updatedAt: session.updatedAt.toISOString(),
        messages: session.messages.slice(-MAX_PERSISTED_MESSAGES).map((message) => ({
          id: message.id,
          role: message.role,
          kind: message.kind,
          content: message.content,
          timestamp: message.timestamp.toISOString(),
          stats: message.stats,
          seedRole: message.seedRole,
          seedContent: message.seedContent,
        })),
      }));

    localStorage.setItem(CHAT_SESSIONS_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // localStorage may be unavailable in some browser modes; fail silently.
  }
}

function loadActiveSessionId(sessions: ChatSession[]): string {
  if (typeof window === 'undefined') {
    return sessions[0]?.id ?? createChatSession(false).id;
  }

  const persisted = localStorage.getItem(ACTIVE_CHAT_STORAGE_KEY);
  if (persisted && sessions.some((session) => session.id === persisted)) {
    return persisted;
  }

  return sessions[0]?.id ?? createChatSession(false).id;
}

function persistActiveSessionId(session: ChatSession | undefined): void {
  if (typeof window === 'undefined') {
    return;
  }

  try {
    if (!session || session.temporary) {
      localStorage.removeItem(ACTIVE_CHAT_STORAGE_KEY);
      return;
    }

    localStorage.setItem(ACTIVE_CHAT_STORAGE_KEY, session.id);
  } catch {
    // localStorage may be unavailable in some browser modes; fail silently.
  }
}

function loadInitialChatState(): InitialChatState {
  const persistedSessions = loadPersistedSessions();
  const sessions = persistedSessions.length > 0 ? persistedSessions : [createChatSession(false)];
  const activeSessionId = loadActiveSessionId(sessions);
  return { sessions, activeSessionId };
}

function loadSidebarCollapsed(): boolean {
  if (typeof window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_SIDEBAR_COLLAPSED_STORAGE_KEY) === '1';
  } catch {
    return false;
  }
}

function formatToolResultMessage(msg: WsMessage): string {
  const toolName = msg.name ?? 'unknown';
  const status =
    msg.success === true ? 'SUCCESS' : msg.success === false ? 'FAIL' : 'RESULT';
  const duration =
    typeof msg.duration_secs === 'number' ? ` (${msg.duration_secs}s)` : '';
  const output = msg.output?.trim() ? msg.output : '(no output)';
  return `[Tool ${status}] ${toolName}${duration}\n${output}`;
}

function normalizeSeedToolArgs(
  toolName: string,
  args: unknown,
): Record<string, unknown> {
  if (args && typeof args === 'object' && !Array.isArray(args)) {
    const normalized = { ...(args as Record<string, unknown>) };
    const hint =
      typeof normalized.hint === 'string' ? normalized.hint.trim() : '';
    if (toolName === 'shell' && hint && typeof normalized.command !== 'string') {
      normalized.command = hint;
    }
    return normalized;
  }

  return {};
}

function buildAssistantToolCallSeed(
  toolCallId: string,
  toolName: string,
  args: unknown,
): string {
  return JSON.stringify({
    content: null,
    tool_calls: [
      {
        id: toolCallId,
        name: toolName,
        arguments: JSON.stringify(normalizeSeedToolArgs(toolName, args)),
      },
    ],
  });
}

function extractToolResultOutput(msg: WsMessage): string {
  const output = msg.output?.trim();
  return output && output.length > 0 ? msg.output! : '(no output)';
}

function buildToolResultSeed(
  toolCallId: string,
  toolName: string,
  output: string,
): string {
  return JSON.stringify({
    tool_call_id: toolCallId,
    tool_name: toolName,
    content: output,
  });
}

function buildHistorySeed(messages: ChatMessage[]): SeedChatMessage[] {
  return messages
    .flatMap((message) => {
      const role =
        message.seedRole ?? (message.role === 'agent' ? 'assistant' : 'user');
      const content = (message.seedContent ?? message.content).trim();
      if (!content) {
        return [];
      }

      return [{ role, content }];
    });
}

function federationRoleAllowsWorker(role: FederationRole): boolean {
  return role === 'worker' || role === 'both';
}

function canUseFederationPeer(peer: FederationPeerSummary): boolean {
  return (
    peer.online &&
    peer.allow_remote_subagents &&
    federationRoleAllowsWorker(peer.assigned_role) &&
    federationRoleAllowsWorker(peer.role_support)
  );
}

export default function AgentChat() {
  const initialStateRef = useRef<InitialChatState | null>(null);
  if (initialStateRef.current === null) {
    initialStateRef.current = loadInitialChatState();
  }

  const [sessions, setSessions] = useState<ChatSession[]>(initialStateRef.current.sessions);
  const [activeSessionId, setActiveSessionId] = useState(initialStateRef.current.activeSessionId);
  const [input, setInput] = useState('');
  const [typingSessionIds, setTypingSessionIds] = useState<string[]>([]);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => loadSidebarCollapsed());
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [tps, setTps] = useState(0);
  const [streaming, setStreaming] = useState(false);
  const [federation, setFederation] = useState<FederationPeersResponse | null>(null);
  const [federationLoading, setFederationLoading] = useState(true);
  const [selectedFederationPeerIdsBySession, setSelectedFederationPeerIdsBySession] = useState<
    Record<string, string[]>
  >({});
  const [federationTasksBySession, setFederationTasksBySession] = useState<
    Record<string, FederationTaskState[]>
  >({});

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef<Record<string, string>>({});
  const pendingToolCallsRef = useRef<Record<string, PendingToolCallSeed[]>>({});
  const responseStartRef = useRef<number | null>(null);
  const streamStartRef = useRef<number | null>(null);
  const charCountRef = useRef(0);
  const activeSessionIdRef = useRef(activeSessionId);

  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? sessions[0],
    [sessions, activeSessionId],
  );

  const activeMessages = activeSession?.messages ?? [];
  const activeSessionTyping = activeSession
    ? typingSessionIds.includes(activeSession.id)
    : false;
  const activeFederationTasks = activeSession
    ? federationTasksBySession[activeSession.id] ?? []
    : [];
  const selectedFederationPeerIds = activeSession
    ? selectedFederationPeerIdsBySession[activeSession.id] ?? []
    : [];
  const availableFederationPeers = federation?.peers ?? [];

  useEffect(() => {
    activeSessionIdRef.current = activeSession?.id ?? activeSessionId;
  }, [activeSession, activeSessionId]);

  useEffect(() => {
    let cancelled = false;

    const refreshFederation = async () => {
      try {
        const response = await getFederationPeers();
        if (!cancelled) {
          setFederation(response);
          setFederationLoading(false);
        }
      } catch {
        if (!cancelled) {
          setFederationLoading(false);
        }
      }
    };

    void refreshFederation();
    const interval = window.setInterval(() => {
      void refreshFederation();
    }, 5000);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    if (!federation?.enabled) {
      return;
    }

    const validPeerIds = new Set(
      federation.peers.filter(canUseFederationPeer).map((peer) => peer.peer_id),
    );
    setSelectedFederationPeerIdsBySession((prev) => {
      let changed = false;
      const next = Object.fromEntries(
        Object.entries(prev).map(([sessionId, peerIds]) => {
          const filtered = peerIds.filter((peerId) => validPeerIds.has(peerId));
          if (filtered.length !== peerIds.length) {
            changed = true;
          }
          return [sessionId, filtered];
        }),
      );
      return changed ? next : prev;
    });
  }, [federation]);

  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = () => {
      setConnected(false);
      responseStartRef.current = null;
      streamStartRef.current = null;
      charCountRef.current = 0;
      setStreaming(false);
    };

    ws.onError = () => {
      setError('Connection error. Attempting to reconnect...');
      setStreaming(false);
    };

    ws.onMessage = (msg: WsMessage) => {
      const sessionId = msg.session_id ?? activeSessionIdRef.current;
      if (!sessionId) {
        return;
      }

      const appendMessage = (
        role: ChatRole,
        kind: ChatMessageKind,
        content: string,
        timestamp = new Date(),
        options?: AppendMessageOptions,
      ) => {
        setSessions((prev) => {
          const existing = prev.find((session) => session.id === sessionId);
          const baseSession =
            existing ??
            ({
              ...createChatSession(false),
              id: sessionId,
            } as ChatSession);
          const nextSession = normalizeSession({
            ...baseSession,
            messages: [
              ...baseSession.messages,
              {
                id: makeMessageId(),
                role,
                kind,
                content,
                timestamp,
                stats: options?.stats,
                seedRole: options?.seed?.seedRole,
                seedContent: options?.seed?.seedContent,
              },
            ],
            updatedAt: timestamp,
          });
          const next = existing
            ? prev.map((session) => (session.id === sessionId ? nextSession : session))
            : [nextSession, ...prev];

          return sortSessions(next);
        });
      };

      const clearTypingForSession = () => {
        setTypingSessionIds((prev) => prev.filter((id) => id !== sessionId));
        delete pendingContentRef.current[sessionId];
      };

      const upsertFederationTask = (
        taskId: string,
        update: (current: FederationTaskState | undefined) => FederationTaskState,
      ) => {
        setFederationTasksBySession((prev) => {
          const existingTasks = prev[sessionId] ?? [];
          const taskIndex = existingTasks.findIndex((task) => task.taskId === taskId);
          const current = taskIndex >= 0 ? existingTasks[taskIndex] : undefined;
          const nextTask = update(current);
          const nextTasks =
            taskIndex >= 0
              ? existingTasks.map((task, index) => (index === taskIndex ? nextTask : task))
              : [nextTask, ...existingTasks];

          return {
            ...prev,
            [sessionId]: nextTasks
              .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
              .slice(0, 8),
          };
        });
      };

      switch (msg.type) {
        case 'chunk': {
          setTypingSessionIds((prev) =>
            prev.includes(sessionId) ? prev : [...prev, sessionId],
          );
          pendingContentRef.current[sessionId] =
            (pendingContentRef.current[sessionId] ?? '') + (msg.content ?? '');
          setStreaming(true);
          // TPS tracking
          if (streamStartRef.current === null) {
            streamStartRef.current = performance.now();
            charCountRef.current = 0;
          }
          charCountRef.current += (msg.content ?? '').length;
          const elapsed = (performance.now() - streamStartRef.current) / 1000;
          if (elapsed > 0.2) {
            setTps(Math.max(1, Math.round(charCountRef.current / 4 / elapsed)));
          }
          break;
        }

        case 'message':
        case 'done': {
          const content = (
            msg.full_response ??
            msg.content ??
            pendingContentRef.current[sessionId] ??
            ''
          ).trim();
          const outputTokens = estimateOutputTokens(content);
          const startedAt = responseStartRef.current ?? streamStartRef.current;
          const completionSeconds =
            startedAt !== null ? Math.max((performance.now() - startedAt) / 1000, 0.05) : 0;
          const estimatedTokensPerSecond =
            completionSeconds > 0 && outputTokens > 0 ? outputTokens / completionSeconds : 0;
          if (estimatedTokensPerSecond > 0) {
            setTps(Math.max(1, Math.round(estimatedTokensPerSecond)));
          }
          appendMessage(
            'agent',
            'message',
            content || EMPTY_DONE_FALLBACK,
            new Date(),
            {
              stats:
                completionSeconds > 0 &&
                outputTokens > 0 &&
                estimatedTokensPerSecond > 0
                  ? {
                      completionSeconds,
                      estimatedOutputTokens: outputTokens,
                      estimatedTokensPerSecond,
                    }
                  : undefined,
              seed: {
                seedRole: 'assistant',
                seedContent: content || EMPTY_DONE_FALLBACK,
              },
            },
          );
          clearTypingForSession();
          responseStartRef.current = null;
          streamStartRef.current = null;
          charCountRef.current = 0;
          setStreaming(false);
          break;
        }

        case 'tool_call': {
          const toolName = msg.name ?? 'unknown';
          const toolCallId = `ws_tool_${makeMessageId()}`;
          const pendingCalls = pendingToolCallsRef.current[sessionId] ?? [];
          pendingCalls.push({ id: toolCallId, name: toolName });
          pendingToolCallsRef.current[sessionId] = pendingCalls;
          appendMessage(
            'agent',
            'tool_call',
            `[Tool Call] ${toolName}(${JSON.stringify(msg.args ?? {})})`,
            new Date(),
            {
              seed: {
                seedRole: 'assistant',
                seedContent: buildAssistantToolCallSeed(toolCallId, toolName, msg.args),
              },
            },
          );
          break;
        }

        case 'tool_result': {
          const toolName = msg.name ?? 'unknown';
          const pendingCalls = pendingToolCallsRef.current[sessionId] ?? [];
          const matchingIndex = pendingCalls.findIndex((call) => call.name === toolName);
          const pendingIndex =
            matchingIndex >= 0 ? matchingIndex : pendingCalls.length > 0 ? 0 : -1;
          const [pendingCall] =
            pendingIndex >= 0 ? pendingCalls.splice(pendingIndex, 1) : [];
          if (pendingCalls.length > 0) {
            pendingToolCallsRef.current[sessionId] = pendingCalls;
          } else {
            delete pendingToolCallsRef.current[sessionId];
          }

          const output = extractToolResultOutput(msg);
          appendMessage(
            'agent',
            'tool_result',
            formatToolResultMessage(msg),
            new Date(),
            {
              seed: {
                seedRole: 'tool',
                seedContent: buildToolResultSeed(
                  pendingCall?.id ?? `ws_tool_${makeMessageId()}`,
                  toolName,
                  output,
                ),
              },
            },
          );
          break;
        }

        case 'federation_status':
        case 'federation_chunk':
        case 'federation_tool_call':
        case 'federation_tool_result':
        case 'federation_done':
        case 'federation_error': {
          if (!msg.task_id || !msg.peer_id || !msg.peer_name || !msg.delegate_agent) {
            break;
          }

          const now = new Date();
          upsertFederationTask(msg.task_id, (current) => {
            const base: FederationTaskState =
              current ?? {
                taskId: msg.task_id!,
                peerId: msg.peer_id!,
                peerName: msg.peer_name!,
                delegateAgent: msg.delegate_agent!,
                status: 'status',
                content: '',
                message: '',
                updatedAt: now,
              };

            switch (msg.type) {
              case 'federation_status':
                return {
                  ...base,
                  status: 'status',
                  message: msg.message ?? `Delegated to ${msg.peer_name}`,
                  updatedAt: now,
                };
              case 'federation_chunk':
                return {
                  ...base,
                  status: 'streaming',
                  content: `${base.content}${msg.content ?? ''}`,
                  message: msg.message ?? `Streaming from ${msg.peer_name}`,
                  updatedAt: now,
                };
              case 'federation_tool_call':
                return {
                  ...base,
                  status: base.status === 'done' ? 'done' : 'streaming',
                  lastToolName: msg.name ?? base.lastToolName,
                  message: msg.name
                    ? `${msg.peer_name} running ${msg.name}`
                    : `Tool call on ${msg.peer_name}`,
                  updatedAt: now,
                };
              case 'federation_tool_result':
                return {
                  ...base,
                  status: base.status === 'done' ? 'done' : 'streaming',
                  lastToolName: msg.name ?? base.lastToolName,
                  lastToolOutput: msg.output ?? base.lastToolOutput,
                  lastToolSuccess: msg.success ?? base.lastToolSuccess,
                  lastToolDurationSecs: msg.duration_secs ?? base.lastToolDurationSecs,
                  message: msg.name
                    ? `${msg.peer_name} finished ${msg.name}`
                    : `Tool result from ${msg.peer_name}`,
                  updatedAt: now,
                };
              case 'federation_done':
                return {
                  ...base,
                  status: 'done',
                  content: base.content || msg.message || '',
                  message: msg.message ?? `Remote task completed on ${msg.peer_name}`,
                  updatedAt: now,
                };
              case 'federation_error':
                return {
                  ...base,
                  status: 'error',
                  message: msg.message ?? `Remote task failed on ${msg.peer_name}`,
                  updatedAt: now,
                };
              default:
                return base;
            }
          });
          break;
        }

        case 'error':
          appendMessage(
            'agent',
            'error',
            `[Error] ${msg.message ?? 'Unknown error'}`,
            new Date(),
            {
              seed: {
                seedRole: 'assistant',
                seedContent: `[Error] ${msg.message ?? 'Unknown error'}`,
              },
            },
          );
          clearTypingForSession();
          responseStartRef.current = null;
          streamStartRef.current = null;
          charCountRef.current = 0;
          setStreaming(false);
          break;
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  useEffect(() => {
    const firstSessionId = sessions[0]?.id;
    if (!activeSession && firstSessionId) {
      setActiveSessionId(firstSessionId);
    }
  }, [activeSession, sessions]);

  useEffect(() => {
    persistSessions(sessions);
  }, [sessions]);

  useEffect(() => {
    persistActiveSessionId(activeSession);
  }, [activeSession]);

  useEffect(() => {
    try {
      localStorage.setItem(
        CHAT_SIDEBAR_COLLAPSED_STORAGE_KEY,
        sidebarCollapsed ? '1' : '0',
      );
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [sidebarCollapsed]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [activeMessages, activeSessionTyping]);

  const createAndSelectSession = (temporary: boolean) => {
    const session = createChatSession(temporary);
    setSessions((prev) => sortSessions([session, ...prev]));
    setActiveSessionId(session.id);
    setInput('');
    inputRef.current?.focus();
  };

  const handleDeleteSession = (sessionId: string) => {
    const nextExisting = sessions.find((session) => session.id !== sessionId);
    const replacementSession = !nextExisting ? createChatSession(false) : null;

    wsRef.current?.deleteSession(sessionId);
    delete pendingContentRef.current[sessionId];
    delete pendingToolCallsRef.current[sessionId];
    setTypingSessionIds((prev) => prev.filter((id) => id !== sessionId));
    setSelectedFederationPeerIdsBySession((prev) => {
      const next = { ...prev };
      delete next[sessionId];
      return next;
    });
    setFederationTasksBySession((prev) => {
      const next = { ...prev };
      delete next[sessionId];
      return next;
    });

    setSessions((prev) => {
      const next = prev.filter((session) => session.id !== sessionId);
      if (next.length > 0) {
        return sortSessions(next);
      }
      return replacementSession ? [replacementSession] : [createChatSession(false)];
    });

    if (activeSessionId === sessionId) {
      setActiveSessionId(nextExisting?.id ?? replacementSession?.id ?? activeSessionId);
    }
  };

  const handleFederationPeerToggle = (peerId: string, enabled: boolean) => {
    if (!activeSession) {
      return;
    }

    setSelectedFederationPeerIdsBySession((prev) => {
      const current = prev[activeSession.id] ?? [];
      const nextPeerIds = enabled
        ? Array.from(new Set([...current, peerId]))
        : current.filter((candidate) => candidate !== peerId);
      return {
        ...prev,
        [activeSession.id]: nextPeerIds,
      };
    });
  };

  const handleFederationRoleChange = async (peerId: string, role: FederationRole) => {
    try {
      const response = await updateFederationPeerRole(peerId, role);
      setFederation((prev) =>
        prev
          ? {
              ...prev,
              peers: prev.peers.map((peer) =>
                peer.peer_id === peerId ? response.peer : peer,
              ),
            }
          : prev,
      );
    } catch {
      setError('Failed to update federation role.');
    }
  };

  const handleSend = () => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected || !activeSession) {
      return;
    }

    const timestamp = new Date();
    const userMessage: ChatMessage = {
      id: makeMessageId(),
      role: 'user',
      kind: 'message',
      content: trimmed,
      timestamp,
    };

    const updatedSession = normalizeSession({
      ...activeSession,
      messages: [...activeSession.messages, userMessage],
      updatedAt: timestamp,
    });

    setSessions((prev) =>
      sortSessions(prev.map((session) => (session.id === activeSession.id ? updatedSession : session))),
    );

    try {
      wsRef.current.sendMessage({
        content: trimmed,
        sessionId: activeSession.id,
        temporary: activeSession.temporary,
        historySeed: buildHistorySeed(updatedSession.messages),
        federationPeerIds: selectedFederationPeerIds,
      });
      responseStartRef.current = performance.now();
      streamStartRef.current = null;
      charCountRef.current = 0;
      setTps(0);
      setStreaming(true);
      setTypingSessionIds((prev) =>
        prev.includes(activeSession.id) ? prev : [...prev, activeSession.id],
      );
      pendingContentRef.current[activeSession.id] = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    inputRef.current?.focus();
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === 'Enter' && !event.shiftKey) {
      event.preventDefault();
      handleSend();
    }
  };

  const visibleFederationTasks = [...activeFederationTasks]
    .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
    .slice(0, 6);
  const federationEnabled = federation?.enabled ?? false;

  return (
    <div
      className={`grid h-[calc(100vh-3.5rem)] min-h-0 grid-cols-1 ${
        sidebarCollapsed ? 'md:grid-cols-1' : 'md:grid-cols-[18rem_minmax(0,1fr)]'
      }`}
    >
      <aside
        className={`min-h-0 flex-col border-b border-gray-800 bg-gray-950 md:border-b-0 md:border-r ${
          sidebarCollapsed ? 'hidden' : 'flex'
        }`}
      >
        <div className="border-b border-gray-800 p-4">
          <div className="flex items-center gap-2 text-sm font-semibold text-white">
            <History className="h-4 w-4 text-blue-400" />
            Chats
          </div>
          <div className="mt-3 grid grid-cols-2 gap-2">
            <button
              onClick={() => createAndSelectSession(false)}
              className="inline-flex items-center justify-center gap-2 rounded-lg bg-blue-600 px-3 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700"
            >
              <Plus className="h-4 w-4" />
              New chat
            </button>
            <button
              onClick={() => createAndSelectSession(true)}
              className="rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-blue-500 hover:text-white"
            >
              Temporary
            </button>
          </div>
          <p className="mt-3 text-xs text-gray-500">
            Regular chats persist locally. Temporary chats stay in memory only.
          </p>
        </div>

        <div className="flex-1 space-y-2 overflow-y-auto p-3">
          {sessions.map((session) => {
            const selected = session.id === activeSession?.id;
            const typing = typingSessionIds.includes(session.id);
            return (
              <div
                key={session.id}
                className={`group rounded-xl border transition-colors ${
                  selected
                    ? 'border-blue-500 bg-blue-950/40'
                    : 'border-gray-800 bg-gray-900 hover:border-gray-700'
                }`}
              >
                <button
                  onClick={() => setActiveSessionId(session.id)}
                  className="w-full px-3 py-3 text-left"
                >
                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="flex items-center gap-2">
                        <span className="truncate text-sm font-medium text-white">
                          {session.title}
                        </span>
                        {session.temporary && (
                          <span className="rounded-full border border-amber-500/30 bg-amber-500/10 px-2 py-0.5 text-[10px] uppercase tracking-wide text-amber-300">
                            Temp
                          </span>
                        )}
                        {typing && (
                          <span className="rounded-full border border-emerald-500/30 bg-emerald-500/10 px-2 py-0.5 text-[10px] uppercase tracking-wide text-emerald-300">
                            Live
                          </span>
                        )}
                      </div>
                      <p className="mt-1 line-clamp-2 text-xs text-gray-500">
                        {buildSessionPreview(session)}
                      </p>
                    </div>
                    <span className="whitespace-nowrap text-[10px] text-gray-600">
                      {session.updatedAt.toLocaleTimeString()}
                    </span>
                  </div>
                </button>

                <div className="flex items-center justify-end px-3 pb-3">
                  <button
                    onClick={() => handleDeleteSession(session.id)}
                    className="inline-flex items-center gap-1 rounded-md px-2 py-1 text-xs text-gray-500 transition-colors hover:bg-red-950/40 hover:text-red-300"
                    title="Delete chat"
                  >
                    <Trash2 className="h-3.5 w-3.5" />
                    Delete
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </aside>

      <section className="flex min-h-0 flex-col">
        <div className="border-b border-gray-800 bg-gray-950 px-4 py-3">
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-3">
              <button
                onClick={() => setSidebarCollapsed((prev) => !prev)}
                className="inline-flex h-10 items-center gap-2 rounded-lg border border-gray-700 px-3 text-sm text-gray-300 transition-colors hover:bg-gray-900 hover:text-white"
                title={sidebarCollapsed ? 'Show chat history' : 'Hide chat history'}
              >
                {sidebarCollapsed ? (
                  <PanelLeftOpen className="h-4 w-4" />
                ) : (
                  <PanelLeftClose className="h-4 w-4" />
                )}
                <span className="hidden sm:inline">
                  {sidebarCollapsed ? 'Show history' : 'Hide history'}
                </span>
              </button>

              <div className="min-w-0">
                <h2 className="truncate text-lg font-semibold text-white">
                  {activeSession?.title ?? 'Agent Chat'}
                </h2>
                <p className="mt-1 text-xs text-gray-500">
                  {activeSession?.temporary
                    ? 'Temporary chat with isolated backend session context.'
                    : 'Saved local chat with reusable backend session context.'}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2">
              {streaming && tps > 0 && (
                <span
                  className="font-mono text-xs tabular-nums text-green-400"
                  title="Estimated tokens per second while the current reply is streaming"
                >
                  ~{tps} t/s live
                </span>
              )}
              <div className="rounded-full border border-gray-800 px-3 py-1 text-xs text-gray-400">
                {activeMessages.length} messages
              </div>
            </div>
          </div>
        </div>

        {error && (
          <div className="flex items-center gap-2 border-b border-red-700 bg-red-900/30 px-4 py-2 text-sm text-red-300">
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            {error}
          </div>
        )}

        {(federationLoading || federationEnabled || visibleFederationTasks.length > 0) && (
          <div className="border-b border-gray-800 bg-gray-950/70 px-4 py-4">
            <div className="grid gap-4 xl:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
              <div className="rounded-2xl border border-gray-800 bg-gray-900/80 p-4">
                <div className="flex items-center gap-2 text-sm font-semibold text-white">
                  <Network className="h-4 w-4 text-cyan-400" />
                  LAN Federation
                </div>
                <p className="mt-1 text-xs text-gray-500">
                  {federationEnabled && federation?.local_node
                    ? `${federation.local_node.display_name} on :${federation.local_node.api_port} · discovery ${federation.local_node.discovery_mode}`
                    : 'Federation is disabled. Local-only chat behavior remains unchanged.'}
                </p>

                {federationLoading ? (
                  <p className="mt-3 text-sm text-gray-400">Discovering LAN peers...</p>
                ) : federationEnabled && availableFederationPeers.length > 0 ? (
                  <div className="mt-3 space-y-3">
                    {availableFederationPeers.map((peer) => {
                      const selectable = canUseFederationPeer(peer);
                      const selected = selectedFederationPeerIds.includes(peer.peer_id);
                      return (
                        <div
                          key={peer.peer_id}
                          className="rounded-xl border border-gray-800 bg-gray-950/70 p-3"
                        >
                          <div className="flex flex-wrap items-center justify-between gap-2">
                            <div className="min-w-0">
                              <div className="flex flex-wrap items-center gap-2">
                                <span className="truncate text-sm font-medium text-white">
                                  {peer.display_name}
                                </span>
                                <span
                                  className={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide ${
                                    peer.online
                                      ? 'border border-emerald-500/30 bg-emerald-500/10 text-emerald-300'
                                      : 'border border-gray-700 bg-gray-800 text-gray-400'
                                  }`}
                                >
                                  {peer.online ? 'Online' : 'Offline'}
                                </span>
                                <span className="rounded-full border border-gray-700 bg-gray-800 px-2 py-0.5 text-[10px] uppercase tracking-wide text-gray-400">
                                  {peer.source}
                                </span>
                              </div>
                              <p className="mt-1 font-mono text-[11px] text-cyan-300">
                                {peer.delegate_agent}
                              </p>
                            </div>
                            <label className="inline-flex items-center gap-2 text-xs text-gray-300">
                              <input
                                type="checkbox"
                                checked={selected}
                                disabled={!selectable || !activeSession}
                                onChange={(event) =>
                                  handleFederationPeerToggle(
                                    peer.peer_id,
                                    event.target.checked,
                                  )
                                }
                                className="rounded border-gray-600 bg-gray-900 text-cyan-500 focus:ring-cyan-500 disabled:opacity-50"
                              />
                              Use in chat
                            </label>
                          </div>

                          <div className="mt-3 grid gap-3 md:grid-cols-[minmax(0,1fr)_11rem]">
                            <div className="space-y-2 text-xs text-gray-400">
                              <div className="flex items-start gap-2">
                                <Cpu className="mt-0.5 h-3.5 w-3.5 text-cyan-300" />
                                <span className="break-words">
                                  {peer.model_summary || 'Model summary unavailable'}
                                </span>
                              </div>
                              <div className="flex items-start gap-2">
                                <Wrench className="mt-0.5 h-3.5 w-3.5 text-cyan-300" />
                                <span className="break-words">
                                  {peer.tool_summary || 'Tool summary unavailable'}
                                </span>
                              </div>
                            </div>
                            <div>
                              <label className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                                Assigned role
                              </label>
                              <select
                                value={peer.assigned_role}
                                onChange={(event) =>
                                  void handleFederationRoleChange(
                                    peer.peer_id,
                                    event.target.value as FederationRole,
                                  )
                                }
                                className="w-full rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white focus:border-cyan-500 focus:outline-none"
                              >
                                <option value="master">master</option>
                                <option value="worker">worker</option>
                                <option value="both">both</option>
                                <option value="disabled">disabled</option>
                              </select>
                            </div>
                          </div>
                        </div>
                      );
                    })}
                  </div>
                ) : (
                  <p className="mt-3 text-sm text-gray-400">
                    {federationEnabled
                      ? 'No LAN peers discovered yet. Local-only behavior continues until workers appear.'
                      : 'Enable federation in the environment to discover LAN workers.'}
                  </p>
                )}
              </div>

              <div className="rounded-2xl border border-gray-800 bg-gray-900/80 p-4">
                <div className="flex items-center gap-2 text-sm font-semibold text-white">
                  <Activity className="h-4 w-4 text-emerald-400" />
                  Remote Tasks
                </div>
                <p className="mt-1 text-xs text-gray-500">
                  {selectedFederationPeerIds.length > 0
                    ? `${selectedFederationPeerIds.length} worker${selectedFederationPeerIds.length === 1 ? '' : 's'} selected for this chat.`
                    : 'Select zero, one, or multiple workers for this chat.'}
                </p>

                {visibleFederationTasks.length === 0 ? (
                  <p className="mt-3 text-sm text-gray-400">
                    Remote task routing and streamed worker output will appear here.
                  </p>
                ) : (
                  <div className="mt-3 space-y-3">
                    {visibleFederationTasks.map((task) => (
                      <div
                        key={task.taskId}
                        className="rounded-xl border border-gray-800 bg-gray-950/70 p-3"
                      >
                        <div className="flex flex-wrap items-center justify-between gap-2">
                          <div>
                            <p className="text-sm font-medium text-white">{task.peerName}</p>
                            <p className="font-mono text-[11px] text-cyan-300">
                              {task.delegateAgent}
                            </p>
                          </div>
                          <span
                            className={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide ${
                              task.status === 'error'
                                ? 'border border-red-500/30 bg-red-500/10 text-red-300'
                                : task.status === 'done'
                                  ? 'border border-emerald-500/30 bg-emerald-500/10 text-emerald-300'
                                  : 'border border-cyan-500/30 bg-cyan-500/10 text-cyan-300'
                            }`}
                          >
                            {task.status}
                          </span>
                        </div>
                        <p className="mt-2 text-sm text-gray-200">{task.message}</p>
                        {task.content && (
                          <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap rounded-lg border border-gray-800 bg-gray-900 p-2 text-xs text-gray-300">
                            {task.content}
                          </pre>
                        )}
                        {task.lastToolName && (
                          <div className="mt-2 rounded-lg border border-gray-800 bg-gray-900 px-3 py-2 text-xs text-gray-400">
                            <div className="flex flex-wrap items-center gap-2">
                              <span className="text-gray-200">{task.lastToolName}</span>
                              {typeof task.lastToolSuccess === 'boolean' && (
                                <span>
                                  {task.lastToolSuccess ? 'success' : 'failed'}
                                </span>
                              )}
                              {typeof task.lastToolDurationSecs === 'number' && (
                                <span>{task.lastToolDurationSecs}s</span>
                              )}
                            </div>
                            {task.lastToolOutput && (
                              <p className="mt-1 whitespace-pre-wrap break-words">
                                {task.lastToolOutput}
                              </p>
                            )}
                          </div>
                        )}
                        <p className="mt-2 text-[11px] text-gray-600">
                          {task.updatedAt.toLocaleTimeString()}
                        </p>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </div>
          </div>
        )}

        <div className="flex-1 overflow-y-auto p-4">
          {activeMessages.length === 0 ? (
            <div className="flex h-full flex-col items-center justify-center text-center text-gray-500">
              <Bot className="mb-3 h-12 w-12 text-gray-600" />
              <p className="text-lg font-medium text-gray-300">LlamaFarm Agent</p>
              <p className="mt-1 max-w-md text-sm">
                Start a new local chat, open a temporary session, or resume an older one from the
                chat list.
              </p>
            </div>
          ) : (
            <div className="space-y-4">
              {activeMessages.map((message) => (
                <div
                  key={message.id}
                  className={`flex items-start gap-3 ${
                    message.role === 'user' ? 'flex-row-reverse' : ''
                  }`}
                >
                  <div
                    className={`flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full ${
                      message.role === 'user' ? 'bg-blue-600' : 'bg-gray-700'
                    }`}
                  >
                    {message.role === 'user' ? (
                      <User className="h-4 w-4 text-white" />
                    ) : (
                      <Bot className="h-4 w-4 text-white" />
                    )}
                  </div>
                  <div
                    className={`max-w-[85%] rounded-2xl px-4 py-3 ${
                      message.role === 'user'
                        ? 'bg-blue-600 text-white'
                        : 'border border-gray-700 bg-gray-800 text-gray-100'
                    }`}
                  >
                    <p className="whitespace-pre-wrap break-words text-sm">{message.content}</p>
                    <p
                      className={`mt-2 text-xs ${
                        message.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                      }`}
                    >
                      {formatAssistantMessageMeta(message)}
                    </p>
                  </div>
                </div>
              ))}

              {activeSessionTyping && (
                <div className="flex items-start gap-3">
                  <div className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full bg-gray-700">
                    <Bot className="h-4 w-4 text-white" />
                  </div>
                  <div className="rounded-2xl border border-gray-700 bg-gray-800 px-4 py-3">
                    <div className="flex items-center gap-1">
                      <span
                        className="h-2 w-2 animate-bounce rounded-full bg-gray-400"
                        style={{ animationDelay: '0ms' }}
                      />
                      <span
                        className="h-2 w-2 animate-bounce rounded-full bg-gray-400"
                        style={{ animationDelay: '150ms' }}
                      />
                      <span
                        className="h-2 w-2 animate-bounce rounded-full bg-gray-400"
                        style={{ animationDelay: '300ms' }}
                      />
                    </div>
                    <p className="mt-1 text-xs text-gray-500">Typing...</p>
                  </div>
                </div>
              )}
            </div>
          )}

          <div ref={messagesEndRef} />
        </div>

        <div className="border-t border-gray-800 bg-gray-900 p-4">
          <div className="mx-auto flex max-w-5xl items-center gap-3">
            <div className="flex-1">
              <input
                ref={inputRef}
                type="text"
                value={input}
                onChange={(event) => setInput(event.target.value)}
                onKeyDown={handleKeyDown}
                placeholder={connected ? 'Type a message...' : 'Connecting...'}
                disabled={!connected || !activeSession}
                className="w-full rounded-xl border border-gray-700 bg-gray-800 px-4 py-3 text-sm text-white placeholder-gray-500 focus:border-transparent focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50"
              />
              {federationEnabled && (
                <p className="mt-2 text-xs text-gray-500">
                  {selectedFederationPeerIds.length > 0
                    ? `Selected remote workers: ${selectedFederationPeerIds.length}. The agent can delegate to those LAN peers from this chat.`
                    : 'No remote workers selected for this chat. Delegation stays local unless you choose peers above.'}
                </p>
              )}
            </div>
            <button
              onClick={handleSend}
              disabled={!connected || !input.trim() || !activeSession}
              className="flex-shrink-0 rounded-xl bg-blue-600 p-3 text-white transition-colors hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500"
            >
              <Send className="h-5 w-5" />
            </button>
          </div>
          <div className="mt-2 flex items-center justify-center gap-2">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                connected ? 'bg-green-500' : 'bg-red-500'
              }`}
            />
            <span className="text-xs text-gray-500">
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </div>
        </div>
      </section>
    </div>
  );
}
