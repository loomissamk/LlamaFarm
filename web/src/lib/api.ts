import type {
  ConfigPresetsResponse,
  StatusResponse,
  ToolSpec,
  CronJob,
  Integration,
  IntegrationSettingsPayload,
  DiagResult,
  MemoryEntry,
  CostSummary,
  CliTool,
  HealthSnapshot,
  WorkspaceBlobWriteResponse,
  WorkspaceBrowserResponse,
  WorkspaceFileResponse,
  WorkspacePathCreateResponse,
  WorkspacePathDeleteResponse,
} from '../types/api';
import { clearToken, getToken, setToken } from './auth';

// ---------------------------------------------------------------------------
// Base fetch wrapper
// ---------------------------------------------------------------------------

export class UnauthorizedError extends Error {
  constructor() {
    super('Unauthorized');
    this.name = 'UnauthorizedError';
  }
}

export async function apiFetch<T = unknown>(
  path: string,
  options: RequestInit = {},
): Promise<T> {
  const token = getToken();
  const headers = new Headers(options.headers);

  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }

  if (
    options.body &&
    typeof options.body === 'string' &&
    !headers.has('Content-Type')
  ) {
    headers.set('Content-Type', 'application/json');
  }

  const response = await fetch(path, { ...options, headers });

  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event('llamafarm-unauthorized'));
    throw new UnauthorizedError();
  }

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }

  // Some endpoints may return 204 No Content
  if (response.status === 204) {
    return undefined as unknown as T;
  }

  const contentType = response.headers.get('content-type')?.toLowerCase() ?? '';
  if (!contentType.includes('application/json')) {
    const text = await response.text().catch(() => '');
    const preview = text.trim().slice(0, 120);
    throw new Error(
      `API ${response.status}: expected JSON response, got ${contentType || 'unknown content type'}${preview ? ` (${preview})` : ''}`,
    );
  }

  return response.json() as Promise<T>;
}

function unwrapField<T>(value: T | Record<string, T>, key: string): T {
  if (value !== null && typeof value === 'object' && !Array.isArray(value) && key in value) {
    const unwrapped = (value as Record<string, T | undefined>)[key];
    if (unwrapped !== undefined) {
      return unwrapped;
    }
  }
  return value as T;
}

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

export async function pair(code: string): Promise<{ token: string }> {
  const response = await fetch('/pair', {
    method: 'POST',
    headers: { 'X-Pairing-Code': code },
  });

  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`Pairing failed (${response.status}): ${text || response.statusText}`);
  }

  const data = (await response.json()) as { token: string };
  setToken(data.token);
  return data;
}

// ---------------------------------------------------------------------------
// Public health (no auth required)
// ---------------------------------------------------------------------------

export async function getPublicHealth(): Promise<{ require_pairing: boolean; paired: boolean }> {
  const response = await fetch('/health');
  if (!response.ok) {
    throw new Error(`Health check failed (${response.status})`);
  }
  return response.json() as Promise<{ require_pairing: boolean; paired: boolean }>;
}

// ---------------------------------------------------------------------------
// Status / Health
// ---------------------------------------------------------------------------

export function getStatus(): Promise<StatusResponse> {
  return apiFetch<StatusResponse>('/api/status');
}

