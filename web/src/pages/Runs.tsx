import { useCallback, useEffect, useState } from 'react';
import {
  Activity,
  CheckCircle,
  ChevronDown,
  ChevronRight,
  ClipboardList,
  FileText,
  Filter,
  Loader2,
  ShieldAlert,
  ShieldCheck,
  XCircle,
} from 'lucide-react';
import { apiFetch } from '@/lib/api';

interface RunMeta {
  run_id: string;
  session_id?: string | null;
  channel: string;
  provider: string;
  model: string;
  mode: string;
  started_at_ms: number;
  ended_at_ms?: number | null;
  status: 'running' | 'completed' | 'completed_unverified' | 'failed' | 'cancelled';
  attempts: number;
  retry_reason?: string | null;
}

interface PlanStep {
  id: number;
  title: string;
  status: string;
  allowed_tools: string[];
  depends_on: number[];
  expected_evidence: string[];
  evidence: number[];
  verified: boolean;
  verifier_note?: string | null;
}

interface ToolEvent {
  seq: number;
  ts_ms: number;
  tool: string;
  args_summary: string;
  success: boolean;
  duration_ms: number;
  output_digest: string;
  output_excerpt: string;
  artifacts: string[];
}

interface ToolRoutingScore {
  name: string;
  score: number;
  matched_terms: string[];
}

interface ToolRoutingRecord {
  seq?: number;
  ts_ms?: number;
  strategy?: string;
  reason?: string;
  query_excerpt?: string;
  selected?: string[];
  excluded?: string[];
  scores?: ToolRoutingScore[];
  total_count?: number;
  selected_count?: number;
  excluded_count?: number;
}

interface RunDetail {
  meta: RunMeta;
  plan: PlanStep[];
  events: ToolEvent[];
  /** Missing on ledgers created before per-turn tool routing was recorded. */
  tool_routing?: ToolRoutingRecord[];
  /** Oldest routing decisions evicted from the bounded ledger window. */
  tool_routing_dropped?: number;
}

const STATUS_STYLES: Record<RunMeta['status'], string> = {
  running: 'bg-blue-900/40 text-blue-300 border-blue-700/40',
  completed: 'bg-green-900/40 text-green-300 border-green-700/40',
  completed_unverified: 'bg-yellow-900/40 text-yellow-300 border-yellow-700/40',
  failed: 'bg-red-900/40 text-red-300 border-red-700/40',
  cancelled: 'bg-gray-800 text-gray-300 border-gray-600/40',
};

function statusLabel(status: RunMeta['status']): string {
  if (status === 'completed_unverified') return 'unverified claim';
  return status;
}

function formatTime(ms: number): string {
  return new Date(ms).toLocaleString();
}

