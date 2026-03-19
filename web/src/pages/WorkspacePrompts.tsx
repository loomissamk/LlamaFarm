import { useEffect, useState } from 'react';
import { BookText, RefreshCw, Save } from 'lucide-react';
import type { WorkspaceFileResponse } from '@/types/api';
import { getWorkspaceFile, putWorkspaceFile } from '@/lib/api';

type WorkspaceFileName = 'AGENTS.md' | 'SOUL.md';

const WORKSPACE_FILES: WorkspaceFileName[] = ['AGENTS.md', 'SOUL.md'];

export default function WorkspacePrompts() {
  const [activeFile, setActiveFile] = useState<WorkspaceFileName>('AGENTS.md');
  const [files, setFiles] = useState<Record<WorkspaceFileName, WorkspaceFileResponse>>({
    'AGENTS.md': { name: 'AGENTS.md', content: '', exists: false },
    'SOUL.md': { name: 'SOUL.md', content: '', exists: false },
  });
  const [drafts, setDrafts] = useState<Record<WorkspaceFileName, string>>({
    'AGENTS.md': '',
    'SOUL.md': '',
  });
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const loadFiles = async (showLoading = true) => {
    if (showLoading) {
      setLoading(true);
    } else {
      setRefreshing(true);
    }
    setError(null);
    try {
      const results = await Promise.all(WORKSPACE_FILES.map((name) => getWorkspaceFile(name)));
      const nextFiles = {
        'AGENTS.md': results.find((file) => file.name === 'AGENTS.md') ?? {
          name: 'AGENTS.md',
          content: '',
          exists: false,
        },
        'SOUL.md': results.find((file) => file.name === 'SOUL.md') ?? {
          name: 'SOUL.md',
          content: '',
          exists: false,
        },
      };
      setFiles(nextFiles);
      setDrafts({
        'AGENTS.md': nextFiles['AGENTS.md'].content,
        'SOUL.md': nextFiles['SOUL.md'].content,
      });
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace prompt files');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  };

  useEffect(() => {
    void loadFiles();
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
      const saved = await putWorkspaceFile(activeFile, drafts[activeFile]);
      setFiles((prev) => ({ ...prev, [activeFile]: saved }));
      setDrafts((prev) => ({ ...prev, [activeFile]: saved.content }));
      setSuccess(`${activeFile} saved to the live workspace.`);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : `Failed to save ${activeFile}`);
    } finally {
      setSaving(false);
    }
  };

  const currentFile = files[activeFile];
  const currentDraft = drafts[activeFile];
  const isDirty = currentDraft !== currentFile.content;

  if (loading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <BookText className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Prompt Files</h2>
          </div>
          <p className="mt-2 max-w-3xl text-sm text-gray-400">
            Edit the live workspace copies of <code>AGENTS.md</code> and <code>SOUL.md</code>.
            New agent turns pick up these files directly from the running workspace after save.
          </p>
        </div>
      </div>

      <div className="rounded-xl border border-gray-800 bg-gray-900">
        <div className="flex items-start justify-between gap-4 border-b border-gray-800 px-4 py-4">
          <div>
            <h3 className="text-sm font-semibold text-white">Live Prompt Editor</h3>
            <p className="mt-1 max-w-2xl text-sm text-gray-400">
              Keep agent behavior files separate from general workspace file management.
            </p>
          </div>

          <div className="flex items-center gap-2">
            <button
              onClick={() => void loadFiles(false)}
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

        <div className="flex flex-wrap items-center gap-2 border-b border-gray-800 px-4 py-3">
          {WORKSPACE_FILES.map((fileName) => {
            const file = files[fileName];
            const dirty = drafts[fileName] !== file.content;
            return (
              <button
                key={fileName}
                onClick={() => setActiveFile(fileName)}
                className={[
                  'rounded-lg px-3 py-2 text-sm transition-colors',
                  activeFile === fileName
                    ? 'bg-blue-600 text-white'
                    : 'bg-gray-950 text-gray-300 hover:bg-gray-800 hover:text-white',
                ].join(' ')}
              >
                {fileName}
                {dirty ? ' *' : ''}
              </button>
            );
          })}
          <div className="ml-auto text-xs text-gray-500">
            {currentFile.exists ? 'Live file exists' : 'Missing file; save will create it'}
          </div>
        </div>

        <div className="flex items-center justify-between border-b border-gray-800 px-4 py-2 text-xs text-gray-500">
          <span>{activeFile}</span>
          <span>{currentDraft.split('\n').length} lines</span>
        </div>

        <textarea
          value={currentDraft}
          onChange={(event) =>
            setDrafts((prev) => ({
              ...prev,
              [activeFile]: event.target.value,
            }))
          }
          spellCheck={false}
          className="min-h-[520px] w-full resize-y bg-gray-950 p-4 font-mono text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-inset"
          style={{ tabSize: 4 }}
        />
      </div>
    </div>
  );
}
