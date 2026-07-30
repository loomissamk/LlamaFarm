import { ConnectionsPanel } from './Connections';
import { IntegrationsPanel } from './Integrations';
import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  AlertTriangle,
  CheckCircle,
  Download,
  Eye,
  RefreshCcw,
  RotateCcw,
  Save,
  Settings,
  ShieldAlert,
} from 'lucide-react';
import type { ConfigPresetsResponse } from '@/types/api';
import {
  getConfig,
  getConfigPresets,
  getWorkspaceFile,
  putConfig,
  putWorkspaceFile,
} from '@/lib/api';
import {
  createBackupFilename,
  downloadTextBackup,
  summarizeTextChange,
} from '@/lib/editorDraft';
import {
  applyPresetBundleWithRollback,
  PresetBundleApplyError,
} from '@/lib/presetBundle';
import { useDirtyDraftGuard } from '@/hooks/useDirtyDraftGuard';

type PresetMode = 'safe' | 'god';

export default function Config() {
  const [liveConfig, setLiveConfig] = useState('');
  const [config, setConfig] = useState('');
  const [presets, setPresets] = useState<ConfigPresetsResponse | null>(null);
  const [presetMode, setPresetMode] = useState<PresetMode>('god');
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [presetPreviewOpen, setPresetPreviewOpen] = useState(false);
  const [presetConfirmed, setPresetConfirmed] = useState(false);
  const [lastServerRefresh, setLastServerRefresh] = useState<Date | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  const selectedPreset = presets?.[presetMode] ?? presets?.god ?? null;
  const isDirty = config !== liveConfig;
  const changeSummary = useMemo(
    () => summarizeTextChange(liveConfig, config),
    [config, liveConfig],
  );
  const presetConfigSummary = useMemo(
    () => summarizeTextChange(liveConfig, selectedPreset?.content ?? liveConfig),
    [liveConfig, selectedPreset],
  );

  useDirtyDraftGuard(
    isDirty,
    'The configuration editor has unsaved changes. Leave and discard the draft?',
  );

  const loadConfiguration = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [currentConfig, presetData] = await Promise.all([
        getConfig(),
        getConfigPresets(),
      ]);
      setLiveConfig(currentConfig);
      setConfig(currentConfig);
      setPresets(presetData);
      setLastServerRefresh(new Date());
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load configuration');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void loadConfiguration();
  }, [loadConfiguration]);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await putConfig(config);
      setLiveConfig(config);
      setLastServerRefresh(new Date());
      setSuccess('Configuration saved successfully.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  };

  const handleDiscardDraft = () => {
    if (
      isDirty &&
      !window.confirm(
        'Discard the configuration draft and restore the cached live copy?',
      )
    ) {
      return;
    }
    setConfig(liveConfig);
    setError(null);
    setSuccess('Draft discarded. The editor now matches the last server response.');
  };

  const handleRefreshFromServer = async () => {
    if (
      isDirty &&
      !window.confirm('Refresh from the server and discard the unsaved configuration draft?')
    ) {
      return;
    }

    setRefreshing(true);
    setError(null);
    setSuccess(null);
    try {
      const currentConfig = await getConfig();
      setLiveConfig(currentConfig);
      setConfig(currentConfig);
      setLastServerRefresh(new Date());
      setSuccess('Fetched the current configuration from the server.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to refresh configuration');
    } finally {
      setRefreshing(false);
    }
  };

  const handleApplyPreset = () => {
    if (!selectedPreset) {
      return;
    }
    if (isDirty && !window.confirm('Replace the unsaved configuration draft with this preset?')) {
      return;
    }

    setConfig(selectedPreset.content);
    setError(null);
    setSuccess(
      `${selectedPreset.label} preset loaded into the editor. Use Apply Live to sync config plus AGENTS.md.`,
    );
  };

  const openPresetPreview = () => {
    setPresetConfirmed(false);
    setPresetPreviewOpen(true);
    setError(null);
    setSuccess(null);
  };

  const handleApplyPresetLive = async () => {
    if (!selectedPreset) {
      return;
    }

    setSaving(true);
    setError(null);
    setSuccess(null);
    const editorDraftBeforeApply = config;

    try {
      await applyPresetBundleWithRollback(selectedPreset, {
        getConfig,
        putConfig,
        getWorkspaceFile,
        putWorkspaceFile,
      });
      setLiveConfig(selectedPreset.content);
      setConfig(selectedPreset.content);
      setLastServerRefresh(new Date());
      setPresetPreviewOpen(false);
      setPresetConfirmed(false);
      setSuccess(
        `${selectedPreset.label} bundle applied live. Configuration and workspace prompt files are now in sync.`,
      );
    } catch (err: unknown) {
      try {
        const currentConfig = await getConfig();
        setLiveConfig(currentConfig);
        setConfig(editorDraftBeforeApply);
        setLastServerRefresh(new Date());
      } catch {
        // Keep the cached editor state and report the primary/rollback outcome.
      }

      setError(
        err instanceof PresetBundleApplyError
          ? err.toDisplayMessage()
          : err instanceof Error
            ? err.message
            : 'Failed to apply preset bundle',
      );
    } finally {
      setSaving(false);
    }
  };

  const handleDownloadBackup = () => {
    downloadTextBackup(createBackupFilename('llamafarm-config.live.toml'), liveConfig);
    setSuccess('Downloaded a backup of the last configuration received from the server.');
  };

  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  useEffect(() => {
    if (!presetPreviewOpen) return;
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && !saving) {
        setPresetPreviewOpen(false);
        setPresetConfirmed(false);
      }
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [presetPreviewOpen, saving]);

  if (loading) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  if (!presets || !selectedPreset) {
    return (
      <div className="p-6">
        <div
          role="alert"
          className="mx-auto flex max-w-2xl flex-wrap items-center gap-3 rounded-xl border border-red-700 bg-red-900/30 p-4 text-red-300"
        >
          <AlertTriangle className="h-5 w-5 flex-shrink-0" />
          <p className="min-w-0 flex-1 text-sm">
            Configuration could not be loaded: {error ?? 'preset data was unavailable'}
          </p>
          <button
            type="button"
            onClick={() => void loadConfiguration()}
            className="inline-flex items-center gap-2 rounded-lg border border-red-700 px-3 py-2 text-sm font-medium hover:bg-red-900/50"
          >
            <RefreshCcw className="h-4 w-4" />
            Retry
          </button>
        </div>
      </div>
    );
  }

  const lineCount = config.split('\n').length;

  return (
    <div className="space-y-6 p-6">
      <ConnectionsPanel />
      <div className="flex flex-col gap-4 lg:flex-row lg:items-start lg:justify-between">
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            <Settings className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Configuration</h2>
          </div>
          <p className="max-w-3xl text-sm text-gray-400">
            Toggle between a safer autonomous bundle and an escalated God bundle, then edit the
            raw TOML directly or apply the full runtime preset live.
          </p>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <button
            type="button"
            onClick={handleDownloadBackup}
            disabled={saving || refreshing || !liveConfig}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
          >
            <Download className="h-4 w-4" />
            Backup Live
          </button>
          <button
            type="button"
            onClick={handleDiscardDraft}
            disabled={saving || refreshing || !isDirty}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
          >
            <RotateCcw className="h-4 w-4" />
            Discard Draft
          </button>
          <button
            type="button"
            onClick={() => void handleRefreshFromServer()}
            disabled={saving || refreshing}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
          >
            <RefreshCcw className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`} />
            {refreshing ? 'Refreshing...' : 'Refresh Server'}
          </button>
          <button
            type="button"
            onClick={handleSave}
            disabled={saving || refreshing || !isDirty}
            className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
          >
            <Save className="h-4 w-4" />
            {saving ? 'Saving...' : 'Save'}
          </button>
        </div>
      </div>

      <div className="rounded-2xl border border-gray-800 bg-gray-900/80 p-5">
        <div className="flex flex-col gap-5 xl:flex-row xl:items-start xl:justify-between">
          <div className="space-y-4">
            <div className="inline-flex rounded-xl border border-gray-800 bg-gray-950 p-1">
              {(['safe', 'god'] as PresetMode[]).map((mode) => {
                const active = mode === presetMode;
                return (
                  <button
                    key={mode}
                    type="button"
                    onClick={() => setPresetMode(mode)}
                    className={[
                      'min-w-[132px] rounded-lg px-4 py-2 text-sm font-semibold transition-colors',
                      active
                        ? mode === 'safe'
                          ? 'bg-emerald-600 text-white'
                          : 'bg-amber-500 text-gray-950'
                        : 'text-gray-400 hover:bg-gray-900 hover:text-white',
                    ].join(' ')}
                  >
                    {mode === 'safe' ? 'Safe' : 'God'}
                  </button>
                );
              })}
            </div>

            <div className="space-y-3">
              <div className="flex items-center gap-3">
                <span
                  className={[
                    'inline-flex rounded-full px-2.5 py-1 text-xs font-semibold uppercase tracking-[0.18em]',
                    presetMode === 'safe'
                      ? 'bg-emerald-900/50 text-emerald-300'
                      : 'bg-amber-900/50 text-amber-200',
                  ].join(' ')}
                >
                  {selectedPreset.label}
                </span>
                <span className="text-xs font-medium uppercase tracking-[0.18em] text-gray-500">
                  editor preset
                </span>
              </div>
              <p className="max-w-3xl text-sm text-gray-300">{selectedPreset.summary}</p>
              <div className="flex flex-wrap gap-2">
                {selectedPreset.highlights.map((highlight) => (
                  <span
                    key={highlight}
                    className="rounded-full border border-gray-700 bg-gray-950 px-3 py-1 text-xs text-gray-300"
                  >
                    {highlight}
                  </span>
                ))}
              </div>
            </div>
          </div>

          <div className="min-w-[280px] rounded-xl border border-gray-800 bg-gray-950/80 p-4">
            <p className="text-sm font-medium text-white">Preset controls</p>
            <p className="mt-1 text-sm text-gray-400">
              Loading a preset only replaces the editor contents. Apply Live also syncs the
              preset&apos;s AGENTS.md into the workspace.
            </p>
            <button
              type="button"
              onClick={handleApplyPreset}
              disabled={saving}
              className={[
                'mt-4 w-full rounded-lg px-4 py-2 text-sm font-semibold transition-colors',
                presetMode === 'safe'
                  ? 'bg-emerald-600 text-white hover:bg-emerald-500'
                  : 'bg-amber-500 text-gray-950 hover:bg-amber-400',
              ].join(' ')}
            >
              Load {selectedPreset.label} Into Editor
            </button>
            <button
              type="button"
              onClick={openPresetPreview}
              disabled={saving}
              className="mt-3 w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-semibold text-white transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
            >
              <span className="inline-flex items-center gap-2">
                <Eye className="h-4 w-4" />
                Preview {selectedPreset.label} Live Apply
              </span>
            </button>
            <div className="mt-4 rounded-lg border border-gray-800 bg-gray-950 px-3 py-3">
              <p className="text-xs font-semibold uppercase tracking-[0.18em] text-gray-500">
                Runtime Bundle
              </p>
              <div className="mt-2 flex flex-wrap gap-2">
                {selectedPreset.workspace_files.map((file) => (
                  <span
                    key={file.name}
                    className="rounded-full border border-gray-700 bg-gray-900 px-3 py-1 text-xs text-gray-300"
                  >
                    {file.name}
                  </span>
                ))}
              </div>
            </div>
            <div className="mt-4 flex items-center justify-between text-xs text-gray-500">
              <span>{lineCount} lines in editor</span>
              <span>{isDirty ? 'unsaved changes' : 'synced to live config'}</span>
            </div>
            <p className="mt-2 text-xs text-gray-600">
              {lastServerRefresh
                ? `Last server response ${lastServerRefresh.toLocaleTimeString()}`
                : 'No server response recorded'}
            </p>
          </div>
        </div>
      </div>

      <div
        className={[
          'rounded-xl border p-4',
          isDirty
            ? 'border-blue-700/50 bg-blue-950/20'
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
              <p className="text-xs font-semibold uppercase tracking-wider text-blue-300">Draft</p>
              <pre className="mt-2 overflow-auto whitespace-pre-wrap text-xs text-gray-300">
                {changeSummary.draftPreview.join('\n') || '(no lines)'}
              </pre>
            </div>
          </div>
        )}
      </div>

      <div className="flex items-start gap-3 rounded-lg border border-yellow-700/40 bg-yellow-900/20 p-4">
        <ShieldAlert className="mt-0.5 h-5 w-5 flex-shrink-0 text-yellow-400" />
        <div>
          <p className="text-sm font-medium text-yellow-300">Sensitive fields are masked</p>
          <p className="mt-0.5 text-sm text-yellow-400/70">
            API keys, tokens, and passwords are hidden for security. To update a masked field,
            replace the entire masked value with your new value.
          </p>
        </div>
      </div>

      {success && (
        <div
          role="status"
          className="flex items-center gap-2 rounded-lg border border-green-700 bg-green-900/30 p-3"
        >
          <CheckCircle className="h-4 w-4 flex-shrink-0 text-green-400" />
          <span className="text-sm text-green-300">{success}</span>
        </div>
      )}

      {error && (
        <div
          role="alert"
          className="flex items-center gap-2 rounded-lg border border-red-700 bg-red-900/30 p-3"
        >
          <AlertTriangle className="h-4 w-4 flex-shrink-0 text-red-400" />
          <span className="text-sm text-red-300">{error}</span>
        </div>
      )}

      {presetPreviewOpen && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
          role="dialog"
          aria-modal="true"
          aria-labelledby="preset-preview-title"
        >
          <div className="max-h-[90vh] w-full max-w-2xl overflow-y-auto rounded-2xl border border-gray-700 bg-gray-900 p-6 shadow-2xl">
            <div className="flex items-start justify-between gap-4">
              <div>
                <h3 id="preset-preview-title" className="text-lg font-semibold text-white">
                  Apply {selectedPreset.label} bundle live?
                </h3>
                <p className="mt-2 text-sm text-gray-400">
                  This preview does not change the server. Applying captures the current config and
                  workspace files first, then restores completed steps if a later write fails.
                </p>
                {isDirty && (
                  <p className="mt-2 text-sm text-amber-300">
                    The current editor draft is unsaved. A successful live apply replaces it; a
                    failed apply keeps it available in the editor.
                  </p>
                )}
              </div>
              <span
                className={[
                  'rounded-full px-3 py-1 text-xs font-semibold uppercase tracking-wider',
                  presetMode === 'safe'
                    ? 'bg-emerald-900/60 text-emerald-300'
                    : 'bg-amber-900/60 text-amber-200',
                ].join(' ')}
              >
                {selectedPreset.label}
              </span>
            </div>

            <div className="mt-5 rounded-xl border border-gray-800 bg-gray-950 p-4">
              <p className="text-sm font-semibold text-white">Configuration</p>
              <p className="mt-2 text-sm text-gray-300">{presetConfigSummary.description}</p>
              <div className="mt-2 flex flex-wrap gap-3 text-xs text-gray-500">
                <span>{presetConfigSummary.originalChangedLines} live lines replaced</span>
                <span>{presetConfigSummary.draftChangedLines} preset lines written</span>
                <span>
                  {presetConfigSummary.characterDelta >= 0 ? '+' : ''}
                  {presetConfigSummary.characterDelta} characters
                </span>
              </div>
            </div>

            <div className="mt-4 rounded-xl border border-gray-800 bg-gray-950 p-4">
              <p className="text-sm font-semibold text-white">Workspace files</p>
              <div className="mt-3 space-y-2">
                {selectedPreset.workspace_files.map((file) => (
                  <div
                    key={file.name}
                    className="flex items-center justify-between rounded-lg border border-gray-800 bg-gray-900 px-3 py-2"
                  >
                    <span className="font-mono text-sm text-gray-200">{file.name}</span>
                    <span className="text-xs text-gray-500">
                      {file.content.split('\n').length} preset lines
                    </span>
                  </div>
                ))}
              </div>
            </div>

            <label className="mt-5 flex cursor-pointer items-start gap-3 rounded-xl border border-gray-700 bg-gray-950 p-4">
              <input
                type="checkbox"
                checked={presetConfirmed}
                onChange={(event) => setPresetConfirmed(event.target.checked)}
                className="mt-0.5 h-4 w-4"
              />
              <span className="text-sm text-gray-300">
                I reviewed the configuration and workspace-file changes and want to apply this
                bundle to the live runtime.
              </span>
            </label>

            <div className="mt-6 flex flex-wrap justify-end gap-3">
              <button
                type="button"
                onClick={() => {
                  setPresetPreviewOpen(false);
                  setPresetConfirmed(false);
                }}
                disabled={saving}
                className="rounded-lg border border-gray-700 px-4 py-2 text-sm font-medium text-gray-300 hover:bg-gray-800 disabled:opacity-50"
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => void handleApplyPresetLive()}
                disabled={saving || !presetConfirmed}
                className={[
                  'rounded-lg px-4 py-2 text-sm font-semibold disabled:cursor-not-allowed disabled:opacity-50',
                  presetMode === 'safe'
                    ? 'bg-emerald-600 text-white hover:bg-emerald-500'
                    : 'bg-amber-500 text-gray-950 hover:bg-amber-400',
                ].join(' ')}
              >
                {saving ? 'Applying with rollback snapshot...' : `Apply ${selectedPreset.label} Live`}
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="overflow-hidden rounded-2xl border border-gray-800 bg-gray-900">
        <div className="flex items-center justify-between border-b border-gray-800 bg-gray-800/50 px-4 py-3">
          <div>
            <span className="text-xs font-medium uppercase tracking-wider text-gray-400">
              TOML Configuration
            </span>
            <p className="mt-1 text-xs text-gray-500">
              Safe stays autonomous inside tighter guardrails. God widens budgets, system reach,
              and persona aggression.
            </p>
          </div>
          <span className="text-xs text-gray-500">{lineCount} lines</span>
        </div>
        <textarea
          aria-label="TOML configuration editor"
          value={config}
          onChange={(event) => setConfig(event.target.value)}
          spellCheck={false}
          className="min-h-[560px] w-full resize-y bg-gray-950 p-4 font-mono text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:ring-inset"
          style={{ tabSize: 4 }}
        />
      </div>
      <div className="mt-8 border-t border-gray-800 pt-2">
        <IntegrationsPanel />
      </div>
    </div>
  );
}
