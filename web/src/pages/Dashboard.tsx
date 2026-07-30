import { useEffect, useState } from 'react';
import { Link } from 'react-router-dom';
import {
  Activity,
  CheckCircle2,
  Cpu,
  Database,
  Radio,
  RefreshCw,
  Server,
  MemoryStick,
  Gauge,
  GitCommit,
  Network,
} from 'lucide-react';
import type { StatusResponse } from '@/types/api';
import { getStatus, putIntegrationCredentials } from '@/lib/api';

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return 'Unavailable';
  if (bytes === 0) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const exponent = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** exponent).toFixed(exponent >= 3 ? 1 : 0)} ${units[exponent]}`;
}

function healthColor(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'bg-green-500';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'bg-yellow-500';
    default:
      return 'bg-red-500';
  }
}

function healthBorder(status: string): string {
  switch (status.toLowerCase()) {
    case 'ok':
    case 'healthy':
      return 'border-green-500/30';
    case 'warn':
    case 'warning':
    case 'degraded':
      return 'border-yellow-500/30';
    default:
      return 'border-red-500/30';
  }
}

function canonicalOllamaModelName(model: string): string {
  const trimmed = model.trim();
  const parts = trimmed.split('/');
  const leaf = parts[parts.length - 1] ?? trimmed;
  return leaf.includes(':') ? trimmed : `${trimmed}:latest`;
}

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [selectedModel, setSelectedModel] = useState('');
  const [switchingModel, setSwitchingModel] = useState(false);
  const [switchError, setSwitchError] = useState<string | null>(null);
  const [switchSuccess, setSwitchSuccess] = useState<string | null>(null);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);

  const fetchStatus = () => {
    setRefreshing(true);
    getStatus()
      .then((s) => {
        setStatus(s);
        const configuredModel = canonicalOllamaModelName(s.ollama.configured_model);
        setSelectedModel((current) => {
          if (s.ollama.installed_models.includes(current)) return current;
          return (
            s.ollama.installed_models.find(
              (model) => canonicalOllamaModelName(model) === configuredModel,
            ) ??
            s.ollama.installed_models[0] ??
            ''
          );
        });
        setLastUpdated(new Date());
        setError(null);
      })
      .catch((err) => setError(err.message))
      .finally(() => setRefreshing(false));
  };

  useEffect(() => {
    fetchStatus();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (error && !status) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load dashboard: {error}
        </div>
      </div>
    );
  }

  if (!status) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  const installedModels = status.ollama.installed_models;
  const loadedModels = status.ollama.loaded_models;
  const configuredModel = status.ollama.configured_model || status.model;
  const capacity = status.capacity;
  const runtime = status.runtime;
  const buildCommit = status.build?.commit ?? status.build_commit;
  const modelSelectionChanged =
    selectedModel.length > 0 &&
    canonicalOllamaModelName(selectedModel) !== canonicalOllamaModelName(configuredModel);

  const switchDefaultModel = async () => {
    const targetModel = selectedModel.trim();
    if (!targetModel || !modelSelectionChanged) return;

    setSwitchingModel(true);
    setSwitchError(null);
    setSwitchSuccess(null);
    try {
      await putIntegrationCredentials('ollama', {
        revision: status.ollama.revision,
        fields: { default_model: targetModel },
      });
      const refreshed = await getStatus();
      setStatus(refreshed);
      setSelectedModel(targetModel);
      setSwitchSuccess(
        `${targetModel} is now the default for new chat turns and was saved to config.`,
      );
    } catch (err: unknown) {
      setSwitchError(err instanceof Error ? err.message : 'Failed to switch the Ollama model');
    } finally {
      setSwitchingModel(false);
    }
  };

  return (
    <div className="space-y-6 p-4 sm:p-6">
      <div className="mb-4 flex flex-wrap gap-2 text-xs">
        {[
          { to: '/runs', label: 'Runs' },
          { to: '/federation', label: 'Fleet' },
          { to: '/tools', label: 'Tools' },
          { to: '/logs', label: 'Logs' },
          { to: '/doctor', label: 'Diagnostics' },
        ].map(({ to, label }) => (
          <Link key={to} to={to} className="rounded-full border border-gray-800 px-3 py-1 text-gray-400 hover:border-gray-600 hover:text-white">{label}</Link>
        ))}
      </div>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <h2 className="text-base font-semibold text-white">Dashboard</h2>
          <p className="mt-1 text-xs text-gray-500">
            {lastUpdated ? `Runtime facts updated ${lastUpdated.toLocaleTimeString()}` : 'Runtime facts'}
          </p>
        </div>
        <button
          onClick={fetchStatus}
          disabled={refreshing}
          className="flex items-center gap-1.5 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
        >
          <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>

      {error && (
        <div role="alert" className="rounded-lg border border-amber-700/50 bg-amber-950/20 p-3 text-sm text-amber-200">
          Latest refresh failed; showing the last successful snapshot. {error}
        </div>
      )}

      <section className="rounded-xl border border-blue-800/60 bg-gray-900 p-5">
        <div className="flex flex-col gap-5 xl:flex-row xl:items-start xl:justify-between">
          <div className="min-w-0 flex-1">
            <div className="flex flex-wrap items-center gap-2">
              <Server className="h-5 w-5 text-blue-400" />
              <h2 className="text-base font-semibold text-white">Ollama Model Control</h2>
              <span
                className={`rounded-full px-2.5 py-1 text-xs font-medium ${
                  status.ollama.reachable
                    ? 'bg-emerald-900/40 text-emerald-300'
                    : 'bg-red-900/40 text-red-300'
                }`}
              >
                {status.ollama.reachable ? 'Ollama reachable' : 'Ollama unavailable'}
              </span>
            </div>
            <p className="mt-2 text-sm text-gray-400">
              Choose an installed model for subsequent chat turns. Switching updates the live
              gateway and atomically persists the default; it does not pull, delete, or unload
              Ollama models.
            </p>

            <div className="mt-4 flex flex-col gap-3 sm:flex-row">
              <select
                aria-label="Installed Ollama model"
                value={selectedModel}
                onChange={(event) => {
                  setSelectedModel(event.target.value);
                  setSwitchError(null);
                  setSwitchSuccess(null);
                }}
                disabled={switchingModel || installedModels.length === 0}
                className="min-w-0 flex-1 rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 disabled:opacity-50"
              >
                {installedModels.length === 0 && (
                  <option value="">No installed models reported</option>
                )}
                {installedModels.map((model) => (
                  <option key={model} value={model}>
                    {model}
                  </option>
                ))}
              </select>
              <button
                type="button"
                onClick={() => void switchDefaultModel()}
                disabled={switchingModel || !modelSelectionChanged}
                className="inline-flex items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-500 disabled:cursor-not-allowed disabled:opacity-50"
              >
                <RefreshCw className={`h-4 w-4 ${switchingModel ? 'animate-spin' : ''}`} />
                {switchingModel ? 'Switching…' : 'Use for New Chats'}
              </button>
            </div>

            {switchSuccess && (
              <div className="mt-3 flex items-start gap-2 rounded-lg border border-emerald-800 bg-emerald-950/30 p-3 text-sm text-emerald-300">
                <CheckCircle2 className="mt-0.5 h-4 w-4 flex-shrink-0" />
                <span>{switchSuccess}</span>
              </div>
            )}
            {switchError && (
              <div className="mt-3 rounded-lg border border-red-800 bg-red-950/30 p-3 text-sm text-red-300">
                {switchError}
              </div>
            )}
            {status.ollama.model_environment_override && (
              <div className="mt-3 rounded-lg border border-amber-800 bg-amber-950/30 p-3 text-sm text-amber-200">
                {status.ollama.model_environment_override} currently overrides the saved model on
                process restart. Remove that environment override to make dashboard selections
                restart-persistent.
              </div>
            )}
          </div>

          <div className="grid min-w-0 gap-3 sm:grid-cols-2 xl:w-[38rem]">
            <div className="rounded-lg border border-gray-800 bg-gray-950/60 p-4">
              <p className="text-xs font-medium uppercase tracking-[0.16em] text-gray-500">
                Configured default
              </p>
              <p className="mt-2 break-all text-sm font-semibold text-white">{configuredModel}</p>
              <p className="mt-2 text-xs text-gray-500">Used by the next dashboard chat turn</p>
            </div>
            <div className="rounded-lg border border-gray-800 bg-gray-950/60 p-4">
              <p className="text-xs font-medium uppercase tracking-[0.16em] text-gray-500">
                Resident now
              </p>
              {loadedModels.length === 0 ? (
                <p className="mt-2 text-sm text-gray-500">None reported</p>
              ) : (
                <div className="mt-2 flex flex-wrap gap-2">
                  {loadedModels.map((model) => (
                    <span
                      key={model}
                      className="max-w-full break-all rounded-full bg-emerald-900/40 px-2.5 py-1 text-xs text-emerald-300"
                    >
                      {model}
                    </span>
                  ))}
                </div>
              )}
              <p className="mt-2 text-xs text-gray-500">
                Ollama loads the configured model on first use
              </p>
            </div>
          </div>
        </div>
      </section>

      <section aria-labelledby="runtime-capacity-title" className="space-y-3">
        <div>
          <h2 id="runtime-capacity-title" className="text-base font-semibold text-white">Runtime and capacity</h2>
          <p className="mt-1 text-sm text-gray-400">Facts reported by this running node, not template assumptions.</p>
        </div>
        <div className="grid gap-4 sm:grid-cols-2 xl:grid-cols-4">
          <div className="rounded-xl border border-gray-800 bg-gray-900 p-4">
            <div className="flex items-center gap-2 text-gray-400"><GitCommit className="h-4 w-4" aria-hidden="true" /><span className="text-sm">Build</span></div>
            <p className="mt-3 font-mono text-sm text-white">{status.app_version ?? status.version ?? 'Unknown version'}</p>
            <p className="mt-1 truncate font-mono text-xs text-gray-500" title={buildCommit ?? undefined}>{buildCommit ? buildCommit.slice(0, 12) : 'Commit not injected'}</p>
          </div>
          <div className="rounded-xl border border-gray-800 bg-gray-900 p-4">
            <div className="flex items-center gap-2 text-gray-400"><Cpu className="h-4 w-4" aria-hidden="true" /><span className="text-sm">Compute</span></div>
            <p className="mt-3 text-sm font-semibold text-white">{capacity ? `${capacity.logical_cpus} logical CPUs` : 'Unavailable'}</p>
            <p className="mt-1 text-xs text-gray-500">GPU free {formatBytes(capacity?.gpu_free_memory_bytes)}</p>
          </div>
          <div className="rounded-xl border border-gray-800 bg-gray-900 p-4">
            <div className="flex items-center gap-2 text-gray-400"><MemoryStick className="h-4 w-4" aria-hidden="true" /><span className="text-sm">Memory</span></div>
            <p className="mt-3 text-sm font-semibold text-white">{formatBytes(capacity?.memory_available_bytes)} available</p>
            <p className="mt-1 text-xs text-gray-500">Effective total {formatBytes(capacity?.memory_limit_bytes ?? capacity?.total_memory_bytes)}</p>
          </div>
          <div className="rounded-xl border border-gray-800 bg-gray-900 p-4">
            <div className="flex items-center gap-2 text-gray-400"><Gauge className="h-4 w-4" aria-hidden="true" /><span className="text-sm">Work</span></div>
            <p className="mt-3 text-sm font-semibold text-white">{capacity?.active_runs ?? status.queue?.active_runs ?? 0} active runs</p>
            <p className="mt-1 text-xs text-gray-500">{status.queue?.queue_depth_available ? `${status.queue.queued_runs ?? 0} queued` : 'Queue depth unavailable'}</p>
          </div>
        </div>
        {runtime && (
          <div className={`flex flex-col gap-3 rounded-xl border p-4 text-sm sm:flex-row sm:items-center sm:justify-between ${runtime.gateway.restart_required ? 'border-amber-700/50 bg-amber-950/20' : 'border-gray-800 bg-gray-900'}`}>
            <div className="flex items-start gap-2">
              <Network className={`mt-0.5 h-4 w-4 ${runtime.gateway.restart_required ? 'text-amber-300' : 'text-blue-300'}`} aria-hidden="true" />
              <div>
                <p className="font-medium text-white">Gateway {runtime.gateway.host}:{runtime.gateway.port}</p>
                <p className="mt-1 text-xs text-gray-400">
                  Configured {runtime.gateway.configured_host}:{runtime.gateway.configured_port}
                  {runtime.gateway.restart_required ? ' · restart required to apply listener changes' : ' · effective configuration matches'}
                </p>
              </div>
            </div>
            <span className="font-mono text-xs text-gray-500">{runtime.tool_count} tools · {runtime.max_tool_iterations.toLocaleString()} max iterations</span>
          </div>
        )}
      </section>

      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-blue-600/20 rounded-lg">
              <Cpu className="h-5 w-5 text-blue-400" />
            </div>
            <span className="text-sm text-gray-400">Current Model</span>
          </div>
          <p className="text-lg font-semibold text-white truncate">{configuredModel}</p>
          <p className="text-sm text-gray-400">
            {status.ollama.active_model_loaded ? 'Loaded in Ollama' : 'Configured, not loaded'}
          </p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-emerald-600/20 rounded-lg">
              <Server className="h-5 w-5 text-emerald-400" />
            </div>
            <span className="text-sm text-gray-400">Ollama Endpoint</span>
          </div>
          <p className="text-sm font-semibold text-white break-all">{status.ollama.endpoint}</p>
          <p className="text-sm text-gray-400">Gateway :{status.gateway_port}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-orange-600/20 rounded-lg">
              <Database className="h-5 w-5 text-orange-400" />
            </div>
            <span className="text-sm text-gray-400">Memory Backend</span>
          </div>
          <p className="text-lg font-semibold text-white capitalize">{status.memory_backend}</p>
          <p className="text-sm text-gray-400">Installed models: {installedModels.length}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-violet-600/20 rounded-lg">
              <Activity className="h-5 w-5 text-violet-400" />
            </div>
            <span className="text-sm text-gray-400">Uptime</span>
          </div>
          <p className="text-lg font-semibold text-white">
            {formatUptime(status.uptime_seconds)}
          </p>
          <p className="text-sm text-gray-400">Since last restart</p>
        </div>
      </div>

      <div className="grid grid-cols-1 lg:grid-cols-3 gap-6">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <Server className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Installed Models</h2>
          </div>
          <div className="space-y-2 max-h-80 overflow-y-auto">
            {installedModels.length === 0 ? (
              <p className="text-sm text-gray-500">No installed models reported by Ollama.</p>
            ) : (
              installedModels.map((model) => (
                <div
                  key={model}
                  className={`rounded-lg border px-3 py-2 text-sm ${
                    model === status.model
                      ? 'border-emerald-700/70 bg-emerald-950/30 text-emerald-200'
                      : 'border-gray-800 bg-gray-800/50 text-gray-200'
                  }`}
                >
                  {model}
                </div>
              ))
            )}
          </div>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <Radio className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Loaded Models</h2>
          </div>
          <div className="space-y-2 max-h-80 overflow-y-auto">
            {loadedModels.length === 0 ? (
              <p className="text-sm text-gray-500">No models are currently loaded in Ollama.</p>
            ) : (
              loadedModels.map((model) => (
                <div
                  key={model}
                  className="rounded-lg border border-gray-800 bg-gray-800/50 px-3 py-2 text-sm text-gray-200"
                >
                  {model}
                </div>
              ))
            )}
          </div>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-2 mb-4">
            <Activity className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Component Health</h2>
          </div>
          <div className="grid grid-cols-2 gap-3">
            {Object.entries(status.health.components).length === 0 ? (
              <p className="text-sm text-gray-500 col-span-2">No components reporting.</p>
            ) : (
              Object.entries(status.health.components).map(([name, comp]) => (
                <div
                  key={name}
                  className={`rounded-lg p-3 border ${healthBorder(comp.status)} bg-gray-800/50`}
                >
                  <div className="flex items-center gap-2 mb-1">
                    <span className={`inline-block h-2 w-2 rounded-full ${healthColor(comp.status)}`} />
                    <span className="text-sm font-medium text-white capitalize truncate">
                      {name}
                    </span>
                  </div>
                  <p className="text-xs text-gray-400 capitalize">{comp.status}</p>
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
