import { useEffect, useState } from 'react';
import { Cpu, Plus, Save, Trash2, Zap } from 'lucide-react';
import {
  getOllamaGpuPlacement,
  manageOllamaGpuWorker,
  putOllamaGpuPlacement,
  setOllamaModelResidency,
} from '@/lib/api';
import {
  canonicalOllamaModel,
  placementWorkerId,
  primaryRuntimeLabel,
  withModelPlacement,
} from '@/lib/gpuPlacement';
import type { OllamaGpuPlacement, OllamaGpuWorker, OllamaModelPlacement } from '@/types/api';

function workerEndpoint(id: string) {
  return `http://llamafarm-ollama-worker-${id.trim().toLowerCase().replace(/_/g, '-')}:11434`;
}

interface OllamaGpuPlacementPanelProps {
  models: string[];
  currentModel: string;
}

export default function OllamaGpuPlacementPanel({
  models,
  currentModel,
}: OllamaGpuPlacementPanelProps) {
  const [data, setData] = useState<OllamaGpuPlacement | null>(null);
  const [workers, setWorkers] = useState<OllamaGpuWorker[]>([]);
  const [placements, setPlacements] = useState<OllamaModelPlacement[]>([]);
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState('');

  const refresh = async () => {
    const next = await getOllamaGpuPlacement();
    setData(next);
    setWorkers(next.workers);
    setPlacements(next.placements);
  };

  useEffect(() => {
    void refresh().catch((error: unknown) => setMessage(error instanceof Error ? error.message : 'GPU placement unavailable'));
  }, []);

  const addWorker = () => {
    let number = workers.length + 1;
    while (workers.some((worker) => worker.id === `gpu${number}`)) number += 1;
    const id = `gpu${number}`;
    const managed = data?.managed_workers_supported !== false;
    setWorkers([
      ...workers,
      {
        id,
        label: managed ? `GPU pool ${number}` : `External runtime ${number}`,
        endpoint: managed ? workerEndpoint(id) : '',
        gpu_ids: [],
        spread: false,
        managed,
      },
    ]);
  };

  const updateWorker = (index: number, patch: Partial<OllamaGpuWorker>) => {
    setWorkers(workers.map((worker, current) => current === index ? { ...worker, ...patch } : worker));
  };

  const renameWorker = (index: number, id: string) => {
    const previousId = workers[index]?.id;
    const worker = workers[index];
    if (!previousId || !worker) return;
    updateWorker(index, {
      id,
      endpoint: worker.managed ? workerEndpoint(id) : worker.endpoint,
    });
    setPlacements(placements.map((placement) => (
      placement.worker_id === previousId ? { ...placement, worker_id: id } : placement
    )));
  };

  const save = async () => {
    setBusy('save'); setMessage('');
    try {
      await putOllamaGpuPlacement({ workers, placements: placements.filter((item) => item.model && item.worker_id) });
      setMessage('Runtime definitions and model routes applied live.');
      await refresh();
    } catch (error) { setMessage(error instanceof Error ? error.message : 'Save failed'); }
    finally { setBusy(''); }
  };

  const savePreference = async (workerId: string) => {
    if (!currentModel || !data) return;
    setBusy(`preference:${currentModel}`); setMessage('');
    const nextPlacements = withModelPlacement(data.placements, currentModel, workerId);
    try {
      await putOllamaGpuPlacement({ workers: data.workers, placements: nextPlacements });
      const selectedWorker = data.workers.find((worker) => worker.id === workerId);
      setMessage(
        `${currentModel} now prefers ${selectedWorker?.label || selectedWorker?.id || primaryRuntimeLabel(data.gpus)}.`,
      );
      await refresh();
    } catch (error) {
      setMessage(error instanceof Error ? error.message : 'Runtime preference could not be saved');
    } finally {
      setBusy('');
    }
  };

  const workerAction = async (worker: OllamaGpuWorker, action: 'reconcile' | 'remove') => {
    setBusy(`${action}:${worker.id}`); setMessage('');
    try { await manageOllamaGpuWorker(worker.id, action); setMessage(`${worker.label || worker.id} ${action === 'remove' ? 'removed' : 'started'}.`); await refresh(); }
    catch (error) { setMessage(error instanceof Error ? error.message : 'Worker action failed'); }
    finally { setBusy(''); }
  };

  const residency = async (model: string, workerId: string, action: 'load' | 'unload') => {
    setBusy(`${action}:${model}`); setMessage('');
    try { await setOllamaModelResidency(model, workerId || null, action); setMessage(`${model} ${action === 'load' ? 'loaded and pinned' : 'unloaded'}.`); await refresh(); }
    catch (error) { setMessage(error instanceof Error ? error.message : 'Model action failed'); }
    finally { setBusy(''); }
  };

  const primaryLabel = primaryRuntimeLabel(data?.gpus ?? []);
  const currentWorkerId = placementWorkerId(data?.placements ?? placements, currentModel);
  const missingCurrentWorker = currentWorkerId
    && !workers.some((worker) => worker.id === currentWorkerId);
  const routableModels = currentModel
    && !models.some((model) => canonicalOllamaModel(model) === canonicalOllamaModel(currentModel))
    ? [currentModel, ...models]
    : models;

  return (
    <section className="rounded-xl border border-gray-800 bg-gray-900 p-5">
      <div className="flex items-center gap-2">
        <Cpu className="h-5 w-5 text-violet-400" />
        <h3 className="font-semibold text-white">Runtime preference</h3>
      </div>
      <p className="mt-2 text-sm text-gray-400">
        Choose where the effective model runs. Primary stays the default until you select a configured runtime.
      </p>
      <div className="mt-4 grid gap-2 md:grid-cols-[minmax(0,1fr)_minmax(15rem,1fr)] md:items-center">
        <div className="min-w-0">
          <p className="text-xs uppercase tracking-wide text-gray-500">Effective model</p>
          <p className="mt-1 truncate text-sm font-medium text-white">{currentModel || 'No model selected'}</p>
        </div>
        <div>
          <label htmlFor="ollama-preferred-runtime" className="sr-only">Preferred runtime</label>
          <select
            id="ollama-preferred-runtime"
            value={currentWorkerId}
            onChange={(event) => void savePreference(event.target.value)}
            disabled={!data || !currentModel || !!busy}
            className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 disabled:opacity-50"
          >
            <option value="">{primaryLabel}</option>
            {missingCurrentWorker && (
              <option value={currentWorkerId}>Unavailable runtime · {currentWorkerId}</option>
            )}
            {workers.map((worker) => (
              <option key={worker.id} value={worker.id}>
                {worker.label || worker.id}{worker.reachable ? ' · online' : ' · offline'}
              </option>
            ))}
          </select>
        </div>
      </div>
      <p className="mt-3 text-xs text-gray-500">
        Primary endpoint: {data?.primary_endpoint ?? 'loading…'}
      </p>

      <details className="group mt-5 border-t border-gray-800 pt-4">
        <summary className="cursor-pointer select-none text-sm font-medium text-gray-300 hover:text-white">
          Advanced runtime pools and per-model routes
          <span className="ml-2 text-xs font-normal text-gray-500">
            {workers.length} configured
          </span>
        </summary>
        <div className="mt-4">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <p className="max-w-3xl text-sm text-gray-400">
              A managed runtime binds a local GPU set. An external runtime can point to another Ollama-compatible endpoint.
            </p>
            <button type="button" onClick={addWorker} className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-200 hover:bg-gray-800">
              <Plus className="h-4 w-4" />Add runtime
            </button>
          </div>
          <div className="mt-4 space-y-3">
            {workers.map((worker, index) => (
              <div key={worker.id} className="rounded-lg border border-gray-800 bg-gray-950 p-4">
                <div className="grid gap-3 lg:grid-cols-[0.8fr_1fr_1.4fr_auto]">
                  <div>
                    <label htmlFor={`ollama-worker-id-${index}`} className="text-xs text-gray-500">Runtime ID</label>
                    <input id={`ollama-worker-id-${index}`} value={worker.id} onChange={(event) => renameWorker(index, event.target.value)} className="mt-1 w-full rounded border border-gray-700 bg-gray-900 px-2 py-2 text-sm text-white" />
                  </div>
                  <div>
                    <label htmlFor={`ollama-worker-label-${index}`} className="text-xs text-gray-500">Label</label>
                    <input id={`ollama-worker-label-${index}`} value={worker.label ?? ''} onChange={(event) => updateWorker(index, { label: event.target.value || null })} placeholder="Dedicated GPU runtime" className="mt-1 w-full rounded border border-gray-700 bg-gray-900 px-2 py-2 text-sm text-white" />
                  </div>
                  <div>
                    <label htmlFor={`ollama-worker-endpoint-${index}`} className="text-xs text-gray-500">Endpoint</label>
                    <input id={`ollama-worker-endpoint-${index}`} value={worker.endpoint} onChange={(event) => updateWorker(index, { endpoint: event.target.value })} disabled={worker.managed} placeholder="http://ollama-worker:11434" className="mt-1 w-full rounded border border-gray-700 bg-gray-900 px-2 py-2 text-sm text-white disabled:text-gray-500" />
                  </div>
                  <div className="flex items-end gap-2">
                    {worker.managed && (
                      <button type="button" disabled={!!busy} onClick={() => void workerAction(worker, 'reconcile')} className="rounded bg-violet-700 px-3 py-2 text-sm text-white disabled:opacity-40">Start / reconcile</button>
                    )}
                    {worker.reachable && worker.managed && (
                      <button type="button" disabled={!!busy} onClick={() => void workerAction(worker, 'remove')} className="rounded border border-gray-700 px-3 py-2 text-sm text-gray-300 disabled:opacity-40">Stop</button>
                    )}
                    <button type="button" title="Remove runtime definition" disabled={!!busy} onClick={() => { setWorkers(workers.filter((_, current) => current !== index)); setPlacements(placements.filter((placement) => placement.worker_id !== worker.id)); }} className="rounded border border-gray-700 p-2 text-gray-300 disabled:opacity-40"><Trash2 className="h-4 w-4" /></button>
                  </div>
                </div>
                <div className="mt-3 flex flex-wrap items-center gap-3">
                  <label className="text-xs text-gray-500" htmlFor={`ollama-worker-kind-${index}`}>Runtime type</label>
                  <select
                    id={`ollama-worker-kind-${index}`}
                    value={worker.managed ? 'managed' : 'external'}
                    onChange={(event) => {
                      const managed = event.target.value === 'managed';
                      updateWorker(index, managed
                        ? { managed: true, endpoint: workerEndpoint(worker.id) }
                        : { managed: false, endpoint: '', gpu_ids: [], spread: false });
                    }}
                    className="rounded border border-gray-700 bg-gray-900 px-2 py-1.5 text-sm text-gray-200"
                  >
                    <option value="managed" disabled={!data?.managed_workers_supported}>Managed local GPUs</option>
                    <option value="external">External Ollama-compatible runtime</option>
                  </select>
                  {worker.managed ? (
                    <>
                      {data?.gpus.map((gpu) => (
                        <label key={gpu.uuid} className="flex items-center gap-2 rounded border border-gray-800 px-3 py-2 text-sm text-gray-300">
                          <input type="checkbox" checked={worker.gpu_ids.includes(gpu.uuid)} onChange={(event) => updateWorker(index, { gpu_ids: event.target.checked ? [...worker.gpu_ids, gpu.uuid] : worker.gpu_ids.filter((id) => id !== gpu.uuid) })} />
                          <span>GPU {gpu.index}: {gpu.name} ({Math.round(gpu.memory_total_mb / 1024)} GB)</span>
                        </label>
                      ))}
                      <label className="flex items-center gap-2 text-sm text-gray-300">
                        <input type="checkbox" checked={worker.spread} onChange={(event) => updateWorker(index, { spread: event.target.checked })} />
                        Spread across selected GPUs
                      </label>
                    </>
                  ) : (
                    <span className="text-xs text-gray-500">The external Ollama-compatible service owns its GPU allocation.</span>
                  )}
                  <span className={worker.reachable ? 'text-sm text-emerald-400' : 'text-sm text-gray-500'}>
                    {worker.reachable ? `Online · ${worker.loaded_models?.length ?? 0} loaded` : 'Offline'}
                  </span>
                </div>
              </div>
            ))}
          </div>

          <div className="mt-5">
            <h4 className="text-sm font-medium text-white">Per-model routes and residency</h4>
            <div className="mt-2 space-y-2">
              {routableModels.map((model) => {
                const workerId = placementWorkerId(placements, model);
                return (
                  <div key={model} className="grid items-center gap-2 rounded-lg border border-gray-800 px-3 py-2 md:grid-cols-[1fr_1fr_auto]">
                    <span className="break-all text-sm text-gray-200">{model}</span>
                    <select value={workerId} onChange={(event) => setPlacements(withModelPlacement(placements, model, event.target.value))} className="rounded border border-gray-700 bg-gray-950 px-2 py-2 text-sm text-gray-200">
                      <option value="">{primaryLabel}</option>
                      {workers.map((worker) => <option key={worker.id} value={worker.id}>{worker.label || worker.id}</option>)}
                    </select>
                    <div className="flex gap-2">
                      <button type="button" disabled={!!busy} onClick={() => void residency(model, workerId, 'load')} className="inline-flex items-center gap-1 rounded bg-emerald-700 px-2 py-2 text-xs text-white disabled:opacity-40"><Zap className="h-3 w-3" />Load + pin</button>
                      <button type="button" disabled={!!busy} onClick={() => void residency(model, workerId, 'unload')} className="rounded border border-gray-700 px-2 py-2 text-xs text-gray-300 disabled:opacity-40">Unload</button>
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
          <div className="mt-4 flex items-center justify-end">
            <button type="button" disabled={!!busy} onClick={() => void save()} className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm text-white disabled:opacity-40"><Save className="h-4 w-4" />{busy === 'save' ? 'Saving…' : 'Save advanced settings'}</button>
          </div>
        </div>
      </details>
      <p aria-live="polite" className="mt-4 text-sm text-gray-400">{message || data?.note}</p>
    </section>
  );
}