export function getHealth(): Promise<HealthSnapshot> {
  return apiFetch<HealthSnapshot | { health: HealthSnapshot }>('/api/health').then((data) =>
    unwrapField(data, 'health'),
  );
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

export function getConfig(): Promise<string> {
  return apiFetch<string | { format?: string; content: string }>('/api/config').then((data) =>
    typeof data === 'string' ? data : data.content,
  );
}

export function putConfig(toml: string): Promise<void> {
  return apiFetch<void>('/api/config', {
    method: 'PUT',
    headers: { 'Content-Type': 'application/toml' },
    body: toml,
  });
}

export function getConfigPresets(): Promise<ConfigPresetsResponse> {
  return apiFetch<ConfigPresetsResponse>('/api/config/presets');
}

export function getWorkspaceFile(name: string): Promise<WorkspaceFileResponse> {
  return apiFetch<WorkspaceFileResponse>(`/api/workspace-files/${encodeURIComponent(name)}`);
}

export function putWorkspaceFile(
  name: string,
  content: string,
): Promise<WorkspaceFileResponse> {
  return apiFetch<WorkspaceFileResponse>(
    `/api/workspace-files/${encodeURIComponent(name)}`,
    {
      method: 'PUT',
      body: JSON.stringify({ content }),
    },
  );
}

export function getWorkspaceBrowser(path = ''): Promise<WorkspaceBrowserResponse> {
  const query = path ? `?path=${encodeURIComponent(path)}` : '';
  return apiFetch<WorkspaceBrowserResponse>(`/api/workspace/browser${query}`);
}

export function uploadWorkspaceBlob(
  path: string,
  blob: Blob,
): Promise<WorkspaceBlobWriteResponse> {
  const token = getToken();
  const headers = new Headers();
  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }
  headers.set('Content-Type', blob.type || 'application/octet-stream');

  return fetch(`/api/workspace/blob?path=${encodeURIComponent(path)}`, {
    method: 'PUT',
    headers,
    body: blob,
  }).then(async (response) => {
    if (response.status === 401) {
      clearToken();
      window.dispatchEvent(new Event('llamafarm-unauthorized'));
      throw new UnauthorizedError();
    }
    if (!response.ok) {
      const text = await response.text().catch(() => '');
      throw new Error(`API ${response.status}: ${text || response.statusText}`);
    }
    return response.json() as Promise<WorkspaceBlobWriteResponse>;
  });
}

