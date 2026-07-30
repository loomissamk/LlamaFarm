import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import {
  Activity,
  AlertCircle,
  ArrowDown,
  Check,
  ChevronDown,
  ChevronUp,
  Clipboard,
  Download,
  Pause,
  Play,
  RefreshCw,
  Search,
} from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { SSEClient } from '@/lib/sse';
import type { RuntimeLogEntry, RuntimeLogsResponse, SSEEvent } from '@/types/api';

function formatTimestamp(ts?: string): string {
  if (!ts) return new Date().toLocaleTimeString();
  return new Date(ts).toLocaleTimeString();
}

function deriveLevel(line: string): string {
  const match = line.toUpperCase().match(/(?:^|\s)(ERROR|WARN|INFO|DEBUG|TRACE)(?:\s|$)/);
  return match?.[1]?.toLowerCase() ?? 'log';
}

function deriveSource(line: string): string {
  const match = line.match(
    /(?:^|\s)(?:ERROR|WARN|INFO|DEBUG|TRACE)\s+([a-zA-Z_][\w:-]*)/i,
  );
  return match?.[1]?.replace(/:+$/, '') ?? 'runtime';
}

function levelBadgeColor(level: string): string {
  switch (level) {
    case 'error':
      return 'bg-red-900/50 text-red-400 border-red-700/50';
    case 'warn':
      return 'bg-yellow-900/50 text-yellow-400 border-yellow-700/50';
    case 'info':
      return 'bg-green-900/50 text-green-400 border-green-700/50';
    case 'debug':
      return 'bg-cyan-900/50 text-cyan-300 border-cyan-700/50';
    case 'trace':
      return 'bg-indigo-900/50 text-indigo-300 border-indigo-700/50';
    default:
      return 'bg-gray-800 text-gray-400 border-gray-700';
  }
}

interface LogEntry {
  id: string;
  timestamp: string;
  line: string;
  level: string;
  source: string;
}

function toLogEntry(entry: RuntimeLogEntry): LogEntry {
  return {
    id: `runtime-log-${entry.id}`,
    timestamp: entry.timestamp,
    line: entry.line,
    level: deriveLevel(entry.line),
    source: deriveSource(entry.line),
  };
}

function mergeLogEntries(existing: LogEntry[], incoming: LogEntry[]): LogEntry[] {
  const merged = new Map(existing.map((entry) => [entry.id, entry]));
  incoming.forEach((entry) => merged.set(entry.id, entry));
  return Array.from(merged.values()).slice(-500);
}

const LEVELS = ['all', 'error', 'warn', 'info', 'debug', 'trace'] as const;
type LevelFilter = (typeof LEVELS)[number];
type StreamStatus = 'connecting' | 'connected' | 'reconnecting';

const LEVEL_COLORS: Record<LevelFilter, string> = {
  all: 'border-gray-700 text-gray-300 hover:bg-gray-800',
  error: 'border-red-700/60 text-red-400 hover:bg-red-950/40',
  warn: 'border-yellow-700/60 text-yellow-400 hover:bg-yellow-950/40',
  info: 'border-green-700/60 text-green-400 hover:bg-green-950/40',
  debug: 'border-cyan-700/60 text-cyan-300 hover:bg-cyan-950/40',
  trace: 'border-indigo-700/60 text-indigo-300 hover:bg-indigo-950/40',
};

const LEVEL_ACTIVE_COLORS: Record<LevelFilter, string> = {
  all: 'bg-gray-700 text-white border-gray-600',
  error: 'bg-red-900/60 text-red-300 border-red-700',
  warn: 'bg-yellow-900/60 text-yellow-300 border-yellow-700',
  info: 'bg-green-900/60 text-green-300 border-green-700',
  debug: 'bg-cyan-900/60 text-cyan-200 border-cyan-700',
  trace: 'bg-indigo-900/60 text-indigo-200 border-indigo-700',
};

