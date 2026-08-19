export interface StatusResponse {
  version?: string;
  app_version?: string;
  build_commit?: string | null;
  build_time?: string | null;
  build?: {
    commit: string | null;
    time: string | null;
  };
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
  runtime?: RuntimeFacts;
  capacity?: FederationNodeCapacity;
  queue?: {
    active_runs: number;
    queued_runs: number | null;
    queue_depth_available: boolean;
  };
}

export interface RuntimeFacts {
  provider: string;
  model: string;
  temperature: number;
  memory_backend: string;
  max_tool_iterations: number;
  tool_count: number;
  config_revision: string;
  gateway: {
    host: string;
    port: number;
    configured_host: string;
    configured_port: number;
    restart_required: boolean;
  };
  federation: {
    configured_enabled: boolean;
    effective_enabled: boolean;
    delegation_enabled: boolean;
  };
}

export interface RuntimeShell {
  path: string;
  name: string;
  available: boolean;
}

export interface OllamaStatus {
  endpoint: string;
  reachable: boolean;
  configured_model: string;
  installed_models: string[];
  loaded_models: string[];
  installed_model_details?: OllamaModelDetail[];
  loaded_model_details?: OllamaModelDetail[];
  active_model_loaded: boolean;
  revision: string;
  model_environment_override: string | null;
}

export interface OllamaModelDetail {
  name: string;
  size_bytes?: number;
  size_vram_bytes?: number;
  context_length?: number | null;
  expires_at?: string | null;
}

export interface OllamaGpuDevice {
  index: number;
  uuid: string;
  name: string;
  memory_total_mb: number;
}

export interface OllamaGpuWorker {
  id: string;
  endpoint: string;
  label?: string | null;
  gpu_ids: string[];
  spread: boolean;
  managed: boolean;
  reachable?: boolean;
  loaded_models?: string[];
  container_name?: string | null;
}

export interface OllamaModelPlacement {
  model: string;
  worker_id: string;
}

export interface OllamaGpuPlacement {
  primary_endpoint: string;
  gpus: OllamaGpuDevice[];
  workers: OllamaGpuWorker[];
  placements: OllamaModelPlacement[];
  managed_workers_supported: boolean;
  note: string;
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
    tz?: string;
    at?: string;
    every_ms?: number;
  };
  next_run: string;
  last_run: string | null;
  last_status: string | null;
  last_output: string | null;
  enabled: boolean;
}

export interface CronRun {
  id: number;
  job_id: string;
  started_at: string;
  finished_at: string;
  status: string;
  output: string | null;
  duration_ms: number | null;
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

export interface FederationLoadedModel {
  name: string;
  size_bytes: number;
  size_vram_bytes: number;
  context_length?: number | null;
  expires_at?: string | null;
}

export interface FederationNodeCapacity {
  logical_cpus: number;
  total_memory_bytes?: number | null;
  memory_limit_bytes?: number | null;
  memory_current_bytes?: number | null;
  memory_available_bytes?: number | null;
  gpu_total_memory_bytes?: number | null;
  gpu_used_memory_bytes?: number | null;
  gpu_free_memory_bytes?: number | null;
  loaded_model_vram_bytes: number;
  active_runs: number;
  queued_runs?: number | null;
}

export interface FederationRuntimeFacts {
  gateway_host: string;
  gateway_port: number;
  configured_gateway_host: string;
  configured_gateway_port: number;
  gateway_restart_required: boolean;
  federation_configured: boolean;
  federation_effective: boolean;
  delegation_enabled: boolean;
  max_tool_iterations: number;
  config_revision: string;
}

export interface FederationCapabilities {
  node_id: string;
  display_name: string;
  app_version: string;
  build_commit?: string | null;
  build_time?: string | null;
  provider?: string | null;
  model: string;
  installed_models: string[];
  loaded_models: FederationLoadedModel[];
  active_model_loaded: boolean;
  capacity: FederationNodeCapacity;
  runtime: FederationRuntimeFacts;
  tools: FederationToolCapability[];
  role_support: FederationRole;
  allow_remote_subagents: boolean;
  health: string;
  api_port: number;
  last_seen: string;
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
  loaded_models?: FederationLoadedModel[];
  active_model_loaded?: boolean;
  capacity?: FederationNodeCapacity;
  runtime?: FederationRuntimeFacts;
  build_commit?: string | null;
  build_time?: string | null;
  tools: FederationToolCapability[];
  last_seen?: string | null;
  specialization: string;
  priority: number;
}

export interface FederationPeersResponse {
  enabled: boolean;
  configured_enabled?: boolean;
  local_node: FederationLocalNodeSummary;
  local_capabilities?: FederationCapabilities | null;
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
    | 'metrics'
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
    | 'federation_metrics'
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
  metrics?: {
    ttft_ms?: number | null;
    generation_tps?: number | null;
    prefill_tps?: number | null;
    total_ms?: number | null;
    prompt_tokens?: number | null;
  };
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
  tables?: number;
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

export type DbDiscoveryStatus = 'connected' | 'needs_configuration' | 'unsupported';

export interface DbDiscoveryResult {
  host: string;
  port: number;
  driver: string;
  connection_name?: string;
  status: DbDiscoveryStatus;
  newly_added: boolean;
  schema?: DbSchema;
  error?: string;
}

export interface DbDiscoveryResponse {
  discovered: DbDiscoveryResult[];
}

/** Summary of a chat session persisted on this node's disk — may have been
 * started from a different browser/device than the one currently viewing it. */
export interface ChatSessionSummary {
  session_id: string;
  title: string;
  updated_at_unix: number;
  updated_at_unix_ms?: number;
  revision?: number;
  message_count: number;
  active?: boolean;
}

export interface ChatSessionsListResponse {
  sessions: ChatSessionSummary[];
}

/** Raw stored message: same {role, content} shape used on the wire for
 * history_seed. `content` may itself be a JSON-encoded tool_calls/tool_result
 * envelope (see AgentChat's reconstructFromStoredMessages), matching exactly
 * what the WS protocol already sends as history_seed. */
export interface StoredChatMessage {
  role: 'user' | 'assistant' | 'agent' | 'tool';
  content: string;
}

export interface ChatSessionDetailResponse {
  session_id: string;
  updated_at_unix: number;
  updated_at_unix_ms?: number;
  revision?: number;
  active?: boolean;
  messages: StoredChatMessage[];
}
