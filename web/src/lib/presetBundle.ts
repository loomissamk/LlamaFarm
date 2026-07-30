import type { ConfigPresetEntry, WorkspaceFileResponse } from '../types/api';

export interface PresetBundleOperations {
  getConfig: () => Promise<string>;
  putConfig: (content: string) => Promise<unknown>;
  getWorkspaceFile: (name: string) => Promise<WorkspaceFileResponse>;
  putWorkspaceFile: (name: string, content: string) => Promise<unknown>;
}

export class PresetBundleApplyError extends Error {
  readonly mutationsStarted: boolean;
  readonly rollbackFailures: string[];

  constructor(message: string, mutationsStarted: boolean, rollbackFailures: string[]) {
    super(message);
    this.name = 'PresetBundleApplyError';
    this.mutationsStarted = mutationsStarted;
    this.rollbackFailures = rollbackFailures;
  }

  toDisplayMessage(): string {
    if (!this.mutationsStarted) {
      return `${this.message}. No live bundle steps completed.`;
    }
    if (this.rollbackFailures.length === 0) {
      return `${this.message}. Applied steps were rolled back to the captured server state.`;
    }
    return `${this.message}. Rollback needs attention: ${this.rollbackFailures.join('; ')}.`;
  }
}

function getErrorMessage(error: unknown, fallback: string): string {
  return error instanceof Error ? error.message : fallback;
}

export async function applyPresetBundleWithRollback(
  preset: ConfigPresetEntry,
  operations: PresetBundleOperations,
): Promise<void> {
  let originalConfig: string;
  const originalFiles = new Map<string, WorkspaceFileResponse>();

  try {
    originalConfig = await operations.getConfig();
    for (const file of preset.workspace_files) {
      originalFiles.set(file.name, await operations.getWorkspaceFile(file.name));
    }
  } catch (error: unknown) {
    throw new PresetBundleApplyError(
      getErrorMessage(error, 'Failed to capture the live bundle before applying the preset'),
      false,
      [],
    );
  }

  let configApplied = false;
  const appliedFileNames: string[] = [];

  try {
    await operations.putConfig(preset.content);
    configApplied = true;
    for (const file of preset.workspace_files) {
      await operations.putWorkspaceFile(file.name, file.content);
      appliedFileNames.push(file.name);
    }
  } catch (error: unknown) {
    const rollbackFailures: string[] = [];

    for (const fileName of [...appliedFileNames].reverse()) {
      const original = originalFiles.get(fileName);
      if (!original) continue;

      try {
        await operations.putWorkspaceFile(fileName, original.content);
        if (!original.exists) {
          rollbackFailures.push(
            `${fileName} was previously absent and was restored as an empty file because the current API has no file-delete operation`,
          );
        }
      } catch (rollbackError: unknown) {
        rollbackFailures.push(
          `${fileName}: ${getErrorMessage(rollbackError, 'restore failed')}`,
        );
      }
    }

    if (configApplied) {
      try {
        await operations.putConfig(originalConfig);
      } catch (rollbackError: unknown) {
        rollbackFailures.push(
          `configuration: ${getErrorMessage(rollbackError, 'restore failed')}`,
        );
      }
    }

    throw new PresetBundleApplyError(
      getErrorMessage(error, 'Failed to apply preset bundle'),
      configApplied || appliedFileNames.length > 0,
      rollbackFailures,
    );
  }
}