function formatElapsed(meta: RunMeta): string {
  const end = meta.ended_at_ms ?? Date.now();
  const secs = Math.max(0, Math.round((end - meta.started_at_ms) / 1000));
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ${secs % 60}s`;
  return `${Math.floor(secs / 3600)}h ${Math.floor((secs % 3600) / 60)}m`;
}

function StatusChip({ status }: { status: RunMeta['status'] }) {
  return (
    <span
      className={`inline-block px-2 py-0.5 rounded border text-xs font-medium whitespace-nowrap ${STATUS_STYLES[status] ?? STATUS_STYLES.cancelled}`}
    >
      {statusLabel(status)}
    </span>
  );
}

function PlanTable({ plan }: { plan: PlanStep[] }) {
  if (plan.length === 0) {
    return <p className="text-sm text-gray-500">No plan was recorded for this run.</p>;
  }
  return (
    <div className="overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr className="text-left text-gray-400 border-b border-gray-800">
            <th className="py-2 pr-3 font-medium">#</th>
            <th className="py-2 pr-3 font-medium">Step</th>
            <th className="py-2 pr-3 font-medium">Status</th>
            <th className="py-2 pr-3 font-medium">Evidence</th>
            <th className="py-2 font-medium">Verified</th>
          </tr>
        </thead>
        <tbody>
          {plan.map((step) => (
            <tr key={step.id} className="border-b border-gray-800/60 align-top">
              <td className="py-2 pr-3 text-gray-500">{step.id}</td>
              <td className="py-2 pr-3">
                <span className="text-gray-200">{step.title}</span>
                {step.expected_evidence.length > 0 && (
                  <div className="text-xs text-gray-500 mt-0.5">
                    expects: {step.expected_evidence.join(', ')}
                  </div>
                )}
              </td>
              <td className="py-2 pr-3 text-gray-300">{step.status}</td>
              <td className="py-2 pr-3 text-gray-300">{step.evidence.length}</td>
              <td className="py-2">
                {step.verified ? (
                  <span className="inline-flex items-center gap-1 text-green-400">
                    <ShieldCheck className="h-4 w-4" /> yes
                  </span>
                ) : (
                  <span className="inline-flex items-center gap-1 text-yellow-400">
                    <ShieldAlert className="h-4 w-4" />
                    {step.verifier_note ?? 'not verified'}
                  </span>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function EventRow({ event }: { event: ToolEvent }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border border-gray-800 rounded-md bg-gray-900/40">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="w-full flex items-center gap-2 px-3 py-2 text-left text-sm hover:bg-gray-800/40"
      >
        {open ? (
          <ChevronDown className="h-4 w-4 text-gray-500 flex-shrink-0" />
        ) : (
          <ChevronRight className="h-4 w-4 text-gray-500 flex-shrink-0" />
        )}
        {event.success ? (
          <CheckCircle className="h-4 w-4 text-green-400 flex-shrink-0" />
        ) : (
          <XCircle className="h-4 w-4 text-red-400 flex-shrink-0" />
        )}
        <span className="font-mono text-gray-200">{event.tool}</span>
        <span className="text-gray-500 truncate flex-1">{event.args_summary}</span>
        <span className="text-xs text-gray-500 whitespace-nowrap">{event.duration_ms} ms</span>
      </button>
      {open && (
        <div className="px-3 pb-3 space-y-2">
          <pre className="text-xs text-gray-300 bg-gray-950 border border-gray-800 rounded p-2 overflow-x-auto whitespace-pre-wrap">
            {event.output_excerpt || '(empty output)'}
          </pre>
          <div className="text-xs text-gray-500">
            {formatTime(event.ts_ms)} · output sha256/16: {event.output_digest}
          </div>
          {event.artifacts.length > 0 && (
            <div className="text-xs text-gray-400 flex items-center gap-1">
              <FileText className="h-3 w-3" /> artifacts: {event.artifacts.join(', ')}
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function routingLabel(value: string | undefined, fallback: string): string {
  return value?.trim() ? value.replace(/_/g, ' ') : fallback;
}

function ToolRoutingPanel({
  records,
  dropped = 0,
}: {
  records?: ToolRoutingRecord[];
  dropped?: number;
}) {
  if (!records || records.length === 0) {
    return (
      <p className="text-sm text-gray-500">
        No tool-routing decision was recorded. This is expected for legacy runs and turns that did
        not use the model tool loop.
      </p>
    );
  }

  const visibleRecords = records.slice(-20).reverse();
  const retainedNotShown = Math.max(0, records.length - visibleRecords.length);

  return (
    <div className="space-y-2">
      {(dropped > 0 || retainedNotShown > 0) && (
        <p className="text-xs text-gray-500">
          Showing the latest {visibleRecords.length} decisions
          {retainedNotShown > 0 ? `; ${retainedNotShown} older retained decisions are hidden` : ''}
          {dropped > 0 ? `; ${dropped} oldest decisions were evicted from the bounded ledger` : ''}.
        </p>
      )}
      {visibleRecords.map((record, index) => {
        const selected = record.selected ?? [];
        const excluded = record.excluded ?? [];
        const scores = record.scores ?? [];
        const selectedCount = Math.max(record.selected_count ?? 0, selected.length);
        const excludedCount = Math.max(record.excluded_count ?? 0, excluded.length);
        const partitionCount = selectedCount + excludedCount;
        const totalCount = Math.max(record.total_count ?? partitionCount, partitionCount);
        const hiddenPercent = totalCount > 0 ? Math.round((excludedCount / totalCount) * 100) : 0;
        const turnNumber = record.seq === undefined ? records.length - index : record.seq + 1;

        return (
          <div
            key={`${record.seq ?? index}-${record.ts_ms ?? 0}`}
            className="border border-gray-800 rounded-md bg-gray-900/40 p-3 space-y-3"
          >
            <div className="flex items-center gap-2 flex-wrap text-sm">
              <Filter className="h-4 w-4 text-cyan-400" />
              <span className="font-medium text-gray-200">Turn {turnNumber}</span>
              <span className="rounded border border-cyan-800/60 bg-cyan-950/40 px-2 py-0.5 text-xs text-cyan-300">
                {routingLabel(record.strategy, 'unknown strategy')}
              </span>
              <span className="rounded border border-gray-700 bg-gray-950 px-2 py-0.5 text-xs text-gray-300">
                {routingLabel(record.reason, 'reason not recorded')}
              </span>
              {record.ts_ms !== undefined && record.ts_ms > 0 && (
                <span className="ml-auto text-xs text-gray-500">{formatTime(record.ts_ms)}</span>
              )}
            </div>

            {record.query_excerpt && (
              <div className="rounded border border-gray-800 bg-gray-950 px-2.5 py-2 text-xs text-gray-400">
                <span className="text-gray-600">current turn: </span>
                {record.query_excerpt}
              </div>
            )}

            <div className="grid grid-cols-3 gap-2 text-xs">
              <div className="rounded border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">Registry</div>
                <div className="font-mono text-gray-200">{totalCount} tools</div>
              </div>
              <div className="rounded border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">Selected</div>
                <div className="font-mono text-green-300">{selectedCount}</div>
              </div>
              <div className="rounded border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">Hidden</div>
                <div className="font-mono text-cyan-300">
                  {excludedCount} ({hiddenPercent}%)
                </div>
              </div>
            </div>

            <div className="space-y-1.5">
              <div className="text-xs font-medium uppercase tracking-wide text-gray-500">
                Selected tools
              </div>
              {selected.length === 0 ? (
                <p className="text-xs text-gray-500">No tools selected.</p>
              ) : (
                <div className="flex flex-wrap gap-1.5">
                  {selected.map((tool) => {
                    const ranked = scores.find((entry) => entry.name === tool);
                    const title = ranked?.matched_terms?.length
                      ? `matched: ${ranked.matched_terms.join(', ')}`
                      : ranked
                        ? 'ranked query match'
                        : 'always available or directly selected';
                    return (
                      <span
                        key={tool}
                        title={title}
                        className="rounded border border-green-800/50 bg-green-950/30 px-2 py-1 font-mono text-xs text-green-300"
                      >
                        {tool}
                        {ranked && (
                          <span className="ml-1 text-green-600">{ranked.score.toFixed(1)}</span>
                        )}
                      </span>
                    );
                  })}
                </div>
              )}
            </div>

            {scores.length > 0 && (
              <div className="text-xs text-gray-500">
                Ranked matches:{' '}
                {scores
                  .map((entry) => {
                    const terms = entry.matched_terms?.length
                      ? ` [${entry.matched_terms.join(', ')}]`
                      : '';
                    return `${entry.name} ${entry.score.toFixed(1)}${terms}`;
                  })
                  .join(' · ')}
              </div>
            )}

            <details className="text-xs text-gray-500">
              <summary className="cursor-pointer hover:text-gray-300">
                Excluded tools ({excludedCount})
              </summary>
              <div className="mt-2 flex flex-wrap gap-1.5">
                {excluded.length === 0 ? (
                  <span>None</span>
                ) : (
                  excluded.map((tool) => (
                    <span
                      key={tool}
                      className="rounded border border-gray-800 bg-gray-950 px-2 py-1 font-mono text-gray-500"
                    >
                      {tool}
                    </span>
                  ))
                )}
              </div>
            </details>
          </div>
        );
      })}
    </div>
  );
}

export default function Runs() {
  const [runs, setRuns] = useState<RunMeta[]>([]);
  const [liveIds, setLiveIds] = useState<string[]>([]);
  const [selected, setSelected] = useState<string | null>(null);
  const [detail, setDetail] = useState<RunDetail | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const refreshList = useCallback(async () => {
    try {
      const data = await apiFetch<{ runs: RunMeta[]; live: string[] }>('/api/runs');
      setRuns(data.runs);
      setLiveIds(data.live);
      setError(null);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load runs');
    } finally {
      setLoading(false);
    }
  }, []);

  const refreshDetail = useCallback(async (runId: string) => {
    try {
      const data = await apiFetch<{ run: RunDetail }>(`/api/runs/${encodeURIComponent(runId)}`);
      setDetail(data.run);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load run detail');
    }
  }, []);

  useEffect(() => {
    refreshList();
    const timer = setInterval(refreshList, 5000);
    return () => clearInterval(timer);
  }, [refreshList]);

  useEffect(() => {
    if (!selected) return;
    refreshDetail(selected);
    const timer = setInterval(() => refreshDetail(selected), 5000);
    return () => clearInterval(timer);
  }, [selected, refreshDetail]);

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center gap-3">
        <ClipboardList className="h-6 w-6 text-blue-400" />
        <div>
          <h1 className="text-xl font-semibold">Run Inspector</h1>
          <p className="text-sm text-gray-400">
            Plan state, tool evidence, and verifier results for every agent run. Completion is
            recorded from tool evidence, not model prose.
          </p>
        </div>
      </div>

      {error && (
        <div className="border border-red-700/40 bg-red-900/10 text-red-300 text-sm rounded-md px-3 py-2">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 xl:grid-cols-3 gap-6">
        <div className="space-y-2">
          {loading ? (
            <div className="flex items-center gap-2 text-gray-400 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" /> Loading runs…
            </div>
          ) : runs.length === 0 ? (
            <p className="text-sm text-gray-500">
              No runs recorded yet. Start an agent chat or autonomous run and its ledger will
              appear here.
            </p>
          ) : (
            runs.map((run) => (
              <button
                type="button"
                key={run.run_id}
                onClick={() => {
                  setSelected(run.run_id);
                  setDetail(null);
                }}
                className={`w-full text-left border rounded-md px-3 py-2 space-y-1 transition-colors ${
                  selected === run.run_id
                    ? 'border-blue-600/60 bg-blue-900/10'
                    : 'border-gray-800 bg-gray-900/40 hover:bg-gray-800/40'
                }`}
              >
                <div className="flex items-center gap-2">
                  <StatusChip status={run.status} />
                  {liveIds.includes(run.run_id) && (
                    <span className="inline-flex items-center gap-1 text-xs text-blue-300">
                      <Activity className="h-3 w-3 animate-pulse" /> live
                    </span>
                  )}
                  <span className="text-xs text-gray-500 ml-auto">{formatElapsed(run)}</span>
                </div>
                <div className="font-mono text-xs text-gray-300 truncate">{run.run_id}</div>
                <div className="text-xs text-gray-500">
                  {run.model} · {run.channel} · attempts {run.attempts}
                  {run.retry_reason ? ` · ${run.retry_reason}` : ''}
                </div>
                <div className="text-xs text-gray-600">{formatTime(run.started_at_ms)}</div>
              </button>
            ))
          )}
        </div>

        <div className="xl:col-span-2 space-y-6">
          {!selected ? (
            <p className="text-sm text-gray-500">Select a run to inspect its ledger.</p>
          ) : !detail ? (
            <div className="flex items-center gap-2 text-gray-400 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" /> Loading run…
            </div>
          ) : (
            <>
              <div className="border border-gray-800 rounded-md bg-gray-900/40 p-4 space-y-2">
                <div className="flex items-center gap-2 flex-wrap">
                  <StatusChip status={detail.meta.status} />
                  <span className="font-mono text-sm text-gray-200">{detail.meta.run_id}</span>
                  {detail.meta.session_id && (
                    <span className="text-xs text-gray-500">
                      session {detail.meta.session_id} — use Stop in Agent Chat to cancel a live
                      run
                    </span>
                  )}
                </div>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-x-6 gap-y-1 text-sm text-gray-300">
                  <span>model: {detail.meta.model}</span>
                  <span>provider: {detail.meta.provider}</span>
                  <span>mode: {detail.meta.mode}</span>
                  <span>channel: {detail.meta.channel}</span>
                  <span>attempts: {detail.meta.attempts}</span>
                  <span>elapsed: {formatElapsed(detail.meta)}</span>
                  <span>started: {formatTime(detail.meta.started_at_ms)}</span>
                  {detail.meta.retry_reason && (
                    <span className="text-yellow-300">retry: {detail.meta.retry_reason}</span>
                  )}
                </div>
              </div>

              <section className="space-y-2">
                <h2 className="text-sm font-semibold text-gray-300 uppercase tracking-wide">
                  Tool routing · {detail.tool_routing?.length ?? 0}{' '}
                  {(detail.tool_routing?.length ?? 0) === 1 ? 'decision' : 'decisions'}
                </h2>
                <ToolRoutingPanel
                  records={detail.tool_routing}
                  dropped={detail.tool_routing_dropped}
                />
              </section>

              <section className="space-y-2">
                <h2 className="text-sm font-semibold text-gray-300 uppercase tracking-wide">
                  Plan · {detail.plan.filter((s) => s.verified).length}/{detail.plan.length}{' '}
                  verified
                </h2>
                <PlanTable plan={detail.plan} />
              </section>

              <section className="space-y-2">
                <h2 className="text-sm font-semibold text-gray-300 uppercase tracking-wide">
                  Tool timeline · {detail.events.length} calls
                </h2>
                {detail.events.length === 0 ? (
                  <p className="text-sm text-gray-500">No tool calls recorded.</p>
                ) : (
                  <div className="space-y-1">
                    {detail.events.map((event) => (
                      <EventRow key={event.seq} event={event} />
                    ))}
                  </div>
                )}
              </section>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
