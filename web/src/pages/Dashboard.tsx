import { DiagnosticsPanel } from './Doctor';
import { LogsPanel } from './Logs';
import { useEffect, useState } from 'react';
import { Activity, Cpu, Database, Radio, RefreshCw, Server } from 'lucide-react';
import type { StatusResponse } from '@/types/api';
import { getStatus } from '@/lib/api';

function formatUptime(seconds: number): string {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
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

export default function Dashboard() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  const fetchStatus = () => {
    setRefreshing(true);
    getStatus()
      .then((s) => { setStatus(s); setError(null); })
      .catch((err) => setError(err.message))
      .finally(() => setRefreshing(false));
  };

  useEffect(() => {
    fetchStatus();
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (error) {
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

  return (
    <div className="p-6 space-y-6">
      <div className="mb-4 flex flex-wrap gap-2 text-xs">
        {[['/runs','Runs'],['/memory','Memory'],['/logs','Logs'],['/doctor','Doctor']].map(([to,label]) => (
          <a key={to} href={to} className="rounded-full border border-gray-800 px-3 py-1 text-gray-400 hover:text-white hover:border-gray-600">{label}</a>
        ))}
      </div>
      <div className="flex items-center justify-between">
        <h2 className="text-base font-semibold text-white">Dashboard</h2>
        <button
          onClick={fetchStatus}
          disabled={refreshing}
          className="flex items-center gap-1.5 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
        >
          <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
          Refresh
        </button>
      </div>
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-blue-600/20 rounded-lg">
              <Cpu className="h-5 w-5 text-blue-400" />
            </div>
            <span className="text-sm text-gray-400">Current Model</span>
          </div>
          <p className="text-lg font-semibold text-white truncate">{status.model}</p>
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
      <div className="mt-6">
        <DiagnosticsPanel />
      </div>
      <div className="mt-6 overflow-hidden rounded-lg border border-gray-800 [&>div]:!h-[28rem]">
        <LogsPanel />
      </div>
    </div>
  );
}
