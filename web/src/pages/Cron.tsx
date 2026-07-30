import { useEffect, useState } from 'react';
import {
  AlertCircle,
  CheckCircle,
  ChevronDown,
  ChevronUp,
  Clock,
  History,
  Pencil,
  Play,
  Plus,
  RefreshCw,
  Trash2,
  X,
  XCircle,
} from 'lucide-react';

import { formatInterval, intervalFromMs, intervalToMs, type IntervalUnit } from '@/lib/cron';
import {
  addCronJob,
  deleteCronJob,
  getCronJobs,
  getCronRuns,
  runCronJob,
  updateCronJob,
} from '@/lib/api';
import type { CronJob, CronRun } from '@/types/api';

type ScheduleKind = 'cron' | 'at' | 'every';

const PORTABLE_TEMPLATE_COMMAND = 'date';
const PORTABLE_TEMPLATE_NAME = 'Record current time';

function formatDate(iso: string | null): string {
  return iso ? new Date(iso).toLocaleString() : 'Not yet';
}

function formatDuration(milliseconds: number | null): string {
  if (milliseconds === null) return 'Duration unavailable';
  if (milliseconds < 1_000) return `${milliseconds} ms`;
  return `${(milliseconds / 1_000).toFixed(milliseconds < 10_000 ? 1 : 0)} s`;
}

function browserTimezone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC';
}

function scheduleSummary(job: CronJob): string {
  switch (job.schedule.kind) {
    case 'cron': {
      const expression = job.schedule.expr || job.expression;
      return `${expression} (${job.schedule.tz || 'UTC'})`;
    }
    case 'at':
      return job.schedule.at ? `Once at ${formatDate(job.schedule.at)}` : 'One-time run';
    case 'every':
      return job.schedule.every_ms
        ? `Every ${formatInterval(job.schedule.every_ms)}`
        : 'Interval';
  }
}

