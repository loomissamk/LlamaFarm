import { useEffect, useState } from 'react';
import { Cpu, HardDrive, Server } from 'lucide-react';
import type { StatusResponse } from '@/types/api';
import { getStatus } from '@/lib/api';

export default function Models() {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getStatus()
      .then(setStatus)
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load Ollama model data: {error}
        </div>
      </div>
    );
  }

  if (loading || !status) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  const installed = status.ollama.installed_models;
  const loaded = new Set(status.ollama.loaded_models);

  return (
    <div className="p-6 space-y-6">
      <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-blue-600/20 rounded-lg">
              <Cpu className="h-5 w-5 text-blue-400" />
            </div>
            <span className="text-sm text-gray-400">Active Model</span>
          </div>
          <p className="text-lg font-semibold text-white break-all">{status.model}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-emerald-600/20 rounded-lg">
              <Server className="h-5 w-5 text-emerald-400" />
            </div>
            <span className="text-sm text-gray-400">Ollama Endpoint</span>
          </div>
          <p className="text-sm font-semibold text-white break-all">{status.ollama.endpoint}</p>
        </div>

        <div className="bg-gray-900 rounded-xl p-5 border border-gray-800">
          <div className="flex items-center gap-3 mb-3">
            <div className="p-2 bg-orange-600/20 rounded-lg">
              <HardDrive className="h-5 w-5 text-orange-400" />
            </div>
            <span className="text-sm text-gray-400">Loaded Status</span>
          </div>
          <p className="text-lg font-semibold text-white">
            {status.ollama.active_model_loaded ? 'Loaded' : 'Not loaded'}
          </p>
        </div>
      </div>

      <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
        <div className="px-5 py-4 border-b border-gray-800">
          <h3 className="text-base font-semibold text-white">Installed Ollama Models</h3>
        </div>
        {installed.length === 0 ? (
          <div className="p-8 text-center text-gray-500">
            No installed models reported by the configured Ollama endpoint.
          </div>
        ) : (
          <div className="divide-y divide-gray-800">
            {installed.map((model) => (
              <div
                key={model}
                className="flex items-center justify-between px-5 py-3 text-sm"
              >
                <span className="text-white break-all">{model}</span>
                <span
                  className={`inline-flex items-center rounded-full px-2.5 py-1 text-xs font-medium ${
                    loaded.has(model)
                      ? 'bg-emerald-900/40 text-emerald-300'
                      : 'bg-gray-800 text-gray-400'
                  }`}
                >
                  {loaded.has(model) ? 'Loaded' : 'Installed'}
                </span>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
