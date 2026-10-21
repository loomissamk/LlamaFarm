import { useEffect, useState } from 'react';
import { BookText, RefreshCw, Save } from 'lucide-react';
import type { WorkspaceFileResponse } from '@/types/api';
import { getWorkspaceFile, putWorkspaceFile } from '@/lib/api';

const FILE_NAME = 'AGENTS.md';

export default function WorkspacePrompts() {
  const [file, setFile] = useState<WorkspaceFileResponse>({
    name: FILE_NAME,
    content: '',
    exists: false,
  });
  const [draft, setDraft] = useState('');
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const loadFile = async (showLoading = true) => {
    if (showLoading) {
      setLoading(true);
    } else {
      setRefreshing(true);
    }
    setError(null);
    try {
      const result = await getWorkspaceFile(FILE_NAME);
      setFile(result);
      setDraft(result.content);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace prompt file');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  };

  useEffect(() => {
    void loadFile();
  }, []);

  useEffect(() => {
    if (!success) return;
    const timer = window.setTimeout(() => setSuccess(null), 3000);
    return () => window.clearTimeout(timer);
  }, [success]);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      const saved = await putWorkspaceFile(FILE_NAME, draft);
      setFile(saved);
      setDraft(saved.content);
      setSuccess(`${FILE_NAME} saved to the live workspace.`);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : `Failed to save ${FILE_NAME}`);
    } finally {
      setSaving(false);
    }
  };

  const isDirty = draft !== file.content;

  if (loading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="space-y-6 p-6">
      <div>
        <div className="flex items-center gap-2">
          <BookText className="h-5 w-5 text-blue-400" />
          <h2 className="text-base font-semibold text-white">Prompt Files</h2>
        </div>
        <p className="mt-2 max-w-3xl text-sm text-gray-400">
          Edit the live workspace copy of <code>{FILE_NAME}</code>. New agent turns pick up this
          file directly from the running workspace after save.
        </p>
      </div>

      <div className="rounded-xl border border-gray-800 bg-gray-900">
        <div className="flex flex-wrap items-center justify-between gap-3 border-b border-gray-800 px-4 py-3">
          <div className="flex items-center gap-3 text-xs text-gray-500">
            <span className="text-sm font-medium text-white">{FILE_NAME}</span>
            {isDirty && <span className="text-amber-400">unsaved changes</span>}
            <span>{draft.split('\n').length} lines</span>
            <span>{file.exists ? 'live file exists' : 'missing; save will create it'}</span>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={() => void loadFile(false)}
              disabled={refreshing || saving}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
              Refresh
            </button>
            <button
              onClick={() => void handleSave()}
              disabled={saving || !isDirty}
              className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
            >
              <Save className="h-4 w-4" />
              {saving ? 'Saving...' : 'Save'}
            </button>
          </div>
        </div>

        {success && (
          <div className="border-b border-green-800 bg-green-900/20 px-4 py-3 text-sm text-green-300">
            {success}
          </div>
        )}

        {error && (
          <div className="border-b border-red-800 bg-red-900/20 px-4 py-3 text-sm text-red-300">
            {error}
          </div>
        )}

        <textarea
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          spellCheck={false}
          className="min-h-[520px] w-full resize-y bg-gray-950 p-4 font-mono text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-inset"
          style={{ tabSize: 4 }}
        />
      </div>
    </div>
  );
}
