export type ContextSource = 'config' | 'environment' | 'model-native';

export interface AdaptiveContextInfo {
  enabled: boolean;
  active?: boolean;
  baseline: number | null;
  max: number | null;
}

export interface ContextPolicyInfo {
  num_ctx: number | null;
  effective_default_num_ctx?: number | null;
  source?: ContextSource;
  adaptive?: AdaptiveContextInfo;
}

export interface ContextPreset {
  label: string;
  value: number;
}

export const CONTEXT_PRESETS: ContextPreset[] = [
  { label: 'Auto', value: 0 },
  { label: '64K', value: 65_536 },
  { label: '128K', value: 131_072 },
  { label: '256K', value: 262_144 },
];

export function formatContextTokens(value: number): string {
  if (value >= 1024 && value % 1024 === 0) {
    return `${value / 1024}K`;
  }
  return value.toLocaleString();
}

export function contextSourceLabel(source: ContextSource | undefined): string {
  switch (source) {
    case 'config':
      return 'Saved override';
    case 'environment':
      return 'Node profile';
    case 'model-native':
    default:
      return 'Model native';
  }
}

export function isAdaptiveContextActive(info: ContextPolicyInfo): boolean {
  return info.adaptive?.active ?? info.adaptive?.enabled ?? false;
}

export function hasAdaptiveContextPolicy(info: ContextPolicyInfo): boolean {
  return Boolean(info.adaptive?.enabled && info.adaptive.baseline && info.adaptive.max);
}

export function automaticContextLabel(info: ContextPolicyInfo): string {
  const adaptive = info.adaptive;
  if (hasAdaptiveContextPolicy(info) && adaptive?.baseline && adaptive.max) {
    return `Adaptive ${formatContextTokens(adaptive.baseline)} → ${formatContextTokens(adaptive.max)}`;
  }

  if (info.effective_default_num_ctx) {
    return `${contextSourceLabel(info.source)} · ${formatContextTokens(info.effective_default_num_ctx)}`;
  }

  return 'Model native';
}

export function contextDraftLabel(info: ContextPolicyInfo, value: number): string {
  return value === 0 ? automaticContextLabel(info) : `${formatContextTokens(value)} tokens`;
}
