import { MemoryPanel } from './Memory';
import { useState, useEffect, useCallback, useRef } from 'react';
import {
  Database,
  Table,
  ChevronDown,
  ChevronRight,
  Play,
  RefreshCw,
  AlertCircle,
  Loader2,
  Lock,
  ChevronLeft,
  ChevronRight as ChevronRightIcon,

  Plus,
  Trash2,
  X,
  Pencil,
  CheckCircle,
  Radar,
} from 'lucide-react';
import type {
  DbConnection,
  DbDiscoveryResult,
  DbSchema,
  DbTableInfo,
  DbQueryResult,
} from '@/types/api';
import {
  discoverDbConnections,
  getDbConnections,
  getDbSchema,
  runDbQuery,
  addDbConnection,
  updateDbConnection,
  removeDbConnection,
  testDbConnection,
} from '@/lib/api';
import { pickDiscoveredConnection } from '@/lib/databaseDiscovery';

// ── Driver badge ──────────────────────────────────────────────────────────────

/// Agent memory is browsable like any other datastore, so it appears as a
/// first-class connection in the sidebar rather than a bolted-on panel.
const MEMORY_CONN = '__agent_memory__';

function DriverBadge({ driver }: { driver: string }) {
  const colors: Record<string, string> = {
    sqlite: 'bg-blue-900 text-blue-200',
    postgres: 'bg-indigo-900 text-indigo-200',
    mysql: 'bg-orange-900 text-orange-200',
    mongodb: 'bg-green-900 text-green-200',
    memory: 'bg-violet-900 text-violet-200',
  };
  return (
    <span className={`text-xs px-1.5 py-0.5 rounded font-mono flex-shrink-0 ${colors[driver] ?? 'bg-gray-700 text-gray-300'}`}>
      {driver}
    </span>
  );
}

// ── Shared connection form fields + test button ───────────────────────────────

const URI_PLACEHOLDER: Record<string, string> = {
  mongodb: 'mongodb://192.168.1.154:27017',
  sqlite: '/path/to/database.db',
  postgres: 'postgresql://user:pass@host:5432/db',
  mysql: 'mysql://user:pass@host:3306/db',
};

interface ConnForm {
  name: string; driver: string; uri: string;
  database: string; label: string; read_only: boolean; max_rows: number;
}

function ConnectionFormFields({
  form, set, testStatus, onTest,
}: {
  form: ConnForm;
  set: (k: string, v: string | boolean | number) => void;
  testStatus: { state: 'idle' | 'testing' | 'ok' | 'err'; msg: string };
  onTest: () => void;
}) {
  const usingStoredUri = form.uri === '***MASKED***';
  return (
    <div className="space-y-3">
      <div className="grid grid-cols-2 gap-3">
        <div>
          <label className="text-xs text-gray-400 mb-1 block">Name *</label>
          <input value={form.name} onChange={(e) => set('name', e.target.value)} placeholder="arxiv" className="w-full bg-gray-800 border border-gray-700 rounded px-2.5 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-blue-500" />
        </div>
        <div>
          <label className="text-xs text-gray-400 mb-1 block">Driver</label>
          <select value={form.driver} onChange={(e) => set('driver', e.target.value)} className="w-full bg-gray-800 border border-gray-700 rounded px-2.5 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-blue-500">
            <option value="mongodb">MongoDB</option>
            <option value="sqlite">SQLite</option>
            <option value="postgres">PostgreSQL</option>
            <option value="mysql">MySQL</option>
          </select>
        </div>
      </div>

      <div>
        <div className="flex items-center justify-between mb-1">
          <label className="text-xs text-gray-400">
            {usingStoredUri ? 'Connection URI (stored)' : 'Connection URI *'}
          </label>
          <button
            type="button"
            onClick={onTest}
            disabled={!form.uri.trim() || testStatus.state === 'testing'}
            className="text-xs px-2 py-0.5 rounded bg-gray-800 hover:bg-gray-700 text-gray-400 hover:text-gray-200 disabled:opacity-40 border border-gray-700"
          >
            {testStatus.state === 'testing' ? <Loader2 className="h-3 w-3 animate-spin inline mr-1" /> : null}
            Test
          </button>
        </div>
        <input
          value={form.uri}
          onChange={(e) => set('uri', e.target.value)}
          placeholder={usingStoredUri ? 'Enter a new URI only to replace the stored connection' : (URI_PLACEHOLDER[form.driver] ?? '')}
          className="w-full bg-gray-800 border border-gray-700 rounded px-2.5 py-1.5 text-sm text-gray-200 font-mono focus:outline-none focus:border-blue-500"
        />
        {usingStoredUri && (
          <p className="mt-1 text-xs text-gray-500">
            The saved URI remains active. Replace the masked value only when the host or credentials changed.
          </p>
        )}
        {testStatus.state === 'ok' && (
          <p className="mt-1 text-xs text-green-400 flex items-center gap-1"><CheckCircle className="h-3 w-3" />{testStatus.msg}</p>
        )}
        {testStatus.state === 'err' && (
          <p className="mt-1 text-xs text-red-400">{testStatus.msg}</p>
        )}
      </div>

      {form.driver === 'mongodb' && (
        <div>
          <label className="text-xs text-gray-400 mb-1 block">Database name</label>
          <input value={form.database} onChange={(e) => set('database', e.target.value)} placeholder="MyDatabase" className="w-full bg-gray-800 border border-gray-700 rounded px-2.5 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-blue-500" />
        </div>
      )}

      <div>
        <label className="text-xs text-gray-400 mb-1 block">Display label</label>
        <input value={form.label} onChange={(e) => set('label', e.target.value)} placeholder={form.name || 'My Database'} className="w-full bg-gray-800 border border-gray-700 rounded px-2.5 py-1.5 text-sm text-gray-200 focus:outline-none focus:border-blue-500" />
      </div>

      <div className="flex items-center gap-4">
        <label className="flex items-center gap-2 text-sm text-gray-300 cursor-pointer">
          <input type="checkbox" checked={form.read_only} onChange={(e) => set('read_only', e.target.checked)} className="rounded" />
          Read-only
        </label>
        <div className="flex items-center gap-2 text-sm text-gray-300">
          <span className="text-xs text-gray-400">Max rows</span>
          <input type="number" min={1} max={5000} value={form.max_rows} onChange={(e) => set('max_rows', parseInt(e.target.value) || 500)} className="w-20 bg-gray-800 border border-gray-700 rounded px-2 py-1 text-sm text-gray-200 focus:outline-none focus:border-blue-500" />
        </div>
      </div>
    </div>
  );
}

