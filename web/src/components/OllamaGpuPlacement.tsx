import { useEffect, useState } from 'react';
import { Cpu, Plus, Save, Trash2, Zap } from 'lucide-react';
import {
  getOllamaGpuPlacement,
  manageOllamaGpuWorker,
  putOllamaGpuPlacement,
  setOllamaModelResidency,
} from '@/lib/api';
import type { OllamaGpuPlacement, OllamaGpuWorker, OllamaModelPlacement } from '@/types/api';

function workerEndpoint(id: string) {
  return `http://llamafarm-ollama-worker-${id.trim().toLowerCase().replace(/_/g, '-')}:11434`;
}

export default function OllamaGpuPlacementPanel({ models }: { models: string[] }) {
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
    setWorkers([...workers, { id, label: `GPU pool ${number}`, endpoint: workerEndpoint(id), gpu_ids: [], spread: false, managed: true }]);
  };

  const updateWorker = (index: number, patch: Partial<OllamaGpuWorker>) => {
    setWorkers(workers.map((worker, current) => current === index ? { ...worker, ...patch } : worker));
  };

  const save = async () => {
    setBusy('save'); setMessage('');
    try {
      await putOllamaGpuPlacement({ workers, placements: placements.filter((item) => item.model && item.worker_id) });
      setMessage('GPU pools and model routes applied live.');
      await refresh();
    } catch (error) { setMessage(error instanceof Error ? error.message : 'Save failed'); }
    finally { setBusy(''); }
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

  return (
    <section className="rounded-xl border border-gray-800 bg-gray-900 p-5">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div><div className="flex items-center gap-2"><Cpu className="h-5 w-5 text-violet-400"/><h3 className="font-semibold text-white">GPU Placement</h3></div><p className="mt-2 max-w-3xl text-sm text-gray-400">Create an Ollama worker for one GPU or a GPU set, then route each model to it. A multi-GPU pool lets Ollama split a model; separate pools can keep different models resident simultaneously.</p></div>
        <button onClick={addWorker} className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-200 hover:bg-gray-800"><Plus className="h-4 w-4"/>Add pool</button>
      </div>
      <div className="mt-4 rounded-lg bg-gray-950 p-3 text-xs text-gray-400">Primary/default: {data?.primary_endpoint ?? 'loading…'} · Detected: {data?.gpus.length ?? 0} GPU(s)</div>
      <div className="mt-4 space-y-3">
        {workers.map((worker, index) => (
          <div key={worker.id} className="rounded-lg border border-gray-800 bg-gray-950 p-4">
            <div className="grid gap-3 lg:grid-cols-[1fr_1.4fr_auto]">
              <div><label className="text-xs text-gray-500">Pool ID</label><input value={worker.id} onChange={(e) => { const id=e.target.value; updateWorker(index,{id, endpoint: worker.managed ? workerEndpoint(id) : worker.endpoint}); }} className="mt-1 w-full rounded border border-gray-700 bg-gray-900 px-2 py-2 text-sm text-white"/></div>
              <div><label className="text-xs text-gray-500">Endpoint</label><input value={worker.endpoint} onChange={(e) => updateWorker(index,{endpoint:e.target.value})} disabled={worker.managed} className="mt-1 w-full rounded border border-gray-700 bg-gray-900 px-2 py-2 text-sm text-white disabled:text-gray-500"/></div>
              <div className="flex items-end gap-2"><button disabled={!!busy || !worker.managed} onClick={() => void workerAction(worker,'reconcile')} className="rounded bg-violet-700 px-3 py-2 text-sm text-white disabled:opacity-40">Start / reconcile</button>{worker.reachable && <button disabled={!!busy || !worker.managed} onClick={() => void workerAction(worker,'remove')} className="rounded border border-gray-700 px-3 py-2 text-sm text-gray-300 disabled:opacity-40">Stop</button>}<button title="Remove pool definition" disabled={!!busy} onClick={() => { setWorkers(workers.filter((_,i)=>i!==index)); setPlacements(placements.filter((p)=>p.worker_id!==worker.id)); }} className="rounded border border-gray-700 p-2 text-gray-300"><Trash2 className="h-4 w-4"/></button></div>
            </div>
            <div className="mt-3 flex flex-wrap gap-3">
              {data?.gpus.map((gpu) => <label key={gpu.uuid} className="flex items-center gap-2 rounded border border-gray-800 px-3 py-2 text-sm text-gray-300"><input type="checkbox" checked={worker.gpu_ids.includes(gpu.uuid)} onChange={(e)=>updateWorker(index,{gpu_ids:e.target.checked?[...worker.gpu_ids,gpu.uuid]:worker.gpu_ids.filter((id)=>id!==gpu.uuid)})}/><span>GPU {gpu.index}: {gpu.name} ({Math.round(gpu.memory_total_mb/1024)} GB)</span></label>)}
              <label className="flex items-center gap-2 text-sm text-gray-300"><input type="checkbox" checked={worker.spread} onChange={(e)=>updateWorker(index,{spread:e.target.checked})}/>Spread model across selected GPUs</label>
              <span className={worker.reachable ? 'text-sm text-emerald-400' : 'text-sm text-gray-500'}>{worker.reachable ? `Online · ${worker.loaded_models?.length ?? 0} loaded` : 'Not running'}</span>
            </div>
          </div>
        ))}
      </div>
      <div className="mt-5"><h4 className="text-sm font-medium text-white">Per-model routes and residency</h4><div className="mt-2 space-y-2">
        {models.map((model) => { const placement=placements.find((item)=>item.model===model); const workerId=placement?.worker_id ?? ''; return <div key={model} className="grid items-center gap-2 rounded-lg border border-gray-800 px-3 py-2 md:grid-cols-[1fr_1fr_auto]"><span className="break-all text-sm text-gray-200">{model}</span><select value={workerId} onChange={(e)=>setPlacements([...placements.filter((item)=>item.model!==model),...(e.target.value?[{model,worker_id:e.target.value}]:[])])} className="rounded border border-gray-700 bg-gray-950 px-2 py-2 text-sm text-gray-200"><option value="">Primary / automatic</option>{workers.map((worker)=><option key={worker.id} value={worker.id}>{worker.label || worker.id}: {worker.gpu_ids.length ? worker.gpu_ids.length+' GPU(s)' : 'select GPUs'}</option>)}</select><div className="flex gap-2"><button disabled={!!busy} onClick={()=>void residency(model,workerId,'load')} className="inline-flex items-center gap-1 rounded bg-emerald-700 px-2 py-2 text-xs text-white disabled:opacity-40"><Zap className="h-3 w-3"/>Load + pin</button><button disabled={!!busy} onClick={()=>void residency(model,workerId,'unload')} className="rounded border border-gray-700 px-2 py-2 text-xs text-gray-300 disabled:opacity-40">Unload</button></div></div>; })}
      </div></div>
      <div className="mt-4 flex items-center justify-between gap-3"><span className="text-sm text-gray-400">{message || data?.note}</span><button disabled={!!busy} onClick={()=>void save()} className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm text-white disabled:opacity-40"><Save className="h-4 w-4"/>{busy==='save'?'Saving…':'Save routes'}</button></div>
    </section>
  );
}
