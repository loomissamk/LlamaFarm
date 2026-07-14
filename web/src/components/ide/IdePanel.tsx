import { useCallback, useEffect, useRef, useState } from 'react';
import { AlertCircle, Bot, Circle, Loader2, Save, SquareTerminal, X } from 'lucide-react';
import { getWorkspaceFileText, saveWorkspaceFileText } from '@/lib/api';
import FileTree from './FileTree';
import CodeEditor from './CodeEditor';
import TerminalPanel, { type AgentShellCall } from './TerminalPanel';

export interface AgentFileTouch {
  paths: string[];
  nonce: number;
  tool: string;
}

interface OpenTab {
  path: string;
  content: string;
  savedContent: string;
  loading: boolean;
  error: string | null;
}

const FLASH_DURATION_MS = 2500;

export interface IdePanelProps {
  agentTouch?: AgentFileTouch | null;
  shellCalls?: AgentShellCall[];
}

export default function IdePanel({ agentTouch, shellCalls = [] }: Readonly<IdePanelProps>) {
  const [tabs, setTabs] = useState<OpenTab[]>([]);
  const [activePath, setActivePath] = useState<string | null>(null);
  const [terminalOpen, setTerminalOpen] = useState(false);
  const [flashPath, setFlashPath] = useState<string | null>(null);
  const [revealSignal, setRevealSignal] = useState<{ path: string; nonce: number } | null>(null);
  const [lastAgentTool, setLastAgentTool] = useState<string | null>(null);
  const flashTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const seenNonceRef = useRef<number | null>(null);

  const openFile = useCallback(async (path: string, opts?: { forceReload?: boolean }) => {
    setActivePath(path);
    setTabs((prev) => {
      const existing = prev.find((tab) => tab.path === path);
      if (existing && !opts?.forceReload) {
        return prev;
      }
      if (existing) {
        return prev.map((tab) => (tab.path === path ? { ...tab, loading: true, error: null } : tab));
      }
      return [...prev, { path, content: '', savedContent: '', loading: true, error: null }];
    });

    try {
      const content = await getWorkspaceFileText(path);
      setTabs((prev) =>
        prev.map((tab) =>
          tab.path === path ? { path, content, savedContent: content, loading: false, error: null } : tab,
        ),
      );
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : 'Failed to load file';
      setTabs((prev) =>
        prev.map((tab) => (tab.path === path ? { ...tab, loading: false, error: message } : tab)),
      );
    }
  }, []);

  const closeTab = (path: string) => {
    setTabs((prev) => {
      const tab = prev.find((t) => t.path === path);
      if (tab && tab.content !== tab.savedContent) {
        const proceed = window.confirm(`${path} has unsaved changes. Close without saving?`);
        if (!proceed) return prev;
      }
      const next = prev.filter((t) => t.path !== path);
      if (activePath === path) {
        setActivePath(next[next.length - 1]?.path ?? null);
      }
      return next;
    });
  };

  const updateContent = (path: string, content: string) => {
    setTabs((prev) => prev.map((tab) => (tab.path === path ? { ...tab, content } : tab)));
  };

  const saveTab = useCallback(async (path: string) => {
    const tab = tabs.find((t) => t.path === path);
    if (!tab) return;
    try {
      await saveWorkspaceFileText(path, tab.content);
      setTabs((prev) =>
        prev.map((t) => (t.path === path ? { ...t, savedContent: t.content, error: null } : t)),
      );
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : 'Failed to save file';
      setTabs((prev) => prev.map((t) => (t.path === path ? { ...t, error: message } : t)));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [tabs]);

  // Live sync: when the agent's file_write / file_edit / apply_patch tool calls touch a path,
  // open (or reload) it and flash it — mirrors watching Claude Code / Codex edit files in an IDE.
  useEffect(() => {
    if (!agentTouch || agentTouch.paths.length === 0) return;
    if (seenNonceRef.current === agentTouch.nonce) return;
    seenNonceRef.current = agentTouch.nonce;
    setLastAgentTool(agentTouch.tool);

    const paths = agentTouch.paths.slice(0, 5);
    for (const path of paths) {
      void openFile(path, { forceReload: true });
    }

    const lastPath = paths[paths.length - 1] as string;
    setRevealSignal({ path: lastPath, nonce: agentTouch.nonce });
    setFlashPath(lastPath);
    if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
    flashTimerRef.current = setTimeout(() => setFlashPath(null), FLASH_DURATION_MS);
  }, [agentTouch, openFile]);

  useEffect(
    () => () => {
      if (flashTimerRef.current) clearTimeout(flashTimerRef.current);
    },
    [],
  );

  const activeTab = tabs.find((tab) => tab.path === activePath) ?? null;
  const isDirty = (tab: OpenTab) => tab.content !== tab.savedContent;
  const runningShellCount = shellCalls.filter((call) => call.status === 'running').length;

  return (
    <div className="grid h-full min-h-0 grid-cols-[13rem_minmax(0,1fr)]">
      <div className="min-h-0 border-r border-gray-800 bg-gray-950">
        <FileTree
          activePath={activePath}
          flashPath={flashPath}
          revealSignal={revealSignal}
          onOpenFile={(path) => void openFile(path)}
          onFileRenamed={(fromPath, toPath) => {
            setTabs((prev) => prev.map((tab) => (tab.path === fromPath ? { ...tab, path: toPath } : tab)));
            setActivePath((prev) => (prev === fromPath ? toPath : prev));
          }}
          onFileDeleted={(path) => {
            setTabs((prev) => prev.filter((tab) => tab.path !== path));
            setActivePath((prev) => (prev === path ? null : prev));
          }}
        />
      </div>

      <div className="flex min-h-0 flex-col">
        {lastAgentTool && (
          <div className="flex items-center gap-1.5 border-b border-emerald-900/50 bg-emerald-950/20 px-2 py-1 text-[11px] text-emerald-300">
            <Bot className="h-3 w-3 flex-shrink-0" />
            Agent ran <span className="font-mono">{lastAgentTool}</span>
          </div>
        )}

        <div className="flex flex-shrink-0 items-stretch border-b border-gray-800 bg-gray-950">
          <div className="flex flex-1 items-center overflow-x-auto">
            {tabs.map((tab) => {
              const dirty = isDirty(tab);
              return (
                <button
                  key={tab.path}
                  onClick={() => setActivePath(tab.path)}
                  className={`group flex flex-shrink-0 items-center gap-1.5 border-r border-gray-800 px-2.5 py-1.5 text-xs ${
                    activePath === tab.path
                      ? 'bg-gray-900 text-white'
                      : 'text-gray-400 hover:bg-gray-900/60 hover:text-gray-200'
                  }`}
                >
                  {tab.loading && <Loader2 className="h-3 w-3 flex-shrink-0 animate-spin" />}
                  <span className="max-w-[10rem] truncate font-mono">{tab.path.split('/').pop()}</span>
                  {dirty ? (
                    <Circle className="h-2 w-2 flex-shrink-0 fill-current text-amber-400" />
                  ) : (
                    <span
                      role="button"
                      tabIndex={0}
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(tab.path);
                      }}
                      className="rounded p-0.5 opacity-0 hover:bg-gray-700 group-hover:opacity-100"
                    >
                      <X className="h-3 w-3" />
                    </span>
                  )}
                  {dirty && (
                    <span
                      role="button"
                      tabIndex={0}
                      onClick={(e) => {
                        e.stopPropagation();
                        closeTab(tab.path);
                      }}
                      className="rounded p-0.5 hover:bg-gray-700"
                    >
                      <X className="h-3 w-3" />
                    </span>
                  )}
                </button>
              );
            })}
          </div>
          <button
            onClick={() => setTerminalOpen((prev) => !prev)}
            className={`flex flex-shrink-0 items-center gap-1.5 border-l border-gray-800 px-2.5 text-xs transition-colors ${
              terminalOpen ? 'bg-gray-900 text-white' : 'text-gray-400 hover:bg-gray-900/60 hover:text-gray-200'
            }`}
            title="Toggle terminal"
          >
            <SquareTerminal className="h-3.5 w-3.5" />
            Terminal
            {runningShellCount > 0 && (
              <span className="rounded-full bg-amber-500/20 px-1.5 py-0.5 text-[10px] text-amber-300">
                {runningShellCount}
              </span>
            )}
          </button>
        </div>

        <div className="flex min-h-0 flex-1 flex-col">
          {activeTab ? (
            <>
              <div className="flex flex-shrink-0 items-center justify-between gap-2 border-b border-gray-800 bg-gray-900 px-2 py-1">
                <span className="truncate font-mono text-[11px] text-gray-500">{activeTab.path}</span>
                <button
                  onClick={() => void saveTab(activeTab.path)}
                  disabled={!isDirty(activeTab) || activeTab.loading}
                  className="inline-flex flex-shrink-0 items-center gap-1 rounded bg-blue-600 px-2 py-0.5 text-[11px] font-medium text-white hover:bg-blue-700 disabled:opacity-40"
                >
                  <Save className="h-3 w-3" />
                  Save
                  <span className="text-blue-200">⌘S</span>
                </button>
              </div>
              {activeTab.error && (
                <div className="flex items-center gap-1.5 border-b border-red-900/60 bg-red-950/30 px-2 py-1 text-[11px] text-red-300">
                  <AlertCircle className="h-3 w-3 flex-shrink-0" />
                  {activeTab.error}
                </div>
              )}
              <div className="min-h-0 flex-1">
                {activeTab.loading ? (
                  <div className="flex h-full items-center justify-center text-gray-500">
                    <Loader2 className="h-5 w-5 animate-spin" />
                  </div>
                ) : (
                  <CodeEditor
                    path={activeTab.path}
                    value={activeTab.content}
                    onChange={(value) => updateContent(activeTab.path, value)}
                    onSave={() => void saveTab(activeTab.path)}
                  />
                )}
              </div>
            </>
          ) : (
            <div className="flex flex-1 items-center justify-center p-6 text-center text-sm text-gray-600">
              Open a file from the tree, or wait for the agent to edit one — it'll open here
              automatically.
            </div>
          )}
        </div>

        {terminalOpen && (
          <div className="flex h-56 flex-shrink-0 flex-col border-t border-gray-800">
            <div className="flex flex-shrink-0 items-center justify-between border-b border-gray-800 bg-gray-950 px-2 py-1">
              <span className="flex items-center gap-1.5 text-xs font-semibold text-gray-300">
                <SquareTerminal className="h-3.5 w-3.5" />
                Terminal
              </span>
              <button
                onClick={() => setTerminalOpen(false)}
                className="rounded p-0.5 text-gray-500 hover:bg-gray-800 hover:text-white"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
            <div className="min-h-0 flex-1">
              <TerminalPanel calls={shellCalls} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
