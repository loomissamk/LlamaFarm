import { useEffect, useMemo, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Cpu,
  Network,
  Plus,
  RefreshCw,
  Server,
  TriangleAlert,
  Wrench,
  XCircle,
} from 'lucide-react';
import { addFederationManualPeer, getDelegationEnabled, getFederationPeers, setDelegationEnabled, updateFederationPeerHints, updateFederationPeerRole } from '@/lib/api';
import {
  loadFederationPeerSelections,
  loadFederationTasksBySession,
  loadPersistedActiveChatSessionId,
  loadPersistedChatSessionSummaries,
  persistActiveChatSessionId,
  persistFederationPeerSelections,
  type FederationTaskState,
} from '@/lib/federationState';
import type {
  FederationPeerSummary,
  FederationPeersResponse,
  FederationRole,
} from '@/types/api';

function federationRoleAllowsWorker(role: FederationRole): boolean {
  return role === 'worker' || role === 'both';
}

function canUseFederationPeer(peer: FederationPeerSummary): boolean {
  return (
    peer.online &&
    peer.allow_remote_subagents &&
    federationRoleAllowsWorker(peer.assigned_role) &&
    federationRoleAllowsWorker(peer.role_support)
  );
}

function federationPeerBlockReason(peer: FederationPeerSummary): string {
  if (!peer.online) {
    return 'Offline';
  }
  if (!peer.allow_remote_subagents) {
    return 'Remote subagents disabled on this node';
  }
  if (!federationRoleAllowsWorker(peer.role_support)) {
    return 'Node does not advertise worker support';
  }
  if (!federationRoleAllowsWorker(peer.assigned_role)) {
    return 'Assigned role excludes worker delegation';
  }
  return 'Ready for delegation';
}

function taskStatusClasses(task: FederationTaskState): string {
  if (task.status === 'error') {
    return 'border border-red-500/30 bg-red-500/10 text-red-300';
  }
  if (task.status === 'done') {
    return 'border border-emerald-500/30 bg-emerald-500/10 text-emerald-300';
  }
  return 'border border-cyan-500/30 bg-cyan-500/10 text-cyan-300';
}

function peerStatusClasses(peer: FederationPeerSummary): string {
  return peer.online
    ? 'border border-emerald-500/30 bg-emerald-500/10 text-emerald-300'
    : 'border border-gray-700 bg-gray-800 text-gray-400';
}

function isLocalhostBind(host: string): boolean {
  const h = host.trim().toLowerCase();
  return h === '127.0.0.1' || h === 'localhost' || h === '::1' || h === '[::1]' || h === '';
}

