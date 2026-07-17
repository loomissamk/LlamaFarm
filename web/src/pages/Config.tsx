import { ConnectionsPanel } from './Connections';
import { IntegrationsPanel } from './Integrations';
import { useEffect, useState } from 'react';
import {
  AlertTriangle,
  CheckCircle,
  RefreshCcw,
  Save,
  Settings,
  ShieldAlert,
} from 'lucide-react';
import type { ConfigPresetsResponse } from '@/types/api';
import { getConfig, getConfigPresets, putConfig, putWorkspaceFile } from '@/lib/api';

type PresetMode = 'safe' | 'god';

export default function Config() {
  const [liveConfig, setLiveConfig] = useState('');
  const [config, setConfig] = useState('');
  const [presets, setPresets] = useState<ConfigPresetsResponse | null>(null);
  const [presetMode, setPresetMode] = useState<PresetMode>('god');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    Promise.all([getConfig(), getConfigPresets()])
      .then(([currentConfig, presetData]) => {
        setLiveConfig(currentConfig);
        setConfig(currentConfig);
        setPresets(presetData);
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  const handleSave = async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await putConfig(config);
      setLiveConfig(config);
      setSuccess('Configuration saved successfully.');
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  };

  const handleReloadLive = () => {
    setConfig(liveConfig);
    setError(null);
    setSuccess('Reloaded the current live configuration into the editor.');
  };

  const handleApplyPreset = () => {
    if (!selectedPreset) {
      return;
    }

    setConfig(selectedPreset.content);
    setError(null);
    setSuccess(
      `${selectedPreset.label} preset loaded into the editor. Use Apply Live to sync config plus AGENTS.md and SOUL.md.`,
    );
  };

  const handleApplyPresetLive = async () => {
    if (!selectedPreset) {
      return;
    }

    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      await putConfig(selectedPreset.content);
      for (const file of selectedPreset.workspace_files) {
        await putWorkspaceFile(file.name, file.content);
      }
      setLiveConfig(selectedPreset.content);
      setConfig(selectedPreset.content);
      setSuccess(
        `${selectedPreset.label} bundle applied live. Configuration, AGENTS.md, and SOUL.md are now in sync.`,
      );
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to apply preset bundle');
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  if (loading || !presets) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  const selectedPreset = presets[presetMode] ?? presets.god;
  const lineCount = config.split('\n').length;
  const isDirty = config !== liveConfig;

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
            onClick={handleReloadLive}
            disabled={saving}
            className="inline-flex items-center gap-2 rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-medium text-gray-200 transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
          >
            <RefreshCcw className="h-4 w-4" />
            Reload Live
          </button>
          <button
            onClick={handleSave}
            disabled={saving}
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
              preset&apos;s AGENTS.md and SOUL.md bundle into the workspace.
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
              onClick={handleApplyPresetLive}
              disabled={saving}
              className="mt-3 w-full rounded-lg border border-gray-700 bg-gray-900 px-4 py-2 text-sm font-semibold text-white transition-colors hover:border-gray-600 hover:bg-gray-800 disabled:opacity-50"
            >
              Apply {selectedPreset.label} Live
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
          </div>
        </div>
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
        <div className="flex items-center gap-2 rounded-lg border border-green-700 bg-green-900/30 p-3">
          <CheckCircle className="h-4 w-4 flex-shrink-0 text-green-400" />
          <span className="text-sm text-green-300">{success}</span>
        </div>
      )}

      {error && (
        <div className="flex items-center gap-2 rounded-lg border border-red-700 bg-red-900/30 p-3">
          <AlertTriangle className="h-4 w-4 flex-shrink-0 text-red-400" />
          <span className="text-sm text-red-300">{error}</span>
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
