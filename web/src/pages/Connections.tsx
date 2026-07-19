import { useCallback, useEffect, useRef, useState } from 'react';
import {
  CheckCircle2,
  Circle,
  Copy,
  ExternalLink,
  Github,
  Loader2,
  Plug,
  Unplug,
} from 'lucide-react';
import { apiFetch } from '@/lib/api';

interface ConnectionsResponse {
  github:
    | { status: 'connected'; login: string; scopes: string; connected_at: string }
    | { status: 'not_connected' };
  ollama: { status: string; model: string; provider: string };
  memory: { status: string; backend: string };
  discord?: { status: string };
  tailscale?: { status: string };
}

interface ContextInfo {
  num_ctx: number | null;
  min: number;
  max: number;
  gpu_layers: number | null;
  server: {
    max_loaded_models: string | null;
    keep_alive: string | null;
    kv_cache_type: string | null;
  };
  gpu: { total_mb: number; used_mb: number; free_mb: number };
  budget: {
    persona_md_tokens: number;
    tool_count: number;
    tool_schema_tokens: number;
    fixed_prompt_tokens_est: number;
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

function StatusDot({ connected }: { connected: boolean }) {
  return connected ? (
    <CheckCircle2 className="h-4 w-4 text-green-400" />
  ) : (
    <Circle className="h-4 w-4 text-gray-600" />
  );
}

export function ConnectionsPanel() {
  const [data, setData] = useState<ConnectionsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [device, setDevice] = useState<DeviceStart | null>(null);
  const [connecting, setConnecting] = useState(false);
  const pollTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [ctx, setCtx] = useState<ContextInfo | null>(null);
  const [ctxDraft, setCtxDraft] = useState<number>(0);
  const [ctxSaving, setCtxSaving] = useState(false);

  const loadContext = useCallback(async () => {
    try {
      const info = await apiFetch<ContextInfo>('/api/context');
      setCtx(info);
      setCtxDraft(info.num_ctx ?? 0);
    } catch {
      /* non-fatal */
    }
  }, []);

  const saveContext = async (value: number) => {
    setCtxSaving(true);
    try {
      await apiFetch('/api/context', {
        method: 'PUT',
        body: JSON.stringify({ num_ctx: value === 0 ? null : value }),
      });
      await loadContext();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to set context');
    } finally {
      setCtxSaving(false);
    }
  };

  const saveGpuLayers = async (value: number) => {
    setCtxSaving(true);
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
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to set GPU layers');
    } finally {
      setCtxSaving(false);
    }
  };

  const refresh = useCallback(async () => {
    try {
      setData(await apiFetch<ConnectionsResponse>('/api/connections'));
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load connections');
    }
  }, []);

  useEffect(() => {
    refresh();
    loadContext();
    return () => {
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, [refresh, loadContext]);

  const poll = useCallback(
    async (deviceCode: string, intervalSecs: number) => {
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
          setError(`GitHub: ${result.error}`);
          setDevice(null);
          setConnecting(false);
          return;
        }
        const next = result.status === 'slow_down' ? result.interval : intervalSecs;
        pollTimer.current = setTimeout(() => poll(deviceCode, next), next * 1000);
      } catch (err) {
        setError(err instanceof Error ? err.message : 'GitHub poll failed');
        setConnecting(false);
      }
    },
    [refresh],
  );

  const connectGithub = async () => {
    setError(null);
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
      setError(err instanceof Error ? err.message : 'Could not start GitHub sign-in');
      setConnecting(false);
    }
  };

  const disconnectGithub = async () => {
    if (!window.confirm('Disconnect GitHub from this node?')) return;
    try {
      await apiFetch('/api/connections/github/disconnect', { method: 'POST' });
      await refresh();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Disconnect failed');
    }
  };

  const githubConnected = data?.github.status === 'connected';

