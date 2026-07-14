import { useEffect, useRef, useState } from 'react';
import { Loader2 } from 'lucide-react';
import { execWorkspaceCommand } from '@/lib/api';

export interface AgentShellCall {
  id: string;
  tool: string;
  command: string;
  status: 'running' | 'success' | 'error';
  output?: string;
  durationSecs?: number;
  updatedAt: Date;
  source?: 'agent' | 'user';
}

function statusColor(status: AgentShellCall['status']): string {
  if (status === 'running') return 'text-amber-400';
  if (status === 'error') return 'text-red-400';
  return 'text-emerald-400';
}

let userCallCounter = 0;

export default function TerminalPanel({ calls }: Readonly<{ calls: AgentShellCall[] }>) {
  const bottomRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const [userCalls, setUserCalls] = useState<AgentShellCall[]>([]);
  const [input, setInput] = useState('');
  const [history, setHistory] = useState<string[]>([]);
  const [historyIndex, setHistoryIndex] = useState(-1);

  const ordered = [...calls, ...userCalls].sort(
    (a, b) => a.updatedAt.getTime() - b.updatedAt.getTime(),
  );

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ordered.length, userCalls]);

  const runCommand = async () => {
    const command = input.trim();
    if (!command) return;

    userCallCounter += 1;
    const id = `user_exec_${Date.now()}_${userCallCounter}`;
    setUserCalls((prev) => [
      ...prev.slice(-49),
      { id, tool: 'terminal', command, status: 'running', updatedAt: new Date(), source: 'user' },
    ]);
    setHistory((prev) => [...prev.slice(-49), command]);
    setHistoryIndex(-1);
    setInput('');

    try {
      const result = await execWorkspaceCommand(command);
      const output = [result.stdout, result.stderr].filter(Boolean).join('\n').trim();
      setUserCalls((prev) =>
        prev.map((call) =>
          call.id === id
            ? {
                ...call,
                status: result.exit_code === 0 ? 'success' : 'error',
                output: output || '(no output)',
                durationSecs: Math.round(result.duration_secs * 10) / 10,
                updatedAt: new Date(),
              }
            : call,
        ),
      );
    } catch (err: unknown) {
      const message = err instanceof Error ? err.message : 'Command failed';
      setUserCalls((prev) =>
        prev.map((call) =>
          call.id === id
            ? { ...call, status: 'error', output: message, updatedAt: new Date() }
            : call,
        ),
      );
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      void runCommand();
    } else if (e.key === 'ArrowUp') {
      e.preventDefault();
      if (history.length === 0) return;
      const next = historyIndex === -1 ? history.length - 1 : Math.max(0, historyIndex - 1);
      setHistoryIndex(next);
      setInput(history[next] ?? '');
    } else if (e.key === 'ArrowDown') {
      e.preventDefault();
      if (historyIndex === -1) return;
      const next = historyIndex + 1;
      if (next >= history.length) {
        setHistoryIndex(-1);
        setInput('');
      } else {
        setHistoryIndex(next);
        setInput(history[next] ?? '');
      }
    }
  };

  return (
    <div className="flex h-full min-h-0 flex-col bg-black">
      {/* eslint-disable-next-line jsx-a11y/no-static-element-interactions, jsx-a11y/click-events-have-key-events */}
      <div
        className="flex-1 overflow-y-auto p-2 font-mono text-xs"
        onClick={() => inputRef.current?.focus()}
      >
        {ordered.length === 0 ? (
          <p className="p-2 text-gray-600">
            Type a command below (python uses the workspace .venv; ollama ls / pull / rm manage
            models), or watch the agent's shell / git activity appear here live.
          </p>
        ) : (
          ordered.map((call) => (
            <div key={call.id} className="mb-3">
              <div className="flex items-center gap-1.5">
                <span className={call.source === 'user' ? 'text-cyan-400' : 'text-gray-500'}>
                  {call.source === 'user' ? '❯' : '$'}
                </span>
                <span className="text-gray-100">{call.command}</span>
                {call.status === 'running' && (
                  <Loader2 className="h-3 w-3 flex-shrink-0 animate-spin text-amber-400" />
                )}
                {typeof call.durationSecs === 'number' && (
                  <span className="text-gray-600">({call.durationSecs}s)</span>
                )}
                <span className={`ml-auto flex-shrink-0 uppercase ${statusColor(call.status)}`}>
                  {call.source === 'agent' ? `agent ${call.status}` : call.status}
                </span>
              </div>
              {call.output && (
                <pre className="mt-1 whitespace-pre-wrap break-words border-l-2 border-gray-800 pl-2 text-gray-400">
                  {call.output}
                </pre>
              )}
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>

      <div className="flex flex-shrink-0 items-center gap-1.5 border-t border-gray-800 px-2 py-1.5 font-mono text-xs">
        <span className="text-cyan-400">❯</span>
        <input
          ref={inputRef}
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          placeholder="Run anything: python (.venv), pip, git, ollama ls / pull / rm ..."
          spellCheck={false}
          className="min-w-0 flex-1 bg-transparent text-gray-100 placeholder-gray-600 focus:outline-none"
        />
      </div>
    </div>
  );
}
