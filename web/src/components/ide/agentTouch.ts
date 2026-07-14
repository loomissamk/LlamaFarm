export const AGENT_FILE_TOOLS = new Set(['file_write', 'file_edit', 'apply_patch']);

const PATCH_PATH_RE = /^(?:\+\+\+|---) [ab]\/(.+)$/gm;

export function extractTouchedPaths(toolName: string, args: unknown): string[] {
  if (!args || typeof args !== 'object') return [];
  const record = args as Record<string, unknown>;

  if ((toolName === 'file_write' || toolName === 'file_edit') && typeof record.path === 'string') {
    return [record.path];
  }

  if (toolName === 'apply_patch' && typeof record.patch === 'string') {
    const paths = new Set<string>();
    for (const match of record.patch.matchAll(PATCH_PATH_RE)) {
      const path = match[1]?.trim();
      if (path && path !== 'dev/null') {
        paths.add(path);
      }
    }
    return [...paths];
  }

  return [];
}