  return (
    <div className="space-y-4">
      <div className="flex items-center gap-3">
        <Plug className="h-5 w-5 text-blue-400" />
        <div>
          <h2 className="text-base font-semibold text-white">Connections</h2>
          <p className="text-xs text-gray-500">
            Services this node can use. Credentials are stored owner-only on the node and are
            never exposed to the model, tools, or this browser.
          </p>
        </div>
      </div>

      {error && (
        <div className="rounded-md border border-red-700/40 bg-red-900/10 px-3 py-2 text-sm text-red-300">
          {error}
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
              onClick={disconnectGithub}
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
          <div className="flex items-center justify-between">
            <span className="font-medium text-white">Chat context window</span>
            <span className="font-mono text-sm text-blue-300">
              {ctxDraft === 0 ? 'auto (model native)' : `${ctxDraft.toLocaleString()} tokens`}
            </span>
          </div>
          <input
            type="range"
            min={0}
            max={ctx.max}
            step={2048}
            value={ctxDraft}
            onChange={(e) => setCtxDraft(Number(e.target.value))}
            onMouseUp={() => saveContext(ctxDraft)}
            onTouchEnd={() => saveContext(ctxDraft)}
            disabled={ctxSaving}
            className="mt-3 w-full accent-blue-500"
          />
          <div className="mt-1 flex justify-between text-[10px] text-gray-600">
            <span>auto</span>
            <span>{(ctx.max / 1024).toFixed(0)}k</span>
          </div>
          <div className="mt-3 grid grid-cols-3 gap-2 text-xs">
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">Persona .md</div>
              <div className="font-mono text-gray-300">~{ctx.budget.persona_md_tokens} tok</div>
            </div>
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">Tools ({ctx.budget.tool_count})</div>
              <div className="font-mono text-gray-300">~{ctx.budget.tool_schema_tokens} tok</div>
            </div>
            <div className="rounded-lg border border-gray-800 bg-gray-950 p-2">
              <div className="text-gray-500">Fixed prompt</div>
              <div className="font-mono text-amber-300">~{ctx.budget.fixed_prompt_tokens_est} tok</div>
            </div>
          </div>
          <p className="mt-2 text-xs text-gray-500">
            Fixed prompt cost (persona + tool schemas) is spent before your conversation. Subtasks
            keep their own separate contexts. Raise the window on a bigger server; trim AGENTS.md or
            disable unused tools if the fixed cost is a large share of a small window.
          </p>

          {/* GPU layer offload */}
          <div className="mt-4 border-t border-gray-800 pt-3">
            <div className="flex items-center justify-between">
              <span className="text-sm text-gray-300">GPU layer offload</span>
              <span className="font-mono text-xs text-blue-300">
                {ctx.gpu_layers === null ? 'auto' : ctx.gpu_layers === 999 ? 'all on GPU' : ctx.gpu_layers}
              </span>
            </div>
            <div className="mt-2 flex gap-2">
              {[
                ['All GPU', 999],
                ['Auto', -1],
                ['CPU only', 0],
              ].map(([label, val]) => (
                <button
                  key={label as string}
                  disabled={ctxSaving}
                  onClick={() => saveGpuLayers(val as number)}
                  className="rounded-lg border border-gray-700 px-3 py-1 text-xs text-gray-300 hover:border-blue-500 hover:text-white disabled:opacity-40"
                >
                  {label}
                </button>
              ))}
            </div>
            <p className="mt-1 text-[11px] text-gray-600">
              "All GPU" (999) fills VRAM before spilling to CPU. You're only using part of the card —
              raise context or keep more models resident to use it.
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
          <div className="flex items-center gap-2">
            <span className="font-medium text-white">Discord</span>
            <StatusDot connected={data?.discord?.status === 'connected'} />
          </div>
          <p className="mt-1 text-sm text-gray-500">
            {data?.discord?.status === 'connected'
              ? 'Bot connected — drive the node from Discord.'
              : 'Add a bot token under [channels.discord] in config to enable.'}
          </p>
        </div>
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          <div className="flex items-center gap-2">
            <span className="font-medium text-white">Tailscale VPN</span>
            <StatusDot connected={data?.tailscale?.status === 'configured'} />
          </div>
          <p className="mt-1 text-sm text-gray-500">
            {data?.tailscale?.status === 'configured'
              ? 'Auth key set — deploy with --profile vpn for worldwide access.'
              : 'Set TS_AUTHKEY and deploy with --profile vpn to reach this node anywhere.'}
          </p>
        </div>
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
