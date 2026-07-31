import { lazy, Suspense, useEffect, useMemo, useRef, useState } from 'react';
import {
  AlertCircle,
  Bot,
  Check,
  ChevronDown,
  ChevronUp,
  Code2,
  Copy,
  History,
  Maximize2,
  Minimize2,
  Network,
  PanelLeftClose,
  PanelLeftOpen,
  Plus,
  Send,
  Square,
  Trash2,
  User,
  Wrench,
  X,
} from 'lucide-react';
import { getChatSession, getChatSessions, getAgentModes, type AgentModeOption } from '@/lib/api';
import { getFederationPeers } from '@/lib/api';
import {
  loadFederationPeerSelections,
  loadFederationTasksBySession,
  persistFederationPeerSelections,
  persistFederationTasksBySession,
  type FederationTaskState,
} from '@/lib/federationState';
import {
  WebSocketClient,
  type SeedChatMessage,
  type SendChatMessageOptions,
} from '@/lib/ws';
import type {
  FederationPeerSummary,
  FederationPeersResponse,
  FederationRole,
  StoredChatMessage,
  WsMessage,
} from '@/types/api';
import type { AgentFileTouch } from '@/components/ide/IdePanel';
import { AGENT_FILE_TOOLS, extractTouchedPaths } from '@/components/ide/agentTouch';
import { AGENT_SHELL_TOOLS, formatShellCommandLine } from '@/components/ide/agentShell';
import type { AgentShellCall } from '@/components/ide/TerminalPanel';

// Lazy-loaded: Monaco is a multi-megabyte bundle, so only fetch it once the IDE panel is opened.
const IdePanel = lazy(() => import('@/components/ide/IdePanel'));

type ChatRole = 'user' | 'agent';
type ChatMessageKind = 'message' | 'tool_call' | 'tool_result' | 'status' | 'error';

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

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK =
  'Tool execution completed, but no final response text was returned.';
const CHAT_SESSIONS_STORAGE_KEY = 'llamafarm.agent_chat.sessions.v2';
const ACTIVE_CHAT_STORAGE_KEY = 'llamafarm.agent_chat.active_session.v2';
const CHAT_DRAFT_STORAGE_KEY = 'llamafarm.agent_chat.draft.v1';
const CHAT_SIDEBAR_COLLAPSED_STORAGE_KEY = 'llamafarm.agent_chat.sidebar_collapsed.v1';
const CHAT_IDE_OPEN_STORAGE_KEY = 'llamafarm.agent_chat.ide_open.v1';
const CHAT_IDE_WIDTH_STORAGE_KEY = 'llamafarm.agent_chat.ide_width.v1';
const CHAT_IDE_MAXIMIZED_STORAGE_KEY = 'llamafarm.agent_chat.ide_maximized.v1';
const IDE_MIN_WIDTH = 380;
const IDE_DEFAULT_WIDTH = 560;
const CHAT_FEDERATION_COLLAPSED_STORAGE_KEY = 'llamafarm.agent_chat.federation_collapsed.v1';
const CHAT_CONTROLS_COLLAPSED_STORAGE_KEY = 'llamafarm.agent_chat.controls_collapsed.v1';
const MAX_PERSISTED_MESSAGES = 500;
// Kicks off a self-contained acceptance run: the model must enumerate every
// registered tool via task_plan (not just guess a few) and chain through
// them one at a time so each gets a real, verified call before the plan can
// reach a terminal state.
const TEST_ALL_TOOLS_PROMPT =
  'Look at the entire tool catalogue available to you right now. Create a task_plan with one pending step per tool in that catalogue — do not group tools together or skip any. Then execute the plan step by step: call each tool once with a safe, minimal, non-destructive test input, mark the step completed or blocked based on the real result, and immediately continue to the next pending step without stopping to ask me anything. When every step has reached a terminal state, report a pass/fail summary for the whole catalogue.';
const MAX_PERSISTED_SESSIONS = 40;

type AgentConnectionState = 'connecting' | 'connected' | 'reconnecting';

function connectionLabel(state: AgentConnectionState): string {
  if (state === 'connected') return 'Connected';
  if (state === 'reconnecting') return 'Reconnecting…';
  return 'Connecting…';
}

function CopyButton({ text }: Readonly<{ text: string }>) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      onClick={() => {
        navigator.clipboard.writeText(text).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 1500);
        });
      }}
      className="absolute right-2 top-2 opacity-0 group-hover/msg:opacity-100 transition-opacity p-1 rounded text-gray-500 hover:text-gray-300 hover:bg-gray-700"
      title="Copy"
    >
      {copied ? <Check className="h-3.5 w-3.5 text-green-400" /> : <Copy className="h-3.5 w-3.5" />}
    </button>
  );
}

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
  // Keep the complete live transcript in memory. The browser's localStorage
  // copy remains a bounded startup cache; the gateway is the durable source
  // of truth and the unlimited node profile retains the full saved history.
  const liveMessages = session.messages;
  const latestTimestamp =
    liveMessages[liveMessages.length - 1]?.timestamp ?? session.updatedAt;

  return {
    ...session,
    messages: liveMessages,
    updatedAt: latestTimestamp,
    title: deriveSessionTitle(liveMessages, session.temporary),
  };
}

function loadPersistedSessions(): ChatSession[] {
  if (typeof globalThis.window === 'undefined') {
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
              !['message', 'tool_call', 'tool_result', 'status', 'error'].includes(message.kind) ||
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
  if (typeof globalThis.window === 'undefined') {
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
  if (typeof globalThis.window === 'undefined') {
    return sessions[0]?.id ?? createChatSession(false).id;
  }

  const persisted = localStorage.getItem(ACTIVE_CHAT_STORAGE_KEY);
  if (persisted && sessions.some((session) => session.id === persisted)) {
    return persisted;
  }

  return sessions[0]?.id ?? createChatSession(false).id;
}

function persistActiveSessionId(session: ChatSession | undefined): void {
  if (typeof globalThis.window === 'undefined') {
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

function loadDraft(): string {
  if (typeof globalThis.window === 'undefined') {
    return '';
  }

  try {
    return localStorage.getItem(CHAT_DRAFT_STORAGE_KEY) ?? '';
  } catch {
    return '';
  }
}

function loadSidebarCollapsed(): boolean {
  if (typeof globalThis.window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_SIDEBAR_COLLAPSED_STORAGE_KEY) === '1';
  } catch {
    return false;
  }
}

function loadIdeOpen(): boolean {
  if (typeof globalThis.window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_IDE_OPEN_STORAGE_KEY) === '1';
  } catch {
    return false;
  }
}

function loadIdeWidth(): number {
  if (typeof globalThis.window === 'undefined') {
    return IDE_DEFAULT_WIDTH;
  }

  try {
    const parsed = Number.parseInt(localStorage.getItem(CHAT_IDE_WIDTH_STORAGE_KEY) ?? '', 10);
    if (Number.isFinite(parsed) && parsed >= IDE_MIN_WIDTH) {
      return parsed;
    }
  } catch {
    // fall through to default
  }
  return IDE_DEFAULT_WIDTH;
}

function loadIdeMaximized(): boolean {
  if (typeof globalThis.window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_IDE_MAXIMIZED_STORAGE_KEY) === '1';
  } catch {
    return false;
  }
}

function ideGridTemplate(
  sidebarCollapsed: boolean,
  ideOpen: boolean,
  ideMaximized: boolean,
  ideWidth: number,
): string {
  if (ideOpen && ideMaximized) return 'minmax(0,1fr)';
  const chat = 'minmax(0,1fr)';
  const history = sidebarCollapsed ? '' : '18rem ';
  const ide = ideOpen ? ` 6px ${ideWidth}px` : '';
  return `${history}${chat}${ide}`;
}

function loadFederationBarCollapsed(): boolean {
  if (typeof globalThis.window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_FEDERATION_COLLAPSED_STORAGE_KEY) === '1';
  } catch {
    return false;
  }
}

