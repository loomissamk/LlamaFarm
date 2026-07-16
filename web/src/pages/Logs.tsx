import { useState, useEffect, useRef, useCallback } from 'react';
import { Activity, Pause, Play, ArrowDown, AlertCircle, Download, ChevronDown, ChevronUp } from 'lucide-react';
import { apiFetch } from '@/lib/api';
import { SSEClient } from '@/lib/sse';
import type { RuntimeLogEntry, RuntimeLogsResponse, SSEEvent } from '@/types/api';

function formatTimestamp(ts?: string): string {
  if (!ts) return new Date().toLocaleTimeString();
  return new Date(ts).toLocaleTimeString();
}

function deriveLevel(line: string): string {
  if (line.includes(' ERROR ')) return 'error';
  if (line.includes(' WARN ')) return 'warn';
  if (line.includes(' INFO ')) return 'info';
  if (line.includes(' DEBUG ')) return 'debug';
  if (line.includes(' TRACE ')) return 'trace';
  return 'log';
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
}

function toLogEntry(entry: RuntimeLogEntry): LogEntry {
  return {
    id: `runtime-log-${entry.id}`,
    timestamp: entry.timestamp,
    line: entry.line,
    level: deriveLevel(entry.line),
  };
}

const LEVELS = ['all', 'error', 'warn', 'info', 'debug', 'trace'] as const;
type LevelFilter = (typeof LEVELS)[number];

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
  const [connected, setConnected] = useState(false);
  const [autoScroll, setAutoScroll] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [levelFilter, setLevelFilter] = useState<LevelFilter>('all');
  const [filtersCollapsed, setFiltersCollapsed] = useState(false);

  const containerRef = useRef<HTMLDivElement>(null);
  const pausedRef = useRef(false);
  const entryIdRef = useRef(0);

  useEffect(() => {
    pausedRef.current = paused;
  }, [paused]);

  useEffect(() => {
    let active = true;

    apiFetch<RuntimeLogsResponse>('/api/logs?limit=300')
      .then((payload) => {
        if (!active) return;
        const nextEntries = payload.entries.map(toLogEntry);
        setEntries(nextEntries);
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

    client.onConnect = () => {
      setConnected(true);
      setError(null);
    };

    client.onError = (err) => {
      setConnected(false);
      setError(err instanceof Error ? err.message : 'Log stream disconnected');
    };

    client.onEvent = (event: SSEEvent) => {
      if (pausedRef.current) return;
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
      };

      setEntries((prev) => {
        const next = [...prev, entry];
        return next.length > 500 ? next.slice(-500) : next;
      });
    };

    client.connect();

    return () => {
      client.disconnect();
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

  const filteredEntries =
    levelFilter === 'all' ? entries : entries.filter((e) => e.level === levelFilter);

  const handleExport = () => {
    const text = filteredEntries
      .map((e) => `[${formatTimestamp(e.timestamp)}] [${e.level.toUpperCase()}] ${e.line}`)
      .join('\n');
    const blob = new Blob([text], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `llamafarm-logs-${new Date().toISOString().slice(0, 19).replace(/:/g, '-')}.txt`;
    a.click();
    URL.revokeObjectURL(url);
  };

  const levelCounts = entries.reduce<Record<string, number>>((acc, e) => {
    acc[e.level] = (acc[e.level] ?? 0) + 1;
    return acc;
  }, {});

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Toolbar */}
      <div className="border-b border-gray-800 bg-gray-900 px-6 py-3">
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-3">
            <Activity className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Runtime Logs</h2>
            <div className="flex items-center gap-1.5">
              <span className={`inline-block h-2 w-2 rounded-full ${connected ? 'bg-green-500' : 'bg-red-500'}`} />
              <span className="text-xs text-gray-500">{connected ? 'Connected' : 'Disconnected'}</span>
            </div>
            <span className="text-xs text-gray-500">
              {filteredEntries.length}{levelFilter !== 'all' ? `/${entries.length}` : ''} lines
            </span>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={() => setPaused(!paused)}
              className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium transition-colors ${
                paused
                  ? 'bg-green-600 hover:bg-green-700 text-white'
                  : 'bg-yellow-600 hover:bg-yellow-700 text-white'
              }`}
            >
              {paused ? <><Play className="h-3.5 w-3.5" /> Resume</> : <><Pause className="h-3.5 w-3.5" /> Pause</>}
            </button>

            <button
              onClick={handleExport}
              disabled={filteredEntries.length === 0}
              className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border border-gray-700 text-gray-300 hover:bg-gray-800 hover:text-white transition-colors disabled:opacity-50"
              title="Export visible logs as .txt"
            >
              <Download className="h-3.5 w-3.5" />
              Export
            </button>

            {!autoScroll && (
              <button
                onClick={jumpToBottom}
                className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium bg-blue-600 hover:bg-blue-700 text-white transition-colors"
              >
                <ArrowDown className="h-3.5 w-3.5" />
                Jump to bottom
              </button>
            )}

            <button
              onClick={() => setFiltersCollapsed((prev) => !prev)}
              className="rounded p-1 text-gray-500 transition-colors hover:bg-gray-800 hover:text-gray-300"
              title={filtersCollapsed ? 'Show level filters' : 'Hide level filters'}
            >
              {filtersCollapsed ? <ChevronDown className="h-4 w-4" /> : <ChevronUp className="h-4 w-4" />}
            </button>
          </div>
        </div>

        {!filtersCollapsed && (
          <div className="mt-3 flex flex-wrap items-center gap-1.5">
            {LEVELS.map((level) => {
              const count = level === 'all' ? entries.length : (levelCounts[level] ?? 0);
              const isActive = levelFilter === level;
              return (
                <button
                  key={level}
                  onClick={() => setLevelFilter(level)}
                  className={`flex items-center gap-1.5 rounded-full border px-3 py-1 text-xs font-medium uppercase tracking-wide transition-colors ${
                    isActive ? LEVEL_ACTIVE_COLORS[level] : LEVEL_COLORS[level]
                  }`}
                >
                  {level}
                  <span className="tabular-nums opacity-70">{count}</span>
                </button>
              );
            })}
          </div>
        )}
      </div>

      {error && (
        <div className="flex items-center gap-2 px-6 py-2 border-b border-red-900/50 bg-red-950/40 text-red-200">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          <p className="text-xs">{error}</p>
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
                ? paused ? 'Log streaming is paused.' : 'Waiting for runtime logs...'
                : `No ${levelFilter} entries.`}
            </p>
          </div>
        ) : (
          <div className="space-y-2">
            {filteredEntries.map((entry) => (
              <div
                key={entry.id}
                className="rounded-lg border border-gray-800 bg-gray-900/60 px-3 py-2"
              >
                <div className="flex items-start gap-3">
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
