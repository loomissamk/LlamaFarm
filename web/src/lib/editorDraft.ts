export interface TextChangeSummary {
  changed: boolean;
  firstChangedLine: number | null;
  originalChangedLines: number;
  draftChangedLines: number;
  addedLines: number;
  removedLines: number;
  characterDelta: number;
  description: string;
  originalPreview: string[];
  draftPreview: string[];
}

const PREVIEW_LINES = 5;

export function summarizeTextChange(original: string, draft: string): TextChangeSummary {
  if (original === draft) {
    return {
      changed: false,
      firstChangedLine: null,
      originalChangedLines: 0,
      draftChangedLines: 0,
      addedLines: 0,
      removedLines: 0,
      characterDelta: 0,
      description: 'No draft changes.',
      originalPreview: [],
      draftPreview: [],
    };
  }

  const originalLines = original.split('\n');
  const draftLines = draft.split('\n');
  let prefix = 0;
  while (
    prefix < originalLines.length &&
    prefix < draftLines.length &&
    originalLines[prefix] === draftLines[prefix]
  ) {
    prefix += 1;
  }

  let suffix = 0;
  while (
    suffix < originalLines.length - prefix &&
    suffix < draftLines.length - prefix &&
    originalLines[originalLines.length - 1 - suffix] ===
      draftLines[draftLines.length - 1 - suffix]
  ) {
    suffix += 1;
  }

  const originalEnd = originalLines.length - suffix;
  const draftEnd = draftLines.length - suffix;
  const originalChanged = originalLines.slice(prefix, originalEnd);
  const draftChanged = draftLines.slice(prefix, draftEnd);
  const addedLines = Math.max(0, draftChanged.length - originalChanged.length);
  const removedLines = Math.max(0, originalChanged.length - draftChanged.length);
  const firstChangedLine = prefix + 1;

  let description: string;
  if (originalChanged.length === 0) {
    description = `Insert ${draftChanged.length} line${draftChanged.length === 1 ? '' : 's'} at line ${firstChangedLine}.`;
  } else if (draftChanged.length === 0) {
    const end = prefix + originalChanged.length;
    description = `Remove original line${originalChanged.length === 1 ? '' : 's'} ${firstChangedLine}${end === firstChangedLine ? '' : `–${end}`}.`;
  } else {
    const end = prefix + originalChanged.length;
    description = `Replace original line${originalChanged.length === 1 ? '' : 's'} ${firstChangedLine}${end === firstChangedLine ? '' : `–${end}`} with ${draftChanged.length} draft line${draftChanged.length === 1 ? '' : 's'}.`;
  }

  return {
    changed: true,
    firstChangedLine,
    originalChangedLines: originalChanged.length,
    draftChangedLines: draftChanged.length,
    addedLines,
    removedLines,
    characterDelta: draft.length - original.length,
    description,
    originalPreview: originalChanged.slice(0, PREVIEW_LINES),
    draftPreview: draftChanged.slice(0, PREVIEW_LINES),
  };
}

export function createBackupFilename(baseName: string, now = new Date()): string {
  const safeBase = baseName
    .trim()
    .replace(/[^a-zA-Z0-9._-]+/g, '-')
    .replace(/^-+|-+$/g, '') || 'backup';
  const timestamp = now.toISOString().replace(/[:.]/g, '-');
  return `${safeBase}.${timestamp}.bak`;
}

export function downloadTextBackup(filename: string, content: string) {
  const url = URL.createObjectURL(new Blob([content], { type: 'text/plain;charset=utf-8' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  URL.revokeObjectURL(url);
}