function parseDownloadFilename(contentDisposition: string | null, fallback: string): string {
  if (!contentDisposition) {
    return fallback;
  }

  const utf8Match = contentDisposition.match(/filename\*=UTF-8''([^;]+)/i);
  if (utf8Match?.[1]) {
    return decodeURIComponent(utf8Match[1]);
  }

  const plainMatch = contentDisposition.match(/filename=\"?([^\";]+)\"?/i);
  if (plainMatch?.[1]) {
    return plainMatch[1];
  }

  return fallback;
}

export async function downloadWorkspacePath(path: string): Promise<void> {
  const token = getToken();
  const headers = new Headers();
  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }

  const query = path ? `?path=${encodeURIComponent(path)}` : '';
  const response = await fetch(`/api/workspace/download${query}`, { headers });

  if (response.status === 401) {
    clearToken();
    window.dispatchEvent(new Event('llamafarm-unauthorized'));
    throw new UnauthorizedError();
  }
  if (!response.ok) {
    const text = await response.text().catch(() => '');
    throw new Error(`API ${response.status}: ${text || response.statusText}`);
  }

  const blob = await response.blob();
  const fallback = path.split('/').filter(Boolean).pop() || 'workspace-download';
  const filename = parseDownloadFilename(
    response.headers.get('content-disposition'),
    fallback,
  );
  const url = URL.createObjectURL(blob);
  const link = document.createElement('a');
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

export function deleteWorkspacePath(path: string): Promise<WorkspacePathDeleteResponse> {
  return apiFetch<WorkspacePathDeleteResponse>(
    `/api/workspace/path?path=${encodeURIComponent(path)}`,
    {
      method: 'DELETE',
    },
  );
}

export function createWorkspaceDirectory(path: string): Promise<WorkspacePathCreateResponse> {
  return apiFetch<WorkspacePathCreateResponse>(
    `/api/workspace/directory?path=${encodeURIComponent(path)}`,
    {
      method: 'PUT',
    },
  );
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

export function getTools(): Promise<ToolSpec[]> {
  return apiFetch<ToolSpec[] | { tools: ToolSpec[] }>('/api/tools').then((data) =>
    unwrapField(data, 'tools'),
  );
}

// ---------------------------------------------------------------------------
// Cron
// ---------------------------------------------------------------------------

export function getCronJobs(): Promise<CronJob[]> {
  return apiFetch<CronJob[] | { jobs: CronJob[] }>('/api/cron').then((data) =>
    unwrapField(data, 'jobs'),
  );
}

export function addCronJob(body: {
  name?: string;
  command: string;
  schedule_kind?: 'cron' | 'at' | 'every';
  schedule?: string;
  run_at?: string;
  every_ms?: number;
  enabled?: boolean;
}): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>('/api/cron', {
    method: 'POST',
    body: JSON.stringify(body),
  }).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}

export function updateCronJob(
  id: string,
  body: {
    name?: string;
    command?: string;
    schedule_kind?: 'cron' | 'at' | 'every';
    schedule?: string;
    run_at?: string;
    every_ms?: number;
    enabled?: boolean;
  },
): Promise<CronJob> {
  return apiFetch<CronJob | { status: string; job: CronJob }>(
    `/api/cron/${encodeURIComponent(id)}`,
    {
      method: 'PUT',
      body: JSON.stringify(body),
    },
  ).then((data) => (typeof (data as { job?: CronJob }).job === 'object' ? (data as { job: CronJob }).job : (data as CronJob)));
}

export function runCronJob(id: string): Promise<{ status: string; output: string; job?: CronJob | null }> {
  return apiFetch<{ status: string; output: string; job?: CronJob | null }>(
    `/api/cron/${encodeURIComponent(id)}/run`,
    {
      method: 'POST',
      body: JSON.stringify({}),
    },
  );
}

export function deleteCronJob(id: string): Promise<void> {
  return apiFetch<void>(`/api/cron/${encodeURIComponent(id)}`, {
    method: 'DELETE',
  });
}

// ---------------------------------------------------------------------------
// Integrations
// ---------------------------------------------------------------------------

export function getIntegrations(): Promise<Integration[]> {
  return apiFetch<Integration[] | { integrations: Integration[] }>('/api/integrations').then(
    (data) => unwrapField(data, 'integrations'),
  );
}

export function getIntegrationSettings(): Promise<IntegrationSettingsPayload> {
  return apiFetch<IntegrationSettingsPayload>('/api/integrations/settings');
}

export function putIntegrationCredentials(
  integrationId: string,
  body: { revision?: string; fields: Record<string, string> },
): Promise<{ status: string; revision: string; unchanged?: boolean }> {
  return apiFetch<{ status: string; revision: string; unchanged?: boolean }>(
    `/api/integrations/${encodeURIComponent(integrationId)}/credentials`,
    {
      method: 'PUT',
      body: JSON.stringify(body),
    },
  );
}

// ---------------------------------------------------------------------------
// Doctor / Diagnostics
// ---------------------------------------------------------------------------

export function runDoctor(): Promise<DiagResult[]> {
  return apiFetch<DiagResult[] | { results: DiagResult[]; summary?: unknown }>('/api/doctor', {
    method: 'POST',
    body: JSON.stringify({}),
  }).then((data) => (Array.isArray(data) ? data : data.results));
}

// ---------------------------------------------------------------------------
// Memory
// ---------------------------------------------------------------------------

export function getMemory(
  query?: string,
  category?: string,
): Promise<MemoryEntry[]> {
  const params = new URLSearchParams();
  if (query) params.set('query', query);
  if (category) params.set('category', category);
  const qs = params.toString();
  return apiFetch<MemoryEntry[] | { entries: MemoryEntry[] }>(`/api/memory${qs ? `?${qs}` : ''}`).then(
    (data) => unwrapField(data, 'entries'),
  );
}

export function storeMemory(
  key: string,
  content: string,
  category?: string,
): Promise<void> {
  return apiFetch<unknown>('/api/memory', {
    method: 'POST',
    body: JSON.stringify({ key, content, category }),
  }).then(() => undefined);
}

export function deleteMemory(key: string): Promise<void> {
  return apiFetch<void>(`/api/memory/${encodeURIComponent(key)}`, {
    method: 'DELETE',
  });
}

export function clearMemory(scope: 'conversation' | 'all'): Promise<{ status: string; deleted: number }> {
  return apiFetch<{ status: string; deleted: number }>('/api/memory/clear', {
    method: 'POST',
    body: JSON.stringify({ scope }),
  });
}

// ---------------------------------------------------------------------------
// Cost
// ---------------------------------------------------------------------------

export function getCost(): Promise<CostSummary> {
  return apiFetch<CostSummary | { cost: CostSummary }>('/api/cost').then((data) =>
    unwrapField(data, 'cost'),
  );
}

// ---------------------------------------------------------------------------
// CLI Tools
// ---------------------------------------------------------------------------

export function getCliTools(): Promise<CliTool[]> {
  return apiFetch<CliTool[] | { cli_tools: CliTool[] }>('/api/cli-tools').then((data) =>
    unwrapField(data, 'cli_tools'),
  );
}
