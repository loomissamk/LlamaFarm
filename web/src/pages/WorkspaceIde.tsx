import { lazy, Suspense } from 'react';
import { Code2 } from 'lucide-react';

// Monaco is a multi-megabyte bundle; load it only when this page is visited.
const IdePanel = lazy(() => import('@/components/ide/IdePanel'));

export default function WorkspaceIde() {
  return (
    <div className="flex h-[calc(100vh-3.5rem)] min-h-0 flex-col">
      <div className="flex flex-shrink-0 items-center gap-2 border-b border-gray-800 bg-gray-950 px-4 py-2">
        <Code2 className="h-4 w-4 text-purple-400" />
        <h2 className="text-sm font-semibold text-white">Workspace IDE</h2>
        <p className="ml-2 hidden text-xs text-gray-500 sm:block">
          Full CRUD file tree, Monaco editor, and a live terminal running in the workspace
          (.venv on PATH). The agent's file edits and shell commands show up here in real time
          on the Agent page.
        </p>
      </div>
      <div className="min-h-0 flex-1">
        <Suspense
          fallback={
            <div className="flex h-full items-center justify-center text-sm text-gray-500">
              Loading IDE...
            </div>
          }
        >
          <IdePanel />
        </Suspense>
      </div>
    </div>
  );
}
