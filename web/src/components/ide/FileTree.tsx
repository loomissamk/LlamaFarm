import { useCallback, useEffect, useRef, useState } from 'react';
import {
  ChevronDown,
  ChevronRight,
  Download,
  File,
  FilePlus,
  Folder,
  FolderPlus,
  Loader2,
  Pencil,
  RefreshCw,
  Trash2,
  Upload,
} from 'lucide-react';
import type { WorkspaceBrowserEntry } from '@/types/api';
import {
  createWorkspaceDirectory,
  createWorkspaceFile,
  deleteWorkspacePath,
  downloadWorkspacePath,
  getWorkspaceBrowser,
  renameWorkspaceFile,
  uploadWorkspaceBlob,
} from '@/lib/api';

function joinPath(base: string, name: string): string {
  return base ? `${base}/${name}` : name;
}

function parentOf(path: string): string {
  const idx = path.lastIndexOf('/');
  return idx === -1 ? '' : path.slice(0, idx);
}

function nameOf(path: string): string {
  const idx = path.lastIndexOf('/');
  return idx === -1 ? path : path.slice(idx + 1);
}

interface DraftState {
  parentPath: string;
  kind: 'file' | 'directory';
  value: string;
}

interface RenameState {
  path: string;
  value: string;
}

export interface FileTreeProps {
  activePath?: string | null;
  flashPath?: string | null;
  revealSignal?: { path: string; nonce: number } | null;
  onOpenFile: (path: string) => void;
  onFileRenamed?: (fromPath: string, toPath: string) => void;
  onFileDeleted?: (path: string) => void;
}

