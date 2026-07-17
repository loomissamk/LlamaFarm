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
    return () => {
      if (pollTimer.current) clearTimeout(pollTimer.current);
    };
  }, [refresh]);

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
