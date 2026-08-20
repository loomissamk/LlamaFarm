import { useCallback, useEffect, useRef, useState, type FormEvent } from 'react';
import {
  AlertTriangle,
  CheckCircle2,
  Circle,
  Copy,
  ExternalLink,
  Github,
  Loader2,
  MessageCircle,
  Plug,
  RefreshCw,
  Unplug,
  X,
} from 'lucide-react';
import { apiFetch } from '@/lib/api';
import {
  automaticContextLabel,
  CONTEXT_PRESETS,
  contextDraftLabel,
  contextSourceLabel,
  formatContextTokens,
  hasAdaptiveContextPolicy,
  type ContextPolicyInfo,
  type ContextSource,
} from '@/lib/contextWindow';
import { GPU_LAYER_PRESETS } from '@/lib/gpuPlacement';

interface ConnectionsResponse {
  revision: string;
  github:
    | { status: 'connected'; login: string; scopes: string; connected_at: string }
    | { status: 'not_connected' };
  ollama: { status: string; model: string; provider: string };
  memory: { status: string; backend: string };
  discord: {
    status:
      | 'connected'
      | 'starting'
      | 'error'
      | 'restart_required'
      | 'disconnect_restart_required'
      | 'not_configured';
    configured: boolean;
    restart_required: boolean;
    guild_id?: string;
    allowed_users?: string[];
    bot?: {
      id: string;
      username: string;
      global_name?: string;
    };
    bot_id?: string;
    install_url?: string;
  };
  tailscale?: {
    status: 'up' | 'down' | 'unavailable';
    ipv4?: string;
    dns_name?: string;
    health_warnings?: number;
  };
}

interface ContextInfo extends ContextPolicyInfo {
  num_ctx: number | null;
  effective_default_num_ctx?: number | null;
  source?: ContextSource;
  adaptive?: {
    enabled: boolean;
    active?: boolean;
    baseline: number | null;
    max: number | null;
  };
  min: number;
  max: number;
  gpu_layers: number | null;
  effective_gpu_layers?: number | null;
  server: {
    max_loaded_models: string | null;
    keep_alive: string | null;
    kv_cache_type: string | null;
  };
  gpu: { total_mb: number; used_mb: number; free_mb: number };
  budget: {
    workspace_files: string[];
    soul_included: boolean;
    working_state_included: boolean;
    workspace_instruction_tokens_est: number;
    routed_baseline: {
      tool_count: number;
      schema_tokens_est: number;
      compact_catalogue_tokens_est: number;
      note: string;
    };
    full_catalogue_upper_bound: {
      tool_count: number;
      schema_tokens_est: number;
      note: string;
    };
    actual_prompt_tokens_source: string;
  };
}

interface DeviceStart {
  device_code: string;
  user_code: string;
  verification_uri: string;
  interval: number;
  expires_in: number;
}

type PollResult =
  | { status: 'pending' }
  | { status: 'slow_down'; interval: number }
  | { status: 'connected'; connection: { login: string } }
  | { status: 'failed'; error: string };

interface DiscordPairResult {
  restart_required: boolean;
  bot: {
    id: string;
    username: string;
    global_name?: string;
  };
  install_url: string;
}

function StatusDot({ connected }: { connected: boolean }) {
  return connected ? (
    <CheckCircle2 className="h-4 w-4 text-green-400" />
  ) : (
    <Circle className="h-4 w-4 text-gray-600" />
  );
}