function loadChatControlsCollapsed(): boolean {
  if (typeof globalThis.window === 'undefined') {
    return false;
  }

  try {
    return localStorage.getItem(CHAT_CONTROLS_COLLAPSED_STORAGE_KEY) === '1';
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
      // UI-only lifecycle notices (for example, a user-initiated Stop) should
      // remain visible in the transcript without becoming model context on a
      // later follow-up.
      if (message.kind === 'status') {
        return [];
      }
      const role =
        message.seedRole ?? (message.role === 'agent' ? 'assistant' : 'user');
      const content = (message.seedContent ?? message.content).trim();
      if (!content) {
        return [];
      }

      return [{ role, content }];
    });
}

/**
 * Reverse of buildHistorySeed/buildAssistantToolCallSeed/buildToolResultSeed:
 * turns the server's stored `{role, content}` pairs (fetched via
 * GET /api/chat-sessions/:id, for a session this browser never saw live)
 * back into renderable ChatMessage bubbles, so a chat picked up on a
 * different device looks like the same conversation instead of a wall of
 * raw JSON. Each reconstructed message also carries seedRole/seedContent set
 * to its own original stored form, so continuing the conversation from here
 * round-trips exactly like it would have on the original device.
 *
 * Note: the server only ever persists *completed* tool traces as a
 * collapsed final-answer summary (see normalize_ws_history_for_storage in
 * ws.rs) rather than every individual tool_call/tool_result step — that's
 * intentional, to keep stored history lean for the model, not a bug here.
 * So a session with a lot of tool use will show up with fewer, denser
 * bubbles than it had live; nothing said or produced is missing, just the
 * blow-by-blow of *how* older turns got there.
 */
function reconstructFromStoredMessages(stored: StoredChatMessage[]): ChatMessage[] {
  const result: ChatMessage[] = [];

  for (const entry of stored) {
    const trimmed = entry.content.trim();
    if (!trimmed) continue;

    if (entry.role === 'user') {
      result.push({
        id: makeMessageId(),
        role: 'user',
        kind: 'message',
        content: trimmed,
        timestamp: new Date(),
        seedRole: 'user',
        seedContent: entry.content,
      });
      continue;
    }

    if (entry.role === 'tool') {
      const parsed = safeJsonParse<{ tool_name?: string; content?: string }>(trimmed);
      const toolName = parsed?.tool_name ?? 'unknown';
      const output = parsed?.content?.trim() || '(no output)';
      result.push({
        id: makeMessageId(),
        role: 'agent',
        kind: 'tool_result',
        content: `[Tool Result] ${toolName}\n${output}`,
        timestamp: new Date(),
        seedRole: 'tool',
        seedContent: entry.content,
      });
      continue;
    }

    // assistant (or legacy 'agent'): either a tool_calls envelope or plain text.
    const parsed = safeJsonParse<{
      tool_calls?: { id: string; name: string; arguments: string }[];
    }>(trimmed);
    if (parsed?.tool_calls && parsed.tool_calls.length > 0) {
      for (const call of parsed.tool_calls) {
        const args = safeJsonParse<Record<string, unknown>>(call.arguments) ?? {};
        result.push({
          id: makeMessageId(),
          role: 'agent',
          kind: 'tool_call',
          content: `[Tool Call] ${call.name}(${JSON.stringify(args)})`,
          timestamp: new Date(),
          seedRole: 'assistant',
          seedContent: entry.content,
        });
      }
      continue;
    }

    result.push({
      id: makeMessageId(),
      role: 'agent',
      kind: 'message',
      content: trimmed,
      timestamp: new Date(),
      seedRole: 'assistant',
      seedContent: entry.content,
    });
  }

  return result;
}

