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
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([getTools(), getCliTools(), getStatus()])
      .then(([t, c, s]) => {
        setTools(t);
        setCliTools(c);
        setStatus(s);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-lg bg-red-900/30 border border-red-700 p-4 text-red-300">
          Failed to load tools: {error}
        </div>
      </div>
    );
  }

  if (loading || !status) {
    return (
      <div className="flex items-center justify-center h-64">
        <div className="animate-spin rounded-full h-8 w-8 border-2 border-blue-500 border-t-transparent" />
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
    buildOllamaRuntimeCard(status),
    ...commandChecks.map((check) => buildCommandCheckCard(check, availableCli)),
  ];

  return (
    <div className="p-6 space-y-6">
      <div>
        <div className="flex items-center gap-2">
          <Wrench className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">Local Tooling</h2>
        </div>
        <p className="mt-2 text-sm text-gray-400">
          Review the registered agent tools and the binaries this running LlamaFarm deployment can
          actually see inside its local runtime.
        </p>
      </div>

      <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
        {localChecks.map((check) => {
          return (
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
          );
        })}
      </div>

      <div className="relative max-w-md">
        <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-gray-500" />
        <input
          type="text"
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
                    onClick={() =>
                      setExpandedTool(isExpanded ? null : tool.name)
                    }
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
                      <p className="text-xs text-gray-500 mb-2 font-medium uppercase tracking-wider">
                        Parameter Schema
                      </p>
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
