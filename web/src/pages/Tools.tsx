import { useState, useEffect } from 'react';
import {
  Wrench,
  Search,
  ChevronDown,
  ChevronRight,
  Terminal,
  Package,
  CircleCheck,
  CircleAlert,
  Copy,
  RefreshCw,
} from 'lucide-react';
import type { ToolSpec, CliTool, StatusResponse } from '@/types/api';
import { getTools, getCliTools, getStatus } from '@/lib/api';

const commandChecks = [
  {
    id: 'shell',
    label: 'Shell runtime',
    detail: 'Needed for scheduler and shell tools',
    matches: ['bash', 'sh'],
    optional: false,
  },
  {
    id: 'ollama-cli',
    label: 'Ollama CLI',
    detail: 'Optional inside this container; useful for manual smoke checks',
    matches: ['ollama'],
    optional: true,
  },
  {
    id: 'browser',
    label: 'Browser binary',
    detail: 'Optional until you need Chromium-backed browser tools',
    matches: ['chromium', 'chromium-browser', 'google-chrome', 'google-chrome-stable'],
    optional: true,
  },
  {
    id: 'ripgrep',
    label: 'Ripgrep',
    detail: 'Helps file/content search tools behave well',
    matches: ['rg'],
    optional: false,
  },
  {
    id: 'sqlite',
    label: 'SQLite CLI',
    detail: 'Optional; useful for direct local database inspection',
    matches: ['sqlite3'],
    optional: true,
  },
  {
    id: 'curl',
    label: 'HTTP CLI',
    detail: 'Useful for endpoint and local network checks',
    matches: ['curl'],
    optional: false,
  },
];

type CheckTone = 'ok' | 'warn' | 'error';

type LocalCheckCard = {
  id: string;
  label: string;
  detail: string;
  status: string;
  tone: CheckTone;
};

function formatCliCategory(category: string): string {
  return category.replace(/([a-z])([A-Z])/g, '$1 $2');
}

function cardToneClasses(tone: CheckTone): string {
  switch (tone) {
    case 'ok':
      return 'border-green-700/40 bg-green-950/20';
    case 'warn':
      return 'border-yellow-700/40 bg-yellow-950/20';
    case 'error':
      return 'border-red-700/40 bg-red-950/20';
  }
}

function cardIcon(tone: CheckTone) {
  switch (tone) {
    case 'ok':
      return <CircleCheck className="h-4 w-4 text-green-400" />;
    case 'warn':
      return <CircleAlert className="h-4 w-4 text-yellow-400" />;
    case 'error':
      return <CircleAlert className="h-4 w-4 text-red-400" />;
  }
}

function buildCommandCheckCard(
  check: (typeof commandChecks)[number],
  availableCli: Set<string>,
): LocalCheckCard {
  const found = check.matches.some((name) => availableCli.has(name));
  if (found) {
    return {
      id: check.id,
      label: check.label,
      detail: check.detail,
      status: check.optional ? 'Installed locally' : 'Available locally',
      tone: 'ok',
    };
  }

  return {
    id: check.id,
    label: check.label,
    detail: check.detail,
    status: check.optional ? 'Optional locally' : 'Missing locally',
    tone: check.optional ? 'warn' : 'error',
  };
}

function buildOllamaRuntimeCard(status: StatusResponse): LocalCheckCard {
  if (status.ollama.reachable) {
    const runtimeState = status.ollama.active_model_loaded
      ? 'Configured model is loaded in Ollama right now'
      : 'Ollama API is reachable even though the configured model is not loaded yet';
    return {
      id: 'ollama-runtime',
      label: 'Ollama runtime',
      detail: `${runtimeState}. Endpoint: ${status.ollama.endpoint}`,
      status: 'Runtime reachable',
      tone: 'ok',
    };
  }

  return {
    id: 'ollama-runtime',
    label: 'Ollama runtime',
    detail: `LlamaFarm cannot reach the Ollama API at ${status.ollama.endpoint} right now.`,
    status: 'Runtime unreachable',
    tone: 'error',
  };
}

