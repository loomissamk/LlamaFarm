import { useEffect, useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import {
  Activity,
  AlertCircle,
  CheckCircle2,
  Cpu,
  Network,
  RefreshCw,
  Server,
  Wrench,
  XCircle,
} from 'lucide-react';
import { getFederationPeers, updateFederationPeerRole } from '@/lib/api';
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

export default function FederationPage() {
  const [federation, setFederation] = useState<FederationPeersResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
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

                  <div>
                    <label className="mb-1 block text-[11px] uppercase tracking-wide text-gray-500">
                      Assigned role
                    </label>
                    <select
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
