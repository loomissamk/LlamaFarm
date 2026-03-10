import { useState, useEffect } from 'react';
import {
  Clock,
  Plus,
  Play,
  Pencil,
  Trash2,
  X,
  CheckCircle,
  XCircle,
  AlertCircle,
  RefreshCw,
} from 'lucide-react';
import type { CronJob } from '@/types/api';
import { addCronJob, deleteCronJob, getCronJobs, runCronJob, updateCronJob } from '@/lib/api';

function formatDate(iso: string | null): string {
  if (!iso) return '-';
  const d = new Date(iso);
  return d.toLocaleString();
}

type ScheduleKind = 'cron' | 'at' | 'every';

function scheduleSummary(job: CronJob): string {
  switch (job.schedule.kind) {
    case 'cron':
      return job.schedule.expr || job.expression;
    case 'at':
      return job.schedule.at ? `Once at ${formatDate(job.schedule.at)}` : 'One-time run';
    case 'every':
      return job.schedule.every_ms ? `Every ${Math.round(job.schedule.every_ms / 1000)}s` : 'Interval';
  }
}

function toLocalDateTime(iso: string | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  const pad = (value: number) => String(value).padStart(2, '0');
  const year = date.getFullYear();
  const month = pad(date.getMonth() + 1);
  const day = pad(date.getDate());
  const hour = pad(date.getHours());
  const minute = pad(date.getMinutes());
  return `${year}-${month}-${day}T${hour}:${minute}`;
}

