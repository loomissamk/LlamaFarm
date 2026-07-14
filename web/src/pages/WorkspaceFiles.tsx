import { useEffect, useMemo, useRef, useState } from 'react';
import {
  ChevronRight,
  Download,
  FileText,
  Folder,
  FolderDown,
  FolderPlus,
  FolderOpen,
  RefreshCw,
  Trash2,
  Upload,
  X,
} from 'lucide-react';
import type { WorkspaceBrowserEntry, WorkspaceBrowserResponse } from '@/types/api';
import {
  createWorkspaceDirectory,
  deleteWorkspacePath,
  downloadWorkspacePath,
  getWorkspaceBrowser,
  uploadWorkspaceBlob,
} from '@/lib/api';

function formatBytes(sizeBytes?: number): string {
  if (sizeBytes === undefined) return 'Directory';
  if (sizeBytes < 1024) return `${sizeBytes} B`;
  const units = ['KB', 'MB', 'GB', 'TB'];
  let value = sizeBytes / 1024;
  let unitIndex = 0;
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024;
    unitIndex += 1;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unitIndex]}`;
}

function formatTimestamp(value?: string): string {
  if (!value) return 'Unknown';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString();
}

function joinWorkspacePath(basePath: string, fileName: string): string {
  return basePath ? `${basePath}/${fileName}` : fileName;
}

export default function WorkspaceFiles() {
  const uploadInputRef = useRef<HTMLInputElement | null>(null);

  const [browser, setBrowser] = useState<WorkspaceBrowserResponse | null>(null);
  const [loadingBrowser, setLoadingBrowser] = useState(true);
  const [refreshingBrowser, setRefreshingBrowser] = useState(false);
  const [uploading, setUploading] = useState(false);
  const [downloadingPath, setDownloadingPath] = useState<string | null>(null);
  const [creatingFolder, setCreatingFolder] = useState(false);
  const [deletingPath, setDeletingPath] = useState<string | null>(null);
  const [browserError, setBrowserError] = useState<string | null>(null);
  const [browserSuccess, setBrowserSuccess] = useState<string | null>(null);
  const [confirmDeleteEntry, setConfirmDeleteEntry] = useState<WorkspaceBrowserEntry | null>(null);
  const [showNewFolderModal, setShowNewFolderModal] = useState(false);
  const [newFolderName, setNewFolderName] = useState('');

  const loadBrowser = async (path = '', showLoading = true) => {
    if (showLoading) {
      setLoadingBrowser(true);
    } else {
      setRefreshingBrowser(true);
    }
    setBrowserError(null);
    try {
      const nextBrowser = await getWorkspaceBrowser(path);
      setBrowser(nextBrowser);
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to load workspace browser');
    } finally {
      setLoadingBrowser(false);
      setRefreshingBrowser(false);
    }
  };

  useEffect(() => {
    void loadBrowser();
  }, []);

  useEffect(() => {
    if (!browserSuccess) return;
    const timer = globalThis.setTimeout(() => {
      setBrowserSuccess(null);
    }, 3000);
    return () => globalThis.clearTimeout(timer);
  }, [browserSuccess]);

  const handleDownload = async (path: string) => {
    setDownloadingPath(path);
    setBrowserError(null);
    try {
      await downloadWorkspacePath(path);
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to download workspace path');
    } finally {
      setDownloadingPath(null);
    }
  };

  const handleUploadFiles = async (selected: FileList | null) => {
    if (!selected || selected.length === 0 || !browser) return;

    setUploading(true);
    setBrowserError(null);
    setBrowserSuccess(null);

    try {
      for (const file of Array.from(selected)) {
        const targetPath = joinWorkspacePath(browser.current_path, file.name);
        await uploadWorkspaceBlob(targetPath, file);
      }
      await loadBrowser(browser.current_path, false);
      setBrowserSuccess(
        `${selected.length} file${selected.length === 1 ? '' : 's'} uploaded to ${
          browser.current_path || '.'
        }.`,
      );
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to upload workspace files');
    } finally {
      setUploading(false);
      if (uploadInputRef.current) uploadInputRef.current.value = '';
    }
  };

  const handleDelete = (entry: WorkspaceBrowserEntry) => {
    setConfirmDeleteEntry(entry);
  };

  const doDelete = async (entry: WorkspaceBrowserEntry) => {
    if (!browser) return;
    setConfirmDeleteEntry(null);
    setDeletingPath(entry.path);
    setBrowserError(null);
    setBrowserSuccess(null);
    try {
      await deleteWorkspacePath(entry.path);
      await loadBrowser(browser.current_path, false);
      setBrowserSuccess(`${entry.path} deleted from the live workspace.`);
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to delete workspace path');
    } finally {
      setDeletingPath(null);
    }
  };

  const handleCreateFolder = () => {
    setNewFolderName('');
    setShowNewFolderModal(true);
  };

  const doCreateFolder = async () => {
    if (!browser) return;
    const trimmed = newFolderName.trim();
    if (!trimmed) return;
    setShowNewFolderModal(false);
    setNewFolderName('');
    const targetPath = joinWorkspacePath(browser.current_path, trimmed);
    setCreatingFolder(true);
    setBrowserError(null);
    setBrowserSuccess(null);
    try {
      await createWorkspaceDirectory(targetPath);
      await loadBrowser(browser.current_path, false);
      setBrowserSuccess(`${targetPath} created in the live workspace.`);
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to create workspace folder');
    } finally {
      setCreatingFolder(false);
    }
  };

  const breadcrumbs = useMemo(() => {
    if (!browser?.current_path) {
      return [{ label: '.', path: '' }];
    }

    const parts = browser.current_path.split('/').filter(Boolean);
    const crumbs = [{ label: '.', path: '' }];
    let running = '';
    for (const part of parts) {
      running = running ? `${running}/${part}` : part;
      crumbs.push({ label: part, path: running });
    }
    return crumbs;
  }, [browser]);

  if (loadingBrowser) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  return (
    <div className="space-y-6 p-6">
      {/* Delete confirm modal */}
      {confirmDeleteEntry && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
          <div className="mx-4 w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6">
            <div className="mb-2 flex items-center justify-between">
              <h3 className="text-base font-semibold text-white">
                Delete {confirmDeleteEntry.kind === 'directory' ? 'Folder' : 'File'}
              </h3>
              <button
                onClick={() => setConfirmDeleteEntry(null)}
                className="text-gray-400 transition-colors hover:text-white"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            <p className="mb-6 text-sm text-gray-400">
              Delete <span className="font-mono text-white">{confirmDeleteEntry.path}</span>
              {confirmDeleteEntry.kind === 'directory' ? ' and all of its contents' : ''}? This cannot be undone.
            </p>
            <div className="flex justify-end gap-3">
              <button
                onClick={() => setConfirmDeleteEntry(null)}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={() => void doDelete(confirmDeleteEntry)}
                className="rounded-lg bg-red-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-red-700"
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}

      {/* New folder modal */}
      {showNewFolderModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
          <div className="mx-4 w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6">
            <div className="mb-4 flex items-center justify-between">
              <h3 className="text-base font-semibold text-white">New Folder</h3>
              <button
                onClick={() => setShowNewFolderModal(false)}
                className="text-gray-400 transition-colors hover:text-white"
              >
                <X className="h-5 w-5" />
              </button>
            </div>
            <input
              type="text"
              value={newFolderName}
              onChange={(e) => setNewFolderName(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter') void doCreateFolder(); if (e.key === 'Escape') setShowNewFolderModal(false); }}
              placeholder="Folder name or relative path"
              autoFocus
              className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
            <div className="mt-4 flex justify-end gap-3">
              <button
                onClick={() => setShowNewFolderModal(false)}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                onClick={() => void doCreateFolder()}
                disabled={!newFolderName.trim()}
                className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
              >
                Create
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <FolderOpen className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Workspace Files</h2>
          </div>
          <p className="mt-2 max-w-3xl text-sm text-gray-400">
            Browse the live workspace, upload files into it, and download files or folders back
            out. This makes it possible to grab directories like <code>rust_kernel</code> directly
            from the dashboard without finding the Docker volume path first. Prompt files now live
            on their own page so this browser stays focused on file management.
          </p>
        </div>
      </div>

      <div className="rounded-xl border border-gray-800 bg-gray-900">
        <div className="flex flex-col gap-4 border-b border-gray-800 px-4 py-4 lg:flex-row lg:items-start lg:justify-between">
          <div>
            <h3 className="text-sm font-semibold text-white">Workspace Browser</h3>
            <p className="mt-1 text-xs text-gray-500">
              Workspace root: <code>{browser?.root_path || '.'}</code>
            </p>
            <div className="mt-3 flex flex-wrap items-center gap-2 text-xs text-gray-400">
              {breadcrumbs.map((crumb, index) => (
                <div key={crumb.path} className="flex items-center gap-2">
                  {index > 0 && <ChevronRight className="h-3.5 w-3.5 text-gray-600" />}
                  <button
                    onClick={() => void loadBrowser(crumb.path, false)}
                    className="rounded px-2 py-1 transition-colors hover:bg-gray-800 hover:text-white"
                  >
                    {crumb.label}
                  </button>
                </div>
              ))}
            </div>
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <input
              ref={uploadInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={(event) => void handleUploadFiles(event.currentTarget.files)}
            />
            <button
              onClick={() => void loadBrowser(browser?.current_path || '', false)}
              disabled={refreshingBrowser || uploading || deletingPath !== null}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <RefreshCw className={`h-4 w-4 ${refreshingBrowser ? 'animate-spin' : ''}`} />
              Refresh
            </button>
            <button
              onClick={() => uploadInputRef.current?.click()}
              disabled={uploading || creatingFolder || deletingPath !== null}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <Upload className="h-4 w-4" />
              {uploading ? 'Uploading...' : 'Upload Files'}
            </button>
            <button
              onClick={() => void handleCreateFolder()}
              disabled={uploading || creatingFolder || deletingPath !== null}
              className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <FolderPlus className="h-4 w-4" />
              {creatingFolder ? 'Creating...' : 'New Folder'}
            </button>
            <button
              onClick={() => void handleDownload(browser?.current_path || '')}
              disabled={
                uploading ||
                creatingFolder ||
                deletingPath !== null ||
                downloadingPath === (browser?.current_path || '')
              }
              className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
            >
              <FolderDown className="h-4 w-4" />
              {downloadingPath === (browser?.current_path || '')
                ? 'Preparing...'
                : 'Download Current Folder'}
            </button>
          </div>
        </div>

        {browserSuccess && (
          <div className="border-b border-green-800 bg-green-900/20 px-4 py-3 text-sm text-green-300">
            {browserSuccess}
          </div>
        )}

        {browserError && (
          <div className="border-b border-red-800 bg-red-900/20 px-4 py-3 text-sm text-red-300">
            {browserError}
          </div>
        )}

        <div className="overflow-x-auto">
          <table className="min-w-full divide-y divide-gray-800 text-sm">
            <thead className="bg-gray-950/60 text-left text-xs uppercase tracking-wide text-gray-500">
              <tr>
                <th className="px-4 py-3">Name</th>
                <th className="px-4 py-3">Path</th>
                <th className="px-4 py-3">Size</th>
                <th className="px-4 py-3">Modified</th>
                <th className="px-4 py-3 text-right">Actions</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800">
              {browser?.parent_path !== undefined && (
                <tr className="bg-gray-950/20">
                  <td className="px-4 py-3">
                    <button
                      onClick={() => void loadBrowser(browser.parent_path || '', false)}
                      className="inline-flex items-center gap-2 text-gray-300 transition-colors hover:text-white"
                    >
                      <Folder className="h-4 w-4 text-yellow-400" />
                      ..
                    </button>
                  </td>
                  <td className="px-4 py-3 text-gray-500">{browser.parent_path || '.'}</td>
                  <td className="px-4 py-3 text-gray-500">Directory</td>
                  <td className="px-4 py-3 text-gray-500">-</td>
                  <td className="px-4 py-3 text-right">
                    <button
                      onClick={() => void handleDownload(browser.parent_path || '')}
                      disabled={downloadingPath === (browser.parent_path || '')}
                      className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
                    >
                      <Download className="h-3.5 w-3.5" />
                      Download
                    </button>
                  </td>
                </tr>
              )}

              {browser?.entries.map((entry: WorkspaceBrowserEntry) => {
                const isDirectory = entry.kind === 'directory';
                return (
                  <tr key={entry.path}>
                    <td className="px-4 py-3">
                      {isDirectory ? (
                        <button
                          onClick={() => void loadBrowser(entry.path, false)}
                          className="inline-flex items-center gap-2 text-left text-gray-200 transition-colors hover:text-white"
                        >
                          <Folder className="h-4 w-4 text-yellow-400" />
                          {entry.name}
                        </button>
                      ) : (
                        <div className="inline-flex items-center gap-2 text-gray-200">
                          <FileText className="h-4 w-4 text-blue-400" />
                          {entry.name}
                        </div>
                      )}
                    </td>
                    <td className="px-4 py-3 font-mono text-xs text-gray-500">{entry.path}</td>
                    <td className="px-4 py-3 text-gray-400">{formatBytes(entry.size_bytes)}</td>
                    <td className="px-4 py-3 text-gray-400">
                      {formatTimestamp(entry.modified_at)}
                    </td>
                    <td className="px-4 py-3">
                      <div className="flex justify-end gap-2">
                        {isDirectory && (
                          <button
                          onClick={() => void loadBrowser(entry.path, false)}
                          className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
                        >
                          Open
                        </button>
                      )}
                      <button
                        onClick={() => void handleDownload(entry.path)}
                        disabled={
                          downloadingPath === entry.path ||
                          creatingFolder ||
                          deletingPath === entry.path
                        }
                        className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
                      >
                        <Download className="h-3.5 w-3.5" />
                        {downloadingPath === entry.path ? 'Preparing...' : 'Download'}
                      </button>
                      <button
                        onClick={() => void handleDelete(entry)}
                        disabled={
                          downloadingPath === entry.path ||
                          creatingFolder ||
                          deletingPath !== null
                        }
                        className="inline-flex items-center gap-2 rounded-lg border border-red-900/60 px-3 py-1.5 text-xs text-red-300 transition-colors hover:bg-red-950/40 hover:text-red-200 disabled:opacity-50"
                      >
                          <Trash2 className="h-3.5 w-3.5" />
                          {deletingPath === entry.path ? 'Deleting...' : 'Delete'}
                        </button>
                      </div>
                    </td>
                  </tr>
                );
              })}

              {browser?.entries.length === 0 && (
                <tr>
                  <td colSpan={5} className="px-4 py-10 text-center text-sm text-gray-500">
                    This folder is empty.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      </div>
    </div>
  );
}