function safeJsonParse<T>(raw: string): T | null {
  try {
    return JSON.parse(raw) as T;
  } catch {
    return null;
  }
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
  initialStateRef.current ??= loadInitialChatState();

  const [sessions, setSessions] = useState<ChatSession[]>(initialStateRef.current.sessions);
  const [activeSessionId, setActiveSessionId] = useState(initialStateRef.current.activeSessionId);
  const [input, setInput] = useState(() => loadDraft());
  const [typingSessionIds, setTypingSessionIds] = useState<string[]>([]);
  const [stoppingSessionIds, setStoppingSessionIds] = useState<string[]>([]);
  const [streamingContentBySession, setStreamingContentBySession] = useState<
    Record<string, string>
  >({});
  const [streamingPreviewCollapsedBySession, setStreamingPreviewCollapsedBySession] = useState<
    Record<string, boolean>
  >({});
  const [sidebarCollapsed, setSidebarCollapsed] = useState(() => loadSidebarCollapsed());
  const [ideOpen, setIdeOpen] = useState(() => loadIdeOpen());
  const [ideWidth, setIdeWidth] = useState(() => loadIdeWidth());
  const [ideMaximized, setIdeMaximized] = useState(() => loadIdeMaximized());
  const [agentTouch, setAgentTouch] = useState<AgentFileTouch | null>(null);
  const ideResizingRef = useRef(false);
  const [federationBarCollapsed, setFederationBarCollapsed] = useState(() => loadFederationBarCollapsed());
  const [chatControlsCollapsed, setChatControlsCollapsed] = useState(() => loadChatControlsCollapsed());
  const [confirmDeleteSessionId, setConfirmDeleteSessionId] = useState<string | null>(null);
  const [connectionState, setConnectionState] =
    useState<AgentConnectionState>('connecting');
  const [error, setError] = useState<string | null>(null);
  const [tps, setTps] = useState(0);
  const [realMetrics, setRealMetrics] = useState<{
    generationTps: number;
    ttftMs: number | null;
    promptTokens: number | null;
  } | null>(null);
  const [streaming, setStreaming] = useState(false);
  const [federation, setFederation] = useState<FederationPeersResponse | null>(null);
  const [federationLoading, setFederationLoading] = useState(true);
  // "agent" is the unchanged default (full AGENTS.md-driven agentic
  // behavior) — never persisted across reloads so a stale non-default
  // choice can't silently linger into a fresh session.
  const [agentMode, setAgentMode] = useState<string>('agent');
  const [agentModeOptions, setAgentModeOptions] = useState<AgentModeOption[]>([]);
  // Session ids present in the sidebar as a lightweight placeholder fetched
  // from this node's server-side store (title/timestamp only, no messages
  // yet) — hydrated with the real transcript lazily, the first time the
  // operator actually opens one, rather than fetching every session's full
  // history up front.
  const [remoteOnlySessionIds, setRemoteOnlySessionIds] = useState<Set<string>>(new Set());
  const [hydratingSessionId, setHydratingSessionId] = useState<string | null>(null);
  const [selectedFederationPeerIdsBySession, setSelectedFederationPeerIdsBySession] =
    useState<Record<string, string[]>>(() => loadFederationPeerSelections());
  const [federationTasksBySession, setFederationTasksBySession] = useState<
    Record<string, FederationTaskState[]>
  >(() => loadFederationTasksBySession());
  const [shellCallsBySession, setShellCallsBySession] = useState<Record<string, AgentShellCall[]>>(
    {},
  );

  const wsRef = useRef<WebSocketClient | null>(null);
  const runWsRefs = useRef<Record<string, WebSocketClient>>({});
  const runWsQueuesRef = useRef<Record<string, SendChatMessageOptions[]>>({});
  const wsMessageHandlerRef = useRef<((message: WsMessage) => void) | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const confirmDeleteCancelRef = useRef<HTMLButtonElement>(null);
  const pendingContentRef = useRef<Record<string, string>>({});
  const pendingToolCallsRef = useRef<Record<string, PendingToolCallSeed[]>>({});
  const completedResponseSessionIdsRef = useRef<Set<string>>(new Set());
  const deletedSessionIdsRef = useRef<Set<string>>(new Set());
  const queuedFollowupSessionIdsRef = useRef<Set<string>>(new Set());
  const responseStartRef = useRef<Record<string, number>>({});
  const streamStartRef = useRef<Record<string, number>>({});
  const charCountRef = useRef<Record<string, number>>({});
  const activeSessionIdRef = useRef(activeSessionId);
  const hasConnectedRef = useRef(false);
  const connected = connectionState === 'connected';

  const activeSession = useMemo(
    () => sessions.find((session) => session.id === activeSessionId) ?? sessions[0],
    [sessions, activeSessionId],
  );

  const activeMessages = activeSession?.messages ?? [];
  const activeSessionTyping = activeSession
    ? typingSessionIds.includes(activeSession.id)
    : false;
  const activeSessionStopping = activeSession
    ? stoppingSessionIds.includes(activeSession.id)
    : false;
  const activeStreamingContent = activeSession
    ? streamingContentBySession[activeSession.id]
    : undefined;
  const activeStreamingPreviewCollapsed = activeSession
    ? streamingPreviewCollapsedBySession[activeSession.id] ?? false
    : false;
  const activeFederationTasks = activeSession
    ? federationTasksBySession[activeSession.id] ?? []
    : [];
  const activeShellCalls = activeSession ? shellCallsBySession[activeSession.id] ?? [] : [];
  const selectedFederationPeerIds = activeSession
    ? selectedFederationPeerIdsBySession[activeSession.id] ?? []
    : [];
  const availableFederationPeers = federation?.peers ?? [];

  useEffect(() => {
    activeSessionIdRef.current = activeSession?.id ?? activeSessionId;
  }, [activeSession, activeSessionId]);

  useEffect(() => {
    if (confirmDeleteSessionId) {
      confirmDeleteCancelRef.current?.focus();
    }
  }, [confirmDeleteSessionId]);

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
    const interval = globalThis.setInterval(() => {
      void refreshFederation();
    }, 5000);

    return () => {
      cancelled = true;
      globalThis.clearInterval(interval);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    getAgentModes()
      .then((modes) => {
        if (!cancelled) setAgentModeOptions(modes);
      })
      .catch(() => {
        // Dropdown just falls back to the two always-present built-ins.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Merge in any non-temporary session this node has persisted that this
  // browser doesn't already know about locally — e.g. started from a phone,
  // or a run that kept going after that browser disconnected. Placeholder
  // only (title + timestamp); the transcript itself loads on first open.
  useEffect(() => {
    let cancelled = false;

    const discoverRemoteSessions = async () => {
      try {
        const { sessions: remote } = await getChatSessions();
        if (cancelled || remote.length === 0) return;

        setSessions((prev) => {
          const knownIds = new Set(prev.map((s) => s.id));
          const localById = new Map(prev.map((session) => [session.id, session]));
          const staleIds = remote
            .filter((summary) => {
              const local = localById.get(summary.session_id);
              return local !== undefined && summary.message_count > local.messages.length;
            })
            .map((summary) => summary.session_id);
          const additions: ChatSession[] = remote
            .filter((r) => !knownIds.has(r.session_id))
            .map((r) => ({
              id: r.session_id,
              title: r.title,
              temporary: false,
              createdAt: new Date(r.updated_at_unix * 1000),
              updatedAt: new Date(r.updated_at_unix * 1000),
              messages: [],
            }));
          if (additions.length === 0 && staleIds.length === 0) return prev;
          setRemoteOnlySessionIds((prevIds) => {
            const next = new Set(prevIds);
            for (const a of additions) next.add(a.id);
            for (const sessionId of staleIds) next.add(sessionId);
            return next;
          });
          return sortSessions([...prev, ...additions]);
        });
      } catch {
        // Best-effort discovery; local sessions still work without it.
      }
    };

    void discoverRemoteSessions();
    return () => {
      cancelled = true;
    };
  }, []);

  const openSession = async (sessionId: string) => {
    if (!remoteOnlySessionIds.has(sessionId)) {
      setActiveSessionId(sessionId);
      setConfirmDeleteSessionId(null);
      return;
    }

    setHydratingSessionId(sessionId);
    try {
      const detail = await getChatSession(sessionId);
      const messages = reconstructFromStoredMessages(detail.messages);
      setSessions((prev) =>
        prev.map((s) => (s.id === sessionId ? { ...s, messages } : s)),
      );
      setRemoteOnlySessionIds((prev) => {
        const next = new Set(prev);
        next.delete(sessionId);
        return next;
      });
    } catch {
      // Leave it as an empty placeholder rather than blocking selection —
      // the operator can still see the title/timestamp and retry by
      // reselecting (remoteOnlySessionIds keeps it eligible for another try).
    } finally {
      setHydratingSessionId(null);
      setActiveSessionId(sessionId);
      setConfirmDeleteSessionId(null);
    }
  };

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
      hasConnectedRef.current = true;
      setConnectionState('connected');
      setError(null);
    };

    ws.onClose = () => {
      setConnectionState(hasConnectedRef.current ? 'reconnecting' : 'connecting');
      setError(
        hasConnectedRef.current
          ? 'Connection lost. Reconnecting automatically…'
          : 'Unable to connect yet. Retrying automatically…',
      );
      // This is the lightweight control connection. Active chat turns own
      // independent sockets, so a control reconnect must not erase their live
      // state or imply that server-side work stopped.
    };

    ws.onError = () => {
      setError('Connection error. Retrying automatically…');
      setStreaming(false);
    };

    const handleWsMessage = (msg: WsMessage) => {
      const sessionId = msg.session_id ?? activeSessionIdRef.current;
      if (!sessionId) {
        return;
      }
      // A Stop acknowledgement can arrive after the user deleted the chat.
      // Never recreate a just-deleted session merely to display that terminal
      // event.
      if (deletedSessionIdsRef.current.has(sessionId)) {
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
        setStoppingSessionIds((prev) => prev.filter((id) => id !== sessionId));
        setStreamingContentBySession((prev) => {
          if (!(sessionId in prev)) return prev;
          const next = { ...prev };
          delete next[sessionId];
          return next;
        });
        setStreamingPreviewCollapsedBySession((prev) => {
          if (!(sessionId in prev)) return prev;
          const next = { ...prev };
          delete next[sessionId];
          return next;
        });
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
          const streamedContent =
            (pendingContentRef.current[sessionId] ?? '') + (msg.content ?? '');
          pendingContentRef.current[sessionId] = streamedContent;
          setStreamingContentBySession((prev) => ({
            ...prev,
            [sessionId]: streamedContent,
          }));
          setStreaming(true);
          // TPS tracking
          if (streamStartRef.current[sessionId] === undefined) {
            streamStartRef.current[sessionId] = performance.now();
            charCountRef.current[sessionId] = 0;
          }
          charCountRef.current[sessionId] =
            (charCountRef.current[sessionId] ?? 0) + (msg.content ?? '').length;
          const streamStartedAt =
            streamStartRef.current[sessionId] ?? performance.now();
          const elapsed = (performance.now() - streamStartedAt) / 1000;
          if (elapsed > 0.2 && sessionId === activeSessionIdRef.current) {
            setTps(
              Math.max(
                1,
                Math.round((charCountRef.current[sessionId] ?? 0) / 4 / elapsed),
              ),
            );
          }
          break;
        }

        case 'metrics': {
          // Real per-segment inference timing from the provider (Ollama):
          // decode TPS and time-to-first-token, replacing wall-clock guesses.
          const m = msg.metrics ?? {};
          if (
            typeof m.generation_tps === 'number' &&
            sessionId === activeSessionIdRef.current
          ) {
            setRealMetrics({
              generationTps: m.generation_tps,
              ttftMs: typeof m.ttft_ms === 'number' ? m.ttft_ms : null,
              promptTokens: typeof m.prompt_tokens === 'number' ? m.prompt_tokens : null,
            });
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
          const startedAt =
            responseStartRef.current[sessionId] ?? streamStartRef.current[sessionId];
          const completionSeconds =
            typeof startedAt === 'number'
              ? Math.max((performance.now() - startedAt) / 1000, 0.05)
              : 0;
          const estimatedTokensPerSecond =
            completionSeconds > 0 && outputTokens > 0 ? outputTokens / completionSeconds : 0;
          if (
            estimatedTokensPerSecond > 0 &&
            sessionId === activeSessionIdRef.current
          ) {
            setTps(Math.max(1, Math.round(estimatedTokensPerSecond)));
          }
          // Some backends emit both a legacy `message` event and the terminal
          // `done` event. The stream preview is deliberately ephemeral, and
          // only one completed assistant message should be retained per turn.
          if (!completedResponseSessionIdsRef.current.has(sessionId)) {
            const finalContent = content || EMPTY_DONE_FALLBACK;
            appendMessage('agent', 'message', finalContent, new Date(), {
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
                seedContent: finalContent,
              },
            });
            completedResponseSessionIdsRef.current.add(sessionId);
          }
          clearTypingForSession();
          delete responseStartRef.current[sessionId];
          delete streamStartRef.current[sessionId];
          delete charCountRef.current[sessionId];
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
          if (AGENT_FILE_TOOLS.has(toolName)) {
            const touchedPaths = extractTouchedPaths(toolName, msg.args);
            if (touchedPaths.length > 0) {
              setAgentTouch({ paths: touchedPaths, tool: toolName, nonce: Date.now() });
              setIdeOpen(true);
            }
          }
          if (AGENT_SHELL_TOOLS.has(toolName)) {
            const command = formatShellCommandLine(toolName, msg.args);
            setShellCallsBySession((prev) => {
              const existing = prev[sessionId] ?? [];
              const nextCall: AgentShellCall = {
                id: toolCallId,
                tool: toolName,
                command,
                status: 'running',
                updatedAt: new Date(),
              };
              return { ...prev, [sessionId]: [nextCall, ...existing].slice(0, 50) };
            });
          }
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
          if (pendingCall && AGENT_SHELL_TOOLS.has(toolName)) {
            setShellCallsBySession((prev) => {
              const existing = prev[sessionId] ?? [];
              const target = existing.find((call) => call.id === pendingCall.id);
              if (!target) return prev;
              const updated = existing.map((call) =>
                call.id === pendingCall.id
                  ? {
                      ...call,
                      status: (msg.success === false ? 'error' : 'success') as AgentShellCall['status'],
                      output,
                      durationSecs: msg.duration_secs,
                      updatedAt: new Date(),
                    }
                  : call,
              );
              return { ...prev, [sessionId]: updated };
            });
          }
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

        case 'cancelling': {
          setStoppingSessionIds((prev) =>
            prev.includes(sessionId) ? prev : [...prev, sessionId],
          );
          break;
        }

        case 'followup_queued': {
          queuedFollowupSessionIdsRef.current.add(sessionId);
          appendMessage(
            'agent',
            'status',
            msg.message ?? 'Follow-up queued; checkpointing the active run first.',
            new Date(),
          );
          break;
        }

        case 'cancelled': {
          const stoppedMessage = msg.message ?? 'Stopped by user.';
          const transitioningToFollowup = queuedFollowupSessionIdsRef.current.delete(sessionId);
          if (!transitioningToFollowup) {
            appendMessage('agent', 'status', `[Stopped] ${stoppedMessage}`, new Date());
          }
          delete pendingToolCallsRef.current[sessionId];
          setShellCallsBySession((prev) => {
            const calls = prev[sessionId];
            if (!calls?.some((call) => call.status === 'running')) return prev;
            return {
              ...prev,
              [sessionId]: calls.map((call) =>
                call.status === 'running'
                  ? {
                      ...call,
                      status: 'error' as const,
                      output: 'Stopped by user before a result was returned.',
                      updatedAt: new Date(),
                    }
                  : call,
              ),
            };
          });
          setFederationTasksBySession((prev) => {
            const tasks = prev[sessionId];
            if (!tasks?.some((task) => task.status === 'status' || task.status === 'streaming')) {
              return prev;
            }
            return {
              ...prev,
              [sessionId]: tasks.map((task) =>
                task.status === 'status' || task.status === 'streaming'
                  ? {
                      ...task,
                      status: 'error' as const,
                      message: 'Stopped by user',
                      updatedAt: new Date(),
                    }
                  : task,
              ),
            };
          });
          clearTypingForSession();
          delete responseStartRef.current[sessionId];
          delete streamStartRef.current[sessionId];
          delete charCountRef.current[sessionId];
          setStreaming(false);
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
          delete responseStartRef.current[sessionId];
          delete streamStartRef.current[sessionId];
          delete charCountRef.current[sessionId];
          setStreaming(false);
          break;
      }
    };
    wsMessageHandlerRef.current = handleWsMessage;
    ws.onMessage = handleWsMessage;

    ws.connect();
    wsRef.current = ws;

    return () => {
      wsMessageHandlerRef.current = null;
      ws.disconnect();
      for (const runWs of Object.values(runWsRefs.current)) {
        runWs.disconnect();
      }
      runWsRefs.current = {};
      runWsQueuesRef.current = {};
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
      if (input) {
        localStorage.setItem(CHAT_DRAFT_STORAGE_KEY, input);
      } else {
        localStorage.removeItem(CHAT_DRAFT_STORAGE_KEY);
      }
    } catch {
      // Keep the in-memory draft when localStorage is unavailable.
    }
  }, [input]);

  useEffect(() => {
    persistFederationPeerSelections(selectedFederationPeerIdsBySession);
  }, [selectedFederationPeerIdsBySession]);

  useEffect(() => {
    persistFederationTasksBySession(federationTasksBySession);
  }, [federationTasksBySession]);

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
    try {
      localStorage.setItem(CHAT_IDE_OPEN_STORAGE_KEY, ideOpen ? '1' : '0');
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [ideOpen]);

  useEffect(() => {
    try {
      localStorage.setItem(CHAT_IDE_WIDTH_STORAGE_KEY, String(ideWidth));
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [ideWidth]);

  useEffect(() => {
    try {
      localStorage.setItem(CHAT_IDE_MAXIMIZED_STORAGE_KEY, ideMaximized ? '1' : '0');
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [ideMaximized]);

  const startIdeResize = (event: React.MouseEvent) => {
    event.preventDefault();
    ideResizingRef.current = true;
    document.body.style.cursor = 'col-resize';
    document.body.style.userSelect = 'none';

    const onMove = (moveEvent: MouseEvent) => {
      if (!ideResizingRef.current) return;
      const next = Math.min(
        Math.max(window.innerWidth - moveEvent.clientX, IDE_MIN_WIDTH),
        Math.max(window.innerWidth - 420, IDE_MIN_WIDTH),
      );
      setIdeWidth(next);
    };
    const onUp = () => {
      ideResizingRef.current = false;
      document.body.style.cursor = '';
      document.body.style.userSelect = '';
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
  };

  useEffect(() => {
    try {
      localStorage.setItem(
        CHAT_FEDERATION_COLLAPSED_STORAGE_KEY,
        federationBarCollapsed ? '1' : '0',
      );
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [federationBarCollapsed]);

  useEffect(() => {
    try {
      localStorage.setItem(
        CHAT_CONTROLS_COLLAPSED_STORAGE_KEY,
        chatControlsCollapsed ? '1' : '0',
      );
    } catch {
      // localStorage may be unavailable in some browser modes; fail silently.
    }
  }, [chatControlsCollapsed]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [activeMessages, activeSessionTyping, activeStreamingContent]);

  const createAndSelectSession = (temporary: boolean) => {
    const session = createChatSession(temporary);
    setSessions((prev) => sortSessions([session, ...prev]));
    setActiveSessionId(session.id);
    setConfirmDeleteSessionId(null);
    setInput('');
    inputRef.current?.focus();
  };

  const handleDeleteSession = (sessionId: string) => {
    setConfirmDeleteSessionId(null);
    const nextExisting = sessions.find((session) => session.id !== sessionId);
    const replacementSession = !nextExisting ? createChatSession(false) : null;

    // session_delete is itself an atomic stop+delete operation for an active
    // run. Send it through the independent control socket so it cannot queue
    // behind that session's streaming connection.
    wsRef.current?.deleteSession(sessionId);
    deletedSessionIdsRef.current.add(sessionId);
    delete pendingContentRef.current[sessionId];
    delete pendingToolCallsRef.current[sessionId];
    completedResponseSessionIdsRef.current.delete(sessionId);
    setStreamingContentBySession((prev) => {
      if (!(sessionId in prev)) return prev;
      const next = { ...prev };
      delete next[sessionId];
      return next;
    });
    setStreamingPreviewCollapsedBySession((prev) => {
      if (!(sessionId in prev)) return prev;
      const next = { ...prev };
      delete next[sessionId];
      return next;
    });
    setTypingSessionIds((prev) => prev.filter((id) => id !== sessionId));
    setStoppingSessionIds((prev) => prev.filter((id) => id !== sessionId));
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
    setShellCallsBySession((prev) => {
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

  const sendSessionMessage = (
    sessionId: string,
    message: SendChatMessageOptions,
  ): boolean => {
    const flushQueue = (client: WebSocketClient) => {
      const queue = runWsQueuesRef.current[sessionId] ?? [];
      while (queue.length > 0 && client.connected) {
        const next = queue[0];
        if (!next) break;
        client.sendMessage(next);
        queue.shift();
      }
      if (queue.length === 0) {
        delete runWsQueuesRef.current[sessionId];
      } else {
        runWsQueuesRef.current[sessionId] = queue;
      }
    };

    const existing = runWsRefs.current[sessionId];
    runWsQueuesRef.current[sessionId] = [
      ...(runWsQueuesRef.current[sessionId] ?? []),
      message,
    ];
    if (existing) {
      try {
        flushQueue(existing);
        return true;
      } catch {
        existing.disconnect();
        delete runWsRefs.current[sessionId];
      }
    }

    const client = new WebSocketClient({ autoReconnect: false });
    runWsRefs.current[sessionId] = client;
    client.onOpen = () => {
      try {
        flushQueue(client);
      } catch {
        setError(`Chat ${sessionId.slice(0, 8)} could not send. Reconnect and try again.`);
      }
    };
    client.onMessage = (incoming) => {
      wsMessageHandlerRef.current?.(incoming);
      const incomingSessionId = incoming.session_id ?? sessionId;
      if (
        incomingSessionId === sessionId &&
        (incoming.type === 'done' ||
          incoming.type === 'message' ||
          incoming.type === 'cancelled' ||
          incoming.type === 'error')
      ) {
        if (runWsRefs.current[sessionId] === client) {
          delete runWsRefs.current[sessionId];
          delete runWsQueuesRef.current[sessionId];
        }
        client.disconnect();
      }
    };
    client.onClose = () => {
      if (runWsRefs.current[sessionId] === client) {
        delete runWsRefs.current[sessionId];
        delete runWsQueuesRef.current[sessionId];
        setError(
          `Chat ${sessionId.slice(0, 8)} detached; its server-side run continues in the background.`,
        );
      }
    };
    client.onError = () => {
      setError(`Chat ${sessionId.slice(0, 8)} connection failed; the control channel remains available.`);
    };
    client.connect();
    return true;
  };

  // Shared by the manual send box and programmatic triggers (e.g. "Test All
  // Tools") that need to fire a message into a session that may have just
  // been created in the same event handler — so it takes `session` and
  // `content` explicitly rather than reading `activeSession`/`input` state,
  // which would not have settled yet in that same-tick scenario.
  const dispatchUserMessage = (session: ChatSession, content: string): boolean => {
    const trimmed = content.trim();
    if (!trimmed) {
      return false;
    }
    if (!wsRef.current?.connected) {
      setConnectionState(hasConnectedRef.current ? 'reconnecting' : 'connecting');
      setError('Message kept as a draft while the connection recovers.');
      return false;
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
      ...session,
      messages: [...session.messages, userMessage],
      updatedAt: timestamp,
    });

    try {
      deletedSessionIdsRef.current.delete(session.id);
      completedResponseSessionIdsRef.current.delete(session.id);
      const sent = sendSessionMessage(session.id, {
        content: trimmed,
        sessionId: session.id,
        temporary: session.temporary,
        historySeed: buildHistorySeed(updatedSession.messages),
        federationPeerIds: federationEnabled ? selectedFederationPeerIds : [],
        agentMode,
      });
      if (!sent) {
        return false;
      }
      setSessions((prev) =>
        sortSessions(prev.map((s) => (s.id === session.id ? updatedSession : s))),
      );
      setError(null);
      responseStartRef.current[session.id] = performance.now();
      delete streamStartRef.current[session.id];
      charCountRef.current[session.id] = 0;
      if (session.id === activeSessionIdRef.current) {
        setTps(0);
        setRealMetrics(null);
      }
      setStreaming(true);
      setTypingSessionIds((prev) => (prev.includes(session.id) ? prev : [...prev, session.id]));
      setStoppingSessionIds((prev) => prev.filter((id) => id !== session.id));
      pendingContentRef.current[session.id] = '';
      setStreamingContentBySession((prev) => ({ ...prev, [session.id]: '' }));
      setStreamingPreviewCollapsedBySession((prev) => ({ ...prev, [session.id]: false }));
      return true;
    } catch {
      setConnectionState('reconnecting');
      setError('Message kept as a draft because it could not be sent. Reconnecting…');
      return false;
    }
  };

  const handleSend = () => {
    if (!activeSession) {
      return;
    }
    const sent = dispatchUserMessage(activeSession, input);
    if (!sent) {
      inputRef.current?.focus();
      return;
    }

    setInput('');
    if (inputRef.current) {
      inputRef.current.style.height = 'auto';
      inputRef.current.focus();
    }
  };

  const handleTestAllTools = () => {
    if (!wsRef.current?.connected) {
      return;
    }
    const session = createChatSession(true);
    setSessions((prev) => sortSessions([session, ...prev]));
    setActiveSessionId(session.id);
    setConfirmDeleteSessionId(null);
    dispatchUserMessage(session, TEST_ALL_TOOLS_PROMPT);
  };

  const handleStop = () => {
    if (!activeSession || !activeSessionTyping || activeSessionStopping) {
      return;
    }

    const runWs = runWsRefs.current[activeSession.id];
    if (runWs?.connected) {
      runWs.cancelSession(activeSession.id);
    } else {
      // A run whose viewer socket detached is still server-owned. The control
      // connection can attach solely to deliver Stop and receive its terminal
      // acknowledgement.
      wsRef.current?.cancelSession(activeSession.id);
    }
    setStoppingSessionIds((prev) =>
      prev.includes(activeSession.id) ? prev : [...prev, activeSession.id],
    );
  };

  const handleKeyDown = (event: React.KeyboardEvent) => {
    if (
      event.key === 'Enter' &&
      !event.shiftKey &&
      !event.nativeEvent.isComposing
    ) {
      event.preventDefault();
      handleSend();
    }
  };

  const visibleFederationTasks = [...activeFederationTasks]
    .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
    .slice(0, 6);
  const federationEnabled = federation?.enabled ?? false;
  const onlineFederationPeers = availableFederationPeers.filter((peer) => peer.online).length;
  const selectedFederationWorkerCount = federationEnabled ? selectedFederationPeerIds.length : 0;
  const latestFederationTask = visibleFederationTasks[0];

  return (
    <div
      className="grid h-[calc(100vh-3.5rem)] min-h-0 grid-cols-1"
      style={{ gridTemplateColumns: ideGridTemplate(sidebarCollapsed, ideOpen, ideMaximized, ideWidth) }}
    >
      <aside
        className={`min-h-0 flex-col border-b border-gray-800 bg-gray-950 md:border-b-0 md:border-r ${
          sidebarCollapsed || (ideOpen && ideMaximized) ? 'hidden' : 'flex'
        }`}
      >
        <div className="border-b border-gray-800 p-4">
          <div className="flex items-center gap-2">
            <History className="h-4 w-4 text-blue-400" />
            <span className="flex-1 text-sm font-semibold text-white">Chats</span>
            <button
              onClick={() => setChatControlsCollapsed((prev) => !prev)}
              className="rounded p-0.5 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              title={chatControlsCollapsed ? 'Show new chat controls' : 'Hide new chat controls'}
            >
              {chatControlsCollapsed ? (
                <ChevronDown className="h-4 w-4" />
              ) : (
                <ChevronUp className="h-4 w-4" />
              )}
            </button>
          </div>
          {!chatControlsCollapsed && (
            <>
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
              <button
                onClick={handleTestAllTools}
                disabled={!connected}
                title="Start a temporary chat that has the agent enumerate and exercise every registered tool via a task plan"
                className="mt-3 flex w-full items-center justify-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-amber-500 hover:text-white disabled:cursor-not-allowed disabled:opacity-50"
              >
                <Wrench className="h-4 w-4" />
                Test All Tools
              </button>
            </>
          )}
        </div>

        <div className="flex-1 space-y-2 overflow-y-auto p-3">
          {sessions.map((session) => {
            const selected = session.id === activeSession?.id;
            const typing = typingSessionIds.includes(session.id);
            const isRemoteOnly = remoteOnlySessionIds.has(session.id);
            const isHydrating = hydratingSessionId === session.id;
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
                  onClick={() => void openSession(session.id)}
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
                        {isHydrating && (
                          <span className="rounded-full border border-blue-500/30 bg-blue-500/10 px-2 py-0.5 text-[10px] uppercase tracking-wide text-blue-300">
                            Loading…
                          </span>
                        )}
                        {isRemoteOnly && !isHydrating && (
                          <span
                            title="Started on another device or node — opening it loads the full conversation"
                            className="rounded-full border border-gray-600 bg-gray-800 px-2 py-0.5 text-[10px] uppercase tracking-wide text-gray-400"
                          >
                            From server
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
                  {confirmDeleteSessionId === session.id ? (
                    <div
                      className="flex items-center gap-1 text-xs"
                      role="status"
                      aria-live="polite"
                    >
                      <span className="mr-1 text-red-400">Delete?</span>
                      <button
                        type="button"
                        onClick={() => handleDeleteSession(session.id)}
                        className="inline-flex h-8 items-center rounded px-2 font-medium text-red-400 transition-colors hover:bg-red-950/40 hover:text-red-300"
                      >
                        Yes
                      </button>
                      <button
                        ref={confirmDeleteCancelRef}
                        type="button"
                        onClick={() => setConfirmDeleteSessionId(null)}
                        className="inline-flex h-8 items-center rounded px-2 font-medium text-gray-400 transition-colors hover:bg-gray-800 hover:text-white"
                      >
                        No
                      </button>
                    </div>
                  ) : (
                    <button
                      type="button"
                      onClick={() => setConfirmDeleteSessionId(session.id)}
                      className="inline-flex h-8 w-8 items-center justify-center rounded-md text-gray-500 transition-colors hover:bg-red-950/40 hover:text-red-400"
                      title="Delete chat"
                      aria-label={`Delete ${session.title}`}
                    >
                      <Trash2 className="h-4 w-4" />
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </div>
      </aside>

      <section className={`min-h-0 flex-col ${ideOpen && ideMaximized ? 'hidden' : 'flex'}`}>
        <div className="border-b border-gray-800 bg-gray-950 px-4 py-2">
          <div className="flex items-center justify-between gap-3">
            <div className="flex min-w-0 items-center gap-2">
              <button
                onClick={() => setSidebarCollapsed((prev) => !prev)}
                className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-gray-700 px-2.5 text-sm text-gray-300 transition-colors hover:bg-gray-900 hover:text-white"
                title={sidebarCollapsed ? 'Show chat history' : 'Hide chat history'}
              >
                {sidebarCollapsed ? (
                  <PanelLeftOpen className="h-4 w-4" />
                ) : (
                  <PanelLeftClose className="h-4 w-4" />
                )}
                <span className="hidden sm:inline text-xs">
                  {sidebarCollapsed ? 'Show history' : 'Hide history'}
                </span>
              </button>

              <h2 className="truncate text-sm font-semibold text-white">
                {activeSession?.title ?? 'Agent Chat'}
              </h2>
            </div>
            <div className="flex items-center gap-2">
              {realMetrics ? (
                <span
                  className="font-mono text-xs tabular-nums text-green-400"
                  title="Measured decode throughput and time-to-first-token from the model runtime"
                >
                  {realMetrics.generationTps.toFixed(1)} t/s
                  {realMetrics.ttftMs !== null &&
                    ` · ttft ${(realMetrics.ttftMs / 1000).toFixed(2)}s`}
                  {realMetrics.promptTokens !== null &&
                    ` · ${realMetrics.promptTokens.toLocaleString()} prompt tok`}
                </span>
              ) : (
                streaming &&
                tps > 0 && (
                  <span
                    className="font-mono text-xs tabular-nums text-green-400"
                    title="Estimated tokens per second while the current reply is streaming"
                  >
                    ~{tps} t/s live
                  </span>
                )
              )}
              <div className="rounded-full border border-gray-800 px-3 py-1 text-xs text-gray-400">
                {activeMessages.length} messages
              </div>
              <button
                onClick={() => setIdeOpen((prev) => !prev)}
                className={`hidden h-8 items-center gap-1.5 rounded-lg border px-2.5 text-sm transition-colors md:inline-flex ${
                  ideOpen
                    ? 'border-purple-500 bg-purple-950/40 text-purple-200'
                    : 'border-gray-700 text-gray-300 hover:bg-gray-900 hover:text-white'
                }`}
                title={ideOpen ? 'Hide IDE panel' : 'Show IDE panel'}
              >
                <Code2 className="h-4 w-4" />
                <span className="hidden text-xs sm:inline">{ideOpen ? 'Hide IDE' : 'IDE'}</span>
              </button>
            </div>
          </div>
        </div>

        {error && (
          <div
            role="alert"
            className="flex items-center gap-2 border-b border-red-700 bg-red-900/30 px-4 py-2 text-sm text-red-300"
          >
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            <span className="flex-1">{error}</span>
          </div>
        )}

        <div className="border-b border-gray-800 bg-gray-950/70 px-4 py-2">
          <div className="flex min-w-0 items-center gap-2">
            <Network className="h-4 w-4 flex-shrink-0 text-cyan-400" />
            <span className="text-sm font-semibold text-white">Federation</span>
            <span
              className={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide ${
                federationEnabled
                  ? 'border border-emerald-500/30 bg-emerald-500/10 text-emerald-300'
                  : 'border border-amber-500/30 bg-amber-500/10 text-amber-300'
              }`}
            >
              {federationEnabled ? 'enabled' : 'disabled'}
            </span>
            {federationLoading && (
              <span className="text-xs font-normal text-gray-500">checking peers...</span>
            )}
            {!federationBarCollapsed && federationEnabled && (
              <span className="ml-1 text-xs text-gray-500">
                {onlineFederationPeers}/{availableFederationPeers.length} online
                {selectedFederationWorkerCount > 0 ? ` · ${selectedFederationWorkerCount} selected` : ''}
              </span>
            )}
            <select
              value={agentMode}
              onChange={(event) => setAgentMode(event.target.value)}
              title="Chat mode: how this turn's system prompt and tools are built"
              className={`ml-auto rounded-full border px-2 py-0.5 text-[11px] ${
                agentMode === 'agent'
                  ? 'border-gray-700 bg-gray-900 text-gray-300'
                  : 'border-cyan-500/40 bg-cyan-500/10 text-cyan-200'
              }`}
            >
              <option value="agent">Agent (default)</option>
              {agentModeOptions
                .filter((mode) => mode.id !== 'agent')
                .map((mode) => (
                  <option key={mode.id} value={mode.id}>
                    {mode.label}
                    {mode.kind === 'variant' ? ' (AGENTS.md variant)' : ''}
                  </option>
                ))}
            </select>
            <button
              onClick={() => setFederationBarCollapsed((prev) => !prev)}
              className="rounded p-0.5 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              title={federationBarCollapsed ? 'Expand federation bar' : 'Collapse federation bar'}
            >
              {federationBarCollapsed ? (
                <ChevronDown className="h-4 w-4" />
              ) : (
                <ChevronUp className="h-4 w-4" />
              )}
            </button>
          </div>

          {!federationBarCollapsed && (
            <>
              <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-gray-400">
                <span className="rounded-full border border-gray-800 bg-gray-900 px-2.5 py-1">
                  {federationEnabled && federation?.local_node
                    ? `${federation.local_node.display_name} on :${federation.local_node.api_port}`
                    : 'Local-only chat'}
                </span>
                <span className="rounded-full border border-gray-800 bg-gray-900 px-2.5 py-1">
                  {federationEnabled
                    ? `${onlineFederationPeers}/${availableFederationPeers.length} peers online`
                    : 'federation runtime off'}
                </span>
                <span className="rounded-full border border-gray-800 bg-gray-900 px-2.5 py-1">
                  {selectedFederationWorkerCount} worker
                  {selectedFederationWorkerCount === 1 ? '' : 's'} selected
                </span>
                <span className="rounded-full border border-gray-800 bg-gray-900 px-2.5 py-1">
                  {visibleFederationTasks.length} recent remote task
                  {visibleFederationTasks.length === 1 ? '' : 's'}
                </span>
                {federationEnabled && federation?.local_node && (
                  <span className="rounded-full border border-gray-800 bg-gray-900 px-2.5 py-1">
                    discovery {federation.local_node.discovery_mode}
                  </span>
                )}
              </div>

              {latestFederationTask && (
                <p className="mt-2 text-xs text-gray-500">
                  Latest remote task: {latestFederationTask.peerName} {latestFederationTask.status}{' '}
                  at {latestFederationTask.updatedAt.toLocaleTimeString()}.
                </p>
              )}
            </>
          )}
        </div>

        <div className="flex-1 overflow-y-auto p-4">
          {activeMessages.length === 0 && !activeSessionTyping ? (
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
                  data-testid={`agent-message-${message.kind}`}
                  data-message-role={message.role}
                  data-message-kind={message.kind}
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
                    className={`group/msg relative max-w-[85%] rounded-2xl px-4 py-3 ${
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
                    <CopyButton text={message.content} />
                  </div>
                </div>
              ))}

              {activeSessionTyping && activeSession && (
                <div className="flex items-start gap-3">
                  <div className="flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full bg-gray-700">
                    <Bot className="h-4 w-4 text-white" />
                  </div>
                  <div
                    data-testid="agent-streaming-preview"
                    className="max-w-[85%] rounded-2xl border border-emerald-500/30 bg-gray-800 px-4 py-3 text-gray-100"
                  >
                    <div className="flex items-center justify-between gap-4">
                      <div className="flex min-w-0 items-center gap-2">
                        <span className="text-sm font-medium text-emerald-300">Working</span>
                        <span className="truncate text-xs text-gray-500">Live model output</span>
                      </div>
                      <button
                        type="button"
                        aria-expanded={!activeStreamingPreviewCollapsed}
                        onClick={() =>
                          setStreamingPreviewCollapsedBySession((prev) => ({
                            ...prev,
                            [activeSession.id]: !(prev[activeSession.id] ?? false),
                          }))
                        }
                        className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-700 hover:text-gray-200"
                        title={
                          activeStreamingPreviewCollapsed
                            ? 'Expand live model output'
                            : 'Collapse live model output'
                        }
                      >
                        {activeStreamingPreviewCollapsed ? (
                          <ChevronDown className="h-4 w-4" />
                        ) : (
                          <ChevronUp className="h-4 w-4" />
                        )}
                      </button>
                    </div>
                    {!activeStreamingPreviewCollapsed &&
                      (activeStreamingContent ? (
                        <p className="mt-3 whitespace-pre-wrap break-words text-sm">
                          {activeStreamingContent}
                        </p>
                      ) : (
                        <div className="mt-3 flex items-center gap-1" aria-label="Waiting for model output">
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
                      ))}
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
              <textarea
                ref={inputRef}
                data-testid="agent-chat-input"
                rows={1}
                value={input}
                onChange={(event) => {
                  setInput(event.target.value);
                  const el = event.target;
                  el.style.height = 'auto';
                  el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
                }}
                onKeyDown={handleKeyDown}
                aria-label="Message the agent"
                aria-describedby="agent-composer-help agent-connection-status"
                placeholder={
                  !connected
                    ? 'Write a draft while the connection recovers…'
                    : activeSessionStopping
                      ? 'Stopping the active run...'
                      : activeSessionTyping
                        ? 'Add a follow-up to redirect the active run…'
                        : 'Type a message...'
                }
                disabled={!activeSession}
                className="w-full resize-none overflow-y-auto rounded-xl border border-gray-700 bg-gray-800 px-4 py-3 text-sm text-white placeholder-gray-500 focus:border-transparent focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50"
              />
              <p id="agent-composer-help" className="mt-1.5 px-1 text-xs text-gray-500">
                Enter to send · Shift+Enter for a new line
                {!connected && ' · your draft is preserved while reconnecting'}
              </p>
            </div>
            {activeSessionTyping && activeSession && (
              <button
                type="button"
                onClick={handleStop}
                data-testid="agent-chat-stop"
                disabled={!connected || activeSessionStopping}
                className="flex-shrink-0 rounded-xl border border-red-500/50 bg-red-950/40 px-3 py-3 text-sm font-medium text-red-200 transition-colors hover:bg-red-900/60 disabled:cursor-wait disabled:opacity-60"
                title="Stop the active agent run"
              >
                <span className="inline-flex items-center gap-2">
                  <Square className="h-4 w-4 fill-current" />
                  {activeSessionStopping ? 'Stopping…' : 'Stop'}
                </span>
              </button>
            )}
            <button
              type="button"
              onClick={handleSend}
              data-testid="agent-chat-send"
              disabled={!connected || !input.trim() || !activeSession}
              aria-label="Send message"
              title={connected ? 'Send message' : 'Waiting for the agent connection'}
              className="flex-shrink-0 rounded-xl bg-blue-600 p-3 text-white transition-colors hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500"
            >
              <Send className="h-5 w-5" aria-hidden="true" />
            </button>
          </div>
          <div
            id="agent-connection-status"
            role="status"
            aria-live="polite"
            className="mt-2 flex items-center justify-center gap-2"
          >
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                connected
                  ? 'bg-green-500'
                  : connectionState === 'reconnecting'
                    ? 'bg-yellow-500'
                    : 'bg-blue-500'
              }`}
            />
            <span className="text-xs text-gray-500">
              {connectionLabel(connectionState)}
              {activeSessionTyping && connected ? ' · response streaming' : ''}
            </span>
          </div>
        </div>
      </section>

      {ideOpen && !ideMaximized && (
        <div
          role="separator"
          aria-orientation="vertical"
          onMouseDown={startIdeResize}
          className="hidden cursor-col-resize bg-gray-800 transition-colors hover:bg-blue-500 md:block"
          title="Drag to resize IDE panel"
        />
      )}

      {ideOpen && (
        <aside className="hidden min-h-0 flex-col border-l border-gray-800 bg-gray-950 md:flex">
          <div className="flex flex-shrink-0 items-center justify-between border-b border-gray-800 px-3 py-2">
            <div className="flex items-center gap-2">
              <Code2 className="h-4 w-4 text-purple-400" />
              <span className="text-sm font-semibold text-white">IDE</span>
            </div>
            <div className="flex items-center gap-1">
              <button
                onClick={() => setIdeMaximized((prev) => !prev)}
                className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-white"
                title={ideMaximized ? 'Restore chat view' : 'Maximize IDE (hide chat)'}
              >
                {ideMaximized ? (
                  <Minimize2 className="h-4 w-4" />
                ) : (
                  <Maximize2 className="h-4 w-4" />
                )}
              </button>
              <button
                onClick={() => {
                  setIdeOpen(false);
                  setIdeMaximized(false);
                }}
                className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-white"
                title="Close IDE panel"
              >
                <X className="h-4 w-4" />
              </button>
            </div>
          </div>
          <div className="min-h-0 flex-1">
            <Suspense
              fallback={
                <div className="flex h-full items-center justify-center text-sm text-gray-500">
                  Loading IDE...
                </div>
              }
            >
              <IdePanel agentTouch={agentTouch} shellCalls={activeShellCalls} />
            </Suspense>
          </div>
        </aside>
      )}
    </div>
  );
}