export function LogsPanel() {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [paused, setPaused] = useState(false);
  const [pausedCount, setPausedCount] = useState(0);
  const [streamStatus, setStreamStatus] = useState<StreamStatus>('connecting');
  const [autoScroll, setAutoScroll] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [levelFilter, setLevelFilter] = useState<LevelFilter>('all');
  const [sourceFilter, setSourceFilter] = useState('all');
  const [searchQuery, setSearchQuery] = useState('');
  const [filtersCollapsed, setFiltersCollapsed] = useState(false);
  const [copied, setCopied] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const clientRef = useRef<SSEClient | null>(null);
  const pausedRef = useRef(false);
  const pausedEntriesRef = useRef<LogEntry[]>([]);
  const entryIdRef = useRef(0);

  useEffect(() => {
    let active = true;

    apiFetch<RuntimeLogsResponse>('/api/logs?limit=300')
      .then((payload) => {
        if (!active) return;
        const nextEntries = payload.entries.map(toLogEntry);
        setEntries((current) => mergeLogEntries(nextEntries, current));
        entryIdRef.current = nextEntries.length;
        setError(null);
      })
      .catch((err) => {
        if (!active) return;
        setError(err instanceof Error ? err.message : 'Failed to load logs');
      });

    return () => {
      active = false;
    };
  }, []);

  useEffect(() => {
    const client = new SSEClient({ path: '/api/logs/stream' });
    clientRef.current = client;

    client.onConnect = () => {
      setStreamStatus('connected');
      setError(null);
    };

    client.onError = (err) => {
      setStreamStatus('reconnecting');
      setError(err instanceof Error ? err.message : 'Log stream disconnected');
    };

    client.onEvent = (event: SSEEvent) => {
      if (event.type !== 'runtime_log' || typeof event.line !== 'string') return;

      entryIdRef.current += 1;
      const entry: LogEntry = {
        id:
          typeof event.id === 'number'
            ? `runtime-log-${event.id}`
            : `runtime-log-live-${entryIdRef.current}`,
        timestamp:
          typeof event.timestamp === 'string'
            ? event.timestamp
            : new Date().toISOString(),
        line: event.line,
        level: deriveLevel(event.line),
        source: deriveSource(event.line),
      };

      if (pausedRef.current) {
        pausedEntriesRef.current = [...pausedEntriesRef.current, entry].slice(-500);
        setPausedCount(pausedEntriesRef.current.length);
        return;
      }

      setEntries((prev) => {
        return mergeLogEntries(prev, [entry]);
      });
    };

    client.connect();

    return () => {
      client.disconnect();
      clientRef.current = null;
    };
  }, []);

  useEffect(() => {
    if (autoScroll && containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
  }, [entries, autoScroll]);

  const handleScroll = useCallback(() => {
    if (!containerRef.current) return;
    const { scrollTop, scrollHeight, clientHeight } = containerRef.current;
    const isAtBottom = scrollHeight - scrollTop - clientHeight < 50;
    setAutoScroll(isAtBottom);
  }, []);

  const jumpToBottom = () => {
    if (containerRef.current) {
      containerRef.current.scrollTop = containerRef.current.scrollHeight;
    }
    setAutoScroll(true);
  };

  const sourceOptions = useMemo(
    () => Array.from(new Set(entries.map((entry) => entry.source))).sort(),
    [entries],
  );
  const filteredEntries = useMemo(() => {
    const normalizedQuery = searchQuery.trim().toLowerCase();
    return entries.filter((entry) => {
      if (levelFilter !== 'all' && entry.level !== levelFilter) return false;
      if (sourceFilter !== 'all' && entry.source !== sourceFilter) return false;
      if (!normalizedQuery) return true;
      return `${entry.source} ${entry.line}`.toLowerCase().includes(normalizedQuery);
    });
  }, [entries, levelFilter, searchQuery, sourceFilter]);

  const formatEntries = (items: LogEntry[]) =>
    items
      .map(
        (entry) =>
          `[${formatTimestamp(entry.timestamp)}] [${entry.level.toUpperCase()}] [${entry.source}] ${entry.line}`,
      )
      .join('\n');

  const togglePaused = () => {
    if (!paused) {
      pausedRef.current = true;
      setPaused(true);
      return;
    }

    const buffered = pausedEntriesRef.current;
    pausedEntriesRef.current = [];
    pausedRef.current = false;
    setPaused(false);
    setPausedCount(0);
    if (buffered.length > 0) {
      setEntries((prev) => mergeLogEntries(prev, buffered));
    }
  };

  const handleExport = () => {
    const text = formatEntries(filteredEntries);
    const blob = new Blob([text], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `llamafarm-logs-${new Date().toISOString().slice(0, 19).replace(/:/g, '-')}.txt`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const handleCopy = async () => {
    try {
      await navigator.clipboard.writeText(formatEntries(filteredEntries));
      setCopied(true);
      globalThis.setTimeout(() => setCopied(false), 1500);
    } catch {
      setError('Could not copy the visible logs.');
    }
  };

  const handleReconnect = () => {
    setStreamStatus('connecting');
    setError(null);
    clientRef.current?.connect();
  };

  const levelCounts = entries.reduce<Record<string, number>>((acc, e) => {
    acc[e.level] = (acc[e.level] ?? 0) + 1;
    return acc;
  }, {});

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Toolbar */}
      <div className="border-b border-gray-800 bg-gray-900 px-3 py-3 sm:px-6">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 flex-wrap items-center gap-3">
            <Activity className="h-5 w-5 text-blue-400" aria-hidden="true" />
            <h2 className="text-base font-semibold text-white">Runtime Logs</h2>
            <div
              role="status"
              aria-live="polite"
              className="flex items-center gap-1.5"
            >
              <span
                className={`inline-block h-2 w-2 rounded-full ${
                  streamStatus === 'connected'
                    ? 'bg-green-500'
                    : streamStatus === 'reconnecting'
                      ? 'bg-yellow-500'
                      : 'bg-blue-500'
                }`}
              />
              <span className="text-xs text-gray-500">
                {streamStatus === 'connected'
                  ? 'Live'
                  : streamStatus === 'reconnecting'
                    ? 'Reconnecting…'
                    : 'Connecting…'}
              </span>
            </div>
            <span className="text-xs text-gray-500">
              {filteredEntries.length}/{entries.length} visible
            </span>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <button
              type="button"
              onClick={togglePaused}
              className={`flex min-h-9 items-center gap-1.5 rounded-lg px-3 text-sm font-medium transition-colors ${
                paused
                  ? 'bg-green-600 hover:bg-green-700 text-white'
                  : 'bg-yellow-600 hover:bg-yellow-700 text-white'
              }`}
            >
              {paused ? (
                <>
                  <Play className="h-3.5 w-3.5" aria-hidden="true" />
                  Resume{pausedCount > 0 ? ` (${pausedCount})` : ''}
                </>
              ) : (
                <>
                  <Pause className="h-3.5 w-3.5" aria-hidden="true" />
                  Pause
                </>
              )}
            </button>

            <button
              type="button"
              onClick={() => void handleCopy()}
              disabled={filteredEntries.length === 0}
              className="flex min-h-9 items-center gap-1.5 rounded-lg border border-gray-700 px-3 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
              title="Copy visible logs"
            >
              {copied ? (
                <Check className="h-3.5 w-3.5 text-green-400" aria-hidden="true" />
              ) : (
                <Clipboard className="h-3.5 w-3.5" aria-hidden="true" />
              )}
              {copied ? 'Copied' : 'Copy'}
            </button>

            <button
              type="button"
              onClick={handleExport}
              disabled={filteredEntries.length === 0}
              className="flex min-h-9 items-center gap-1.5 rounded-lg border border-gray-700 px-3 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
              title="Download visible logs as .txt"
            >
              <Download className="h-3.5 w-3.5" aria-hidden="true" />
              Download
            </button>

            <button
              type="button"
              onClick={autoScroll ? () => setAutoScroll(false) : jumpToBottom}
              className={`flex min-h-9 items-center gap-1.5 rounded-lg px-3 text-sm font-medium transition-colors ${
                autoScroll
                  ? 'border border-blue-700/60 bg-blue-950/40 text-blue-200 hover:bg-blue-900/50'
                  : 'bg-blue-600 text-white hover:bg-blue-700'
              }`}
            >
              <ArrowDown className="h-3.5 w-3.5" aria-hidden="true" />
              {autoScroll ? 'Following' : 'Follow latest'}
            </button>

            <button
              type="button"
              onClick={() => setFiltersCollapsed((prev) => !prev)}
              className="inline-flex min-h-9 min-w-9 items-center justify-center rounded text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              title={filtersCollapsed ? 'Show log filters' : 'Hide log filters'}
              aria-expanded={!filtersCollapsed}
            >
              {filtersCollapsed ? (
                <ChevronDown className="h-4 w-4" aria-hidden="true" />
              ) : (
                <ChevronUp className="h-4 w-4" aria-hidden="true" />
              )}
            </button>
          </div>
        </div>

        {!filtersCollapsed && (
          <div className="mt-3 space-y-2">
            <div className="flex flex-wrap items-center gap-2">
              <label className="relative min-w-[14rem] flex-1">
                <span className="sr-only">Search runtime logs</span>
                <Search
                  className="pointer-events-none absolute left-3 top-2.5 h-4 w-4 text-gray-500"
                  aria-hidden="true"
                />
                <input
                  type="search"
                  value={searchQuery}
                  onChange={(event) => setSearchQuery(event.target.value)}
                  placeholder="Search logs…"
                  className="w-full rounded-md border border-gray-700 bg-gray-950 py-2 pl-9 pr-3 text-sm text-gray-200 placeholder:text-gray-600"
                />
              </label>
              <label className="flex min-h-9 items-center gap-2 text-xs text-gray-400">
                <span>Source</span>
                <select
                  value={sourceFilter}
                  onChange={(event) => setSourceFilter(event.target.value)}
                  className="max-w-64 rounded-md border border-gray-700 bg-gray-950 px-2 py-2 text-gray-200"
                >
                  <option value="all">All sources</option>
                  {sourceOptions.map((source) => (
                    <option key={source} value={source}>
                      {source}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <div className="flex flex-wrap items-center gap-1.5">
              {LEVELS.map((level) => {
                const count = level === 'all' ? entries.length : (levelCounts[level] ?? 0);
                const isActive = levelFilter === level;
                return (
                  <button
                    type="button"
                    key={level}
                    onClick={() => setLevelFilter(level)}
                    aria-pressed={isActive}
                    className={`flex min-h-8 items-center gap-1.5 rounded-full border px-3 text-xs font-medium uppercase tracking-wide transition-colors ${
                      isActive ? LEVEL_ACTIVE_COLORS[level] : LEVEL_COLORS[level]
                    }`}
                  >
                    {level}
                    <span className="tabular-nums opacity-70">{count}</span>
                  </button>
                );
              })}
            </div>
          </div>
        )}
      </div>

      {error && (
        <div
          role="status"
          className="flex items-center gap-2 border-b border-yellow-900/50 bg-yellow-950/40 px-3 py-2 text-yellow-100 sm:px-6"
        >
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          <p className="flex-1 text-xs">{error}</p>
          {streamStatus !== 'connected' && (
            <button
              type="button"
              onClick={handleReconnect}
              disabled={streamStatus === 'connecting'}
              className="inline-flex min-h-8 items-center gap-1.5 rounded border border-yellow-700/60 px-2 text-xs transition-colors hover:bg-yellow-900/40 disabled:cursor-wait disabled:opacity-60"
            >
              <RefreshCw
                className={`h-3.5 w-3.5 ${streamStatus === 'connecting' ? 'animate-spin' : ''}`}
                aria-hidden="true"
              />
              Reconnect
            </button>
          )}
        </div>
      )}

      <div
        ref={containerRef}
        onScroll={handleScroll}
        className="flex-1 overflow-y-auto bg-gray-950 px-4 py-4"
      >
        {filteredEntries.length === 0 ? (
          <div className="flex flex-col items-center justify-center h-full text-gray-500">
            <Activity className="h-10 w-10 text-gray-600 mb-3" />
            <p className="text-sm">
              {entries.length === 0
                ? paused
                  ? `Display paused${pausedCount > 0 ? ` with ${pausedCount} buffered` : ''}.`
                  : 'Waiting for runtime logs...'
                : 'No logs match the current filters.'}
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {filteredEntries.map((entry) => (
              <div
                key={entry.id}
                className="rounded-lg border border-gray-800 bg-gray-900/60 px-3 py-2"
              >
                <div className="flex flex-wrap items-start gap-3">
                  <span className="mt-0.5 text-xs text-gray-500 font-mono whitespace-nowrap">
                    {formatTimestamp(entry.timestamp)}
                  </span>
                  <span
                    className={`inline-flex items-center rounded border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${levelBadgeColor(
                      entry.level,
                    )}`}
                  >
                    {entry.level}
                  </span>
                  <span
                    className="max-w-48 truncate font-mono text-[10px] text-cyan-400"
                    title={entry.source}
                  >
                    {entry.source}
                  </span>
                  <pre className="min-w-0 flex-1 whitespace-pre-wrap break-all text-xs text-gray-200">
                    {entry.line}
                  </pre>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export default function Logs() {
  return <LogsPanel />;
}
