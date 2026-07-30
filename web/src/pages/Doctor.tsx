import { useEffect, useMemo, useState } from 'react';
import {
  AlertTriangle,
  CheckCircle,
  Loader2,
  Play,
  RotateCcw,
  Stethoscope,
  X,
  XCircle,
} from 'lucide-react';
import type { DiagResult } from '@/types/api';
import { runDoctor } from '@/lib/api';
import {
  diagnosticCategories,
  diagnosticCounts,
  filterDiagnostics,
  type DiagnosticSeverityFilter,
} from '@/lib/diagnostics';

interface DiagnosticRunMeta {
  completedAt: Date;
  durationMs: number;
  status: 'completed' | 'failed';
}

function severityIcon(severity: DiagResult['severity']) {
  switch (severity) {
    case 'ok':
      return <CheckCircle className="h-4 w-4 flex-shrink-0 text-green-400" />;
    case 'warn':
      return <AlertTriangle className="h-4 w-4 flex-shrink-0 text-yellow-400" />;
    case 'error':
      return <XCircle className="h-4 w-4 flex-shrink-0 text-red-400" />;
  }
}

function severityBorder(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'border-green-700/40';
    case 'warn':
      return 'border-yellow-700/40';
    case 'error':
      return 'border-red-700/40';
  }
}

function severityBg(severity: DiagResult['severity']): string {
  switch (severity) {
    case 'ok':
      return 'bg-green-900/10';
    case 'warn':
      return 'bg-yellow-900/10';
    case 'error':
      return 'bg-red-900/10';
  }
}

function formatDuration(durationMs: number): string {
  if (durationMs < 1_000) return `${Math.round(durationMs)} ms`;
  return `${(durationMs / 1_000).toFixed(1)} s`;
}