export function ConnectionsPanel() {
  const [data, setData] = useState<ConnectionsResponse | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [actionStatus, setActionStatus] = useState<string | null>(null);
  const [contextError, setContextError] = useState<string | null>(null);
  const [contextStatus, setContextStatus] = useState<string | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [lastUpdated, setLastUpdated] = useState<Date | null>(null);
  const [device, setDevice] = useState<DeviceStart | null>(null);
  const [connecting, setConnecting] = useState(false);
  const [confirmDisconnect, setConfirmDisconnect] = useState(false);
  const [confirmDiscordDisconnect, setConfirmDiscordDisconnect] = useState(false);
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const disconnectCancelRef = useRef<HTMLButtonElement | null>(null);
  const discordDisconnectCancelRef = useRef<HTMLButtonElement | null>(null);
  const contextSaveInFlightRef = useRef<number | null>(null);
  const [ctx, setCtx] = useState<ContextInfo | null>(null);
  const [ctxDraft, setCtxDraft] = useState<number>(0);
  const [ctxSaving, setCtxSaving] = useState(false);
  const [discordEditorOpen, setDiscordEditorOpen] = useState(false);
  const [discordToken, setDiscordToken] = useState('');
  const [discordGuildId, setDiscordGuildId] = useState('');
  const [discordAllowedUsers, setDiscordAllowedUsers] = useState('');
  const [discordSaving, setDiscordSaving] = useState(false);

  const loadContext = useCallback(async () => {
    try {
      const info = await apiFetch<ContextInfo>('/api/context');
      setCtx(info);
      setCtxDraft(info.num_ctx ?? 0);
      setContextError(null);
    } catch (err) {
      setContextError(err instanceof Error ? err.message : 'Failed to load context settings');
    }
  }, []);

  const saveContext = async (value: number) => {
    if (value === (ctx?.num_ctx ?? 0) || contextSaveInFlightRef.current !== null) {
      return;
    }
    contextSaveInFlightRef.current = value;
    setCtxSaving(true);
    setContextError(null);
    setContextStatus(null);
    try {
      await apiFetch('/api/context', {
        method: 'PUT',
        body: JSON.stringify({ num_ctx: value === 0 ? null : value }),
      });
      await loadContext();
      setContextStatus(
        value === 0
          ? 'Explicit override cleared. The automatic runtime context policy is active.'
          : `Context window saved at ${value.toLocaleString()} tokens.`,
      );
    } catch (err) {
      setContextError(err instanceof Error ? err.message : 'Failed to set context');
    } finally {
      contextSaveInFlightRef.current = null;
      setCtxSaving(false);
    }
  };

  const saveGpuLayers = async (value: number | null) => {
    setCtxSaving(true);
    setContextError(null);
    setContextStatus(null);
    try {
      await apiFetch('/api/context', {
        method: 'PUT',
        body: JSON.stringify({
          num_ctx: ctxDraft === 0 ? null : ctxDraft,
          set_gpu_layers: true,
          gpu_layers: value,
        }),
      });
      await loadContext();
      setContextStatus('GPU layer preference saved.');
    } catch (err) {
      setContextError(err instanceof Error ? err.message : 'Failed to set GPU layers');
    } finally {
      setCtxSaving(false);
    }
  };

  const refresh = useCallback(async () => {
    setRefreshing(true);
    try {
      setData(await apiFetch<ConnectionsResponse>('/api/connections'));
      setLoadError(null);
      setLastUpdated(new Date());
    } catch (err) {
      setLoadError(err instanceof Error ? err.message : 'Failed to load connections');
    } finally {
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    const refreshWhenVisible = () => {
      if (document.visibilityState !== 'visible') return;
      void refresh();
      void loadContext();
    };

    refreshWhenVisible();
    const connectionRefresh = setInterval(refreshWhenVisible, 30_000);
    document.addEventListener('visibilitychange', refreshWhenVisible);
    return () => {
      clearInterval(connectionRefresh);
      document.removeEventListener('visibilitychange', refreshWhenVisible);
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, [refresh, loadContext]);

  const poll = useCallback(
    async (deviceCode: string, intervalSecs: number) => {
      if (document.visibilityState === 'hidden') {
        pollTimer.current = setTimeout(() => poll(deviceCode, intervalSecs), 1000);
        return;
      }
      try {
        const result = await apiFetch<PollResult>('/api/connections/github/poll', {
          method: 'POST',
          body: JSON.stringify({ device_code: deviceCode }),
        });
        if (result.status === 'connected') {
          setDevice(null);
          setConnecting(false);
          await refresh();
          return;
        }
        if (result.status === 'failed') {
          setActionError(`GitHub: ${result.error}`);
          setDevice(null);
          setConnecting(false);
          return;
        }
        const next = result.status === 'slow_down' ? result.interval : intervalSecs;
        pollTimer.current = setTimeout(() => poll(deviceCode, next), next * 1000);
      } catch (err) {
        setActionError(err instanceof Error ? err.message : 'GitHub poll failed');
        setDevice(null);
        setConnecting(false);
      }
    },
    [refresh],
  );

  const connectGithub = async () => {
    setActionError(null);
    setConnecting(true);
    try {
      const start = await apiFetch<DeviceStart>('/api/connections/github/start', {
        method: 'POST',
      });
      setDevice(start);
      // Open GitHub's device page for the operator, like Copilot does.
      window.open(start.verification_uri, '_blank', 'noopener,noreferrer');
      poll(start.device_code, start.interval);
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Could not start GitHub sign-in');
      setConnecting(false);
    }
  };

  const disconnectGithub = async () => {
    setConfirmDisconnect(false);
    setActionError(null);
    try {
      await apiFetch('/api/connections/github/disconnect', { method: 'POST' });
      await refresh();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Disconnect failed');
    }
  };

  const openDiscordEditor = () => {
    setActionError(null);
    setActionStatus(null);
    setDiscordToken('');
    setDiscordGuildId(data?.discord.guild_id ?? '');
    setDiscordAllowedUsers((data?.discord.allowed_users ?? []).join(', '));
    setDiscordEditorOpen(true);
  };

  const saveDiscord = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    const allowedUsers = discordAllowedUsers
      .split(/[\s,]+/)
      .map((value) => value.trim())
      .filter(Boolean);
    if (allowedUsers.length === 0) {
      setActionError(
        "Add your numeric Discord user ID. Use '*' only if you deliberately want every user allowed.",
      );
      return;
    }

    setDiscordSaving(true);
    setActionError(null);
    setActionStatus(null);
    try {
      const result = await apiFetch<DiscordPairResult>('/api/connections/discord', {
        method: 'PUT',
        body: JSON.stringify({
          bot_token: discordToken || null,
          guild_id: discordGuildId || null,
          allowed_users: allowedUsers,
          revision: data?.revision,
        }),
      });
      setDiscordEditorOpen(false);
      setDiscordToken('');
      const botName = result.bot.global_name || result.bot.username;
      setActionStatus(
        `Discord bot ${botName} paired and saved. Add it to your server, then restart this LlamaFarm node once to start the listener.`,
      );
      await refresh();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Could not save Discord settings');
    } finally {
      setDiscordSaving(false);
    }
  };

  const disconnectDiscord = async () => {
    setConfirmDiscordDisconnect(false);
    setActionError(null);
    setActionStatus(null);
    try {
      await apiFetch('/api/connections/discord/disconnect', {
        method: 'POST',
        body: JSON.stringify({ revision: data?.revision }),
      });
      setActionStatus(
        'Discord was disconnected. Restart this LlamaFarm node once to stop the existing listener.',
      );
      await refresh();
    } catch (err) {
      setActionError(err instanceof Error ? err.message : 'Could not disconnect Discord');
    }
  };

  useEffect(() => {
    if (!confirmDisconnect) return;
    disconnectCancelRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setConfirmDisconnect(false);
    };
    document.addEventListener('keydown', closeOnEscape);
    return () => document.removeEventListener('keydown', closeOnEscape);
  }, [confirmDisconnect]);

  useEffect(() => {
    if (!confirmDiscordDisconnect) return;
    discordDisconnectCancelRef.current?.focus();
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setConfirmDiscordDisconnect(false);
    };
    document.addEventListener('keydown', closeOnEscape);
    return () => document.removeEventListener('keydown', closeOnEscape);
  }, [confirmDiscordDisconnect]);

  const githubConnected = data?.github.status === 'connected';
  const contextDirty = ctxDraft !== (ctx?.num_ctx ?? 0);
  const effectiveContext = ctx?.effective_default_num_ctx ?? ctx?.num_ctx ?? null;
  const effectiveSource = ctx?.source ?? (ctx?.num_ctx !== null ? 'config' : 'model-native');
  const saveRangeFromKeyboard = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (
      [
        'ArrowLeft',
        'ArrowRight',
        'ArrowUp',
        'ArrowDown',
        'Home',
        'End',
        'PageUp',
        'PageDown',
      ].includes(event.key)
    ) {
      void saveContext(ctxDraft);
    }
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="flex items-center gap-3">
          <Plug className="h-5 w-5 text-blue-400" />
          <div>
            <h2 className="text-base font-semibold text-white">Connections</h2>
            <p className="text-xs text-gray-500">
              Services and runtime capacity currently available to this node.
            </p>
          </div>
        </div>
        <div className="flex items-center gap-3">
          <span className="text-xs text-gray-500">
            {lastUpdated ? `Updated ${lastUpdated.toLocaleTimeString()}` : 'Not updated yet'}
          </span>
          <button
            type="button"
            onClick={() => {
              void refresh();
              void loadContext();
            }}
            disabled={refreshing}
            className="inline-flex min-h-9 items-center gap-2 rounded-lg border border-gray-700 px-3 text-sm text-gray-300 transition-colors hover:bg-gray-800 disabled:cursor-wait disabled:opacity-60"
          >
            <RefreshCw
              className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`}
              aria-hidden="true"
            />
            Refresh
          </button>
        </div>
      </div>

      {[loadError, actionError, contextError].filter(Boolean).map((message, index) => (
        <div
          key={`${index}-${message}`}
          role="alert"
          className="rounded-md border border-red-700/40 bg-red-900/10 px-3 py-2 text-sm text-red-300"
        >
          {message}
        </div>
      ))}

      {actionStatus && (
        <div role="status" className="rounded-md border border-green-700/40 bg-green-900/10 px-3 py-2 text-sm text-green-300">
          {actionStatus}
        </div>
      )}

      {contextStatus && (
        <div role="status" className="rounded-md border border-green-700/40 bg-green-900/10 px-3 py-2 text-sm text-green-300">
          {contextStatus}
        </div>
      )}

      {confirmDisconnect && (
        <div
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="disconnect-github-title"
          aria-describedby="disconnect-github-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
        >
          <div className="w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-5 shadow-xl">
            <div className="flex items-start gap-3">
              <AlertTriangle className="mt-0.5 h-5 w-5 text-yellow-400" aria-hidden="true" />
              <div>
                <h3 id="disconnect-github-title" className="font-semibold text-white">
                  Disconnect GitHub?
                </h3>
                <p id="disconnect-github-description" className="mt-1 text-sm text-gray-400">
                  GitHub operations will remain unavailable until this node is connected again.
                </p>
              </div>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button
                ref={disconnectCancelRef}
                type="button"
                onClick={() => setConfirmDisconnect(false)}
                className="rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 hover:bg-gray-800"
              >
                Keep connected
              </button>
              <button
                type="button"
                onClick={() => void disconnectGithub()}
                className="rounded-lg bg-red-600 px-3 py-2 text-sm font-medium text-white hover:bg-red-700"
              >
                Disconnect
              </button>
            </div>
          </div>
        </div>
      )}

      {confirmDiscordDisconnect && (
        <div
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="disconnect-discord-title"
          aria-describedby="disconnect-discord-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
        >
          <div className="w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-5 shadow-xl">
            <div className="flex items-start gap-3">
              <AlertTriangle className="mt-0.5 h-5 w-5 text-yellow-400" aria-hidden="true" />
              <div>
                <h3 id="disconnect-discord-title" className="font-semibold text-white">
                  Disconnect Discord?
                </h3>
                <p id="disconnect-discord-description" className="mt-1 text-sm text-gray-400">
                  Discord messages will stop reaching this node after its required restart.
                </p>
              </div>
            </div>
            <div className="mt-5 flex justify-end gap-2">
              <button
                ref={discordDisconnectCancelRef}
                type="button"
                onClick={() => setConfirmDiscordDisconnect(false)}
                className="rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 hover:bg-gray-800"
              >
                Keep paired
              </button>
              <button
                type="button"
                onClick={() => void disconnectDiscord()}
                className="rounded-lg bg-red-600 px-3 py-2 text-sm font-medium text-white hover:bg-red-700"
              >
                Disconnect
              </button>
            </div>
          </div>
        </div>
      )}

      {discordEditorOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="discord-editor-title"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
        >
          <form
            onSubmit={(event) => void saveDiscord(event)}
            className="w-full max-w-lg rounded-xl border border-gray-700 bg-gray-900 p-5 shadow-xl"
          >
            <div className="flex items-start justify-between gap-4">
              <div>
                <h3 id="discord-editor-title" className="font-semibold text-white">
                  {data?.discord.configured ? 'Update paired Discord bot' : 'Pair Discord bot'}
                </h3>
                <p className="mt-1 text-sm text-gray-400">
                  Create a bot in the Discord Developer Portal, enable Message Content Intent, and
                  paste its token here. LlamaFarm verifies the bot before storing the token.
                </p>
              </div>
              <button
                type="button"
                onClick={() => setDiscordEditorOpen(false)}
                aria-label="Close Discord editor"
                className="rounded-md p-1 text-gray-500 hover:bg-gray-800 hover:text-white"
              >
                <X className="h-4 w-4" />
              </button>
            </div>

            <a
              href="https://discord.com/developers/applications"
              target="_blank"
              rel="noopener noreferrer"
              className="mt-3 inline-flex items-center gap-1.5 text-sm text-blue-300 hover:text-blue-200"
            >
              <ExternalLink className="h-3.5 w-3.5" />
              Open Discord Developer Portal
            </a>

            {actionError && (
              <div role="alert" className="mt-3 rounded-md border border-red-700/40 bg-red-900/10 px-3 py-2 text-sm text-red-300">
                {actionError}
              </div>
            )}

            <div className="mt-4 space-y-4">
              <label className="block text-sm text-gray-300">
                Bot token
                <input
                  type="password"
                  value={discordToken}
                  onChange={(event) => setDiscordToken(event.target.value)}
                  required={!data?.discord.configured}
                  autoComplete="off"
                  placeholder={
                    data?.discord.configured
                      ? 'Leave blank to keep the current token'
                      : 'Paste the bot token'
                  }
                  className="mt-1 w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-white outline-none focus:border-blue-500"
                />
              </label>
              <label className="block text-sm text-gray-300">
                Server (guild) ID <span className="text-gray-600">optional</span>
                <input
                  value={discordGuildId}
                  onChange={(event) => setDiscordGuildId(event.target.value)}
                  inputMode="numeric"
                  placeholder="123456789012345678"
                  className="mt-1 w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-white outline-none focus:border-blue-500"
                />
              </label>
              <label className="block text-sm text-gray-300">
                Allowed user IDs
                <input
                  value={discordAllowedUsers}
                  onChange={(event) => setDiscordAllowedUsers(event.target.value)}
                  placeholder="Your numeric Discord user ID"
                  className="mt-1 w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-white outline-none focus:border-blue-500"
                />
                <span className="mt-1 block text-xs text-gray-500">
                  Separate multiple IDs with commas. Turn on Developer Mode in Discord, then copy
                  your user ID. An asterisk allows everyone and is not recommended.
                </span>
              </label>
            </div>

            <div className="mt-5 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setDiscordEditorOpen(false)}
                disabled={discordSaving}
                className="rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 hover:bg-gray-800 disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="submit"
                disabled={discordSaving}
                className="inline-flex items-center gap-2 rounded-lg bg-indigo-600 px-3 py-2 text-sm font-medium text-white hover:bg-indigo-700 disabled:opacity-50"
              >
                {discordSaving && <Loader2 className="h-4 w-4 animate-spin" />}
                Verify and pair
              </button>
            </div>
          </form>
        </div>
      )}

      {/* GitHub */}
      <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
        <div className="flex items-start justify-between gap-4">
          <div className="flex items-start gap-3">
            <Github className="mt-0.5 h-5 w-5 text-gray-300" />
            <div>
              <div className="flex items-center gap-2">
                <span className="font-medium text-white">GitHub</span>
                <StatusDot connected={!!githubConnected} />
              </div>
              {data?.github.status === 'connected' ? (
                <p className="mt-0.5 text-sm text-gray-400">
                  Connected as <span className="text-gray-200">{data.github.login}</span>
                  {data.github.scopes && ` · ${data.github.scopes}`}
                </p>
              ) : (
                <p className="mt-0.5 text-sm text-gray-500">
                  Not connected — clone, branch, commit, and push are unavailable.
                </p>
              )}
            </div>
          </div>
          {githubConnected ? (
            <button
              type="button"
              onClick={() => setConfirmDisconnect(true)}
              className="inline-flex items-center gap-1.5 rounded-lg border border-gray-700 px-3 py-1.5 text-sm text-gray-300 hover:border-red-700 hover:text-red-300"
            >
              <Unplug className="h-4 w-4" />
              Disconnect
            </button>
          ) : (
            <button
              onClick={connectGithub}
              disabled={connecting}
              className="inline-flex items-center gap-1.5 rounded-lg bg-blue-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50"
            >
              {connecting ? (
                <Loader2 className="h-4 w-4 animate-spin" />
              ) : (
                <Github className="h-4 w-4" />
              )}
              Connect GitHub
            </button>
          )}
        </div>

        {device && (
          <div className="mt-4 rounded-lg border border-blue-800/50 bg-blue-950/20 p-4">
            <p className="text-sm text-gray-300">
              Enter this code on GitHub to authorize this node:
            </p>
            <div className="mt-2 flex items-center gap-3">
              <code className="rounded-md border border-gray-700 bg-gray-950 px-3 py-2 font-mono text-lg tracking-widest text-white">
                {device.user_code}
              </code>
              <button
                onClick={() => navigator.clipboard?.writeText(device.user_code)}
                className="inline-flex items-center gap-1.5 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-gray-300 hover:text-white"
              >
                <Copy className="h-3.5 w-3.5" />
                Copy
              </button>
              <a
                href={device.verification_uri}
                target="_blank"
                rel="noopener noreferrer"
                className="inline-flex items-center gap-1.5 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-blue-300 hover:text-blue-200"
              >
                <ExternalLink className="h-3.5 w-3.5" />
                Open github.com/login/device
              </a>
            </div>
            <p className="mt-2 flex items-center gap-1.5 text-xs text-gray-500">
              <Loader2 className="h-3 w-3 animate-spin" />
              Waiting for authorization…
            </p>
          </div>
        )}
      </div>

      {/* Context window control + token budget */}
      {ctx && (
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          {/* Live VRAM bar — shows the dynamic tradeoff: bigger context / more
              GPU layers use more VRAM. */}
          {ctx.gpu.total_mb > 0 && (
            <div className="mb-4">
              <div className="flex items-center justify-between text-xs">
                <span className="text-gray-400">GPU VRAM</span>
                <span className="font-mono text-gray-300">
                  {(ctx.gpu.used_mb / 1024).toFixed(1)} / {(ctx.gpu.total_mb / 1024).toFixed(1)} GB
                  <span className="text-gray-600"> · {(ctx.gpu.free_mb / 1024).toFixed(1)} free</span>
                </span>
              </div>
              <div className="mt-1 h-2 w-full overflow-hidden rounded-full bg-gray-800">
                <div
                  className={`h-full transition-all ${
                    ctx.gpu.used_mb / ctx.gpu.total_mb > 0.92
                      ? 'bg-red-500'
                      : ctx.gpu.used_mb / ctx.gpu.total_mb > 0.75
                        ? 'bg-amber-500'
                        : 'bg-green-500'
                  }`}
                  style={{ width: `${Math.min(100, (ctx.gpu.used_mb / ctx.gpu.total_mb) * 100)}%` }}
                />
              </div>
              <p className="mt-1 text-[11px] text-gray-600">
                Bigger context and "All GPU" layers use more VRAM. Keep some headroom for the embed
                model so both stay resident (avoids the reload penalty).
              </p>
            </div>
          )}
          <div className="flex flex-wrap items-center justify-between gap-2">
            <label htmlFor="connection-context-window" className="font-medium text-white">
              Chat context window
            </label>
            <span className="font-mono text-sm text-blue-300">
              {contextDraftLabel(ctx, ctxDraft)}
            </span>
          </div>
          <p className="mt-1 text-xs text-gray-500">
            {ctxDraft === 0 ? (
              hasAdaptiveContextPolicy(ctx) ? (
                <>
                  Starts at{' '}
                  {formatContextTokens(
                    ctx.adaptive?.baseline ?? effectiveContext ?? ctx.min,
                  )}
                  {' and expands only when the request needs it, up to '}
                  {formatContextTokens(ctx.adaptive?.max ?? ctx.max)} or the model&apos;s native
                  limit, whichever is lower.
                </>
              ) : effectiveContext ? (
                <>
                  Automatic currently uses the {contextSourceLabel(effectiveSource).toLowerCase()}{' '}
                  setting of {formatContextTokens(effectiveContext)} tokens.
                </>
              ) : (
                <>Automatic resolves the selected model&apos;s native context at request time.</>
              )
            ) : (
              <>
                This is a capacity ceiling for the request, not tokens already spent. Actual
                prompt usage comes from provider-reported chat metrics.
              </>
            )}
          </p>
          <div
            className="mt-3 flex flex-wrap gap-2"
            role="group"
            aria-label="Context window presets"
          >
            {CONTEXT_PRESETS.map((preset) => {
              const unavailable = preset.value > ctx.max;
              return (
                <button
                  key={preset.value}
                  type="button"
                  onClick={() => {
                    setCtxDraft(preset.value);
                    void saveContext(preset.value);
                  }}
                  disabled={ctxSaving || unavailable}
                  aria-pressed={ctxDraft === preset.value}
                  title={
                    unavailable
                      ? `This runtime supports up to ${formatContextTokens(ctx.max)} tokens`
                      : preset.value === 0
                        ? `Use ${automaticContextLabel(ctx)}`
                        : `Use a fixed ${preset.label} context`
                  }
                  className={`min-h-9 rounded-lg border px-3 text-xs transition-colors disabled:cursor-not-allowed disabled:opacity-35 ${
                    ctxDraft === preset.value
                      ? 'border-blue-500 bg-blue-500/10 text-blue-200'
                      : 'border-gray-700 text-gray-300 hover:border-blue-500 hover:text-white'
                  }`}
                >
                  {preset.label}
                </button>
              );
            })}
          </div>
          <input
            id="connection-context-window"
            type="range"
            min={0}
            max={ctx.max}
            step={2048}
            value={ctxDraft}
            onChange={(e) => setCtxDraft(Number(e.target.value))}
            onPointerUp={() => void saveContext(ctxDraft)}
            onKeyUp={saveRangeFromKeyboard}
            onBlur={() => void saveContext(ctxDraft)}
            aria-valuetext={
              ctxDraft === 0
                ? `Automatic context: ${automaticContextLabel(ctx)}`
                : `${ctxDraft} tokens`
            }
            disabled={ctxSaving}
            className="mt-3 w-full accent-blue-500"
          />
          <div className="mt-2 flex justify-end">
            <button
              type="button"
              onClick={() => void saveContext(ctxDraft)}
              disabled={!contextDirty || ctxSaving}
              className="rounded-md border border-gray-700 px-3 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 disabled:opacity-50"
            >
              {ctxSaving ? 'Saving…' : 'Apply context window'}
            </button>
          </div>
          <div className="mt-1 flex justify-between text-[10px] text-gray-600">
            <span>automatic policy</span>
            <span>{formatContextTokens(ctx.max)}</span>
          </div>
          <div className="mt-3 grid grid-cols-3 gap-2 text-xs">
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">Hot workspace</div>
              <div className="font-mono text-gray-300">
                ~{ctx.budget.workspace_instruction_tokens_est.toLocaleString()} tok
              </div>
              <div className="mt-1 text-[10px] text-gray-600">
                AGENTS.md{ctx.budget.working_state_included ? ' + bounded working state' : ' only'}
              </div>
            </div>
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">
                Routed baseline ({ctx.budget.routed_baseline.tool_count})
              </div>
              <div className="font-mono text-green-300">
                ~{ctx.budget.routed_baseline.schema_tokens_est.toLocaleString()} schema tok
              </div>
              <div className="mt-1 text-[10px] text-gray-600">
                +~{ctx.budget.routed_baseline.compact_catalogue_tokens_est.toLocaleString()} tok
                names/purposes
              </div>
            </div>
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">
                Full catalogue ({ctx.budget.full_catalogue_upper_bound.tool_count})
              </div>
              <div className="font-mono text-amber-300">
                ~{ctx.budget.full_catalogue_upper_bound.schema_tokens_est.toLocaleString()} tok max
              </div>
              <div className="mt-1 text-[10px] text-gray-600">
                Explicit full run or one-time discovery recovery
              </div>
            </div>
          </div>
          <p className="mt-2 text-xs text-gray-500">
            Ordinary turns send essential/relevant schemas plus a compact catalogue of tool names
            and purposes. The model activates extra schemas on demand with tool_search; Test All
            Tools explicitly requests the full catalogue. If discovery misses or still cannot make
            progress, one policy-filtered recovery may expose the remaining schemas once. SOUL.md
            is not injected. These are conservative estimates—provider-reported prompt tokens in
            chat are the real usage.
          </p>

          {/* GPU layer offload */}
          <div className="mt-4 border-t border-gray-800 pt-3">
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-300">GPU layer offload</span>
              <span className="font-mono text-xs text-blue-300">
                {ctx.gpu_layers === null
                  ? ctx.effective_gpu_layers === 999
                    ? 'auto · maximum fit'
                    : 'auto'
                  : ctx.gpu_layers === 999
                    ? 'all on GPU'
                    : ctx.gpu_layers}
              </span>
            </div>
            <div className="mt-2 flex gap-2">
              {GPU_LAYER_PRESETS.map(({ label, value }) => (
                <button
                  key={label}
                  disabled={ctxSaving}
                  onClick={() => saveGpuLayers(value)}
                  className="rounded-lg border border-gray-700 px-3 py-1 text-xs text-gray-300 hover:border-blue-500 hover:text-white disabled:opacity-40"
                >
                  {label}
                </button>
              ))}
            </div>
            <p className="mt-1 text-[11px] text-gray-600">
              Auto defaults to maximum-fit on the bundled runtime. Ollama fills VRAM with model
              layers first and spills only the layers that cannot fit to system RAM.
            </p>
          </div>

          {/* Ollama server knobs (apply on redeploy) */}
          <div className="mt-4 border-t border-gray-800 pt-3">
            <div className="text-sm text-gray-300 mb-2">Ollama server (applies on redeploy)</div>
            <div className="grid grid-cols-3 gap-2 text-xs">
              <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">Max loaded models</div>
                <div className={`font-mono ${ctx.server.max_loaded_models === '1' ? 'text-amber-300' : 'text-gray-300'}`}>
                  {ctx.server.max_loaded_models ?? '—'}
                </div>
              </div>
              <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">Keep alive</div>
                <div className="font-mono text-gray-300">{ctx.server.keep_alive ?? '—'}</div>
              </div>
              <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
                <div className="text-gray-500">KV cache</div>
                <div className="font-mono text-gray-300">{ctx.server.kv_cache_type ?? '—'}</div>
              </div>
            </div>
            {ctx.server.max_loaded_models === '1' && (
              <p className="mt-1 text-[11px] text-amber-400">
                Max loaded models = 1 causes the chat model to reload every time a memory/RAG embed
                runs (~40s TTFT). Set to 2+ in the node profile and redeploy.
              </p>
            )}
          </div>
        </div>
      )}

      {/* Ollama + memory: read-only state so the page tells the whole story */}
      <div className="grid gap-4 sm:grid-cols-2">
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          <div className="flex items-center gap-2">
            <span className="font-medium text-white">Ollama</span>
            <StatusDot connected />
          </div>
          <p className="mt-1 text-sm text-gray-400">
            {data?.ollama.model || '—'}{' '}
            <span className="text-gray-600">({data?.ollama.provider || '—'})</span>
          </p>
        </div>
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          <div className="flex items-center gap-2">
            <span className="font-medium text-white">Memory</span>
            <StatusDot connected />
          </div>
          <p className="mt-1 text-sm text-gray-400 capitalize">{data?.memory.backend || '—'}</p>
        </div>
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          <div className="flex items-start justify-between gap-3">
            <div className="flex items-start gap-2">
              <MessageCircle className="mt-0.5 h-5 w-5 text-indigo-300" />
              <div>
                <div className="flex items-center gap-2">
                  <span className="font-medium text-white">Discord</span>
                  <StatusDot connected={!!data?.discord.configured} />
                </div>
                <p className="mt-1 text-sm text-gray-500">
                  {data?.discord.status === 'connected'
                    ? `Paired${data.discord.bot ? ` as ${data.discord.bot.global_name || data.discord.bot.username}` : ''} · listener running.`
                    : data?.discord.status === 'restart_required'
                      ? `Paired${data.discord.bot ? ` as ${data.discord.bot.global_name || data.discord.bot.username}` : ''}. Restart this node once to start the listener.`
                      : data?.discord.status === 'error'
                        ? 'The bot listener hit an error. Check the token, intents, and container logs.'
                        : data?.discord.status === 'starting'
                          ? 'Bot listener is starting.'
                          : data?.discord.status === 'disconnect_restart_required'
                            ? 'Discord was removed from config. Restart this node once to stop the old listener.'
                          : 'Not paired — connect a Discord bot without editing TOML.'}
                </p>
              </div>
            </div>
            <div className="flex shrink-0 flex-wrap justify-end gap-2">
              {data?.discord.install_url && (
                <a
                  href={data.discord.install_url}
                  target="_blank"
                  rel="noopener noreferrer"
                  className="inline-flex items-center gap-1.5 rounded-lg bg-indigo-600 px-3 py-1.5 text-sm font-medium text-white hover:bg-indigo-700"
                >
                  <ExternalLink className="h-3.5 w-3.5" />
                  Add to server
                </a>
              )}
              <button
                type="button"
                onClick={openDiscordEditor}
                className="rounded-lg border border-indigo-700/60 px-3 py-1.5 text-sm text-indigo-200 hover:bg-indigo-950/40"
              >
                {data?.discord.configured ? 'Update' : 'Connect Discord'}
              </button>
              {data?.discord.configured && (
                <button
                  type="button"
                  onClick={() => setConfirmDiscordDisconnect(true)}
                  className="rounded-lg border border-gray-700 px-3 py-1.5 text-sm text-gray-300 hover:border-red-700 hover:text-red-300"
                >
                  Disconnect
                </button>
              )}
            </div>
          </div>
        </div>
        {data?.tailscale?.status === 'up' && (
          <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
            <div className="flex items-center gap-2">
              <span className="font-medium text-white">Tailscale</span>
              <StatusDot connected />
              <span className="text-xs text-green-400">Up</span>
            </div>
            <p className="mt-1 text-sm text-gray-400">
              {data.tailscale.ipv4 || 'Connected'}
              {data.tailscale.dns_name ? (
                <span className="text-gray-600"> · {data.tailscale.dns_name}</span>
              ) : null}
            </p>
          </div>
        )}
      </div>
    </div>
  );
}

export default function Connections() {
  return (
    <div className="p-6">
      <ConnectionsPanel />
    </div>
  );
}
