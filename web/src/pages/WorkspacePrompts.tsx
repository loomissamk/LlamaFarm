import { useCallback, useEffect, useMemo, useState } from 'react';
import { BookText, Download, RefreshCw, RotateCcw, Save } from 'lucide-react';
import type { WorkspaceFileResponse } from '@/types/api';
import { getWorkspaceFile, putWorkspaceFile } from '@/lib/api';
import {
  createBackupFilename,
  downloadTextBackup,
  summarizeTextChange,
} from '@/lib/editorDraft';
import { useDirtyDraftGuard } from '@/hooks/useDirtyDraftGuard';

type WorkspaceFileName = 'AGENTS.md';

const WORKSPACE_FILES: WorkspaceFileName[] = ['AGENTS.md'];

export default function WorkspacePrompts() {
  const [activeFile, setActiveFile] = useState<WorkspaceFileName>('AGENTS.md');
  const [files, setFiles] = useState<Record<WorkspaceFileName, WorkspaceFileResponse>>({
    'AGENTS.md': { name: 'AGENTS.md', content: '', exists: false },
  });
  const [drafts, setDrafts] = useState<Record<WorkspaceFileName, string>>({
    'AGENTS.md': '',
  });
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [lastServerRefresh, setLastServerRefresh] = useState<Date | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const loadFiles = useCallback(async (showLoading = true) => {
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
      };
      setFiles(nextFiles);
      setDrafts({
        'AGENTS.md': nextFiles['AGENTS.md'].content,
      });
      setLastServerRefresh(new Date());
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace prompt files');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  }, []);

  useEffect(() => {
    void loadFiles();
  }, [loadFiles]);

  useEffect(() => {
    if (!success) return;
    const timer = window.setTimeout(() => setSuccess(null), 3000);
    return () => window.clearTimeout(timer);
  }, [success]);

  const handleSave = useCallback(async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      const saved = await putWorkspaceFile(activeFile, drafts[activeFile]);
      setFiles((prev) => ({ ...prev, [activeFile]: saved }));
      setDrafts((prev) => ({ ...prev, [activeFile]: saved.content }));
      setLastServerRefresh(new Date());
      setSuccess(`${activeFile} saved to the live workspace.`);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : `Failed to save ${activeFile}`);
    } finally {
      setSaving(false);
    }
  }, [activeFile, drafts]);

  const currentFile = files[activeFile];
  const currentDraft = drafts[activeFile];
  const isDirty = currentDraft !== currentFile.content;
  const changeSummary = useMemo(
    () => summarizeTextChange(currentFile.content, currentDraft),
    [currentDraft, currentFile.content],
  );

  useDirtyDraftGuard(isDirty, `${activeFile} has unsaved changes. Leave and discard the draft?`);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 's') {
        event.preventDefault();
        if (!saving && isDirty) {
          void handleSave();
        }
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [handleSave, isDirty, saving]);

  const handleRefresh = async () => {
    if (isDirty && !window.confirm(`Refresh ${activeFile} and discard its unsaved draft?`)) {
      return;
    }
    await loadFiles(false);
  };

  const handleResetDraft = () => {
    if (isDirty && !window.confirm(`Reset the ${activeFile} draft to the cached live copy?`)) {
      return;
    }
    setDrafts((previous) => ({ ...previous, [activeFile]: currentFile.content }));
    setError(null);
    setSuccess(`${activeFile} draft reset to the last server response.`);
  };

  const handleDownloadBackup = () => {
    downloadTextBackup(createBackupFilename(`${activeFile}.live`), currentFile.content);
    setSuccess(`Downloaded a backup of the cached live ${activeFile}.`);
  };

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
            Edit the live workspace copy of <code>AGENTS.md</code>. New agent turns pick up this
            file directly from the running workspace after save.
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

          <div className="flex flex-wrap items-center justify-end gap-2">
            <button
              type="button"
              onClick={handleDownloadBackup}
              disabled={refreshing || saving || !currentFile.exists}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <Download className="h-4 w-4" />
              Backup Live
            </button>
            <button
              type="button"
              onClick={handleResetDraft}
              disabled={refreshing || saving || !isDirty}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <RotateCcw className="h-4 w-4" />
              Reset Draft
            </button>
            <button
              type="button"
              onClick={() => void handleRefresh()}
              disabled={refreshing || saving}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <RefreshCw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
              {refreshing ? 'Refreshing...' : 'Refresh Server'}
            </button>
            <button
              type="button"
              onClick={() => void handleSave()}
              disabled={saving || !isDirty}
              title="Save (Ctrl/Cmd+S)"
              className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
            >
              <Save className="h-4 w-4" />
              {saving ? 'Saving...' : 'Save'}
            </button>
          </div>
        </div>

        {success && (
          <div
            role="status"
            className="border-b border-green-800 bg-green-900/20 px-4 py-3 text-sm text-green-300"
          >
            {success}
          </div>
        )}

        {error && (
          <div
            role="alert"
            className="border-b border-red-800 bg-red-900/20 px-4 py-3 text-sm text-red-300"
          >
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
                type="button"
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

        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-gray-800 px-4 py-2 text-xs text-gray-500">
          <span>{activeFile}</span>
          <span>
            {currentDraft.split('\n').length} lines
            {lastServerRefresh
              ? ` · server response ${lastServerRefresh.toLocaleTimeString()}`
              : ''}
          </span>
        </div>

        <div
          className={[
            'border-b p-4',
            isDirty
              ? 'border-blue-800/70 bg-blue-950/20'
              : 'border-gray-800 bg-gray-900/60',
          ].join(' ')}
        >
          <div className="flex flex-wrap items-center justify-between gap-2">
            <p className="text-sm font-semibold text-white">Draft change summary</p>
            <span className="text-xs text-gray-500">
              {changeSummary.characterDelta >= 0 ? '+' : ''}
              {changeSummary.characterDelta} characters
            </span>
          </div>
          <p className="mt-2 text-sm text-gray-300">{changeSummary.description}</p>
          {changeSummary.changed && (
            <div className="mt-3 grid gap-3 lg:grid-cols-2">
              <div className="rounded-lg border border-red-900/50 bg-gray-950 p-3">
                <p className="text-xs font-semibold uppercase tracking-wider text-red-300">
                  Cached live
                </p>
                <pre className="mt-2 overflow-auto whitespace-pre-wrap text-xs text-gray-400">
                  {changeSummary.originalPreview.join('\n') || '(no lines)'}
                </pre>
              </div>
              <div className="rounded-lg border border-blue-900/50 bg-gray-950 p-3">
                <p className="text-xs font-semibold uppercase tracking-wider text-blue-300">
                  Draft
                </p>
                <pre className="mt-2 overflow-auto whitespace-pre-wrap text-xs text-gray-300">
                  {changeSummary.draftPreview.join('\n') || '(no lines)'}
                </pre>
              </div>
            </div>
          )}
        </div>

        <textarea
          aria-label={`${activeFile} draft editor`}
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