export default function Tools() {
  const [tools, setTools] = useState<ToolSpec[]>([]);
  const [cliTools, setCliTools] = useState<CliTool[]>([]);
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [search, setSearch] = useState('');
  const [expandedTool, setExpandedTool] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [errors, setErrors] = useState<string[]>([]);
  const [toolsLoaded, setToolsLoaded] = useState(false);
  const [cliLoaded, setCliLoaded] = useState(false);
  const [copiedTool, setCopiedTool] = useState<string | null>(null);

  const loadTools = async () => {
    setLoading(true);
    setErrors([]);
    const [toolResult, cliResult, statusResult] = await Promise.allSettled([
      getTools(),
      getCliTools(),
      getStatus(),
    ]);
    const nextErrors: string[] = [];
    if (toolResult.status === 'fulfilled') {
      setTools(toolResult.value);
      setToolsLoaded(true);
    } else {
      nextErrors.push(`Agent tools: ${String(toolResult.reason)}`);
    }
    if (cliResult.status === 'fulfilled') {
      setCliTools(cliResult.value);
      setCliLoaded(true);
    } else {
      nextErrors.push(`Local commands: ${String(cliResult.reason)}`);
    }
    if (statusResult.status === 'fulfilled') {
      setStatus(statusResult.value);
    } else {
      nextErrors.push(`Runtime status: ${String(statusResult.reason)}`);
    }
    setErrors(nextErrors);
    setLoading(false);
  };

  useEffect(() => {
    void loadTools();
  }, []);

  useEffect(() => {
    if (!copiedTool) return;
    const timer = window.setTimeout(() => setCopiedTool(null), 2_000);
    return () => window.clearTimeout(timer);
  }, [copiedTool]);

  const copySchema = async (tool: ToolSpec) => {
    try {
      await navigator.clipboard.writeText(JSON.stringify(tool.parameters ?? {}, null, 2));
      setCopiedTool(tool.name);
    } catch (copyError: unknown) {
      setErrors((previous) => [
        ...previous,
        `Could not copy ${tool.name}: ${copyError instanceof Error ? copyError.message : String(copyError)}`,
      ]);
    }
  };

  if (loading && !toolsLoaded && !cliLoaded && !status) {
    return (
      <div className="flex h-64 items-center justify-center" role="status">
        <RefreshCw className="h-8 w-8 animate-spin text-blue-400" aria-hidden="true" />
        <span className="sr-only">Loading local tooling</span>
      </div>
    );
  }

  const searchTerm = search.toLowerCase();

  const filtered = tools.filter(
    (t) =>
      t.name.toLowerCase().includes(searchTerm) || t.description.toLowerCase().includes(searchTerm),
  );

  const filteredCli = cliTools.filter(
    (t) =>
      t.name.toLowerCase().includes(searchTerm) ||
      formatCliCategory(t.category).toLowerCase().includes(searchTerm),
  );
  const availableCli = new Set(cliTools.map((tool) => tool.name.toLowerCase()));
  const localChecks = [
    ...(status ? [buildOllamaRuntimeCard(status)] : []),
    ...(cliLoaded ? commandChecks.map((check) => buildCommandCheckCard(check, availableCli)) : []),
  ];

  return (
    <div className="space-y-6 p-4 sm:p-6">
      <div className="flex flex-col gap-4 sm:flex-row sm:items-start sm:justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Wrench className="h-5 w-5 text-blue-400" aria-hidden="true" />
            <h2 className="text-base font-semibold text-white">Local Tooling</h2>
          </div>
          <p className="mt-2 max-w-3xl text-sm text-gray-400">
            Review the registered agent tools and the binaries this running LlamaFarm deployment
            can actually see.
          </p>
        </div>
        <button type="button" onClick={() => void loadTools()} disabled={loading} className="inline-flex items-center justify-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 hover:bg-gray-800 hover:text-white disabled:opacity-50">
          <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} aria-hidden="true" />
          Refresh inventory
        </button>
      </div>

      {errors.length > 0 && (
        <div role="alert" className="rounded-lg border border-amber-700/50 bg-amber-950/20 p-3 text-sm text-amber-200">
          <p className="font-medium">Some inventory sources could not be refreshed.</p>
          <ul className="mt-2 list-disc space-y-1 pl-5 text-xs text-amber-300/80">
            {errors.map((error) => <li key={error}>{error}</li>)}
          </ul>
        </div>
      )}

      {localChecks.length > 0 && (
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {localChecks.map((check) => (
            <div
              key={check.id}
              className={`rounded-xl border p-4 ${cardToneClasses(check.tone)}`}
            >
              <div className="flex items-center gap-2">
                {cardIcon(check.tone)}
                <h3 className="text-sm font-semibold text-white">{check.label}</h3>
              </div>
              <p className="mt-2 text-sm text-gray-400">{check.detail}</p>
              <p className="mt-2 text-xs uppercase tracking-[0.18em] text-gray-500">
                {check.status}
              </p>
            </div>
          ))}
        </div>
      )}

      <div className="relative max-w-md">
        <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-gray-500" aria-hidden="true" />
        <label htmlFor="tool-search" className="sr-only">Search tools and commands</label>
        <input
          id="tool-search"
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search tools..."
          className="w-full bg-gray-900 border border-gray-700 rounded-lg pl-10 pr-4 py-2.5 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
        />
      </div>

      <div>
        <div className="flex items-center gap-2 mb-4">
          <Wrench className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">
            Registered Agent Tools ({filtered.length})
          </h2>
        </div>

        {filtered.length === 0 ? (
          <p className="text-sm text-gray-500">No tools match your search.</p>
        ) : (
          <div className="grid grid-cols-1 md:grid-cols-2 xl:grid-cols-3 gap-4">
            {filtered.map((tool) => {
              const isExpanded = expandedTool === tool.name;
              return (
                <div
                  key={tool.name}
                  className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden"
                >
                  <button
                    type="button"
                    onClick={() =>
                      setExpandedTool(isExpanded ? null : tool.name)
                    }
                    aria-expanded={isExpanded}
                    className="w-full text-left p-4 hover:bg-gray-800/50 transition-colors"
                  >
                    <div className="flex items-start justify-between gap-2">
                      <div className="flex items-center gap-2 min-w-0">
                        <Package className="h-4 w-4 text-blue-400 flex-shrink-0 mt-0.5" />
                        <h3 className="text-sm font-semibold text-white truncate">
                          {tool.name}
                        </h3>
                      </div>
                      {isExpanded ? (
                        <ChevronDown className="h-4 w-4 text-gray-400 flex-shrink-0" />
                      ) : (
                        <ChevronRight className="h-4 w-4 text-gray-400 flex-shrink-0" />
                      )}
                    </div>
                    <p className="text-sm text-gray-400 mt-2 line-clamp-2">
                      {tool.description}
                    </p>
                  </button>

                  {isExpanded && tool.parameters && (
                    <div className="border-t border-gray-800 p-4">
                      <div className="mb-2 flex items-center justify-between gap-3">
                        <p className="text-xs font-medium uppercase tracking-wider text-gray-500">Parameter Schema</p>
                        <button type="button" onClick={() => void copySchema(tool)} className="inline-flex items-center gap-1 text-xs text-blue-300 hover:text-blue-200">
                          <Copy className="h-3.5 w-3.5" aria-hidden="true" />
                          {copiedTool === tool.name ? 'Copied' : 'Copy JSON'}
                        </button>
                      </div>
                      <pre className="text-xs text-gray-300 bg-gray-950 rounded-lg p-3 overflow-x-auto max-h-64 overflow-y-auto">
                        {JSON.stringify(tool.parameters, null, 2)}
                      </pre>
                    </div>
                  )}
                </div>
              );
            })}
          </div>
        )}
      </div>

      {filteredCli.length > 0 && (
        <div>
          <div className="flex items-center gap-2 mb-4">
            <Terminal className="h-5 w-5 text-green-400" />
            <h2 className="text-base font-semibold text-white">
              Discovered Local Commands ({filteredCli.length})
            </h2>
          </div>

          <div className="bg-gray-900 rounded-xl border border-gray-800 overflow-hidden">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-gray-800">
                  <th className="text-left px-4 py-3 text-gray-400 font-medium">
                    Name
                  </th>
                  <th className="text-left px-4 py-3 text-gray-400 font-medium">
                    Path
                  </th>
                  <th className="text-left px-4 py-3 text-gray-400 font-medium">
                    Version
                  </th>
                  <th className="text-left px-4 py-3 text-gray-400 font-medium">
                    Category
                  </th>
                </tr>
              </thead>
              <tbody>
                {filteredCli.map((tool) => (
                  <tr
                    key={tool.name}
                    className="border-b border-gray-800/50 hover:bg-gray-800/30 transition-colors"
                  >
                    <td className="px-4 py-3 text-white font-medium">
                      {tool.name}
                    </td>
                    <td className="px-4 py-3 text-gray-400 font-mono text-xs truncate max-w-[200px]">
                      {tool.path}
                    </td>
                    <td className="px-4 py-3 text-gray-400">
                      {tool.version ?? '-'}
                    </td>
                    <td className="px-4 py-3">
                      <span className="inline-flex items-center px-2 py-0.5 rounded-full text-xs font-medium bg-gray-800 text-gray-300 capitalize">
                        {formatCliCategory(tool.category)}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}