export default function Cron() {
  const [jobs, setJobs] = useState<CronJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [editingJob, setEditingJob] = useState<CronJob | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [runningId, setRunningId] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  // Form state
  const [formName, setFormName] = useState('');
  const [formScheduleKind, setFormScheduleKind] = useState<ScheduleKind>('cron');
  const [formSchedule, setFormSchedule] = useState('0 * * * *');
  const [formRunAt, setFormRunAt] = useState('');
  const [formEveryMs, setFormEveryMs] = useState('60000');
  const [formCommand, setFormCommand] = useState('/home/bat/test.sh');
  const [formEnabled, setFormEnabled] = useState(true);
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const resetForm = (job?: CronJob | null) => {
    setFormName(job?.name ?? '');
    setFormScheduleKind(job?.schedule.kind ?? 'cron');
    setFormSchedule(job?.schedule.expr ?? '0 * * * *');
    setFormRunAt(job?.schedule.at ? toLocalDateTime(job.schedule.at) : '');
    setFormEveryMs(
      job?.schedule.every_ms ? String(job.schedule.every_ms) : '60000',
    );
    setFormCommand(job?.command ?? '/home/bat/test.sh');
    setFormEnabled(job?.enabled ?? true);
    setFormError(null);
  };

  const fetchJobs = () => {
    setLoading(true);
    getCronJobs()
      .then(setJobs)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchJobs();
  }, []);

  useEffect(() => {
    if (!success) return;
    const timer = window.setTimeout(() => setSuccess(null), 3500);
    return () => window.clearTimeout(timer);
  }, [success]);

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

  const openEditJob = (job: CronJob) => {
    setEditingJob(job);
    resetForm(job);
    setShowForm(true);
  };

  const handleSave = async () => {
    if (!formCommand.trim()) {
      setFormError('Command is required.');
      return;
    }
    if (formScheduleKind === 'cron' && !formSchedule.trim()) {
      setFormError('Cron expression is required.');
      return;
    }
    if (formScheduleKind === 'at' && !formRunAt) {
      setFormError('Run time is required for one-time jobs.');
      return;
    }
    if (formScheduleKind === 'every' && (!formEveryMs.trim() || Number(formEveryMs) <= 0)) {
      setFormError('Interval milliseconds must be greater than zero.');
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
        run_at: formScheduleKind === 'at' ? new Date(formRunAt).toISOString() : undefined,
        every_ms: formScheduleKind === 'every' ? Number(formEveryMs) : undefined,
        enabled: formEnabled,
      };

      const job = editingJob
        ? await updateCronJob(editingJob.id, payload)
        : await addCronJob(payload);

      setJobs((prev) => {
        const rest = prev.filter((existing) => existing.id !== job.id);
        return [...rest, job].sort((a, b) => a.next_run.localeCompare(b.next_run));
      });
      setSuccess(
        editingJob ? `Updated scheduled job ${job.name ?? job.id}.` : `Created scheduled job ${job.name ?? job.id}.`,
      );
      closeForm();
    } catch (err: unknown) {
      setFormError(
        err instanceof Error ? err.message : 'Failed to save scheduled job',
      );
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (id: string) => {
    try {
      await deleteCronJob(id);
      setJobs((prev) => prev.filter((j) => j.id !== id));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to delete job');
    } finally {
      setConfirmDelete(null);
    }
  };

  const handleRun = async (job: CronJob) => {
    setRunningId(job.id);
    setError(null);
    try {
      const result = await runCronJob(job.id);
      if (result.job) {
        setJobs((prev) => prev.map((entry) => (entry.id === job.id ? result.job! : entry)));
      } else {
        fetchJobs();
      }
      setSuccess(
        result.status === 'ok'
          ? `${job.name ?? job.id} ran successfully.`
          : `${job.name ?? job.id} returned an error. Check last output.`,
      );
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to run job');
    } finally {
      setRunningId(null);
    }
  };

  const statusIcon = (status: string | null) => {
    if (!status) return null;
    switch (status.toLowerCase()) {
      case 'ok':
      case 'success':
        return <CheckCircle className="h-4 w-4 text-green-400" />;
      case 'error':
      case 'failed':
        return <XCircle className="h-4 w-4 text-red-400" />;
      default:
        return <AlertCircle className="h-4 w-4 text-yellow-400" />;
    }
  };

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load cron jobs: {error}
        </div>
      </div>
    );
  }

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="p-6 space-y-6">
      <div className="flex items-center justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Clock className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">
              Scheduled Jobs ({jobs.length})
            </h2>
          </div>
          <p className="mt-2 text-sm text-gray-400">
            Create cron, one-time, or interval jobs. The starter command defaults to
            <span className="ml-1 font-mono text-gray-300">/home/bat/test.sh</span>.
          </p>
        </div>
        <div className="flex items-center gap-2">
          <button
            onClick={fetchJobs}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
          >
            <RefreshCw className="h-4 w-4" />
            Refresh
          </button>
          <button
            onClick={openNewJob}
            className="flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700"
          >
            <Plus className="h-4 w-4" />
            Add Job
          </button>
        </div>
      </div>

      {success && (
        <div className="rounded-lg border border-green-700 bg-green-900/30 p-3 text-sm text-green-300">
          {success}
        </div>
      )}

      <div className="rounded-xl border border-gray-800 bg-gray-900 p-4">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-center lg:justify-between">
          <div>
            <p className="text-sm font-medium text-white">Starter Template</p>
            <p className="mt-1 text-sm text-gray-400">
              Use the local test script as a simple smoke job before scheduling anything more
              complex.
            </p>
          </div>
          <button
            onClick={() => {
              setFormCommand('/home/bat/test.sh');
              setFormName('Run local test script');
              setFormScheduleKind('at');
              setShowForm(true);
              setEditingJob(null);
            }}
            className="rounded-lg border border-blue-700/70 bg-blue-900/30 px-4 py-2 text-sm font-medium text-blue-300 transition-colors hover:bg-blue-900/50"
          >
            Use /home/bat/test.sh
          </button>
        </div>
      </div>

      {showForm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
          <div className="mx-4 w-full max-w-xl rounded-xl border border-gray-700 bg-gray-900 p-6">
            <div className="mb-4 flex items-center justify-between">
              <h3 className="text-lg font-semibold text-white">
                {editingJob ? 'Edit Scheduled Job' : 'Add Scheduled Job'}
              </h3>
              <button
                onClick={closeForm}
                className="text-gray-400 transition-colors hover:text-white"
              >
                <X className="h-5 w-5" />
              </button>
            </div>

            {formError && (
              <div className="mb-4 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
                {formError}
              </div>
            )}

            <div className="space-y-4">
              <div>
                <label className="mb-1 block text-sm font-medium text-gray-300">
                  Name (optional)
                </label>
                <input
                  type="text"
                  value={formName}
                  onChange={(e) => setFormName(e.target.value)}
                  placeholder="Run local test script"
                  className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>

              <div>
                <label className="mb-1 block text-sm font-medium text-gray-300">
                  Schedule Type
                </label>
                <select
                  value={formScheduleKind}
                  onChange={(e) => setFormScheduleKind(e.target.value as ScheduleKind)}
                  className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500"
                >
                  <option value="cron">Cron expression</option>
                  <option value="at">Run once at a time</option>
                  <option value="every">Repeat every interval</option>
                </select>
              </div>

              {formScheduleKind === 'cron' && (
                <div>
                  <label className="mb-1 block text-sm font-medium text-gray-300">
                    Cron Expression
                  </label>
                  <input
                    type="text"
                    value={formSchedule}
                    onChange={(e) => setFormSchedule(e.target.value)}
                    placeholder="0 * * * *"
                    className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                  />
                </div>
              )}

              {formScheduleKind === 'at' && (
                <div>
                  <label className="mb-1 block text-sm font-medium text-gray-300">
                    Run At
                  </label>
                  <input
                    type="datetime-local"
                    value={formRunAt}
                    onChange={(e) => setFormRunAt(e.target.value)}
                    className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500"
                  />
                </div>
              )}

              {formScheduleKind === 'every' && (
                <div>
                  <label className="mb-1 block text-sm font-medium text-gray-300">
                    Interval (milliseconds)
                  </label>
                  <input
                    type="number"
                    min="1"
                    step="1000"
                    value={formEveryMs}
                    onChange={(e) => setFormEveryMs(e.target.value)}
                    className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500"
                  />
                </div>
              )}

              <div>
                <div className="mb-1 flex items-center justify-between gap-2">
                  <label className="block text-sm font-medium text-gray-300">
                    Command
                  </label>
                  <button
                    onClick={() => {
                      setFormCommand('/home/bat/test.sh');
                      if (!formName.trim()) {
                        setFormName('Run local test script');
                      }
                    }}
                    className="text-xs font-medium text-blue-300 transition-colors hover:text-blue-200"
                  >
                    Insert template
                  </button>
                </div>
                <input
                  type="text"
                  value={formCommand}
                  onChange={(e) => setFormCommand(e.target.value)}
                  placeholder="/home/bat/test.sh"
                  className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>

              <label className="flex items-center gap-3 rounded-lg border border-gray-800 bg-gray-950/40 px-3 py-2 text-sm text-gray-300">
                <input
                  type="checkbox"
                  checked={formEnabled}
                  onChange={(e) => setFormEnabled(e.target.checked)}
                  className="h-4 w-4 rounded border-gray-600 bg-gray-800"
                />
                Enable this job immediately
              </label>
            </div>

            <div className="mt-6 flex justify-end gap-3">
              <button
                onClick={closeForm}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={() => void handleSave()}
                disabled={submitting}
                className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
              >
                {submitting ? 'Saving...' : editingJob ? 'Save Changes' : 'Create Job'}
              </button>
            </div>
          </div>
        </div>
      )}

      {jobs.length === 0 ? (
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-8 text-center">
          <Clock className="h-5 w-5 text-blue-400" />
          <Clock className="mx-auto mb-3 h-10 w-10 text-gray-600" />
          <p className="text-gray-400">No scheduled tasks configured.</p>
        </div>
      ) : (
        <div className="overflow-x-auto rounded-xl border border-gray-800 bg-gray-900">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-gray-800">
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Name
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Schedule
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Command
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Next Run
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Last Status
                </th>
                <th className="text-left px-4 py-3 text-gray-400 font-medium">
                  Enabled
                </th>
                <th className="text-right px-4 py-3 text-gray-400 font-medium">
                  Actions
                </th>
              </tr>
            </thead>
            <tbody>
              {jobs.map((job) => (
                <tr
                  key={job.id}
                  className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors"
                >
                  <td className="px-4 py-3 text-white font-medium">
                    <div className="space-y-1">
                      <div>{job.name ?? '-'}</div>
                      <div className="font-mono text-[11px] text-gray-500">{job.id}</div>
                    </div>
                  </td>
                  <td className="px-4 py-3 text-gray-300 text-xs">
                    {scheduleSummary(job)}
                  </td>
                  <td className="px-4 py-3 text-gray-300 font-mono text-xs max-w-[200px] truncate">
                    {job.command}
                  </td>
                  <td className="px-4 py-3 text-gray-400 text-xs">
                    {formatDate(job.next_run)}
                  </td>
                  <td className="px-4 py-3">
                    <div className="flex items-center gap-1.5">
                      {statusIcon(job.last_status)}
                      <span className="text-gray-300 text-xs capitalize">
                        {job.last_status ?? '-'}
                      </span>
                    </div>
                  </td>
                  <td className="px-4 py-3">
                    <span
                      className={`inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium ${
                        job.enabled
                          ? 'bg-green-900/40 text-green-400 border border-green-700/50'
                          : 'bg-gray-800 text-gray-500 border border-gray-700'
                      }`}
                    >
                      {job.enabled ? 'Enabled' : 'Disabled'}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right">
                    {confirmDelete === job.id ? (
                      <div className="flex items-center justify-end gap-2">
                        <span className="text-xs text-red-400">Delete?</span>
                        <button
                          onClick={() => handleDelete(job.id)}
                          className="text-red-400 hover:text-red-300 text-xs font-medium"
                        >
                          Yes
                        </button>
                        <button
                          onClick={() => setConfirmDelete(null)}
                          className="text-gray-400 hover:text-white text-xs font-medium"
                        >
                          No
                        </button>
                      </div>
                    ) : (
                      <div className="flex items-center justify-end gap-3">
                        <button
                          onClick={() => void handleRun(job)}
                          disabled={runningId === job.id}
                          className="text-gray-400 transition-colors hover:text-blue-300 disabled:opacity-50"
                          title="Run now"
                        >
                          {runningId === job.id ? (
                            <RefreshCw className="h-4 w-4 animate-spin" />
                          ) : (
                            <Play className="h-4 w-4" />
                          )}
                        </button>
                        <button
                          onClick={() => openEditJob(job)}
                          className="text-gray-400 transition-colors hover:text-white"
                          title="Edit"
                        >
                          <Pencil className="h-4 w-4" />
                        </button>
                        <button
                          onClick={() => setConfirmDelete(job.id)}
                          className="text-gray-400 transition-colors hover:text-red-400"
                          title="Delete"
                        >
                          <Trash2 className="h-4 w-4" />
                        </button>
                      </div>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
