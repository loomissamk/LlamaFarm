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

export interface WorkspaceBrowserEntry {
  name: string;
  path: string;
  kind: 'file' | 'directory';
  size_bytes?: number;
  modified_at?: string;
}

export interface WorkspaceBrowserResponse {
  root_path: string;
  current_path: string;
  parent_path?: string;
  entries: WorkspaceBrowserEntry[];
}

export interface WorkspaceBlobWriteResponse {
  status: 'ok';
  path: string;
  size_bytes: number;
}

export interface WorkspacePathDeleteResponse {
  status: 'ok';
  path: string;
  kind: 'file' | 'directory';
}

export interface WorkspacePathCreateResponse {
  status: 'ok';
  path: string;
  kind: 'directory';
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

export type FederationRole = 'master' | 'worker' | 'both' | 'disabled';
export type FederationDiscoveryMode = 'mdns' | 'manual';

export interface FederationToolCapability {
  name: string;
  description: string;
  local_only: boolean;
}

export interface FederationLocalNodeSummary {
  node_id: string;
  display_name: string;
  api_port: number;
  role: FederationRole;
  allow_remote_subagents: boolean;
  discovery_mode: FederationDiscoveryMode;
  service_name: string;
  gateway_host: string;
}

export interface FederationPeerSummary {
  peer_id: string;
  node_id: string;
  display_name: string;
  delegate_agent: string;
  source: 'mdns' | 'manual';
  base_url: string;
  host: string;
  api_port: number;
  app_version: string;
  role_support: FederationRole;
  assigned_role: FederationRole;
  allow_remote_subagents: boolean;
  online: boolean;
  health: string;
  model_summary: string;
  tool_summary: string;
  installed_models: string[];
  tools: FederationToolCapability[];
  last_seen?: string | null;
  specialization: string;
  priority: number;
}

export interface FederationPeersResponse {
  enabled: boolean;
  local_node: FederationLocalNodeSummary;
  peers: FederationPeerSummary[];
}

export interface FederationPeerRoleUpdateResponse {
  status: 'ok';
  peer: FederationPeerSummary;
}

export interface WsMessage {
  type:
    | 'message'
    | 'chunk'
    | 'tool_call'
    | 'tool_result'
    | 'done'
    | 'followup_queued'
    | 'cancelling'
    | 'cancelled'
    | 'error'
    | 'federation_status'
    | 'federation_chunk'
    | 'federation_tool_call'
    | 'federation_tool_result'
    | 'federation_done'
    | 'federation_error';
  session_id?: string;
  content?: string;
  full_response?: string;
  name?: string;
  args?: any;
  success?: boolean;
  duration_secs?: number;
  output?: string;
  message?: string;
  peer_id?: string;
  peer_name?: string;
  delegate_agent?: string;
  task_id?: string;
}

// ── Database Explorer ─────────────────────────────────────────────────────────

export interface DbConnection {
  name: string;
  driver: 'sqlite' | 'postgres' | 'mysql' | 'mongodb';
  uri: string;
  database: string | null;
  read_only: boolean;
  max_rows: number;
  label: string;
}

export interface DbTestResult {
  ok: boolean;
  driver?: string;
  database?: string | null;
  tables?: string[];
  error?: string;
}

export interface DbColumnInfo {
  name: string;
  data_type: string;
}

export interface DbTableInfo {
  name: string;
  columns: DbColumnInfo[];
  kind: 'table' | 'view' | 'collection';
}

export interface DbSchema {
  driver: string;
  database: string | null;
  tables: DbTableInfo[];
}

export interface DbQueryResult {
  columns: string[];
  rows: any[][];
  row_count: number;
  truncated: boolean;
}

export interface DbConnectionsResponse {
  connections: DbConnection[];
}