function toLocalDateTime(iso: string | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  const pad = (value: number) => String(value).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

function defaultRunAt(): string {
  return toLocalDateTime(new Date(Date.now() + 5 * 60_000).toISOString());
}

function StatusIcon({ status }: { status: string | null }) {
  if (!status) return null;
  switch (status.toLowerCase()) {
    case 'ok':
    case 'success':
      return <CheckCircle className="h-4 w-4 text-green-400" aria-hidden="true" />;
    case 'error':
    case 'failed':
      return <XCircle className="h-4 w-4 text-red-400" aria-hidden="true" />;
    default:
      return <AlertCircle className="h-4 w-4 text-yellow-400" aria-hidden="true" />;
  }
}

export default function Cron() {
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const [showForm, setShowForm] = useState(false);
  const [editingJob, setEditingJob] = useState<CronJob | null>(null);
  const [submitting, setSubmitting] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [formName, setFormName] = useState('');
  const [formScheduleKind, setFormScheduleKind] = useState<ScheduleKind>('cron');
  const [formSchedule, setFormSchedule] = useState('0 * * * *');
  const [formTimezone, setFormTimezone] = useState(browserTimezone);
  const [formRunAt, setFormRunAt] = useState(defaultRunAt);
  const [formIntervalValue, setFormIntervalValue] = useState('1');
  const [formIntervalUnit, setFormIntervalUnit] = useState<IntervalUnit>('minutes');
  const [formCommand, setFormCommand] = useState(PORTABLE_TEMPLATE_COMMAND);
  const [formEnabled, setFormEnabled] = useState(true);

  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [runCandidate, setRunCandidate] = useState<CronJob | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [expandedHistory, setExpandedHistory] = useState<string | null>(null);
  const [historyByJob, setHistoryByJob] = useState<Record<string, CronRun[]>>({});
  const [historyLoading, setHistoryLoading] = useState<string | null>(null);

  const fetchJobs = async () => {
    setLoading(true);
    setLoadError(null);
    try {
      setJobs(await getCronJobs());
    } catch (error: unknown) {
      setLoadError(error instanceof Error ? error.message : 'Failed to load scheduled jobs');
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void fetchJobs();
  }, []);

  useEffect(() => {
    if (!success) return;
    const timer = window.setTimeout(() => setSuccess(null), 4_000);
    return () => window.clearTimeout(timer);
  }, [success]);

  useEffect(() => {
    if (!showForm && !runCandidate) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || submitting || runningId) return;
      setShowForm(false);
      setEditingJob(null);
      setRunCandidate(null);
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [runCandidate, runningId, showForm, submitting]);

  const resetForm = (job?: CronJob | null) => {
    const interval = intervalFromMs(job?.schedule.every_ms ?? 60_000);
    setFormName(job?.name ?? '');
    setFormScheduleKind(job?.schedule.kind ?? 'cron');
    setFormSchedule(job?.schedule.expr ?? '0 * * * *');
    setFormTimezone(job?.schedule.tz ?? browserTimezone());
    setFormRunAt(job?.schedule.at ? toLocalDateTime(job.schedule.at) : defaultRunAt());
    setFormIntervalValue(String(interval.value));
    setFormIntervalUnit(interval.unit);
    setFormCommand(job?.command ?? PORTABLE_TEMPLATE_COMMAND);
    setFormEnabled(job?.enabled ?? true);
    setFormError(null);
  };

  const closeForm = () => {
    if (submitting) return;
    setShowForm(false);
    setEditingJob(null);
    resetForm(null);
  };

  const openNewJob = () => {
    setEditingJob(null);
    resetForm(null);
    setShowForm(true);
  };

  const openTemplate = () => {
    resetForm(null);
    setFormName(PORTABLE_TEMPLATE_NAME);
    setFormCommand(PORTABLE_TEMPLATE_COMMAND);
    setFormScheduleKind('at');
    setEditingJob(null);
    setShowForm(true);
  };

  const openEditJob = (job: CronJob) => {
    setEditingJob(job);
    resetForm(job);
    setShowForm(true);
  };

  const handleSave = async () => {
    const intervalMs = intervalToMs(Number(formIntervalValue), formIntervalUnit);
    if (!formCommand.trim()) {
      setFormError('Command is required.');
      return;
    }
    if (formScheduleKind === 'cron' && !formSchedule.trim()) {
      setFormError('Cron expression is required.');
      return;
    }
    if (formScheduleKind === 'cron' && !formTimezone.trim()) {
      setFormError('Timezone is required for cron jobs.');
      return;
    }
    if (formScheduleKind === 'at' && (!formRunAt || Number.isNaN(new Date(formRunAt).valueOf()))) {
      setFormError('Choose a valid run time.');
      return;
    }
    if (formScheduleKind === 'every' && intervalMs <= 0) {
      setFormError('Interval must be greater than zero.');
      return;
    }

    setSubmitting(true);
    setFormError(null);
    try {
      const payload = {
        name: formName.trim() || undefined,
        command: formCommand.trim(),
        schedule_kind: formScheduleKind,
        schedule: formScheduleKind === 'cron' ? formSchedule.trim() : undefined,
        timezone: formScheduleKind === 'cron' ? formTimezone.trim() : undefined,
        run_at: formScheduleKind === 'at' ? new Date(formRunAt).toISOString() : undefined,
        every_ms: formScheduleKind === 'every' ? intervalMs : undefined,
        enabled: formEnabled,
      };
      const job = editingJob
        ? await updateCronJob(editingJob.id, payload)
        : await addCronJob(payload);
      setJobs((previous) =>
        [...previous.filter((entry) => entry.id !== job.id), job].sort((a, b) =>
          a.next_run.localeCompare(b.next_run),
        ),
      );
      setSuccess(`${editingJob ? 'Updated' : 'Created'} ${job.name ?? job.id}.`);
      closeForm();
    } catch (error: unknown) {
      setFormError(error instanceof Error ? error.message : 'Failed to save scheduled job');
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (job: CronJob) => {
    setActionError(null);
    try {
      await deleteCronJob(job.id);
      setJobs((previous) => previous.filter((entry) => entry.id !== job.id));
      setSuccess(`Deleted ${job.name ?? job.id}.`);
    } catch (error: unknown) {
      setActionError(error instanceof Error ? error.message : 'Failed to delete scheduled job');
    } finally {
      setConfirmDelete(null);
    }
  };

  const refreshHistory = async (jobId: string) => {
    setHistoryLoading(jobId);
    try {
      const runs = await getCronRuns(jobId);
      setHistoryByJob((previous) => ({ ...previous, [jobId]: runs }));
    } catch (error: unknown) {
      setActionError(error instanceof Error ? error.message : 'Failed to load execution history');
    } finally {
      setHistoryLoading(null);
    }
  };

  const toggleHistory = async (job: CronJob) => {
    if (expandedHistory === job.id) {
      setExpandedHistory(null);
      return;
    }
    setExpandedHistory(job.id);
    setActionError(null);
    await refreshHistory(job.id);
  };

  const handleRun = async (job: CronJob) => {
    setRunCandidate(null);
    setRunningId(job.id);
    setActionError(null);
    try {
      const result = await runCronJob(job.id);
      if (result.job) {
        setJobs((previous) =>
          previous.map((entry) => (entry.id === job.id ? result.job! : entry)),
        );
      } else {
        await fetchJobs();
      }
      if (expandedHistory === job.id) await refreshHistory(job.id);
      setSuccess(
        result.status === 'ok'
          ? `${job.name ?? job.id} completed successfully.`
          : `${job.name ?? job.id} completed with an error. Open history for output.`,
      );
    } catch (error: unknown) {
      setActionError(error instanceof Error ? error.message : 'Failed to run scheduled job');
    } finally {
      setRunningId(null);
    }
  };

  if (loading && jobs.length === 0) {
    return (
      <div className="flex h-64 items-center justify-center" role="status">
        <RefreshCw className="h-8 w-8 animate-spin text-blue-400" aria-hidden="true" />
        <span className="sr-only">Loading scheduled jobs</span>
      </div>
    );
  }

  return (
    <div className="space-y-6 p-4 sm:p-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Clock className="h-5 w-5 text-blue-400" aria-hidden="true" />
            <h2 className="text-base font-semibold text-white">Scheduled Jobs ({jobs.length})</h2>
          </div>
          <p className="mt-2 max-w-2xl text-sm text-gray-400">
            Create recurring cron jobs, one-time runs, or readable intervals. Times are shown in
            your browser timezone ({browserTimezone()}).
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <button
            type="button"
            onClick={() => void fetchJobs()}
            disabled={loading}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
          >
            <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} aria-hidden="true" />
            Refresh
          </button>
          <button
            type="button"
            onClick={openNewJob}
            className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700"
          >
            <Plus className="h-4 w-4" aria-hidden="true" />
            Add Job
          </button>
        </div>
      </div>

      {loadError && (
        <div role="alert" className="rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          Could not refresh jobs: {loadError}
        </div>
      )}
      {actionError && (
        <div role="alert" className="flex items-start justify-between gap-4 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          <span>{actionError}</span>
          <button type="button" onClick={() => setActionError(null)} aria-label="Dismiss error">
            <X className="h-4 w-4" aria-hidden="true" />
          </button>
        </div>
      )}
      {success && (
        <div role="status" className="rounded-lg border border-green-700 bg-green-900/30 p-3 text-sm text-green-300">
          {success}
        </div>
      )}

      <section className="rounded-xl border border-gray-800 bg-gray-900 p-4" aria-labelledby="starter-template-title">
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <div>
            <h3 id="starter-template-title" className="text-sm font-medium text-white">Portable starter</h3>
            <p className="mt-1 text-sm text-gray-400">
              Schedule the cross-environment <code className="text-gray-300">date</code> command as
              a quick end-to-end check.
            </p>
          </div>
          <button
            type="button"
            onClick={openTemplate}
            className="rounded-lg border border-blue-700/70 bg-blue-900/30 px-4 py-2 text-sm font-medium text-blue-300 transition-colors hover:bg-blue-900/50"
          >
            Use starter
          </button>
        </div>
      </section>

      {jobs.length === 0 ? (
        <div className="rounded-xl border border-dashed border-gray-700 bg-gray-900 p-10 text-center">
          <Clock className="mx-auto mb-3 h-10 w-10 text-gray-600" aria-hidden="true" />
          <p className="font-medium text-gray-300">No scheduled jobs yet</p>
          <p className="mt-1 text-sm text-gray-500">Use the starter or create a custom schedule.</p>
        </div>
      ) : (
        <div className="space-y-3">
          {jobs.map((job) => {
            const history = historyByJob[job.id] ?? [];
            const isExpanded = expandedHistory === job.id;
            return (
              <article key={job.id} className="overflow-hidden rounded-xl border border-gray-800 bg-gray-900">
                <div className="grid gap-4 p-4 lg:grid-cols-[minmax(0,1.2fr)_minmax(0,1fr)_auto] lg:items-center">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <h3 className="truncate font-medium text-white">{job.name ?? 'Unnamed job'}</h3>
                      <span className={`rounded-full border px-2 py-0.5 text-xs ${job.enabled ? 'border-green-700/50 bg-green-900/40 text-green-400' : 'border-gray-700 bg-gray-800 text-gray-400'}`}>
                        {job.enabled ? 'Enabled' : 'Disabled'}
                      </span>
                    </div>
                    <p className="mt-1 break-all font-mono text-xs text-gray-400">{job.command}</p>
                    <p className="mt-1 font-mono text-[11px] text-gray-600">{job.id}</p>
                  </div>
                  <dl className="grid grid-cols-2 gap-3 text-xs">
                    <div>
                      <dt className="text-gray-500">Schedule</dt>
                      <dd className="mt-1 text-gray-300">{scheduleSummary(job)}</dd>
                    </div>
                    <div>
                      <dt className="text-gray-500">Next run</dt>
                      <dd className="mt-1 text-gray-300">{formatDate(job.next_run)}</dd>
                    </div>
                    <div>
                      <dt className="text-gray-500">Last run</dt>
                      <dd className="mt-1 text-gray-300">{formatDate(job.last_run)}</dd>
                    </div>
                    <div>
                      <dt className="text-gray-500">Last status</dt>
                      <dd className="mt-1 flex items-center gap-1.5 capitalize text-gray-300">
                        <StatusIcon status={job.last_status} />
                        {job.last_status ?? 'Not run'}
                      </dd>
                    </div>
                  </dl>
                  <div className="flex items-center gap-1 lg:justify-end">
                    <button type="button" onClick={() => setRunCandidate(job)} disabled={runningId === job.id} aria-label={`Run ${job.name ?? job.id} now`} className="rounded-lg p-2 text-gray-400 hover:bg-gray-800 hover:text-blue-300 disabled:opacity-50">
                      {runningId === job.id ? <RefreshCw className="h-4 w-4 animate-spin" aria-hidden="true" /> : <Play className="h-4 w-4" aria-hidden="true" />}
                    </button>
                    <button type="button" onClick={() => openEditJob(job)} aria-label={`Edit ${job.name ?? job.id}`} className="rounded-lg p-2 text-gray-400 hover:bg-gray-800 hover:text-white">
                      <Pencil className="h-4 w-4" aria-hidden="true" />
                    </button>
                    {confirmDelete === job.id ? (
                      <div className="ml-1 flex items-center gap-2 rounded-lg border border-red-800 bg-red-950/30 px-2 py-1">
                        <span className="text-xs text-red-300">Delete?</span>
                        <button type="button" onClick={() => void handleDelete(job)} className="text-xs font-medium text-red-300 hover:text-red-200">Yes</button>
                        <button type="button" onClick={() => setConfirmDelete(null)} className="text-xs font-medium text-gray-400 hover:text-white">No</button>
                      </div>
                    ) : (
                      <button type="button" onClick={() => setConfirmDelete(job.id)} aria-label={`Delete ${job.name ?? job.id}`} className="rounded-lg p-2 text-gray-400 hover:bg-gray-800 hover:text-red-400">
                        <Trash2 className="h-4 w-4" aria-hidden="true" />
                      </button>
                    )}
                  </div>
                </div>
                <button type="button" onClick={() => void toggleHistory(job)} aria-expanded={isExpanded} className="flex w-full items-center justify-between border-t border-gray-800 px-4 py-3 text-left text-sm text-gray-400 hover:bg-gray-800/40 hover:text-white">
                  <span className="flex items-center gap-2">
                    <History className="h-4 w-4" aria-hidden="true" />
                    Execution history and output
                  </span>
                  {isExpanded ? <ChevronUp className="h-4 w-4" aria-hidden="true" /> : <ChevronDown className="h-4 w-4" aria-hidden="true" />}
                </button>
                {isExpanded && (
                  <div className="border-t border-gray-800 bg-gray-950/40 p-4">
                    {historyLoading === job.id ? (
                      <p role="status" className="text-sm text-gray-400">Loading execution history…</p>
                    ) : history.length === 0 ? (
                      <p className="text-sm text-gray-500">No recorded executions.</p>
                    ) : (
                      <ol className="space-y-3">
                        {history.map((run) => (
                          <li key={run.id} className="rounded-lg border border-gray-800 bg-gray-950 p-3">
                            <div className="flex flex-wrap items-center justify-between gap-2 text-xs">
                              <span className="flex items-center gap-1.5 capitalize text-gray-300">
                                <StatusIcon status={run.status} />
                                {run.status}
                              </span>
                              <span className="text-gray-500">{formatDate(run.started_at)} · {formatDuration(run.duration_ms)}</span>
                            </div>
                            <pre className="mt-3 max-h-48 overflow-auto whitespace-pre-wrap break-words rounded bg-black/30 p-3 text-xs text-gray-300">{run.output || 'No output captured.'}</pre>
                          </li>
                        ))}
                      </ol>
                    )}
                  </div>
                )}
              </article>
            );
          })}
        </div>
      )}

      {showForm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center overflow-y-auto bg-black/70 p-4">
          <div role="dialog" aria-modal="true" aria-labelledby="cron-form-title" className="my-auto w-full max-w-xl rounded-xl border border-gray-700 bg-gray-900 p-5 shadow-2xl sm:p-6">
            <div className="mb-4 flex items-center justify-between">
              <h3 id="cron-form-title" className="text-lg font-semibold text-white">{editingJob ? 'Edit Scheduled Job' : 'Add Scheduled Job'}</h3>
              <button type="button" onClick={closeForm} aria-label="Close scheduled job form" className="rounded p-1 text-gray-400 hover:bg-gray-800 hover:text-white">
                <X className="h-5 w-5" aria-hidden="true" />
              </button>
            </div>
            {formError && <div role="alert" className="mb-4 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">{formError}</div>}
            <div className="max-h-[70vh] space-y-4 overflow-y-auto pr-1">
              <div>
                <label htmlFor="cron-name" className="mb-1 block text-sm font-medium text-gray-300">Name <span className="font-normal text-gray-500">(optional)</span></label>
                <input id="cron-name" autoFocus type="text" value={formName} onChange={(event) => setFormName(event.target.value)} placeholder={PORTABLE_TEMPLATE_NAME} className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
              </div>
              <div>
                <label htmlFor="cron-kind" className="mb-1 block text-sm font-medium text-gray-300">Schedule type</label>
                <select id="cron-kind" value={formScheduleKind} onChange={(event) => setFormScheduleKind(event.target.value as ScheduleKind)} className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500">
                  <option value="cron">Cron expression</option>
                  <option value="at">Run once</option>
                  <option value="every">Repeat at an interval</option>
                </select>
              </div>
              {formScheduleKind === 'cron' && (
                <div className="grid gap-4 sm:grid-cols-2">
                  <div>
                    <label htmlFor="cron-expression" className="mb-1 block text-sm font-medium text-gray-300">Cron expression</label>
                    <input id="cron-expression" type="text" value={formSchedule} onChange={(event) => setFormSchedule(event.target.value)} placeholder="0 * * * *" className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 font-mono text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
                  </div>
                  <div>
                    <label htmlFor="cron-timezone" className="mb-1 block text-sm font-medium text-gray-300">IANA timezone</label>
                    <input id="cron-timezone" type="text" value={formTimezone} onChange={(event) => setFormTimezone(event.target.value)} placeholder="UTC" className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
                  </div>
                </div>
              )}
              {formScheduleKind === 'at' && (
                <div>
                  <label htmlFor="cron-run-at" className="mb-1 block text-sm font-medium text-gray-300">Run at ({browserTimezone()})</label>
                  <input id="cron-run-at" type="datetime-local" value={formRunAt} onChange={(event) => setFormRunAt(event.target.value)} className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500" />
                </div>
              )}
              {formScheduleKind === 'every' && (
                <div>
                  <label htmlFor="cron-interval-value" className="mb-1 block text-sm font-medium text-gray-300">Repeat every</label>
                  <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-2">
                    <input id="cron-interval-value" type="number" min="0.001" step="any" value={formIntervalValue} onChange={(event) => setFormIntervalValue(event.target.value)} className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500" />
                    <select aria-label="Interval unit" value={formIntervalUnit} onChange={(event) => setFormIntervalUnit(event.target.value as IntervalUnit)} className="rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500">
                      <option value="seconds">Seconds</option>
                      <option value="minutes">Minutes</option>
                      <option value="hours">Hours</option>
                    </select>
                  </div>
                </div>
              )}
              <div>
                <div className="mb-1 flex items-center justify-between gap-2">
                  <label htmlFor="cron-command" className="block text-sm font-medium text-gray-300">Command</label>
                  <button type="button" onClick={() => { setFormCommand(PORTABLE_TEMPLATE_COMMAND); if (!formName.trim()) setFormName(PORTABLE_TEMPLATE_NAME); }} className="text-xs font-medium text-blue-300 hover:text-blue-200">Insert portable starter</button>
                </div>
                <input id="cron-command" type="text" value={formCommand} onChange={(event) => setFormCommand(event.target.value)} placeholder="date" className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 font-mono text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
              </div>
              <label className="flex items-center gap-3 rounded-lg border border-gray-800 bg-gray-950/40 px-3 py-2 text-sm text-gray-300">
                <input type="checkbox" checked={formEnabled} onChange={(event) => setFormEnabled(event.target.checked)} className="h-4 w-4 rounded border-gray-600 bg-gray-800" />
                Enable this job immediately
              </label>
            </div>
            <div className="mt-6 flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
              <button type="button" onClick={closeForm} className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800 hover:text-white">Cancel</button>
              <button type="button" onClick={() => void handleSave()} disabled={submitting} className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50">{submitting ? 'Saving…' : editingJob ? 'Save Changes' : 'Create Job'}</button>
            </div>
          </div>
        </div>
      )}

      {runCandidate && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4">
          <div role="dialog" aria-modal="true" aria-labelledby="run-preview-title" className="w-full max-w-lg rounded-xl border border-gray-700 bg-gray-900 p-6 shadow-2xl">
            <h3 id="run-preview-title" className="text-lg font-semibold text-white">Run job now?</h3>
            <p className="mt-2 text-sm text-gray-400">This starts one execution without changing the saved schedule.</p>
            <dl className="mt-4 space-y-3 rounded-lg border border-gray-800 bg-gray-950/50 p-4 text-sm">
              <div><dt className="text-gray-500">Job</dt><dd className="mt-1 text-white">{runCandidate.name ?? runCandidate.id}</dd></div>
              <div><dt className="text-gray-500">Command</dt><dd className="mt-1 break-all font-mono text-gray-300">{runCandidate.command}</dd></div>
            </dl>
            <div className="mt-6 flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
              <button type="button" autoFocus onClick={() => setRunCandidate(null)} className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800 hover:text-white">Cancel</button>
              <button type="button" onClick={() => void handleRun(runCandidate)} className="inline-flex items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700">
                <Play className="h-4 w-4" aria-hidden="true" />
                Run now
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