// ── Add connection modal ──────────────────────────────────────────────────────

function AddConnectionModal({ onClose, onAdd }: { onClose: () => void; onAdd: (c: DbConnection) => void }) {
  const [form, setForm] = useState<ConnForm>({
    name: '', driver: 'mongodb', uri: '', database: '', label: '', read_only: true, max_rows: 500,
  });
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [testStatus, setTestStatus] = useState<{ state: 'idle' | 'testing' | 'ok' | 'err'; msg: string }>({ state: 'idle', msg: '' });

  const set = (k: string, v: string | boolean | number) => setForm((f) => ({ ...f, [k]: v }));

  const handleTest = async () => {
    setTestStatus({ state: 'testing', msg: '' });
    try {
      const r = await testDbConnection({ name: form.name || 'test', driver: form.driver as DbConnection['driver'], uri: form.uri, database: form.database || null });
      if (r.ok) setTestStatus({ state: 'ok', msg: `Connected · ${r.tables ?? 0} ${r.driver === 'mongodb' ? 'collections' : 'tables'}${r.database ? ` · ${r.database}` : ''}` });
      else setTestStatus({ state: 'err', msg: r.error ?? 'Connection failed' });
    } catch (e) { setTestStatus({ state: 'err', msg: String(e) }); }
  };

  const save = async () => {
    if (!form.name.trim() || !form.uri.trim()) { setErr('Name and URI are required'); return; }
    setSaving(true); setErr(null);
    try {
      await addDbConnection({ ...form, driver: form.driver as DbConnection['driver'], database: form.database || null, label: form.label || form.name });
      onAdd({ ...form, driver: form.driver as DbConnection['driver'], database: form.database || null, label: form.label || form.name, uri: form.uri });
      onClose();
    } catch (e) { setErr(String(e)); }
    finally { setSaving(false); }
  };

  return (
    <div className="fixed inset-0 bg-black/60 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="bg-gray-900 border border-gray-700 rounded-xl w-full max-w-md p-6 shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-5">
          <h2 className="text-white font-semibold text-lg">Add Connection</h2>
          <button onClick={onClose} className="text-gray-500 hover:text-gray-300"><X className="h-5 w-5" /></button>
        </div>

        <ConnectionFormFields form={form} set={set} testStatus={testStatus} onTest={handleTest} />

        {err && <p className="mt-3 text-red-400 text-xs">{err}</p>}

        <div className="flex justify-end gap-2 mt-5">
          <button onClick={onClose} className="px-4 py-1.5 text-sm text-gray-400 hover:text-gray-200">Cancel</button>
          <button onClick={save} disabled={saving} className="px-4 py-1.5 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded disabled:opacity-50">
            {saving ? <Loader2 className="h-4 w-4 animate-spin inline mr-1" /> : null}
            Add Connection
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Edit connection modal ─────────────────────────────────────────────────────

function EditConnectionModal({ conn, onClose, onSave }: { conn: DbConnection; onClose: () => void; onSave: (updated: DbConnection) => void }) {
  const [form, setForm] = useState<ConnForm>({
    name: conn.name,
    driver: conn.driver,
    uri: conn.uri ?? '',
    database: conn.database ?? '',
    label: conn.label ?? '',
    read_only: conn.read_only,
    max_rows: conn.max_rows,
  });
  const [saving, setSaving] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const [testStatus, setTestStatus] = useState<{ state: 'idle' | 'testing' | 'ok' | 'err'; msg: string }>({ state: 'idle', msg: '' });

  const set = (k: string, v: string | boolean | number) => setForm((f) => ({ ...f, [k]: v }));

  const handleTest = async () => {
    setTestStatus({ state: 'testing', msg: '' });
    try {
      // A masked URI is resolved by the connection's currently persisted name.
      // A pending rename must not make the Test action lose that stored value.
      const r = await testDbConnection({ name: conn.name, driver: form.driver as DbConnection['driver'], uri: form.uri, database: form.database || null });
      if (r.ok) setTestStatus({ state: 'ok', msg: `Connected · ${r.tables ?? 0} ${r.driver === 'mongodb' ? 'collections' : 'tables'}${r.database ? ` · ${r.database}` : ''}` });
      else setTestStatus({ state: 'err', msg: r.error ?? 'Connection failed' });
    } catch (e) { setTestStatus({ state: 'err', msg: String(e) }); }
  };

  const save = async () => {
    if (!form.name.trim() || !form.uri.trim()) { setErr('Name and URI are required'); return; }
    setSaving(true); setErr(null);
    try {
      await updateDbConnection(conn.name, { ...form, driver: form.driver as DbConnection['driver'], database: form.database || null, label: form.label || form.name });
      onSave({ ...form, driver: form.driver as DbConnection['driver'], database: form.database || null, label: form.label || form.name, uri: form.uri });
      onClose();
    } catch (e) { setErr(String(e)); }
    finally { setSaving(false); }
  };

  return (
    <div className="fixed inset-0 bg-black/60 z-50 flex items-center justify-center" onClick={onClose}>
      <div className="bg-gray-900 border border-gray-700 rounded-xl w-full max-w-md p-6 shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <div className="flex items-center justify-between mb-5">
          <h2 className="text-white font-semibold text-lg">Edit Connection</h2>
          <button onClick={onClose} className="text-gray-500 hover:text-gray-300"><X className="h-5 w-5" /></button>
        </div>

        <ConnectionFormFields form={form} set={set} testStatus={testStatus} onTest={handleTest} />

        {err && <p className="mt-3 text-red-400 text-xs">{err}</p>}

        <div className="flex justify-end gap-2 mt-5">
          <button onClick={onClose} className="px-4 py-1.5 text-sm text-gray-400 hover:text-gray-200">Cancel</button>
          <button onClick={save} disabled={saving} className="px-4 py-1.5 text-sm bg-blue-600 hover:bg-blue-500 text-white rounded disabled:opacity-50">
            {saving ? <Loader2 className="h-4 w-4 animate-spin inline mr-1" /> : null}
            Save Changes
          </button>
        </div>
      </div>
    </div>
  );
}

// ── Schema tree ───────────────────────────────────────────────────────────────

function SchemaTree({ schema, activeTable, onClickTable }: {
  schema: DbSchema;
  activeTable: string | null;
  onClickTable: (t: DbTableInfo) => void;
}) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>(() => {
    const init: Record<string, boolean> = {};
    schema.tables.forEach((t) => { init[t.name] = true; });
    return init;
  });

  if (schema.tables.length === 0) {
    return <p className="text-gray-500 text-xs px-3 py-2">No tables or collections found.</p>;
  }

  return (
    <div className="text-sm select-none">
      {schema.tables.map((table) => (
        <div key={table.name}>
          <div
            className={`flex items-center gap-1 px-2 py-1 cursor-pointer group ${activeTable === table.name ? 'bg-blue-600/20 border-l-2 border-blue-500' : 'hover:bg-gray-800'}`}
            onClick={() => onClickTable(table)}
          >
            <button
              className="flex-shrink-0 text-gray-500"
              onClick={(e) => { e.stopPropagation(); setExpanded((p) => ({ ...p, [table.name]: !p[table.name] })); }}
            >
              {expanded[table.name]
                ? <ChevronDown className="h-3 w-3" />
                : <ChevronRight className="h-3 w-3" />}
            </button>
            <Table className="h-3 w-3 text-blue-400 flex-shrink-0" />
            <span className="text-gray-200 font-mono text-xs truncate flex-1">{table.name}</span>
            <span className="text-gray-600 text-xs opacity-0 group-hover:opacity-60">{table.kind}</span>
          </div>
          {expanded[table.name] && (
            <div className="ml-6 border-l border-gray-800 mb-0.5">
              {table.columns.map((col) => (
                <div key={col.name} className="flex items-baseline gap-2 px-2 py-0.5 text-xs">
                  <span className="text-gray-400 font-mono">{col.name}</span>
                  <span className="text-gray-600 font-mono">{col.data_type}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      ))}
    </div>
  );
}

// ── Pagination helpers ────────────────────────────────────────────────────────

const PAGE_SIZES = [25, 50, 100, 250] as const;

/** Inject skip+limit into a query string without touching the user's editor text. */
function injectPagination(query: string, driver: string, offset: number, pageSize: number): string {
  if (driver === 'mongodb') {
    try {
      const parsed = JSON.parse(query);
      parsed.limit = pageSize;
      if (offset > 0) parsed.skip = offset; else delete parsed.skip;
      return JSON.stringify(parsed, null, 2);
    } catch { return query; }
  }
  // SQL: strip existing LIMIT/OFFSET, then append
  const stripped = query
    .replace(/\s+LIMIT\s+\d+(\s+OFFSET\s+\d+)?/gi, '')
    .replace(/\s+OFFSET\s+\d+/gi, '')
    .trimEnd().replace(/;$/, '');
  return offset > 0
    ? `${stripped}\nLIMIT ${pageSize} OFFSET ${offset};`
    : `${stripped}\nLIMIT ${pageSize};`;
}

// ── Results table (display only — pagination is handled by DatabasePage) ──────

function ResultsTable({ result, offset }: { result: DbQueryResult; offset: number }) {
  if (result.row_count === 0) return <p className="text-gray-500 text-sm p-4">No rows returned.</p>;

  return (
    <div className="overflow-auto flex-1 relative">
      <table className="text-xs font-mono border-collapse" style={{ minWidth: 'max-content', width: '100%' }}>
        <thead className="sticky top-0 z-20">
          <tr>
            {/* sticky # column */}
            <th className="sticky left-0 z-30 bg-gray-900 px-2 py-1.5 text-gray-600 border-b border-r border-gray-700 w-10 text-right select-none">
              #
            </th>
            {result.columns.map((col) => (
              <th key={col} className="bg-gray-900 text-left px-3 py-1.5 text-gray-300 border-b border-gray-700 whitespace-nowrap font-semibold">
                {col}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {result.rows.map((row, ri) => (
            <tr key={ri} className="hover:bg-gray-800/50 border-b border-gray-800/40 group">
              <td className="sticky left-0 z-10 bg-gray-950 group-hover:bg-gray-800/50 px-2 py-1 text-gray-700 text-right tabular-nums select-none align-top border-r border-gray-800">
                {offset + ri + 1}
              </td>
              {row.map((cell, ci) => {
                const raw = cell === null ? null : typeof cell === 'object' ? JSON.stringify(cell) : String(cell);
                return (
                  <td key={ci} className="px-3 py-1 align-top max-w-xs">
                    <CellValue raw={raw} />
                  </td>
                );
              })}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function CellValue({ raw }: { raw: string | null }) {
  const [expanded, setExpanded] = useState(false);
  if (raw === null) return <span className="text-gray-600 italic">NULL</span>;

  const isJsonObj = (raw.startsWith('{') || raw.startsWith('[')) && raw.length > 4;
  const TRUNCATE = 120;
  const long = raw.length > TRUNCATE;

  // Collapsed JSON: show a summary badge instead of a blob
  if (isJsonObj && !expanded) {
    let summary = raw.startsWith('[') ? '[ … ]' : '{ … }';
    try {
      const p = JSON.parse(raw);
      if (Array.isArray(p)) summary = `[ ${p.length} item${p.length !== 1 ? 's' : ''} ]`;
      else { const keys = Object.keys(p); summary = `{ ${keys.slice(0, 3).join(', ')}${keys.length > 3 ? ', …' : ''} }`; }
    } catch { /* leave default */ }
    return (
      <span className="text-blue-400/70 italic cursor-pointer hover:text-blue-300 select-none" onClick={() => setExpanded(true)}>
        {summary}
      </span>
    );
  }

  if (isJsonObj && expanded) {
    let pretty = raw;
    try { pretty = JSON.stringify(JSON.parse(raw), null, 2); } catch { /* use raw */ }
    return (
      <span className="text-gray-300 break-all whitespace-pre-wrap font-mono text-xs">
        {pretty}
        <button onClick={() => setExpanded(false)} className="ml-2 text-blue-500 hover:text-blue-400 text-xs underline not-italic">less</button>
      </span>
    );
  }

  const shown = long && !expanded ? raw.slice(0, TRUNCATE) + '…' : raw;
  return (
    <span className="text-gray-300 break-all">
      {shown}
      {long && (
        <button onClick={() => setExpanded((e) => !e)} className="ml-1 text-blue-500 hover:text-blue-400 text-xs underline">
          {expanded ? 'less' : 'more'}
        </button>
      )}
    </span>
  );
}

// ── Main page ─────────────────────────────────────────────────────────────────

export default function DatabasePage() {
  const [connections, setConnections] = useState<DbConnection[]>([]);
  const [loadingConns, setLoadingConns] = useState(true);
  const [connsError, setConnsError] = useState<string | null>(null);
  const [showAddModal, setShowAddModal] = useState(false);
  const [scanning, setScanning] = useState(false);
  const [discovered, setDiscovered] = useState<DbDiscoveryResult[] | null>(null);
  const [editConn, setEditConn] = useState<DbConnection | null>(null);

  const [activeConn, setActiveConn] = useState<string | null>(null);
  const [connectionRevision, setConnectionRevision] = useState(0);
  const [schema, setSchema] = useState<DbSchema | null>(null);
  const [loadingSchema, setLoadingSchema] = useState(false);
  const [schemaError, setSchemaError] = useState<string | null>(null);
  const [activeTable, setActiveTable] = useState<string | null>(null);

  const [query, setQuery] = useState('');
  const [pageSize, setPageSize] = useState<number>(50);
  const [pageOffset, setPageOffset] = useState(0);
  const [running, setRunning] = useState(false);
  const [result, setResult] = useState<DbQueryResult | null>(null);
  const [queryError, setQueryError] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const discoveryProbeRef = useRef(
    new Map<string, { schema?: DbSchema; error?: string }>(),
  );

  const loadConnections = async () => {
    setLoadingConns(true);
    try {
      const response = await getDbConnections();
      setConnections(response.connections);
      const first = response.connections[0];
      if (first && !activeConn) setActiveConn(first.name);
    } catch (error) {
      setConnsError(String(error));
    } finally {
      setLoadingConns(false);
    }
  };

  useEffect(() => {
    // Reconcile reachable open databases as soon as the explorer opens. The
    // same pass schema-probes saved connections and feeds auth/routing
    // failures into the existing Update connection / Retry UI.
    void loadConnections().then(handleScan);
  }, []);

  useEffect(() => {
    if (!activeConn || activeConn === MEMORY_CONN) return;
    setSchema(null); setSchemaError(null); setLoadingSchema(true);
    setResult(null); setQueryError(null); setActiveTable(null); setQuery('');
    const connName = activeConn;
    const applySchema = (loadedSchema: DbSchema) => {
      setSchema(loadedSchema);
      // Auto-browse first table on connect (Compass-style).
      if (loadedSchema.tables.length > 0) {
        const conn = connections.find((candidate) => candidate.name === connName);
        const table = loadedSchema.tables[0];
        if (!conn || !table) return;
        setActiveTable(table.name);
        const nextQuery = conn.driver === 'mongodb'
          ? JSON.stringify({ collection: table.name, filter: {} }, null, 2)
          : `SELECT *\nFROM ${table.name};`;
        setQuery(nextQuery);
        setPageOffset(0);
        runQuery(nextQuery, connName, 0, pageSize);
      }
    };

    const discoveryProbe = discoveryProbeRef.current.get(connName);
    if (discoveryProbe) {
      discoveryProbeRef.current.delete(connName);
      if (discoveryProbe.schema) applySchema(discoveryProbe.schema);
      if (discoveryProbe.error) setSchemaError(discoveryProbe.error);
      setLoadingSchema(false);
      return;
    }

    getDbSchema(connName)
      .then(applySchema)
      .catch((e) => setSchemaError(String(e)))
      .finally(() => setLoadingSchema(false));
  }, [activeConn, connectionRevision]);

  const runQuery = useCallback(async (q: string, connName: string, offset: number, ps: number) => {
    if (!connName || !q.trim()) return;
    const conn = connections.find((c) => c.name === connName);
    const paginated = conn ? injectPagination(q, conn.driver, offset, ps) : q;
    setRunning(true); setResult(null); setQueryError(null);
    try { setResult(await runDbQuery(connName, paginated, ps)); }
    catch (e) { setQueryError(String(e)); }
    finally { setRunning(false); }
  }, [connections]);

  const handleRun = useCallback(() => {
    if (activeConn && query.trim()) {
      setPageOffset(0);
      runQuery(query, activeConn, 0, pageSize);
    }
  }, [activeConn, query, pageSize, runQuery]);

  const handlePage = useCallback((newOffset: number) => {
    if (!activeConn || !query.trim()) return;
    setPageOffset(newOffset);
    runQuery(query, activeConn, newOffset, pageSize);
  }, [activeConn, query, pageSize, runQuery]);

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') { e.preventDefault(); handleRun(); }
  };

  // Clicking a table immediately loads it
  const handleClickTable = useCallback((table: DbTableInfo) => {
    const conn = connections.find((c) => c.name === activeConn);
    if (!conn || !activeConn) return;
    setActiveTable(table.name);
    const q = conn.driver === 'mongodb'
      ? JSON.stringify({ collection: table.name, filter: {} }, null, 2)
      : `SELECT *\nFROM ${table.name};`;
    setQuery(q);
    setPageOffset(0);
    runQuery(q, activeConn, 0, pageSize);
  }, [activeConn, connections, pageSize, runQuery]);

  const handleRemoveConn = async (name: string) => {
    try {
      await removeDbConnection(name);
      setConnections((cs) => cs.filter((c) => c.name !== name));
      if (activeConn === name) { setActiveConn(null); setSchema(null); setResult(null); }
    } catch (e) { alert(String(e)); }
  };

  const activeCfg = connections.find((c) => c.name === activeConn);
  const isMongo = activeCfg?.driver === 'mongodb';

  async function handleScan() {
    setScanning(true);
    setConnsError(null);
    try {
      const res = await discoverDbConnections();
      setDiscovered(res.discovered);
      const refreshed = await getDbConnections();
      setConnections(refreshed.connections);

      for (const item of res.discovered) {
        if (!item.connection_name) continue;
        if (item.schema) {
          discoveryProbeRef.current.set(item.connection_name, { schema: item.schema });
        } else if (item.error) {
          discoveryProbeRef.current.set(item.connection_name, { error: item.error });
        }
      }

      const selected = pickDiscoveredConnection(res.discovered);
      if (selected) {
        setActiveConn(selected);
        setConnectionRevision((revision) => revision + 1);
      }
    } catch (e) {
      setConnsError(e instanceof Error ? e.message : 'Network scan failed');
    } finally {
      setScanning(false);
    }
  }

  if (loadingConns) return (
    <div className="flex items-center justify-center h-64 text-gray-400">
      <Loader2 className="h-6 w-6 animate-spin mr-2" /> Loading…
    </div>
  );

  return (
    <div className="flex h-[calc(100vh-4rem)] overflow-hidden">
      {showAddModal && (
        <AddConnectionModal
          onClose={() => setShowAddModal(false)}
          onAdd={(c) => {
            setConnections((cs) => [...cs, { ...c, uri: '***MASKED***' }]);
            setActiveConn(c.name);
          }}
        />
      )}
      {editConn && (
        <EditConnectionModal
          conn={editConn}
          onClose={() => setEditConn(null)}
          onSave={(updated) => {
            const maskedUpdate = { ...updated, uri: '***MASKED***' };
            setConnections((cs) => cs.map((c) => c.name === editConn.name ? maskedUpdate : c));
            if (activeConn === editConn.name) setActiveConn(updated.name);
            setConnectionRevision((revision) => revision + 1);
            setEditConn(null);
          }}
        />
      )}

      {/* Left sidebar */}
      <div className="w-52 flex-shrink-0 border-r border-gray-800 flex flex-col bg-gray-950 overflow-hidden">
        {/* Connections header */}
        <div className="flex items-center gap-1.5 px-2 py-2 border-b border-gray-800 flex-shrink-0">
          <Database className="h-3.5 w-3.5 text-gray-500" />
          <span className="text-xs font-semibold text-gray-500 uppercase tracking-wider flex-1">Connections</span>
          <button
            onClick={handleScan}
            disabled={scanning}
            className="p-0.5 text-gray-600 hover:text-blue-400 rounded disabled:opacity-40"
            title="Scan the local network for databases"
          >
            {scanning ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Radar className="h-3.5 w-3.5" />}
          </button>
          <button
            onClick={() => setShowAddModal(true)}
            className="p-0.5 text-gray-600 hover:text-blue-400 rounded"
            title="Add connection"
          >
            <Plus className="h-3.5 w-3.5" />
          </button>
        </div>

        {connsError && <p className="text-red-400 text-xs px-3 py-2">{connsError}</p>}

        {/* Connection list */}
        <div className="border-b border-gray-800 flex-shrink-0">
          {/* Agent memory: always present, browsable and clearable like a DB */}
          <div
            className={`flex items-center gap-1.5 px-2 py-1.5 cursor-pointer ${activeConn === MEMORY_CONN ? 'bg-blue-600/20 border-l-2 border-blue-500' : 'hover:bg-gray-800 border-l-2 border-transparent'}`}
            onClick={() => setActiveConn(MEMORY_CONN)}
          >
            <span className="text-gray-200 text-xs font-medium truncate flex-1">Agent Memory</span>
            <DriverBadge driver="memory" />
          </div>
          {connections.length === 0 ? (
            <button
              onClick={() => setShowAddModal(true)}
              className="w-full flex items-center gap-2 px-3 py-3 text-gray-500 hover:text-gray-300 text-xs text-left"
            >
              <Plus className="h-3.5 w-3.5" /> Add a connection
            </button>
          ) : connections.map((conn) => (
            <div
              key={conn.name}
              className={`flex items-center gap-1.5 px-2 py-1.5 cursor-pointer group ${activeConn === conn.name ? 'bg-blue-600/20 border-l-2 border-blue-500' : 'hover:bg-gray-800 border-l-2 border-transparent'}`}
              onClick={() => setActiveConn(conn.name)}
            >
              <span className="text-gray-200 text-xs font-medium truncate flex-1">{conn.label ?? conn.name}</span>
              <DriverBadge driver={conn.driver} />
              {conn.read_only && <Lock className="h-3 w-3 text-gray-600 flex-shrink-0" />}
              <button
                className="opacity-0 group-hover:opacity-100 text-gray-600 hover:text-blue-400 flex-shrink-0"
                onClick={(e) => { e.stopPropagation(); setEditConn(conn); }}
                title="Edit connection"
              >
                <Pencil className="h-3 w-3" />
              </button>
              <button
                className="opacity-0 group-hover:opacity-100 text-gray-600 hover:text-red-400 flex-shrink-0"
                onClick={(e) => { e.stopPropagation(); if (confirm(`Remove "${conn.name}"?`)) handleRemoveConn(conn.name); }}
                title="Remove connection"
              >
                <Trash2 className="h-3 w-3" />
              </button>
            </div>
          ))}
        </div>

        {/* Discovered on the network */}
        {discovered && (
          <div className="border-b border-gray-800 flex-shrink-0">
            <div className="flex items-center gap-1.5 px-2 py-1.5">
              <span className="text-xs font-semibold text-gray-500 uppercase tracking-wider flex-1">
                Found on network ({discovered.length})
              </span>
              <button
                onClick={() => setDiscovered(null)}
                className="text-gray-600 hover:text-gray-300 text-xs"
                title="Dismiss"
              >
                ✕
              </button>
            </div>
            {discovered.length === 0 ? (
              <p className="px-3 pb-2 text-xs text-gray-600">
                No databases found on this network.
              </p>
            ) : (
              discovered.map((d) => (
                <div key={`${d.host}:${d.port}`} className="border-t border-gray-900">
                  <button
                    onClick={() => {
                      if (!d.connection_name) return;
                      if (d.schema) {
                        discoveryProbeRef.current.set(d.connection_name, { schema: d.schema });
                      } else if (d.error) {
                        discoveryProbeRef.current.set(d.connection_name, { error: d.error });
                      }
                      setActiveConn(d.connection_name);
                      setConnectionRevision((revision) => revision + 1);
                    }}
                    disabled={!d.connection_name}
                    className="w-full flex items-center gap-1.5 px-2 py-1.5 hover:bg-gray-800 text-left disabled:cursor-default disabled:hover:bg-transparent"
                    title={d.error ?? (d.status === 'connected' ? 'Connected and schema loaded' : 'Discovered database')}
                  >
                    <span className="text-gray-300 text-xs font-mono truncate flex-1">
                      {d.host}:{d.port}
                    </span>
                    {d.status === 'connected' && (
                      <span className="text-[10px] px-1 py-0.5 rounded bg-green-900/60 text-green-300 flex-shrink-0">
                        connected
                      </span>
                    )}
                    {d.status === 'needs_configuration' && (
                      <span className="text-[10px] px-1 py-0.5 rounded bg-amber-900/60 text-amber-300 flex-shrink-0">
                        setup
                      </span>
                    )}
                    {d.status === 'unsupported' && (
                      <span className="text-[10px] px-1 py-0.5 rounded bg-gray-800 text-gray-400 flex-shrink-0">
                        unsupported
                      </span>
                    )}
                    <DriverBadge driver={d.driver} />
                  </button>
                  {d.error && (
                    <p className="px-2 pb-1.5 text-[10px] leading-4 text-amber-400 break-words">
                      {d.error}
                    </p>
                  )}
                </div>
              ))
            )}
          </div>
        )}

        {/* Schema tree — not applicable to the memory store */}
        {activeConn !== MEMORY_CONN && (
        <>
        <div className="flex items-center gap-1.5 px-2 py-1.5 border-b border-gray-800 flex-shrink-0">
          <span className="text-xs font-semibold text-gray-500 uppercase tracking-wider flex-1">Schema</span>
          <button onClick={() => { if (activeConn) { setLoadingSchema(true); getDbSchema(activeConn).then(setSchema).catch((e) => setSchemaError(String(e))).finally(() => setLoadingSchema(false)); } }} disabled={!activeConn || loadingSchema} className="text-gray-600 hover:text-gray-400 disabled:opacity-30">
            <RefreshCw className={`h-3 w-3 ${loadingSchema ? 'animate-spin' : ''}`} />
          </button>
        </div>

        <div className="flex-1 overflow-y-auto">
          {loadingSchema && <div className="flex items-center gap-2 px-3 py-3 text-gray-500 text-xs"><Loader2 className="h-3 w-3 animate-spin" /> Loading…</div>}
          {schemaError && (
            <div className="m-2 rounded border border-red-900/60 bg-red-950/30 p-2 text-xs">
              <p className="text-red-400">Auto-connect failed: {schemaError}</p>
              <p className="mt-1 text-gray-500">
                Confirm how this database should be reached, then update the saved connection.
              </p>
              <div className="mt-2 flex gap-2">
                <button
                  onClick={() => activeCfg && setEditConn(activeCfg)}
                  disabled={!activeCfg}
                  className="rounded bg-blue-600 px-2 py-1 text-white disabled:opacity-40"
                >
                  Update connection
                </button>
                <button
                  onClick={() => setConnectionRevision((revision) => revision + 1)}
                  className="rounded border border-gray-700 px-2 py-1 text-gray-300"
                >
                  Retry
                </button>
              </div>
            </div>
          )}
          {schema && !loadingSchema && (
            <SchemaTree schema={schema} activeTable={activeTable} onClickTable={handleClickTable} />
          )}
          {!activeConn && !loadingConns && (
            <p className="text-gray-600 text-xs px-3 py-3">Select a connection</p>
          )}
        </div>
        </>
        )}
      </div>

      {/* Main panel: agent memory browser, or the SQL/Mongo explorer */}
      {activeConn === MEMORY_CONN ? (
        <div className="flex-1 overflow-y-auto p-6">
          <MemoryPanel />
        </div>
      ) : (
      <div className="flex-1 flex flex-col overflow-hidden">
        {/* Query bar */}
        <div className="border-b border-gray-800 flex-shrink-0">
          <div className="flex items-center gap-2 px-3 py-1 border-b border-gray-800/50 bg-gray-950/50">
            <span className="text-xs text-gray-600 font-mono flex-1">
              {isMongo ? 'MongoDB JSON: {"collection":"…","filter":{},"limit":N}' : 'SQL · ⌘↵ to run'}
            </span>
            {activeCfg?.read_only && (
              <span className="text-xs text-yellow-700 flex items-center gap-1">
                <Lock className="h-3 w-3" /> read-only
              </span>
            )}
          </div>
          <textarea
            ref={textareaRef}
            className="w-full h-24 bg-transparent text-sm font-mono text-gray-200 px-3 py-2 resize-none focus:outline-none"
            placeholder={isMongo
              ? `{"collection": "${schema?.tables[0]?.name ?? 'collection'}", "filter": {}}`
              : `SELECT * FROM ${schema?.tables[0]?.name ?? 'table_name'};`}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
            spellCheck={false}
          />
          <div className="flex items-center gap-2 px-3 py-1.5 border-t border-gray-800/50">
            <button
              onClick={handleRun}
              disabled={running || !query.trim() || !activeConn}
              className="flex items-center gap-1.5 px-3 py-1 bg-blue-600 hover:bg-blue-500 text-white text-sm rounded disabled:opacity-40"
            >
              {running ? <Loader2 className="h-3.5 w-3.5 animate-spin" /> : <Play className="h-3.5 w-3.5" />}
              Run
            </button>
            <div className="flex items-center gap-1 text-xs text-gray-500">
              <span>Page</span>
              <select
                value={pageSize}
                onChange={(e) => setPageSize(Number(e.target.value))}
                className="bg-gray-800 border border-gray-700 rounded px-1.5 py-0.5 text-gray-300 font-mono focus:outline-none focus:border-blue-500 text-xs"
              >
                {PAGE_SIZES.map((s) => <option key={s} value={s}>{s}</option>)}
              </select>
            </div>
          </div>
        </div>

        {/* Results */}
        <div className="flex-1 overflow-hidden flex flex-col">
          {queryError && (
            <div className="p-4 text-red-400 flex items-start gap-2 text-sm flex-shrink-0">
              <AlertCircle className="h-4 w-4 flex-shrink-0 mt-0.5" />
              <pre className="whitespace-pre-wrap font-mono text-xs">{queryError}</pre>
            </div>
          )}
          {result && !queryError && (
            <>
              {/* Pagination toolbar */}
              <div className="flex items-center gap-2 px-3 py-1.5 border-b border-gray-800 bg-gray-950 flex-shrink-0 text-xs text-gray-400">
                {result.row_count === 0 ? (
                  <span className="text-gray-500">No rows</span>
                ) : (
                  <span className="tabular-nums text-gray-300 font-medium">
                    Rows {pageOffset + 1}–{pageOffset + result.row_count}
                    {result.row_count < pageSize && ` of ${pageOffset + result.row_count}`}
                  </span>
                )}
                {result.truncated && <span className="text-yellow-500">· truncated</span>}
                <span className="text-gray-600">
                  Page {Math.floor(pageOffset / pageSize) + 1}
                  {result.row_count < pageSize ? ` of ${Math.floor(pageOffset / pageSize) + 1}` : '+'}
                </span>
                <div className="ml-auto flex items-center gap-1">
                  <button
                    onClick={() => handlePage(Math.max(0, pageOffset - pageSize))}
                    disabled={running || pageOffset === 0}
                    className="px-2 py-0.5 hover:text-white disabled:opacity-25 rounded flex items-center gap-0.5 hover:bg-gray-800"
                    title="Previous page"
                  >
                    <ChevronLeft className="h-3.5 w-3.5" /> Prev
                  </button>
                  <button
                    onClick={() => handlePage(pageOffset + pageSize)}
                    disabled={running || result.row_count < pageSize}
                    className="px-2 py-0.5 hover:text-white disabled:opacity-25 rounded flex items-center gap-0.5 hover:bg-gray-800"
                    title="Next page"
                  >
                    Next <ChevronRightIcon className="h-3.5 w-3.5" />
                  </button>
                </div>
              </div>
              <ResultsTable result={result} offset={pageOffset} />
            </>
          )}
          {running && (
            <div className="flex items-center justify-center flex-1 text-gray-400 gap-2 text-sm">
              <Loader2 className="h-4 w-4 animate-spin" /> Running…
            </div>
          )}
          {!result && !queryError && !running && (
            <div className="flex items-center justify-center flex-1 text-gray-700 text-sm">
              {schema ? 'Click a table to browse, or write a query and press Run' : activeConn ? '' : 'Select a connection'}
            </div>
          )}
        </div>
      </div>
      )}
    </div>
  );
}