export function DiagnosticsPanel() {
  const [results, setResults] = useState<DiagResult[] | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [lastRun, setLastRun] = useState<DiagnosticRunMeta | null>(null);
  const [severityFilter, setSeverityFilter] = useState<DiagnosticSeverityFilter>('all');
  const [categoryFilter, setCategoryFilter] = useState('all');

  const counts = useMemo(() => diagnosticCounts(results ?? []), [results]);
  const categories = useMemo(() => diagnosticCategories(results ?? []), [results]);
  const filteredResults = useMemo(
    () => filterDiagnostics(results ?? [], severityFilter, categoryFilter),
    [categoryFilter, results, severityFilter],
  );
  const grouped = useMemo(
    () =>
      filteredResults.reduce<Record<string, DiagResult[]>>((accumulator, item) => {
        (accumulator[item.category] ??= []).push(item);
        return accumulator;
      }, {}),
    [filteredResults],
  );

  useEffect(() => {
    if (categoryFilter !== 'all' && !categories.includes(categoryFilter)) {
      setCategoryFilter('all');
    }
  }, [categories, categoryFilter]);

  const handleRun = async () => {
    const startedAt = performance.now();
    setLoading(true);
    setError(null);
    try {
      const data = await runDoctor();
      setResults(data);
      setLastRun({
        completedAt: new Date(),
        durationMs: performance.now() - startedAt,
        status: 'completed',
      });
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to run diagnostics');
      setLastRun({
        completedAt: new Date(),
        durationMs: performance.now() - startedAt,
        status: 'failed',
      });
    } finally {
      setLoading(false);
    }
  };

  const severityChoices: Array<{
    id: DiagnosticSeverityFilter;
    label: string;
    count: number;
  }> = [
    { id: 'all', label: 'All', count: results?.length ?? 0 },
    { id: 'error', label: 'Errors', count: counts.error },
    { id: 'warn', label: 'Warnings', count: counts.warn },
    { id: 'ok', label: 'OK', count: counts.ok },
  ];

  return (
    <div className="space-y-6 p-6">
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <Stethoscope className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Diagnostics</h2>
          </div>
          <p className="mt-2 text-sm text-gray-400">
            Run current node checks without clearing the previous result set while the rerun is in
            progress.
          </p>
          {lastRun && (
            <p className="mt-2 text-xs text-gray-500">
              Last attempt {lastRun.completedAt.toLocaleString()} ·{' '}
              {formatDuration(lastRun.durationMs)} ·{' '}
              <span className={lastRun.status === 'completed' ? 'text-green-400' : 'text-red-400'}>
                {lastRun.status}
              </span>
            </p>
          )}
        </div>
        <button
          type="button"
          onClick={() => void handleRun()}
          disabled={loading}
          className="flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
        >
          {loading ? (
            <>
              <Loader2 className="h-4 w-4 animate-spin" />
              Running...
            </>
          ) : (
            <>
              <Play className="h-4 w-4" />
              {results ? 'Run Again' : 'Run Diagnostics'}
            </>
          )}
        </button>
      </div>

      {error && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 rounded-lg border border-red-700 bg-red-900/30 p-4 text-red-300"
        >
          <AlertTriangle className="h-5 w-5 flex-shrink-0" />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">Diagnostic run failed: {error}</p>
            {results && (
              <p className="mt-1 text-xs text-red-300/70">
                The previous successful results remain visible below.
              </p>
            )}
          </div>
          <button
            type="button"
            onClick={() => void handleRun()}
            disabled={loading}
            className="inline-flex items-center gap-2 rounded-lg border border-red-700 px-3 py-2 text-sm font-medium hover:bg-red-900/50 disabled:opacity-50"
          >
            <RotateCcw className="h-4 w-4" />
            Retry
          </button>
          <button
            type="button"
            onClick={() => setError(null)}
            className="rounded-md p-2 hover:bg-red-900/50"
            aria-label="Dismiss diagnostic error"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      )}

      {loading && results && (
        <div
          role="status"
          className="flex items-center gap-3 rounded-lg border border-blue-800 bg-blue-950/30 p-3 text-sm text-blue-300"
        >
          <Loader2 className="h-4 w-4 animate-spin" />
          Refreshing diagnostics; previous results remain available.
        </div>
      )}

      {loading && !results && (
        <div className="flex flex-col items-center justify-center py-16">
          <Loader2 className="mb-4 h-10 w-10 animate-spin text-blue-500" />
          <p className="text-gray-400">Running diagnostics...</p>
          <p className="mt-1 text-sm text-gray-500">This may take a few seconds.</p>
        </div>
      )}

      {results && (
        <>
          <div className="flex flex-wrap items-center gap-4 rounded-xl border border-gray-800 bg-gray-900 p-4">
            <div className="flex items-center gap-2">
              <CheckCircle className="h-5 w-5 text-green-400" />
              <span className="text-sm font-medium text-white">
                {counts.ok} <span className="font-normal text-gray-400">ok</span>
              </span>
            </div>
            <div className="h-5 w-px bg-gray-700" />
            <div className="flex items-center gap-2">
              <AlertTriangle className="h-5 w-5 text-yellow-400" />
              <span className="text-sm font-medium text-white">
                {counts.warn}{' '}
                <span className="font-normal text-gray-400">
                  warning{counts.warn !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="h-5 w-px bg-gray-700" />
            <div className="flex items-center gap-2">
              <XCircle className="h-5 w-5 text-red-400" />
              <span className="text-sm font-medium text-white">
                {counts.error}{' '}
                <span className="font-normal text-gray-400">
                  error{counts.error !== 1 ? 's' : ''}
                </span>
              </span>
            </div>
            <div className="ml-auto">
              {counts.error > 0 ? (
                <span className="inline-flex rounded-full border border-red-700/50 bg-red-900/40 px-3 py-1 text-xs font-medium text-red-400">
                  Issues Found
                </span>
              ) : counts.warn > 0 ? (
                <span className="inline-flex rounded-full border border-yellow-700/50 bg-yellow-900/40 px-3 py-1 text-xs font-medium text-yellow-400">
                  Warnings
                </span>
              ) : (
                <span className="inline-flex rounded-full border border-green-700/50 bg-green-900/40 px-3 py-1 text-xs font-medium text-green-400">
                  All Clear
                </span>
              )}
            </div>
          </div>

          <div className="rounded-xl border border-gray-800 bg-gray-900/70 p-4">
            <div className="flex flex-col gap-4 lg:flex-row lg:items-end lg:justify-between">
              <div>
                <p className="text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Severity
                </p>
                <div className="mt-2 flex flex-wrap gap-2">
                  {severityChoices.map((choice) => (
                    <button
                      type="button"
                      key={choice.id}
                      onClick={() => setSeverityFilter(choice.id)}
                      aria-pressed={severityFilter === choice.id}
                      className={[
                        'rounded-lg border px-3 py-2 text-sm transition-colors',
                        severityFilter === choice.id
                          ? 'border-blue-500 bg-blue-600 text-white'
                          : 'border-gray-700 bg-gray-950 text-gray-300 hover:bg-gray-800',
                      ].join(' ')}
                    >
                      {choice.label} ({choice.count})
                    </button>
                  ))}
                </div>
              </div>
              <label className="text-sm text-gray-300">
                <span className="mb-2 block text-xs font-semibold uppercase tracking-wider text-gray-500">
                  Category
                </span>
                <select
                  aria-label="Diagnostic category"
                  value={categoryFilter}
                  onChange={(event) => setCategoryFilter(event.target.value)}
                  className="min-w-52 rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200"
                >
                  <option value="all">All categories</option>
                  {categories.map((category) => (
                    <option key={category} value={category}>
                      {category}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            <p className="mt-3 text-xs text-gray-500">
              Showing {filteredResults.length} of {results.length} checks.
            </p>
          </div>

          {Object.entries(grouped)
            .sort(([left], [right]) => left.localeCompare(right))
            .map(([category, items]) => (
              <div key={category}>
                <h3 className="mb-3 text-sm font-semibold uppercase tracking-wider text-gray-400 capitalize">
                  {category}
                </h3>
                <div className="space-y-2">
                  {items.map((result, index) => (
                    <div
                      key={`${category}-${result.severity}-${index}`}
                      className={`flex items-start gap-3 rounded-lg border p-3 ${severityBorder(
                        result.severity,
                      )} ${severityBg(result.severity)}`}
                    >
                      {severityIcon(result.severity)}
                      <div className="min-w-0">
                        <p className="text-sm text-white">{result.message}</p>
                        <p className="mt-0.5 text-xs text-gray-500 capitalize">
                          {result.severity}
                        </p>
                      </div>
                    </div>
                  ))}
                </div>
              </div>
            ))}

          {filteredResults.length === 0 && (
            <div className="rounded-xl border border-dashed border-gray-700 py-12 text-center text-gray-500">
              No diagnostic results match the selected filters.
            </div>
          )}
        </>
      )}

      {!results && !loading && !error && (
        <div className="flex flex-col items-center justify-center py-16 text-gray-500">
          <Stethoscope className="mb-4 h-12 w-12 text-gray-600" />
          <p className="text-lg font-medium">System Diagnostics</p>
          <p className="mt-1 text-sm">
            Click &quot;Run Diagnostics&quot; to check your LlamaFarm installation.
          </p>
        </div>
      )}
    </div>
  );
}

export default function Doctor() {
  return <DiagnosticsPanel />;
}
