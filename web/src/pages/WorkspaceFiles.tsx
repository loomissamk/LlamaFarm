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
  const currentPathRef = useRef('');
  const browserRequestRef = useRef(0);
  const deleteCancelRef = useRef<HTMLButtonElement | null>(null);
  const newFolderInputRef = useRef<HTMLInputElement | null>(null);
  const uploadCancelRef = useRef<HTMLButtonElement | null>(null);

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
  const [pendingUploads, setPendingUploads] = useState<File[]>([]);
  const [uploadProgress, setUploadProgress] = useState<{
    completed: number;
    total: number;
    currentFile: string;
  } | null>(null);
  const workspaceMutationInFlight = uploading || creatingFolder || deletingPath !== null;

  const loadBrowser = async (path = currentPathRef.current, showLoading = true) => {
    const requestId = browserRequestRef.current + 1;
    browserRequestRef.current = requestId;
    if (showLoading) {
      setLoadingBrowser(true);
    } else {
      setRefreshingBrowser(true);
    }
    setBrowserError(null);
    try {
      const nextBrowser = await getWorkspaceBrowser(path);
      if (browserRequestRef.current !== requestId) return;
      currentPathRef.current = nextBrowser.current_path;
      setBrowser(nextBrowser);
    } catch (err: unknown) {
      if (browserRequestRef.current !== requestId) return;
      setBrowserError(err instanceof Error ? err.message : 'Failed to load workspace browser');
    } finally {
      if (browserRequestRef.current === requestId) {
        setLoadingBrowser(false);
        setRefreshingBrowser(false);
      }
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

  const prepareUploadFiles = (selected: FileList | null) => {
    if (!selected || selected.length === 0 || !browser) return;
    setPendingUploads(Array.from(selected));
    if (uploadInputRef.current) uploadInputRef.current.value = '';
  };

  const cancelUpload = () => {
    setPendingUploads([]);
  };

  const confirmUploadFiles = async () => {
    if (pendingUploads.length === 0) return;

    const files = [...pendingUploads];
    const destinationPath = currentPathRef.current;
    let completed = 0;
    setPendingUploads([]);
    setUploading(true);
    setBrowserError(null);
    setBrowserSuccess(null);
    setUploadProgress({
      completed,
      total: files.length,
      currentFile: files[0]?.name ?? '',
    });

    try {
      for (const file of files) {
        setUploadProgress({
          completed,
          total: files.length,
          currentFile: file.name,
        });
        const targetPath = joinWorkspacePath(destinationPath, file.name);
        await uploadWorkspaceBlob(targetPath, file);
        completed += 1;
        setUploadProgress({
          completed,
          total: files.length,
          currentFile: file.name,
        });
      }
      if (currentPathRef.current === destinationPath) {
        await loadBrowser(destinationPath, false);
      }
      setBrowserSuccess(
        `${files.length} file${files.length === 1 ? '' : 's'} uploaded to ${
          destinationPath || '.'
        }.`,
      );
    } catch (err: unknown) {
      if (currentPathRef.current === destinationPath) {
        await loadBrowser(destinationPath, false);
      }
      const message = err instanceof Error ? err.message : 'Failed to upload workspace files';
      setBrowserError(
        completed > 0
          ? `${completed} of ${files.length} files uploaded before the error: ${message}`
          : message,
      );
    } finally {
      setUploading(false);
      setUploadProgress(null);
    }
  };

  const handleDelete = (entry: WorkspaceBrowserEntry) => {
    setConfirmDeleteEntry(entry);
  };

  const doDelete = async (entry: WorkspaceBrowserEntry) => {
    const destinationPath = currentPathRef.current;
    setConfirmDeleteEntry(null);
    setDeletingPath(entry.path);
    setBrowserError(null);
    setBrowserSuccess(null);
    try {
      await deleteWorkspacePath(entry.path);
      if (currentPathRef.current === destinationPath) {
        await loadBrowser(destinationPath, false);
      }
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
    const trimmed = newFolderName.trim();
    if (!trimmed) return;
    setShowNewFolderModal(false);
    setNewFolderName('');
    const destinationPath = currentPathRef.current;
    const targetPath = joinWorkspacePath(destinationPath, trimmed);
    setCreatingFolder(true);
    setBrowserError(null);
    setBrowserSuccess(null);
    try {
      await createWorkspaceDirectory(targetPath);
      await loadBrowser(destinationPath, false);
      setBrowserSuccess(`${targetPath} created in the live workspace.`);
    } catch (err: unknown) {
      setBrowserError(err instanceof Error ? err.message : 'Failed to create workspace folder');
    } finally {
      setCreatingFolder(false);
    }
  };

  useEffect(() => {
    if (confirmDeleteEntry) {
      deleteCancelRef.current?.focus();
    } else if (showNewFolderModal) {
      newFolderInputRef.current?.focus();
    } else if (pendingUploads.length > 0) {
      uploadCancelRef.current?.focus();
    } else {
      return;
    }

    const closeOnEscape = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      setConfirmDeleteEntry(null);
      setShowNewFolderModal(false);
      setPendingUploads([]);
    };
    document.addEventListener('keydown', closeOnEscape);
    return () => document.removeEventListener('keydown', closeOnEscape);
  }, [confirmDeleteEntry, pendingUploads.length, showNewFolderModal]);

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
  const pendingUploadBytes = useMemo(
    () => pendingUploads.reduce((total, file) => total + file.size, 0),
    [pendingUploads],
  );
  const pendingUploadConflicts = useMemo(() => {
    const existingNames = new Set(browser?.entries.map((entry) => entry.name) ?? []);
    return pendingUploads.filter((file) => existingNames.has(file.name));
  }, [browser, pendingUploads]);

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
        <div
          role="alertdialog"
          aria-modal="true"
          aria-labelledby="delete-workspace-entry-title"
          aria-describedby="delete-workspace-entry-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
        >
          <div className="mx-4 w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6">
            <div className="mb-2 flex items-center justify-between">
              <h3 id="delete-workspace-entry-title" className="text-base font-semibold text-white">
                Delete {confirmDeleteEntry.kind === 'directory' ? 'Folder' : 'File'}
              </h3>
              <button
                type="button"
                onClick={() => setConfirmDeleteEntry(null)}
                className="text-gray-400 transition-colors hover:text-white"
                aria-label="Close delete confirmation"
              >
                <X className="h-5 w-5" aria-hidden="true" />
              </button>
            </div>
            <p id="delete-workspace-entry-description" className="mb-6 text-sm text-gray-400">
              Delete <span className="font-mono text-white">{confirmDeleteEntry.path}</span>
              {confirmDeleteEntry.kind === 'directory' ? ' and all of its contents' : ''}? This cannot be undone.
            </p>
            <div className="flex justify-end gap-3">
              <button
                ref={deleteCancelRef}
                type="button"
                onClick={() => setConfirmDeleteEntry(null)}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                type="button"
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
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="new-workspace-folder-title"
          aria-describedby="new-workspace-folder-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
        >
          <div className="mx-4 w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6">
            <div className="mb-4 flex items-center justify-between">
              <h3 id="new-workspace-folder-title" className="text-base font-semibold text-white">
                New Folder
              </h3>
              <button
                type="button"
                onClick={() => setShowNewFolderModal(false)}
                className="text-gray-400 transition-colors hover:text-white"
                aria-label="Close new folder dialog"
              >
                <X className="h-5 w-5" aria-hidden="true" />
              </button>
            </div>
            <p id="new-workspace-folder-description" className="mb-3 text-sm text-gray-400">
              Create a folder inside <span className="font-mono">{browser?.current_path || '.'}</span>.
            </p>
            <label htmlFor="new-workspace-folder-name" className="mb-1.5 block text-sm text-gray-300">
              Folder name or relative path
            </label>
            <input
              ref={newFolderInputRef}
              id="new-workspace-folder-name"
              type="text"
              value={newFolderName}
              onChange={(e) => setNewFolderName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void doCreateFolder();
              }}
              placeholder="Folder name or relative path"
              className="w-full rounded-lg border border-gray-700 bg-gray-800 px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
            />
            <div className="mt-4 flex justify-end gap-3">
              <button
                type="button"
                onClick={() => setShowNewFolderModal(false)}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                type="button"
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

      {/* Upload confirmation modal */}
      {pendingUploads.length > 0 && (
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="upload-workspace-files-title"
          aria-describedby="upload-workspace-files-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
        >
          <div className="w-full max-w-md rounded-xl border border-gray-700 bg-gray-900 p-6">
            <h3 id="upload-workspace-files-title" className="text-base font-semibold text-white">
              Confirm file upload
            </h3>
            <p id="upload-workspace-files-description" className="mt-2 text-sm text-gray-400">
              Upload {pendingUploads.length} file{pendingUploads.length === 1 ? '' : 's'} (
              {formatBytes(pendingUploadBytes)}) to{' '}
              <span className="font-mono text-gray-200">{browser?.current_path || '.'}</span>.
            </p>
            <ul className="mt-3 max-h-40 space-y-1 overflow-y-auto rounded-lg bg-gray-950/60 p-3 text-xs text-gray-300">
              {pendingUploads.map((file, index) => (
                <li key={`${file.name}-${file.size}-${index}`} className="flex justify-between gap-3">
                  <span className="min-w-0 truncate">{file.name}</span>
                  <span className="shrink-0 text-gray-500">{formatBytes(file.size)}</span>
                </li>
              ))}
            </ul>
            {pendingUploadConflicts.length > 0 && (
              <p role="alert" className="mt-3 text-sm text-amber-300">
                {pendingUploadConflicts.length} existing file
                {pendingUploadConflicts.length === 1 ? '' : 's'} with the same name may be
                replaced.
              </p>
            )}
            <div className="mt-5 flex justify-end gap-3">
              <button
                ref={uploadCancelRef}
                type="button"
                onClick={cancelUpload}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void confirmUploadFiles()}
                className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700"
              >
                Upload {pendingUploads.length} file{pendingUploads.length === 1 ? '' : 's'}
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
                    disabled={workspaceMutationInFlight}
                    className="rounded px-2 py-1 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
                  >
                    {crumb.label}
                  </button>
                </div>
              ))}
            </div>
          </div>

          <div className="grid grid-cols-2 gap-2 sm:flex sm:flex-wrap sm:items-center">
            <input
              ref={uploadInputRef}
              type="file"
              multiple
              className="hidden"
              aria-label="Choose workspace files to upload"
              onChange={(event) => prepareUploadFiles(event.currentTarget.files)}
            />
            <button
              type="button"
              onClick={() => void loadBrowser(currentPathRef.current, false)}
              disabled={refreshingBrowser || workspaceMutationInFlight}
              className="inline-flex min-h-10 items-center justify-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <RefreshCw
                className={`h-4 w-4 ${refreshingBrowser ? 'animate-spin' : ''}`}
                aria-hidden="true"
              />
              Refresh
            </button>
            <button
              type="button"
              onClick={() => uploadInputRef.current?.click()}
              disabled={!browser || uploading || creatingFolder || deletingPath !== null}
              className="inline-flex min-h-10 items-center justify-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <Upload className="h-4 w-4" aria-hidden="true" />
              {uploadProgress
                ? `Uploading ${uploadProgress.completed}/${uploadProgress.total}`
                : 'Upload Files'}
            </button>
            <button
              type="button"
              onClick={() => void handleCreateFolder()}
              disabled={!browser || uploading || creatingFolder || deletingPath !== null}
              className="inline-flex min-h-10 items-center justify-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50"
            >
              <FolderPlus className="h-4 w-4" aria-hidden="true" />
              {creatingFolder ? 'Creating…' : 'New Folder'}
            </button>
            <button
              type="button"
              onClick={() => void handleDownload(currentPathRef.current)}
              disabled={
                !browser ||
                uploading ||
                creatingFolder ||
                deletingPath !== null ||
                downloadingPath === currentPathRef.current
              }
              className="inline-flex min-h-10 items-center justify-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50 sm:col-auto"
            >
              <FolderDown className="h-4 w-4" aria-hidden="true" />
              {downloadingPath === currentPathRef.current
                ? 'Preparing…'
                : 'Download Current Folder'}
            </button>
          </div>
        </div>

        {uploadProgress && (
          <div
            role="status"
            aria-live="polite"
            className="border-b border-blue-800 bg-blue-900/20 px-4 py-3 text-sm text-blue-300"
          >
            Uploaded {uploadProgress.completed} of {uploadProgress.total}: currently processing{' '}
            <span className="font-mono">{uploadProgress.currentFile}</span>
          </div>
        )}

        {browserSuccess && (
          <div
            role="status"
            className="border-b border-green-800 bg-green-900/20 px-4 py-3 text-sm text-green-300"
          >
            {browserSuccess}
          </div>
        )}

        {browserError && (
          <div
            role="alert"
            className="border-b border-red-800 bg-red-900/20 px-4 py-3 text-sm text-red-300"
          >
            {browserError}
          </div>
        )}

        <div className="overflow-x-auto">
          <table className="w-full divide-y divide-gray-800 text-sm">
            <thead className="bg-gray-950/60 text-left text-xs uppercase tracking-wide text-gray-500">
              <tr>
                <th className="px-3 py-3 sm:px-4">Name</th>
                <th className="hidden px-4 py-3 lg:table-cell">Path</th>
                <th className="hidden px-4 py-3 sm:table-cell">Size</th>
                <th className="hidden px-4 py-3 xl:table-cell">Modified</th>
                <th className="px-3 py-3 text-right sm:px-4">Actions</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-800">
              {browser?.parent_path !== undefined && (
                <tr className="bg-gray-950/20">
                  <td className="px-3 py-3 sm:px-4">
                    <button
                      type="button"
                      onClick={() => void loadBrowser(browser.parent_path || '', false)}
                      disabled={workspaceMutationInFlight}
                      className="inline-flex items-center gap-2 text-gray-300 transition-colors hover:text-white disabled:opacity-50"
                    >
                      <Folder className="h-4 w-4 text-yellow-400" aria-hidden="true" />
                      ..
                    </button>
                    <p className="mt-1 break-all font-mono text-xs text-gray-600 lg:hidden">
                      {browser.parent_path || '.'}
                    </p>
                  </td>
                  <td className="hidden px-4 py-3 text-gray-500 lg:table-cell">
                    {browser.parent_path || '.'}
                  </td>
                  <td className="hidden px-4 py-3 text-gray-500 sm:table-cell">Directory</td>
                  <td className="hidden px-4 py-3 text-gray-500 xl:table-cell">-</td>
                  <td className="px-3 py-3 text-right sm:px-4">
                    <button
                      type="button"
                      onClick={() => void handleDownload(browser.parent_path || '')}
                      disabled={downloadingPath === (browser.parent_path || '')}
                      aria-label="Download parent folder"
                      className="inline-flex min-h-9 items-center gap-2 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50 sm:px-3"
                    >
                      <Download className="h-3.5 w-3.5" aria-hidden="true" />
                      <span className="hidden sm:inline">Download</span>
                    </button>
                  </td>
                </tr>
              )}

              {browser?.entries.map((entry: WorkspaceBrowserEntry) => {
                const isDirectory = entry.kind === 'directory';
                return (
                  <tr key={entry.path}>
                    <td className="min-w-0 px-3 py-3 sm:px-4">
                      {isDirectory ? (
                        <button
                          type="button"
                          onClick={() => void loadBrowser(entry.path, false)}
                          disabled={workspaceMutationInFlight}
                          className="flex min-w-0 items-center gap-2 text-left text-gray-200 transition-colors hover:text-white disabled:opacity-50"
                        >
                          <Folder className="h-4 w-4 shrink-0 text-yellow-400" aria-hidden="true" />
                          <span className="break-all">{entry.name}</span>
                        </button>
                      ) : (
                        <div className="flex min-w-0 items-center gap-2 text-gray-200">
                          <FileText className="h-4 w-4 shrink-0 text-blue-400" aria-hidden="true" />
                          <span className="break-all">{entry.name}</span>
                        </div>
                      )}
                      <p className="mt-1 break-all font-mono text-xs text-gray-600 lg:hidden">
                        {entry.path}
                      </p>
                      <p className="mt-1 text-xs text-gray-500 sm:hidden">
                        {formatBytes(entry.size_bytes)}
                      </p>
                    </td>
                    <td className="hidden px-4 py-3 font-mono text-xs text-gray-500 lg:table-cell">
                      {entry.path}
                    </td>
                    <td className="hidden px-4 py-3 text-gray-400 sm:table-cell">
                      {formatBytes(entry.size_bytes)}
                    </td>
                    <td className="hidden px-4 py-3 text-gray-400 xl:table-cell">
                      {formatTimestamp(entry.modified_at)}
                    </td>
                    <td className="px-3 py-3 sm:px-4">
                      <div className="flex flex-wrap justify-end gap-2">
                        {isDirectory && (
                          <button
                            type="button"
                            onClick={() => void loadBrowser(entry.path, false)}
                            disabled={workspaceMutationInFlight}
                            className="inline-flex min-h-9 items-center gap-2 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50 sm:px-3"
                          >
                            Open
                          </button>
                        )}
                        <button
                          type="button"
                          onClick={() => void handleDownload(entry.path)}
                          disabled={
                            downloadingPath === entry.path ||
                            creatingFolder ||
                            deletingPath === entry.path
                          }
                          aria-label={`Download ${entry.name}`}
                          className="inline-flex min-h-9 items-center gap-2 rounded-lg border border-gray-700 px-2.5 py-1.5 text-xs text-gray-300 transition-colors hover:bg-gray-800 hover:text-white disabled:opacity-50 sm:px-3"
                        >
                          <Download className="h-3.5 w-3.5" aria-hidden="true" />
                          <span className="hidden sm:inline">
                            {downloadingPath === entry.path ? 'Preparing…' : 'Download'}
                          </span>
                        </button>
                        <button
                          type="button"
                          onClick={() => void handleDelete(entry)}
                          disabled={
                            downloadingPath === entry.path ||
                            creatingFolder ||
                            deletingPath !== null
                          }
                          aria-label={`Delete ${entry.name}`}
                          className="inline-flex min-h-9 items-center gap-2 rounded-lg border border-red-900/60 px-2.5 py-1.5 text-xs text-red-300 transition-colors hover:bg-red-950/40 hover:text-red-200 disabled:opacity-50 sm:px-3"
                        >
                          <Trash2 className="h-3.5 w-3.5" aria-hidden="true" />
                          <span className="hidden sm:inline">
                            {deletingPath === entry.path ? 'Deleting…' : 'Delete'}
                          </span>
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
