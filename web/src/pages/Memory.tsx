import { useEffect, useRef, useState } from 'react';
import {
  Brain,
  ChevronDown,
  ChevronUp,
  Eraser,
  Filter,
  Plus,
  RefreshCw,
  Search,
  Trash2,
  X,
} from 'lucide-react';

import { clearMemory, deleteMemory, getMemory, storeMemory } from '@/lib/api';
import type { MemoryEntry } from '@/types/api';

function formatDate(iso: string): string {
  return new Date(iso).toLocaleString();
}

export function MemoryPanel() {
  const [entries, setEntries] = useState<MemoryEntry[]>([]);
  const [knownCategories, setKnownCategories] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [categoryFilter, setCategoryFilter] = useState('');
  const requestSequence = useRef(0);

  const [showForm, setShowForm] = useState(false);
  const [formKey, setFormKey] = useState('');
  const [formContent, setFormContent] = useState('');
  const [formCategory, setFormCategory] = useState('');
  const [formError, setFormError] = useState<string | null>(null);
  const [submitting, setSubmitting] = useState(false);

  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);
  const [confirmClear, setConfirmClear] = useState<'conversation' | 'all' | null>(null);
  const [clearingScope, setClearingScope] = useState<'conversation' | 'all' | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());

  const fetchEntries = async (query = search, category = categoryFilter) => {
    const requestId = ++requestSequence.current;
    setRefreshing(true);
    setError(null);
    try {
      const nextEntries = await getMemory(query.trim() || undefined, category || undefined);
      if (requestId !== requestSequence.current) return;
      setEntries(nextEntries);
      setKnownCategories((previous) =>
        Array.from(new Set([...previous, ...nextEntries.map((entry) => entry.category)])).sort(),
      );
    } catch (fetchError: unknown) {
      if (requestId === requestSequence.current) {
        setError(fetchError instanceof Error ? fetchError.message : 'Failed to load memory');
      }
    } finally {
      if (requestId === requestSequence.current) {
        setLoading(false);
        setRefreshing(false);
      }
    }
  };

  useEffect(() => {
    const delay = search.trim() ? 300 : 0;
    const timer = window.setTimeout(() => {
      void fetchEntries(search, categoryFilter);
    }, delay);
    return () => window.clearTimeout(timer);
  }, [categoryFilter, search]);

  useEffect(() => {
    if (!success) return;
    const timer = window.setTimeout(() => setSuccess(null), 4_000);
    return () => window.clearTimeout(timer);
  }, [success]);

  useEffect(() => {
    if (!showForm && !confirmClear) return;
    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape' || submitting || clearingScope) return;
      setShowForm(false);
      setConfirmClear(null);
      setFormError(null);
    };
    window.addEventListener('keydown', closeOnEscape);
    return () => window.removeEventListener('keydown', closeOnEscape);
  }, [clearingScope, confirmClear, showForm, submitting]);

  const closeForm = () => {
    if (submitting) return;
    setShowForm(false);
    setFormError(null);
  };

  const handleAdd = async () => {
    if (!formKey.trim() || !formContent.trim()) {
      setFormError('Key and content are required.');
      return;
    }
    setSubmitting(true);
    setFormError(null);
    try {
      await storeMemory(formKey.trim(), formContent.trim(), formCategory.trim() || undefined);
      await fetchEntries();
      setShowForm(false);
      setFormKey('');
      setFormContent('');
      setFormCategory('');
      setSuccess(`Stored memory “${formKey.trim()}”.`);
    } catch (storeError: unknown) {
      setFormError(storeError instanceof Error ? storeError.message : 'Failed to store memory');
    } finally {
      setSubmitting(false);
    }
  };

  const handleDelete = async (entry: MemoryEntry) => {
    setError(null);
    try {
      await deleteMemory(entry.key);
      setEntries((previous) => previous.filter((item) => item.id !== entry.id));
      setSuccess(`Deleted memory “${entry.key}”.`);
    } catch (deleteError: unknown) {
      setError(deleteError instanceof Error ? deleteError.message : 'Failed to delete memory');
    } finally {
      setConfirmDelete(null);
    }
  };

  const handleClear = async (scope: 'conversation' | 'all') => {
    setClearingScope(scope);
    setError(null);
    try {
      const result = await clearMemory(scope);
      await fetchEntries();
      setSuccess(
        scope === 'conversation'
          ? `Cleared ${result.deleted} conversation entries.`
          : `Cleared ${result.deleted} memory entries and completed-run history.`,
      );
    } catch (clearError: unknown) {
      setError(clearError instanceof Error ? clearError.message : 'Failed to clear memory');
    } finally {
      setClearingScope(null);
      setConfirmClear(null);
    }
  };

  const toggleExpanded = (id: string) => {
    setExpanded((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <div className="space-y-6 p-4 sm:p-6">
      <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
        <div>
          <div className="flex items-center gap-2">
            <Brain className="h-5 w-5 text-blue-400" aria-hidden="true" />
            <h2 className="text-base font-semibold text-white">Memory ({entries.length})</h2>
          </div>
          <p className="mt-2 text-sm text-gray-400">
            Search, inspect, add, or remove memories stored by the active backend.
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          <button type="button" onClick={() => setConfirmClear('conversation')} disabled={clearingScope !== null} className="inline-flex items-center gap-2 rounded-lg border border-amber-700/60 bg-amber-900/20 px-3 py-2 text-sm font-medium text-amber-300 hover:bg-amber-900/30 disabled:opacity-50">
            <Eraser className="h-4 w-4" aria-hidden="true" />
            Clear conversation
          </button>
          <button type="button" onClick={() => setConfirmClear('all')} disabled={clearingScope !== null} className="inline-flex items-center gap-2 rounded-lg border border-red-700/60 bg-red-900/20 px-3 py-2 text-sm font-medium text-red-300 hover:bg-red-900/30 disabled:opacity-50">
            <Trash2 className="h-4 w-4" aria-hidden="true" />
            Clear all
          </button>
          <button type="button" onClick={() => setShowForm(true)} className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700">
            <Plus className="h-4 w-4" aria-hidden="true" />
            Add Memory
          </button>
        </div>
      </div>

      <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto_auto]">
        <div className="relative">
          <Search className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-gray-500" aria-hidden="true" />
          <label htmlFor="memory-search" className="sr-only">Search memory</label>
          <input id="memory-search" type="search" value={search} onChange={(event) => setSearch(event.target.value)} placeholder="Search keys and content…" className="w-full rounded-lg border border-gray-700 bg-gray-900 py-2.5 pl-10 pr-4 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
        </div>
        <div className="relative">
          <Filter className="absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-gray-500" aria-hidden="true" />
          <label htmlFor="memory-category" className="sr-only">Filter by category</label>
          <select id="memory-category" value={categoryFilter} onChange={(event) => setCategoryFilter(event.target.value)} className="w-full appearance-none rounded-lg border border-gray-700 bg-gray-900 py-2.5 pl-10 pr-8 text-sm text-white focus:outline-none focus:ring-2 focus:ring-blue-500">
            <option value="">All categories</option>
            {knownCategories.map((category) => <option key={category} value={category}>{category}</option>)}
          </select>
        </div>
        <button type="button" onClick={() => void fetchEntries()} disabled={refreshing} className="inline-flex items-center justify-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 hover:bg-gray-800 hover:text-white disabled:opacity-50">
          <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} aria-hidden="true" />
          Refresh
        </button>
      </div>

      {refreshing && !loading && <p role="status" className="text-xs text-gray-500">Updating results…</p>}
      {success && <div role="status" className="rounded-lg border border-green-700 bg-green-900/30 p-3 text-sm text-green-300">{success}</div>}
      {error && (
        <div role="alert" className="flex items-start justify-between gap-4 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          <span>{error}</span>
          <button type="button" onClick={() => setError(null)} aria-label="Dismiss error"><X className="h-4 w-4" aria-hidden="true" /></button>
        </div>
      )}

      {loading ? (
        <div className="flex h-32 items-center justify-center" role="status">
          <RefreshCw className="h-8 w-8 animate-spin text-blue-400" aria-hidden="true" />
          <span className="sr-only">Loading memory</span>
        </div>
      ) : entries.length === 0 ? (
        <div className="rounded-xl border border-dashed border-gray-700 bg-gray-900 p-10 text-center">
          <Brain className="mx-auto mb-3 h-10 w-10 text-gray-600" aria-hidden="true" />
          <p className="font-medium text-gray-300">{search || categoryFilter ? 'No matching memories' : 'No memory entries yet'}</p>
          <p className="mt-1 text-sm text-gray-500">{search || categoryFilter ? 'Try a different search or category.' : 'Add one manually or let the agent store context.'}</p>
        </div>
      ) : (
        <div className="grid gap-3 xl:grid-cols-2">
          {entries.map((entry) => {
            const isExpanded = expanded.has(entry.id);
            const isLong = entry.content.length > 240;
            return (
              <article key={entry.id} className="rounded-xl border border-gray-800 bg-gray-900 p-4">
                <div className="flex items-start justify-between gap-4">
                  <div className="min-w-0">
                    <h3 className="break-all font-mono text-sm font-medium text-white">{entry.key}</h3>
                    <div className="mt-2 flex flex-wrap items-center gap-2 text-xs">
                      <span className="rounded-full bg-gray-800 px-2 py-0.5 text-gray-300">{entry.category}</span>
                      <time className="text-gray-500" dateTime={entry.timestamp}>{formatDate(entry.timestamp)}</time>
                      {entry.score !== null && <span className="text-gray-500">score {entry.score.toFixed(3)}</span>}
                    </div>
                  </div>
                  {confirmDelete === entry.key ? (
                    <div className="flex flex-shrink-0 items-center gap-2 rounded-lg border border-red-800 bg-red-950/30 px-2 py-1">
                      <span className="text-xs text-red-300">Delete?</span>
                      <button type="button" onClick={() => void handleDelete(entry)} className="text-xs font-medium text-red-300 hover:text-red-200">Yes</button>
                      <button type="button" onClick={() => setConfirmDelete(null)} className="text-xs font-medium text-gray-400 hover:text-white">No</button>
                    </div>
                  ) : (
                    <button type="button" onClick={() => setConfirmDelete(entry.key)} aria-label={`Delete memory ${entry.key}`} className="rounded-lg p-2 text-gray-400 hover:bg-gray-800 hover:text-red-400">
                      <Trash2 className="h-4 w-4" aria-hidden="true" />
                    </button>
                  )}
                </div>
                <p className={`mt-4 whitespace-pre-wrap break-words text-sm leading-6 text-gray-300 ${isLong && !isExpanded ? 'max-h-24 overflow-hidden' : ''}`}>{entry.content}</p>
                {isLong && (
                  <button type="button" onClick={() => toggleExpanded(entry.id)} aria-expanded={isExpanded} className="mt-3 inline-flex items-center gap-1 text-xs font-medium text-blue-300 hover:text-blue-200">
                    {isExpanded ? <ChevronUp className="h-3.5 w-3.5" aria-hidden="true" /> : <ChevronDown className="h-3.5 w-3.5" aria-hidden="true" />}
                    {isExpanded ? 'Show less' : 'Show full content'}
                  </button>
                )}
              </article>
            );
          })}
        </div>
      )}

      {showForm && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4">
          <div role="dialog" aria-modal="true" aria-labelledby="add-memory-title" className="w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6 shadow-2xl">
            <div className="mb-4 flex items-center justify-between">
              <h3 id="add-memory-title" className="text-lg font-semibold text-white">Add Memory</h3>
              <button type="button" onClick={closeForm} aria-label="Close add memory dialog" className="rounded p-1 text-gray-400 hover:bg-gray-800 hover:text-white"><X className="h-5 w-5" aria-hidden="true" /></button>
            </div>
            {formError && <div role="alert" className="mb-4 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">{formError}</div>}
            <div className="space-y-4">
              <div>
                <label htmlFor="memory-key" className="mb-1 block text-sm font-medium text-gray-300">Key</label>
                <input id="memory-key" autoFocus type="text" value={formKey} onChange={(event) => setFormKey(event.target.value)} placeholder="project_context" className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
              </div>
              <div>
                <label htmlFor="memory-content" className="mb-1 block text-sm font-medium text-gray-300">Content</label>
                <textarea id="memory-content" value={formContent} onChange={(event) => setFormContent(event.target.value)} placeholder="What should the agent remember?" rows={5} className="w-full resize-y rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
              </div>
              <div>
                <label htmlFor="memory-new-category" className="mb-1 block text-sm font-medium text-gray-300">Category <span className="font-normal text-gray-500">(optional)</span></label>
                <input id="memory-new-category" type="text" list="memory-category-options" value={formCategory} onChange={(event) => setFormCategory(event.target.value)} placeholder="core" className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500" />
                <datalist id="memory-category-options">{knownCategories.map((category) => <option key={category} value={category} />)}</datalist>
              </div>
            </div>
            <div className="mt-6 flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
              <button type="button" onClick={closeForm} className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800 hover:text-white">Cancel</button>
              <button type="button" onClick={() => void handleAdd()} disabled={submitting} className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white hover:bg-blue-700 disabled:opacity-50">{submitting ? 'Saving…' : 'Save Memory'}</button>
            </div>
          </div>
        </div>
      )}

      {confirmClear && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4">
          <div role="dialog" aria-modal="true" aria-labelledby="clear-memory-title" className="w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6 shadow-2xl">
            <h3 id="clear-memory-title" className="text-lg font-semibold text-white">{confirmClear === 'all' ? 'Clear all memory?' : 'Clear conversation memory?'}</h3>
            <p className="mt-2 text-sm leading-6 text-gray-400">
              {confirmClear === 'all'
                ? 'This removes every memory entry plus completed-run ledgers and traces. Live runs remain active.'
                : 'This removes entries in the conversation category and keeps core, daily, and custom memories.'}
            </p>
            <div className="mt-6 flex flex-col-reverse gap-3 sm:flex-row sm:justify-end">
              <button type="button" autoFocus onClick={() => setConfirmClear(null)} disabled={clearingScope !== null} className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800 hover:text-white disabled:opacity-50">Cancel</button>
              <button type="button" onClick={() => void handleClear(confirmClear)} disabled={clearingScope !== null} className="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white hover:bg-red-500 disabled:opacity-50">{clearingScope ? 'Clearing…' : 'Clear memory'}</button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

export default function Memory() {
  return <MemoryPanel />;
}