export default function FederationPage() {
  const [federation, setFederation] = useState<FederationPeersResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [manualPeerInput, setManualPeerInput] = useState('');
  const [addingPeer, setAddingPeer] = useState(false);
  const [addPeerError, setAddPeerError] = useState<string | null>(null);
  const manualPeerRef = useRef<HTMLInputElement>(null);
  const [delegationEnabled, setDelegationEnabledState] = useState(false);
  const [delegationToggling, setDelegationToggling] = useState(false);
  const [selectedPeerIdsBySession, setSelectedPeerIdsBySession] = useState<
    Record<string, string[]>
  >(() => loadFederationPeerSelections());
  const [tasksBySession, setTasksBySession] = useState<
    Record<string, FederationTaskState[]>
  >(() => loadFederationTasksBySession());
  const [chatSessions, setChatSessions] = useState(() => loadPersistedChatSessionSummaries());
  const [selectedSessionId, setSelectedSessionId] = useState<string | null>(() => {
    const persistedSessions = loadPersistedChatSessionSummaries();
    const activeSessionId = loadPersistedActiveChatSessionId();
    if (activeSessionId && persistedSessions.some((session) => session.id === activeSessionId)) {
      return activeSessionId;
    }
    return persistedSessions[0]?.id ?? activeSessionId ?? null;
  });

  useEffect(() => {
    persistFederationPeerSelections(selectedPeerIdsBySession);
  }, [selectedPeerIdsBySession]);

  useEffect(() => {
    persistActiveChatSessionId(selectedSessionId);
  }, [selectedSessionId]);

  useEffect(() => {
    void getDelegationEnabled()
      .then((res) => setDelegationEnabledState(res.enabled))
      .catch(() => {/* ignore — old server without endpoint */});
  }, []);

  const handleDelegationToggle = () => {
    const next = !delegationEnabled;
    setDelegationToggling(true);
    void setDelegationEnabled(next)
      .then((res) => setDelegationEnabledState(res.enabled))
      .catch(() => {/* ignore */})
      .finally(() => setDelegationToggling(false));
  };

  useEffect(() => {
    let cancelled = false;

    const refreshFederation = async () => {
      try {
        const response = await getFederationPeers();
        if (!cancelled) {
          setFederation(response);
          setLoading(false);
          setError(null);
        }
      } catch {
        if (!cancelled) {
          setLoading(false);
          setError('Failed to load federation peers.');
        }
      }
    };

    const refreshLocalState = () => {
      const persistedSessions = loadPersistedChatSessionSummaries();
      setChatSessions(persistedSessions);
      setSelectedPeerIdsBySession(loadFederationPeerSelections());
      setTasksBySession(loadFederationTasksBySession());
      setSelectedSessionId((current) => {
        if (current && persistedSessions.some((session) => session.id === current)) {
          return current;
        }

        const activeSessionId = loadPersistedActiveChatSessionId();
        if (activeSessionId && persistedSessions.some((session) => session.id === activeSessionId)) {
          return activeSessionId;
        }

        return persistedSessions[0]?.id ?? activeSessionId ?? current ?? null;
      });
    };

    void refreshFederation();
    refreshLocalState();

    const interval = window.setInterval(() => {
      void refreshFederation();
    }, 5000);
    window.addEventListener('focus', refreshLocalState);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
      window.removeEventListener('focus', refreshLocalState);
    };
  }, []);

  useEffect(() => {
    if (!federation?.enabled) {
      return;
    }

    const validPeerIds = new Set(
      federation.peers.filter(canUseFederationPeer).map((peer) => peer.peer_id),
    );
    setSelectedPeerIdsBySession((prev) => {
      let changed = false;
      const next = Object.fromEntries(
        Object.entries(prev).map(([sessionId, peerIds]) => {
          const filtered = peerIds.filter((peerId) => validPeerIds.has(peerId));
          if (filtered.length !== peerIds.length) {
            changed = true;
          }
          return [sessionId, filtered];
        }),
      );
      return changed ? next : prev;
    });
  }, [federation]);

  const availablePeers = federation?.peers ?? [];
  const enabled = federation?.enabled ?? false;
  const currentSession = useMemo(
    () => chatSessions.find((session) => session.id === selectedSessionId) ?? null,
    [chatSessions, selectedSessionId],
  );
  const selectedPeerIds = selectedSessionId
    ? selectedPeerIdsBySession[selectedSessionId] ?? []
    : [];
  const visibleTasks = selectedSessionId
    ? [...(tasksBySession[selectedSessionId] ?? [])]
        .sort((left, right) => right.updatedAt.getTime() - left.updatedAt.getTime())
        .slice(0, 8)
    : [];

  const readyWorkers = availablePeers.filter(canUseFederationPeer);
  const standbyPeers = availablePeers.filter(
    (peer) => peer.online && !readyWorkers.some((readyPeer) => readyPeer.peer_id === peer.peer_id),
  );
  const offlinePeers = availablePeers.filter((peer) => !peer.online);
  const onlinePeers = availablePeers.filter((peer) => peer.online).length;

  const handlePeerToggle = (peerId: string, enabledForSession: boolean) => {
    if (!selectedSessionId) {
      return;
    }

    setSelectedPeerIdsBySession((prev) => {
      const currentPeerIds = prev[selectedSessionId] ?? [];
      const nextPeerIds = enabledForSession
        ? Array.from(new Set([...currentPeerIds, peerId]))
        : currentPeerIds.filter((candidate) => candidate !== peerId);

      return {
        ...prev,
        [selectedSessionId]: nextPeerIds,
      };
    });
  };

  const handleRoleChange = async (peerId: string, role: FederationRole) => {
    try {
      const response = await updateFederationPeerRole(peerId, role);
      setFederation((prev) =>
        prev
          ? {
              ...prev,
              peers: prev.peers.map((peer) =>
                peer.peer_id === peerId ? response.peer : peer,
              ),
            }
          : prev,
      );
      setError(null);
    } catch {
      setError('Failed to update federation role.');
    }
  };

  const handleSpecializationEdit = (peerId: string, value: string) => {
    setFederation((prev) =>
      prev
        ? { ...prev, peers: prev.peers.map((p) => (p.peer_id === peerId ? { ...p, specialization: value } : p)) }
        : prev,
    );
  };

  const handleHintsChange = async (peerId: string, specialization: string, priority: number) => {
    try {
      const response = await updateFederationPeerHints(peerId, specialization, priority);
      setFederation((prev) =>
        prev
          ? {
              ...prev,
              peers: prev.peers.map((peer) =>
                peer.peer_id === peerId ? response.peer : peer,
              ),
            }
          : prev,
      );
      setError(null);
    } catch {
      setError('Failed to update peer hints.');
    }
  };

  const renderPeerList = (
    title: string,
    description: string,
    peers: FederationPeerSummary[],
  ) => (
    <section className="rounded-2xl border border-gray-800 bg-gray-900/80 p-4">
      <div className="flex items-center gap-2 text-sm font-semibold text-white">
        <Server className="h-4 w-4 text-cyan-400" />
        {title}
      </div>
      <p className="mt-1 text-xs text-gray-500">{description}</p>

      {peers.length === 0 ? (
        <p className="mt-4 text-sm text-gray-400">No nodes in this group right now.</p>
      ) : (
        <div className="mt-4 space-y-3">
          {peers.map((peer) => {
            const selectable = enabled && canUseFederationPeer(peer);
            const selected = selectedPeerIds.includes(peer.peer_id);
            return (
              <div
                key={peer.peer_id}
                className="rounded-xl border border-gray-800 bg-gray-950/70 p-4"
              >
                <div className="flex flex-wrap items-start justify-between gap-3">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="truncate text-sm font-medium text-white">
                        {peer.display_name}
                      </span>
                      <span className={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide ${peerStatusClasses(peer)}`}>
                        {peer.online ? 'Online' : 'Offline'}
                      </span>
                      <span className="rounded-full border border-gray-700 bg-gray-800 px-2 py-0.5 text-[10px] uppercase tracking-wide text-gray-400">
                        {peer.source}
                      </span>
                      <span className="rounded-full border border-gray-700 bg-gray-800 px-2 py-0.5 text-[10px] uppercase tracking-wide text-gray-300">
                        assigned {peer.assigned_role}
                      </span>
                    </div>
                    <p className="mt-1 font-mono text-[11px] text-cyan-300">
                      {peer.host}:{peer.api_port} · {peer.delegate_agent}
                    </p>
                    <p className="mt-2 text-xs text-gray-500">{federationPeerBlockReason(peer)}</p>
                  </div>

                  <label className="inline-flex items-center gap-2 text-xs text-gray-300">
                    <input
                      type="checkbox"
                      data-testid={`federation-peer-${peer.peer_id}`}
                      checked={selected}
                      disabled={!selectable || !selectedSessionId}
                      onChange={(event) =>
                        handlePeerToggle(peer.peer_id, event.target.checked)
                      }
                      className="rounded border-gray-600 bg-gray-900 text-cyan-500 focus:ring-cyan-500 disabled:opacity-50"
                    />
                    Use in selected chat
                  </label>
                </div>

                <div className="mt-4 grid gap-3 lg:grid-cols-[minmax(0,1fr)_11rem]">
                  <div className="space-y-2 text-xs text-gray-400">
                    <div className="flex items-start gap-2">
                      <Cpu className="mt-0.5 h-3.5 w-3.5 text-cyan-300" />
                      <span className="break-words">
                        {peer.model_summary || 'Model summary unavailable'}
                      </span>
                    </div>
                    <div className="flex items-start gap-2">
                      <Wrench className="mt-0.5 h-3.5 w-3.5 text-cyan-300" />
                      <span className="break-words">
                        {peer.tool_summary || 'Tool summary unavailable'}
                      </span>
                    </div>
                  </div>

                  <div className="space-y-3">
                    <div>
                      <label htmlFor={`role-${peer.peer_id}`} className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                        Assigned role
                      </label>
                      <select
                        id={`role-${peer.peer_id}`}
                        value={peer.assigned_role}
                        onChange={(event) =>
                          void handleRoleChange(peer.peer_id, event.target.value as FederationRole)
                        }
                        className="w-full rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white focus:border-cyan-500 focus:outline-none"
                      >
                        <option value="master">master</option>
                        <option value="worker">worker</option>
                        <option value="both">both</option>
                        <option value="disabled">disabled</option>
                      </select>
                    </div>
                    <div>
                      <label htmlFor={`priority-${peer.peer_id}`} className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                        Priority
                      </label>
                      <input
                        id={`priority-${peer.peer_id}`}
                        type="number"
                        min={0}
                        max={99}
                        value={peer.priority}
                        onChange={(event) =>
                          void handleHintsChange(
                            peer.peer_id,
                            peer.specialization,
                            Number(event.target.value),
                          )
                        }
                        className="w-full rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white focus:border-cyan-500 focus:outline-none"
                      />
                    </div>
                  </div>
                </div>

                <div className="mt-3">
                  <label htmlFor={`spec-${peer.peer_id}`} className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                    Specialization hint
                  </label>
                  <input
                    id={`spec-${peer.peer_id}`}
                    type="text"
                    value={peer.specialization}
                    placeholder="e.g. use for planning and architecture"
                    onBlur={(event) =>
                      void handleHintsChange(peer.peer_id, event.target.value, peer.priority)
                    }
                    onChange={(event) => handleSpecializationEdit(peer.peer_id, event.target.value)}
                    className="w-full rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white placeholder-gray-600 focus:border-cyan-500 focus:outline-none"
                  />
                </div>
              </div>
            );
          })}
        </div>
      )}
    </section>
  );

  return (
    <div className="h-[calc(100vh-3.5rem)] overflow-y-auto bg-gray-950 p-6">
      <div className="mx-auto max-w-7xl space-y-6">
        {error && (
          <div className="flex items-center gap-2 rounded-xl border border-red-700 bg-red-900/30 px-4 py-3 text-sm text-red-300">
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            {error}
          </div>
        )}

        <section className="grid gap-4 xl:grid-cols-[minmax(0,1.4fr)_minmax(0,1fr)]">
          <div className="rounded-2xl border border-gray-800 bg-gray-900/80 p-5">
            <div className="flex flex-wrap items-center justify-between gap-3">
              <div>
                <div className="flex items-center gap-2 text-sm font-semibold text-white">
                  <Network className="h-4 w-4 text-cyan-400" />
                  Federation Runtime
                </div>
                <p className="mt-1 text-xs text-gray-500">
                  Federation stays visible even when no peers are discovered. Local chat still works
                  on this node by itself.
                </p>
              </div>

              <div className="flex items-center gap-2">
                <button
                  onClick={handleDelegationToggle}
                  disabled={delegationToggling}
                  title={delegationEnabled ? 'Delegation ON — click to disable' : 'Delegation OFF — click to enable'}
                  className={`inline-flex items-center gap-2 rounded-lg border px-3 py-2 text-sm font-medium transition-all disabled:opacity-50 ${
                    delegationEnabled
                      ? 'border-cyan-400/60 bg-cyan-500/15 text-cyan-300 shadow-[0_0_8px_rgba(34,211,238,0.25)]'
                      : 'border-gray-700 bg-gray-900 text-gray-400 hover:border-gray-500 hover:text-gray-200'
                  }`}
                >
                  <span className={`h-2 w-2 rounded-full transition-colors ${delegationEnabled ? 'bg-cyan-400' : 'bg-gray-600'}`} />
                  Delegate {delegationEnabled ? 'ON' : 'OFF'}
                </button>
                <button
                  onClick={() => {
                    setLoading(true);
                    void getFederationPeers()
                      .then((response) => {
                        setFederation(response);
                        setError(null);
                      })
                      .catch(() => {
                        setError('Failed to load federation peers.');
                      })
                      .finally(() => {
                        setLoading(false);
                      });
                  }}
                  className="inline-flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-gray-200 transition-colors hover:border-cyan-400 hover:text-white"
                >
                  <RefreshCw className="h-4 w-4" />
                  Refresh
                </button>
              </div>
            </div>

            <div className="mt-4 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Status</p>
                <div className="mt-2 flex items-center gap-2 text-sm font-medium text-white">
                  {enabled ? (
                    <CheckCircle2 className="h-4 w-4 text-emerald-400" />
                  ) : (
                    <XCircle className="h-4 w-4 text-amber-400" />
                  )}
                  {loading ? 'Checking...' : enabled ? 'Enabled' : 'Disabled'}
                </div>
              </div>

              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Local node</p>
                <p className="mt-2 text-sm font-medium text-white">
                  {federation?.local_node.display_name ?? 'llamafarm-node'}
                </p>
                <p className="mt-1 text-xs text-gray-500">
                  :{federation?.local_node.api_port ?? 42617} · role {federation?.local_node.role ?? 'both'}
                </p>
              </div>

              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Discovery</p>
                <p className="mt-2 text-sm font-medium text-white">
                  {federation?.local_node.discovery_mode ?? 'mdns'}
                </p>
                <p className="mt-1 text-xs text-gray-500">
                  {federation?.local_node.service_name ?? '_llamafarm._tcp'}
                </p>
              </div>

              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Peers</p>
                <p className="mt-2 text-sm font-medium text-white">
                  {onlinePeers}/{availablePeers.length} online
                </p>
                <p className="mt-1 text-xs text-gray-500">
                  {readyWorkers.length} ready worker{readyWorkers.length === 1 ? '' : 's'}
                </p>
              </div>
            </div>

            {!enabled && !loading && (
              <div className="mt-4 rounded-xl border border-amber-500/20 bg-amber-500/10 px-4 py-3 text-sm text-amber-200">
                Federation is disabled in the runtime config. Set
                {' '}
                <code className="rounded bg-gray-950 px-1.5 py-0.5 text-xs text-amber-100">
                  LLAMAFARM_FEDERATION_ENABLED=true
                </code>
                {' '}
                and restart if you want LAN discovery and remote delegation on this node.
              </div>
            )}

            {enabled && !loading && federation?.local_node && isLocalhostBind(federation.local_node.gateway_host) && (
              <div className="mt-4 rounded-xl border border-yellow-500/30 bg-yellow-500/10 px-4 py-3 text-sm text-yellow-200">
                <div className="flex items-start gap-2">
                  <TriangleAlert className="mt-0.5 h-4 w-4 flex-shrink-0 text-yellow-400" />
                  <div className="space-y-1.5">
                    <p className="font-semibold">Gateway bound to localhost — LAN peers cannot connect</p>
                    <p className="text-xs text-yellow-300/80">
                      mDNS will discover peers but they will fail to reach this node at{' '}
                      <code className="rounded bg-gray-950 px-1 text-yellow-100">127.0.0.1:{federation.local_node.api_port}</code>.
                      Add this to your <code className="rounded bg-gray-950 px-1 text-yellow-100">config.toml</code> on{' '}
                      <strong>both</strong> machines and restart:
                    </p>
                    <pre className="mt-1 rounded-lg border border-gray-700 bg-gray-950 p-2 text-xs text-emerald-300 select-all">
{`[gateway]
host = "0.0.0.0"
allow_public_bind = true`}
                    </pre>
                    <p className="text-xs text-yellow-300/60">
                      Or add each node as a manual peer below to bypass mDNS entirely.
                    </p>
                  </div>
                </div>
              </div>
            )}

            {enabled && (
              <div className="mt-4">
                <p className="mb-2 text-[11px] uppercase tracking-wide text-gray-500">Add peer manually</p>
                <form
                  className="flex gap-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    const endpoint = manualPeerInput.trim();
                    if (!endpoint) return;
                    void (async () => {
                      setAddingPeer(true);
                      setAddPeerError(null);
                      try {
                        await addFederationManualPeer(endpoint);
                        setManualPeerInput('');
                        manualPeerRef.current?.focus();
                        setFederation(await getFederationPeers());
                      } catch {
                        setAddPeerError('Failed to add peer — check the address and try again.');
                      } finally {
                        setAddingPeer(false);
                      }
                    })();
                  }}
                >
                  <input
                    ref={manualPeerRef}
                    type="text"
                    value={manualPeerInput}
                    onChange={(e) => setManualPeerInput(e.target.value)}
                    placeholder="192.168.1.154:42617 or http://host:port"
                    className="min-w-0 flex-1 rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white placeholder-gray-600 focus:border-cyan-500 focus:outline-none"
                  />
                  <button
                    type="submit"
                    disabled={addingPeer || !manualPeerInput.trim()}
                    className="inline-flex items-center gap-1.5 rounded-lg border border-cyan-500/40 bg-cyan-500/10 px-3 py-2 text-sm font-medium text-cyan-200 transition-colors hover:border-cyan-400 hover:text-white disabled:opacity-50"
                  >
                    <Plus className="h-4 w-4" />
                    {addingPeer ? 'Adding…' : 'Add'}
                  </button>
                </form>
                {addPeerError && (
                  <p className="mt-1.5 text-xs text-red-400">{addPeerError}</p>
                )}
              </div>
            )}
          </div>

          <div className="rounded-2xl border border-gray-800 bg-gray-900/80 p-5">
            <div className="flex items-center gap-2 text-sm font-semibold text-white">
              <Activity className="h-4 w-4 text-emerald-400" />
              Chat Routing
            </div>
            <p className="mt-1 text-xs text-gray-500">
              Worker selections on this page apply to the chosen saved chat session.
            </p>

            <div className="mt-4">
              <label className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                Selected chat
              </label>
              <select
                data-testid="federation-selected-chat"
                value={selectedSessionId ?? ''}
                onChange={(event) => setSelectedSessionId(event.target.value || null)}
                disabled={chatSessions.length === 0}
                className="w-full rounded-lg border border-gray-700 bg-gray-900 px-3 py-2 text-sm text-white focus:border-cyan-500 focus:outline-none disabled:opacity-50"
              >
                {chatSessions.length === 0 ? (
                  <option value="">No saved chats yet</option>
                ) : (
                  chatSessions.map((session) => (
                    <option key={session.id} value={session.id}>
                      {session.title}
                    </option>
                  ))
                )}
              </select>
            </div>

            <div className="mt-4 grid gap-3 sm:grid-cols-2">
              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Selected workers</p>
                <p className="mt-2 text-sm font-medium text-white">{selectedPeerIds.length}</p>
                <p className="mt-1 text-xs text-gray-500">
                  {currentSession
                    ? `Delegation target set for "${currentSession.title}"`
                    : 'Open Agent and create a saved chat to route workers here.'}
                </p>
              </div>

              <div className="rounded-xl border border-gray-800 bg-gray-950/70 p-3">
                <p className="text-[11px] uppercase tracking-wide text-gray-500">Recent tasks</p>
                <p className="mt-2 text-sm font-medium text-white">{visibleTasks.length}</p>
                <p className="mt-1 text-xs text-gray-500">
                  Remote worker history for the selected chat.
                </p>
              </div>
            </div>

            <Link
              to="/agent"
              className="mt-4 inline-flex items-center gap-2 rounded-lg border border-cyan-500/40 bg-cyan-500/10 px-3 py-2 text-sm font-medium text-cyan-200 transition-colors hover:border-cyan-400 hover:text-white"
            >
              Open agent chat
            </Link>
          </div>
        </section>

        <section className="grid gap-4 xl:grid-cols-[minmax(0,1.5fr)_minmax(0,1fr)]">
          <div className="space-y-4">
            {renderPeerList(
              'Worker Nodes',
              'These peers are online and currently eligible for remote subagent delegation.',
              readyWorkers,
            )}
            {renderPeerList(
              'Standby Nodes',
              'These peers are reachable but not currently routable as workers with the present role or capability settings.',
              standbyPeers,
            )}
            {renderPeerList(
              'Offline Nodes',
              'Previously discovered peers stay visible here when they time out or disappear from the LAN.',
              offlinePeers,
            )}
          </div>

          <section className="rounded-2xl border border-gray-800 bg-gray-900/80 p-4">
            <div className="flex items-center gap-2 text-sm font-semibold text-white">
              <Activity className="h-4 w-4 text-emerald-400" />
              Remote Task History
            </div>
            <p className="mt-1 text-xs text-gray-500">
              Recent delegated worker activity for the currently selected chat.
            </p>

            {visibleTasks.length === 0 ? (
              <p className="mt-4 text-sm text-gray-400">
                No remote task history for this chat yet.
              </p>
            ) : (
              <div className="mt-4 space-y-3">
                {visibleTasks.map((task) => (
                  <div
                    key={task.taskId}
                    data-testid={`federation-task-${task.status}`}
                    data-federation-peer={task.peerId}
                    className="rounded-xl border border-gray-800 bg-gray-950/70 p-3"
                  >
                    <div className="flex flex-wrap items-center justify-between gap-2">
                      <div>
                        <p className="text-sm font-medium text-white">{task.peerName}</p>
                        <p className="font-mono text-[11px] text-cyan-300">
                          {task.delegateAgent}
                        </p>
                      </div>
                      <span
                        className={`rounded-full px-2 py-0.5 text-[10px] uppercase tracking-wide ${taskStatusClasses(task)}`}
                      >
                        {task.status}
                      </span>
                    </div>
                    <p className="mt-2 text-sm text-gray-200">{task.message}</p>
                    {task.content && (
                      <pre className="mt-2 max-h-40 overflow-y-auto whitespace-pre-wrap rounded-lg border border-gray-800 bg-gray-900 p-2 text-xs text-gray-300">
                        {task.content}
                      </pre>
                    )}
                    {task.lastToolName && (
                      <div className="mt-2 rounded-lg border border-gray-800 bg-gray-900 px-3 py-2 text-xs text-gray-400">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="text-gray-200">{task.lastToolName}</span>
                          {typeof task.lastToolSuccess === 'boolean' && (
                            <span>{task.lastToolSuccess ? 'success' : 'failed'}</span>
                          )}
                          {typeof task.lastToolDurationSecs === 'number' && (
                            <span>{task.lastToolDurationSecs}s</span>
                          )}
                        </div>
                        {task.lastToolOutput && (
                          <p className="mt-1 whitespace-pre-wrap break-words">
                            {task.lastToolOutput}
                          </p>
                        )}
                      </div>
                    )}
                    <p className="mt-2 text-[11px] text-gray-600">
                      {task.updatedAt.toLocaleTimeString()}
                    </p>
                  </div>
                ))}
              </div>
            )}
          </section>
        </section>
      </div>
    </div>
  );
}