export default function FileTree({
  activePath,
  flashPath,
  revealSignal,
  onOpenFile,
  onFileRenamed,
  onFileDeleted,
}: Readonly<FileTreeProps>) {
  const [rootLabel, setRootLabel] = useState('.');
  const [childrenByDir, setChildrenByDir] = useState<Record<string, WorkspaceBrowserEntry[]>>({});
  const [expanded, setExpanded] = useState<Set<string>>(new Set(['']));
  const [loadingDirs, setLoadingDirs] = useState<Set<string>>(new Set());
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<DraftState | null>(null);
  const [rename, setRename] = useState<RenameState | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<WorkspaceBrowserEntry | null>(null);
  const [uploading, setUploading] = useState(false);
  const draftInputRef = useRef<HTMLInputElement | null>(null);
  const renameInputRef = useRef<HTMLInputElement | null>(null);
  const uploadInputRef = useRef<HTMLInputElement | null>(null);
  const uploadTargetDirRef = useRef('');

  const loadDir = useCallback(async (dirPath: string) => {
    setLoadingDirs((prev) => new Set(prev).add(dirPath));
    setError(null);
    try {
      const response = await getWorkspaceBrowser(dirPath);
      if (dirPath === '') {
        setRootLabel(response.root_path || '.');
      }
      const sorted = [...response.entries].sort((a, b) => {
        if (a.kind !== b.kind) return a.kind === 'directory' ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
      setChildrenByDir((prev) => ({ ...prev, [dirPath]: sorted }));
      return sorted;
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load workspace directory');
      return [];
    } finally {
      setLoadingDirs((prev) => {
        const next = new Set(prev);
        next.delete(dirPath);
        return next;
      });
    }
  }, []);

  useEffect(() => {
    void loadDir('');
  }, [loadDir]);

  useEffect(() => {
    if (draft && draftInputRef.current) {
      draftInputRef.current.focus();
      draftInputRef.current.select();
    }
  }, [draft]);

  useEffect(() => {
    if (rename && renameInputRef.current) {
      renameInputRef.current.focus();
      renameInputRef.current.select();
    }
  }, [rename]);

  // Always refetches (unlike a plain expand-on-click) — used after a mutation (create/rename/delete)
  // where a directory's children may already be cached and stale.
  const refreshDir = useCallback(
    async (dirPath: string) => {
      setExpanded((prev) => new Set(prev).add(dirPath));
      await loadDir(dirPath);
    },
    [loadDir],
  );

  const toggleExpanded = (dirPath: string) => {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(dirPath)) {
        next.delete(dirPath);
      } else {
        next.add(dirPath);
      }
      return next;
    });
    if (!childrenByDir[dirPath]) {
      void loadDir(dirPath);
    }
  };

  // Reveal + auto-expand ancestors when the agent touches a file elsewhere in the tree.
  useEffect(() => {
    if (!revealSignal?.path) return;
    let cancelled = false;

    const reveal = async () => {
      const segments = revealSignal.path.split('/').filter(Boolean);
      segments.pop(); // drop the file name itself; remaining are ancestor dirs
      let running = '';
      // Force-refresh (not just ensureExpanded) since the agent may have just created a new
      // file in a directory whose listing is already cached.
      await refreshDir(running);
      for (const segment of segments) {
        if (cancelled) return;
        running = joinPath(running, segment);
        await refreshDir(running);
      }
    };

    void reveal();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [revealSignal?.nonce]);

  const startDraft = (parentPath: string, kind: 'file' | 'directory') => {
    setDraft({ parentPath, kind, value: '' });
  };

  const commitDraft = async () => {
    if (!draft) return;
    const name = draft.value.trim();
    if (!name) {
      setDraft(null);
      return;
    }
    const targetPath = joinPath(draft.parentPath, name);
    const { parentPath, kind } = draft;
    setDraft(null);
    setError(null);
    try {
      if (kind === 'directory') {
        await createWorkspaceDirectory(targetPath);
      } else {
        await createWorkspaceFile(targetPath);
      }
      await refreshDir(parentPath);
      if (kind === 'file') {
        onOpenFile(targetPath);
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : `Failed to create ${kind}`);
    }
  };

  const startRename = (entry: WorkspaceBrowserEntry) => {
    setRename({ path: entry.path, value: entry.name });
  };

  const commitRename = async () => {
    if (!rename) return;
    const newName = rename.value.trim();
    const oldPath = rename.path;
    setRename(null);
    if (!newName || newName === nameOf(oldPath)) return;
    const newPath = joinPath(parentOf(oldPath), newName);
    setError(null);
    try {
      await renameWorkspaceFile(oldPath, newPath);
      await refreshDir(parentOf(oldPath));
      onFileRenamed?.(oldPath, newPath);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to rename file');
    }
  };

  const doDelete = async (entry: WorkspaceBrowserEntry) => {
    setConfirmDelete(null);
    setError(null);
    try {
      await deleteWorkspacePath(entry.path);
      await refreshDir(parentOf(entry.path));
      onFileDeleted?.(entry.path);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to delete');
    }
  };

  const startUpload = (targetDir: string) => {
    uploadTargetDirRef.current = targetDir;
    uploadInputRef.current?.click();
  };

  const handleUploadFiles = async (selected: FileList | null) => {
    if (!selected || selected.length === 0) return;
    const targetDir = uploadTargetDirRef.current;
    setUploading(true);
    setError(null);
    try {
      for (const file of selected) {
        await uploadWorkspaceBlob(joinPath(targetDir, file.name), file);
      }
      await refreshDir(targetDir);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to upload files');
    } finally {
      setUploading(false);
      if (uploadInputRef.current) uploadInputRef.current.value = '';
    }
  };

  const handleDownload = async (path: string) => {
    setError(null);
    try {
      await downloadWorkspacePath(path);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to download');
    }
  };

  const renderEntry = (entry: WorkspaceBrowserEntry, depth: number) => {
    const isDir = entry.kind === 'directory';
    const isExpanded = expanded.has(entry.path);
    const isRenaming = rename?.path === entry.path;
    const isFlashing = flashPath === entry.path;
    const isActive = !isDir && activePath === entry.path;

    return (
      <div key={entry.path}>
        <div
          className={`group flex items-center gap-1 rounded px-1 py-1 text-sm transition-colors ${
            isActive ? 'bg-blue-950/50 text-white' : 'text-gray-300 hover:bg-gray-800'
          } ${isFlashing ? 'animate-pulse bg-emerald-900/40 ring-1 ring-emerald-500/60' : ''}`}
          style={{ paddingLeft: `${depth * 14 + 4}px` }}
        >
          <button
            onClick={() => (isDir ? toggleExpanded(entry.path) : onOpenFile(entry.path))}
            className="flex min-w-0 flex-1 items-center gap-1 text-left"
          >
            {isDir ? (
              isExpanded ? (
                <ChevronDown className="h-3.5 w-3.5 flex-shrink-0 text-gray-500" />
              ) : (
                <ChevronRight className="h-3.5 w-3.5 flex-shrink-0 text-gray-500" />
              )
            ) : (
              <span className="w-3.5 flex-shrink-0" />
            )}
            {isDir ? (
              <Folder className="h-3.5 w-3.5 flex-shrink-0 text-yellow-400" />
            ) : (
              <File className="h-3.5 w-3.5 flex-shrink-0 text-blue-400" />
            )}
            {isRenaming ? (
              <input
                ref={renameInputRef}
                value={rename.value}
                onChange={(e) => setRename({ ...rename, value: e.target.value })}
                onClick={(e) => e.stopPropagation()}
                onKeyDown={(e) => {
                  if (e.key === 'Enter') void commitRename();
                  if (e.key === 'Escape') setRename(null);
                }}
                onBlur={() => void commitRename()}
                className="min-w-0 flex-1 rounded border border-blue-500 bg-gray-950 px-1 text-xs text-white"
              />
            ) : (
              <span className="truncate">{entry.name}</span>
            )}
          </button>
          {!isRenaming && (
            <div className="hidden flex-shrink-0 items-center gap-0.5 group-hover:flex">
              {isDir && (
                <>
                  <button
                    title="New file"
                    onClick={() => startDraft(entry.path, 'file')}
                    className="rounded p-0.5 text-gray-500 hover:bg-gray-700 hover:text-white"
                  >
                    <FilePlus className="h-3 w-3" />
                  </button>
                  <button
                    title="New folder"
                    onClick={() => startDraft(entry.path, 'directory')}
                    className="rounded p-0.5 text-gray-500 hover:bg-gray-700 hover:text-white"
                  >
                    <FolderPlus className="h-3 w-3" />
                  </button>
                  <button
                    title="Upload files here"
                    onClick={() => startUpload(entry.path)}
                    className="rounded p-0.5 text-gray-500 hover:bg-gray-700 hover:text-white"
                  >
                    <Upload className="h-3 w-3" />
                  </button>
                </>
              )}
              <button
                title={isDir ? 'Download as archive' : 'Download'}
                onClick={() => void handleDownload(entry.path)}
                className="rounded p-0.5 text-gray-500 hover:bg-gray-700 hover:text-white"
              >
                <Download className="h-3 w-3" />
              </button>
              <button
                title="Rename"
                onClick={() => startRename(entry)}
                className="rounded p-0.5 text-gray-500 hover:bg-gray-700 hover:text-white"
              >
                <Pencil className="h-3 w-3" />
              </button>
              <button
                title="Delete"
                onClick={() => setConfirmDelete(entry)}
                className="rounded p-0.5 text-gray-500 hover:bg-red-950/50 hover:text-red-300"
              >
                <Trash2 className="h-3 w-3" />
              </button>
            </div>
          )}
        </div>

        {isDir && isExpanded && (
          <div>
            {loadingDirs.has(entry.path) && !childrenByDir[entry.path] && (
              <div
                className="flex items-center gap-1 py-1 text-xs text-gray-500"
                style={{ paddingLeft: `${(depth + 1) * 14 + 4}px` }}
              >
                <Loader2 className="h-3 w-3 animate-spin" /> Loading...
              </div>
            )}
            {draft?.parentPath === entry.path && (
              <div
                className="flex items-center gap-1 py-1"
                style={{ paddingLeft: `${(depth + 1) * 14 + 4}px` }}
              >
                {draft.kind === 'directory' ? (
                  <Folder className="h-3.5 w-3.5 flex-shrink-0 text-yellow-400" />
                ) : (
                  <File className="h-3.5 w-3.5 flex-shrink-0 text-blue-400" />
                )}
                <input
                  ref={draftInputRef}
                  value={draft.value}
                  onChange={(e) => setDraft({ ...draft, value: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void commitDraft();
                    if (e.key === 'Escape') setDraft(null);
                  }}
                  onBlur={() => void commitDraft()}
                  placeholder={draft.kind === 'directory' ? 'folder name' : 'file name'}
                  className="min-w-0 flex-1 rounded border border-blue-500 bg-gray-950 px-1 text-xs text-white placeholder-gray-600"
                />
              </div>
            )}
            {(childrenByDir[entry.path] ?? []).map((child) => renderEntry(child, depth + 1))}
          </div>
        )}
      </div>
    );
  };

  const rootEntries = childrenByDir[''] ?? [];

  return (
    <div className="flex h-full min-h-0 flex-col">
      {confirmDelete && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60">
          <div className="mx-4 w-full max-w-sm rounded-xl border border-gray-700 bg-gray-900 p-5">
            <h3 className="text-sm font-semibold text-white">
              Delete {confirmDelete.kind === 'directory' ? 'folder' : 'file'}
            </h3>
            <p className="mt-2 text-xs text-gray-400">
              <span className="font-mono text-gray-200">{confirmDelete.path}</span>
              {confirmDelete.kind === 'directory' ? ' and all of its contents' : ''} will be
              permanently deleted.
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                onClick={() => setConfirmDelete(null)}
                className="rounded-lg border border-gray-700 px-3 py-1.5 text-xs text-gray-300 hover:bg-gray-800"
              >
                Cancel
              </button>
              <button
                onClick={() => void doDelete(confirmDelete)}
                className="rounded-lg bg-red-600 px-3 py-1.5 text-xs font-medium text-white hover:bg-red-700"
              >
                Delete
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="flex items-center justify-between gap-2 border-b border-gray-800 px-2 py-1.5">
        <span className="truncate font-mono text-[11px] text-gray-500" title={rootLabel}>
          {rootLabel}
        </span>
        <div className="flex flex-shrink-0 items-center gap-0.5">
          <input
            ref={uploadInputRef}
            type="file"
            multiple
            className="hidden"
            onChange={(event) => void handleUploadFiles(event.currentTarget.files)}
          />
          <button
            title="New file at root"
            onClick={() => startDraft('', 'file')}
            className="rounded p-1 text-gray-500 hover:bg-gray-800 hover:text-white"
          >
            <FilePlus className="h-3.5 w-3.5" />
          </button>
          <button
            title="New folder at root"
            onClick={() => startDraft('', 'directory')}
            className="rounded p-1 text-gray-500 hover:bg-gray-800 hover:text-white"
          >
            <FolderPlus className="h-3.5 w-3.5" />
          </button>
          <button
            title="Upload files to root"
            onClick={() => startUpload('')}
            disabled={uploading}
            className="rounded p-1 text-gray-500 hover:bg-gray-800 hover:text-white disabled:opacity-50"
          >
            {uploading ? (
              <Loader2 className="h-3.5 w-3.5 animate-spin" />
            ) : (
              <Upload className="h-3.5 w-3.5" />
            )}
          </button>
          <button
            title="Download workspace as archive"
            onClick={() => void handleDownload('')}
            className="rounded p-1 text-gray-500 hover:bg-gray-800 hover:text-white"
          >
            <Download className="h-3.5 w-3.5" />
          </button>
          <button
            title="Refresh"
            onClick={() => {
              for (const dir of [...expanded]) void loadDir(dir);
            }}
            className="rounded p-1 text-gray-500 hover:bg-gray-800 hover:text-white"
          >
            <RefreshCw className="h-3.5 w-3.5" />
          </button>
        </div>
      </div>

      {error && (
        <div className="border-b border-red-900/60 bg-red-950/30 px-2 py-1 text-[11px] text-red-300">
          {error}
        </div>
      )}

      <div className="flex-1 overflow-y-auto px-1 py-1">
        {loadingDirs.has('') && rootEntries.length === 0 ? (
          <div className="flex items-center gap-1 px-2 py-2 text-xs text-gray-500">
            <Loader2 className="h-3 w-3 animate-spin" /> Loading workspace...
          </div>
        ) : (
          <>
            {draft?.parentPath === '' && (
              <div className="flex items-center gap-1 px-1 py-1" style={{ paddingLeft: '4px' }}>
                {draft.kind === 'directory' ? (
                  <Folder className="h-3.5 w-3.5 flex-shrink-0 text-yellow-400" />
                ) : (
                  <File className="h-3.5 w-3.5 flex-shrink-0 text-blue-400" />
                )}
                <input
                  ref={draftInputRef}
                  value={draft.value}
                  onChange={(e) => setDraft({ ...draft, value: e.target.value })}
                  onKeyDown={(e) => {
                    if (e.key === 'Enter') void commitDraft();
                    if (e.key === 'Escape') setDraft(null);
                  }}
                  onBlur={() => void commitDraft()}
                  placeholder={draft.kind === 'directory' ? 'folder name' : 'file name'}
                  className="min-w-0 flex-1 rounded border border-blue-500 bg-gray-950 px-1 text-xs text-white placeholder-gray-600"
                />
              </div>
            )}
            {rootEntries.map((entry) => renderEntry(entry, 0))}
            {rootEntries.length === 0 && !draft && (
              <p className="px-2 py-4 text-center text-xs text-gray-600">Workspace is empty.</p>
            )}
          </>
        )}
      </div>
    </div>
  );
}
