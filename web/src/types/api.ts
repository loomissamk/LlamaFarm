export interface StatusResponse {
  provider: string | null;
  model: string;
  temperature: number;
  uptime_seconds: number;
  gateway_port: number;
  locale: string;
  memory_backend: string;
  shell: RuntimeShell;
  channels: Record<string, boolean>;
  health: HealthSnapshot;
  ollama: OllamaStatus;
}

export interface RuntimeShell {
  path: string;
  name: string;
  available: boolean;
}

export interface OllamaStatus {
  endpoint: string;
  reachable: boolean;
  installed_models: string[];
  loaded_models: string[];
  active_model_loaded: boolean;
}

export interface HealthSnapshot {
  pid: number;
  updated_at: string;
  uptime_seconds: number;
  components: Record<string, ComponentHealth>;
}

export interface ComponentHealth {
  status: string;
  updated_at: string;
  last_ok: string | null;
  last_error: string | null;
  restart_count: number;
}

export interface ToolSpec {
  name: string;
  description: string;
  parameters: any;
}

export interface CronJob {
  id: string;
  name: string | null;
  command: string;
  expression: string;
  schedule: {
    kind: 'cron' | 'at' | 'every';
    expr?: string;
    at?: string;
    every_ms?: number;
  };
  next_run: string;
  last_run: string | null;
  last_status: string | null;
  last_output: string | null;
  enabled: boolean;
}

export interface Integration {
  name: string;
  description: string;
  category: string;
  status: 'Available' | 'Active' | 'ComingSoon';
}

export interface IntegrationCredentialsField {
  key: string;
  label: string;
  required: boolean;
  has_value: boolean;
  input_type: 'secret' | 'text' | 'select';
  options: string[];
  current_value?: string;
  masked_value?: string;
}

export interface IntegrationSettingsEntry {
  id: string;
  name: string;
  description: string;
  category: string;
  status: Integration['status'];
  configured: boolean;
  activates_default_provider: boolean;
  fields: IntegrationCredentialsField[];
}

export interface IntegrationSettingsPayload {
  revision: string;
  active_default_provider_integration_id?: string;
  integrations: IntegrationSettingsEntry[];
}

export interface DiagResult {
  severity: 'ok' | 'warn' | 'error';
  category: string;
  message: string;
}

export interface MemoryEntry {
  id: string;
  key: string;
  content: string;
  category: string;
  timestamp: string;
  session_id: string | null;
  score: number | null;
}

export interface CostSummary {
  session_cost_usd: number;
  daily_cost_usd: number;
  monthly_cost_usd: number;
  total_tokens: number;
  request_count: number;
  by_model: Record<string, ModelStats>;
}

export interface ModelStats {
  model: string;
  cost_usd: number;
  total_tokens: number;
  request_count: number;
}

export interface CliTool {
  name: string;
  path: string;
  version: string | null;
  category: string;
}

export interface CliToolProbeResponse {
  found: boolean;
  name: string;
  tool: CliTool | null;
}

export interface WorkspaceFileResponse {
  name: string;
  content: string;
  exists: boolean;
}

export interface ConfigPresetWorkspaceFile {
  name: string;
  content: string;
}

export interface ConfigPresetEntry {
  id: 'safe' | 'god';
  label: string;
  summary: string;
  highlights: string[];
  content: string;
  workspace_files: ConfigPresetWorkspaceFile[];
}

export interface ConfigPresetsResponse {
  safe: ConfigPresetEntry;
  god: ConfigPresetEntry;
}

export interface SSEEvent {
  type: string;
  timestamp?: string;
  [key: string]: any;
}

export interface RuntimeLogEntry {
  id: number;
  timestamp: string;
  line: string;
}

export interface RuntimeLogsResponse {
  entries: RuntimeLogEntry[];
}

export interface WsMessage {
  type: 'message' | 'chunk' | 'tool_call' | 'tool_result' | 'done' | 'error';
  session_id?: string;
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  success?: boolean;
  duration_secs?: number;
  output?: string;
  message?: string;
}
