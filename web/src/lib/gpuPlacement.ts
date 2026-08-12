import type { OllamaGpuDevice, OllamaModelPlacement } from '@/types/api';

export const GPU_LAYER_PRESETS: ReadonlyArray<{
  label: string;
  value: number | null;
}> = [
  { label: 'All GPU', value: 999 },
  { label: 'Auto', value: null },
  { label: 'CPU only', value: 0 },
];

export function canonicalOllamaModel(model: string): string {
  const normalized = model.trim().toLowerCase();
  return normalized.includes(':') ? normalized : `${normalized}:latest`;
}

export function placementWorkerId(
  placements: OllamaModelPlacement[],
  model: string,
): string {
  const modelKey = canonicalOllamaModel(model);
  return placements.find((placement) => canonicalOllamaModel(placement.model) === modelKey)
    ?.worker_id ?? '';
}

export function withModelPlacement(
  placements: OllamaModelPlacement[],
  model: string,
  workerId: string,
): OllamaModelPlacement[] {
  const modelKey = canonicalOllamaModel(model);
  const retained = placements.filter(
    (placement) => canonicalOllamaModel(placement.model) !== modelKey,
  );
  return workerId ? [...retained, { model: model.trim(), worker_id: workerId }] : retained;
}

export function primaryRuntimeLabel(gpus: OllamaGpuDevice[]): string {
  if (gpus.length === 0) return 'Primary / automatic';
  const [gpu] = gpus;
  if (gpu && gpus.length === 1) {
    return `Primary · ${gpu.name} · ${Math.round(gpu.memory_total_mb / 1024)} GB`;
  }
  return `Primary · ${gpus.length} visible GPUs`;
}
