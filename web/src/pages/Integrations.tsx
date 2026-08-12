import { useEffect, useRef, useState } from 'react';
import {
  CheckCircle2,
  KeyRound,
  Package,
  Pencil,
  Puzzle,
  Radio,
  RefreshCw,
  Server,
  X,
} from 'lucide-react';
import type { IntegrationSettingsEntry, StatusResponse } from '@/types/api';
import { getIntegrationSettings, getStatus, putIntegrationCredentials } from '@/lib/api';
import OllamaGpuPlacementPanel from '@/components/OllamaGpuPlacement';

const CUSTOM_MODEL = '__custom__';

function fieldFor(entry: IntegrationSettingsEntry | null, key: string) {
  return entry?.fields.find((field) => field.key === key) ?? null;
}

function normalizeTemperatureInput(raw: string): { value: string | null; error: string | null } {
  const trimmed = raw.trim();
  if (!trimmed) {
    return { value: null, error: 'Temperature is required.' };
  }

  const parsed = Number(trimmed);
  if (!Number.isFinite(parsed) || parsed < 0 || parsed > 2) {
    return { value: null, error: 'Temperature must be between 0.0 and 2.0.' };
  }

  return { value: parsed.toString(), error: null };
}

export function IntegrationsPanel() {
  const [integration, setIntegration] = useState<IntegrationSettingsEntry | null>(null);
  const [revision, setRevision] = useState('');
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [editorOpen, setEditorOpen] = useState(false);
  const [discardAction, setDiscardAction] = useState<'close' | 'refresh' | null>(null);
  const [quickModel, setQuickModel] = useState('');
  const [quickTemperature, setQuickTemperature] = useState('');
  const [quickSaveBusy, setQuickSaveBusy] = useState(false);
  const [formModelChoice, setFormModelChoice] = useState('');
  const [formCustomModel, setFormCustomModel] = useState('');
  const [formTemperature, setFormTemperature] = useState('');
  const [formEndpoint, setFormEndpoint] = useState('');
  const [formApiKey, setFormApiKey] = useState('');
  const [saveBusy, setSaveBusy] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [saveSuccess, setSaveSuccess] = useState<string | null>(null);
  const editButtonRef = useRef<HTMLButtonElement | null>(null);
  const firstEditorFieldRef = useRef<HTMLSelectElement | null>(null);
  const discardCancelRef = useRef<HTMLButtonElement | null>(null);

  const loadData = async (showLoading = true) => {
    if (showLoading) {
      setLoading(true);
    } else {
      setRefreshing(true);
    }
    try {
      const [settings, runtimeStatus] = await Promise.all([
        getIntegrationSettings(),
        getStatus(),
      ]);
      const ollamaEntry = settings.integrations[0] ?? null;
      setIntegration(ollamaEntry);
      setRevision(settings.revision);
      setStatus(runtimeStatus);
      if (ollamaEntry) {
        const modelOptions = fieldFor(ollamaEntry, 'default_model')?.options ?? [];
        const activeModel = runtimeStatus.model.trim();
        setQuickModel(activeModel || modelOptions[0] || '');
        setQuickTemperature(runtimeStatus.temperature.toString());
      }
      setError(null);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to load Ollama settings');
    } finally {
      setLoading(false);
      setRefreshing(false);
    }
  };

  useEffect(() => {
    void loadData();
  }, []);

  useEffect(() => {
    if (!saveSuccess) return;
    const timer = window.setTimeout(() => setSaveSuccess(null), 3500);
    return () => window.clearTimeout(timer);
  }, [saveSuccess]);

  const modelField = fieldFor(integration, 'default_model');
  const temperatureField = fieldFor(integration, 'default_temperature');
  const endpointField = fieldFor(integration, 'api_url');
  const apiKeyField = fieldFor(integration, 'api_key');
  const modelOptions = modelField?.options ?? [];
  const configuredModel = modelField?.current_value?.trim() || '';
  const currentModel = status?.model?.trim() || configuredModel;
  const currentTemperatureText =
    normalizeTemperatureInput(
      typeof status?.temperature === 'number'
        ? status.temperature.toString()
        : temperatureField?.current_value ?? '0.1',
    ).value ?? '0.1';
  const configuredTemperatureText =
    normalizeTemperatureInput(temperatureField?.current_value ?? currentTemperatureText).value ??
    currentTemperatureText;
  const currentEndpoint =
    status?.ollama.endpoint || endpointField?.current_value || 'http://localhost:11434';
  const configuredEndpoint = endpointField?.current_value?.trim() || currentEndpoint;
  const currentLoadedModels = status?.ollama.loaded_models ?? [];
  const installedModels = status?.ollama.installed_models ?? modelOptions;
  const quickTemperatureState = normalizeTemperatureInput(quickTemperature);
  const quickSettingsChanged =
    quickModel.trim() !== currentModel ||
    quickTemperatureState.value !== null && quickTemperatureState.value !== currentTemperatureText;
  const resolvedFormModel =
    formModelChoice === CUSTOM_MODEL ? formCustomModel.trim() : formModelChoice.trim();
  const configuredEditorModel = configuredModel || currentModel;
  const editorDirty =
    editorOpen &&
    (
      resolvedFormModel !== configuredEditorModel ||
      normalizeTemperatureInput(formTemperature).value !== configuredTemperatureText ||
      formEndpoint.trim() !== configuredEndpoint ||
      formApiKey.trim().length > 0
    );

  const openEditor = () => {
    const selectedModel = configuredEditorModel && modelOptions.includes(configuredEditorModel)
      ? configuredEditorModel
      : configuredEditorModel
        ? CUSTOM_MODEL
        : modelOptions[0] || '';
    setFormModelChoice(selectedModel);
    setFormCustomModel(
      configuredEditorModel && !modelOptions.includes(configuredEditorModel)
        ? configuredEditorModel
        : '',
    );
    setFormTemperature(configuredTemperatureText);
    setFormEndpoint(configuredEndpoint);
    setFormApiKey('');
    setSaveError(null);
    setDiscardAction(null);
    setEditorOpen(true);
  };

  const closeEditor = (restoreFocus = true, force = false) => {
    if (saveBusy && !force) return;
    setEditorOpen(false);
    setDiscardAction(null);
    setSaveError(null);
    if (restoreFocus) {
      requestAnimationFrame(() => editButtonRef.current?.focus());
    }
  };

  const requestCloseEditor = () => {
    if (saveBusy) return;
    if (editorDirty) {
      setDiscardAction('close');
      return;
    }
    closeEditor();
  };

  const requestRefresh = () => {
    if (editorOpen && editorDirty) {
      setDiscardAction('refresh');
      return;
    }
    void loadData(false);
  };

  const discardEditorChanges = () => {
    const action = discardAction;
    closeEditor();
    if (action === 'refresh') {
      void loadData(false);
    }
  };

  const saveQuickRuntime = async () => {
    const targetModel = quickModel.trim();
    const normalizedTemperature = normalizeTemperatureInput(quickTemperature);
    if (!integration || !targetModel) {
      return;
    }
    if (normalizedTemperature.error) {
      setSaveError(normalizedTemperature.error);
      return;
    }

    const payload: Record<string, string> = {};
    if (targetModel !== currentModel) {
      payload.default_model = targetModel;
    }
    if (normalizedTemperature.value !== currentTemperatureText) {
      payload.default_temperature = normalizedTemperature.value!;
    }
    if (Object.keys(payload).length === 0) {
      return;
    }

    setQuickSaveBusy(true);
    setSaveError(null);
    try {
      await putIntegrationCredentials(integration.id, {
        revision,
        fields: payload,
      });
      await loadData(false);
      setSaveSuccess('Live model and temperature updated.');
    } catch (err: unknown) {
      setSaveError(err instanceof Error ? err.message : 'Failed to update live Ollama settings');
    } finally {
      setQuickSaveBusy(false);
    }
  };

  const saveSettings = async () => {
    if (!integration) return;

    const resolvedModel =
      formModelChoice === CUSTOM_MODEL ? formCustomModel.trim() : formModelChoice.trim();
    if (!resolvedModel) {
      setSaveError('Choose an installed model or enter a custom Ollama model ID.');
      return;
    }
    const normalizedTemperature = normalizeTemperatureInput(formTemperature);
    if (normalizedTemperature.error) {
      setSaveError(normalizedTemperature.error);
      return;
    }

    const payload: Record<string, string> = {};
    if (resolvedModel !== configuredEditorModel) {
      payload.default_model = resolvedModel;
    }
    if (normalizedTemperature.value !== configuredTemperatureText) {
      payload.default_temperature = normalizedTemperature.value!;
    }

    const trimmedEndpoint = formEndpoint.trim();
    if (trimmedEndpoint !== configuredEndpoint) {
      payload.api_url = trimmedEndpoint;
    }

    if (formApiKey.trim()) {
      payload.api_key = formApiKey.trim();
    }

    if (Object.keys(payload).length === 0) {
      setSaveError('No changes to save.');
      return;
    }

    setSaveBusy(true);
    setSaveError(null);
    try {
      await putIntegrationCredentials(integration.id, {
        revision,
        fields: payload,
      });
      await loadData(false);
      closeEditor(true, true);
      setSaveSuccess('Ollama settings saved and applied live.');
    } catch (err: unknown) {
      setSaveError(err instanceof Error ? err.message : 'Failed to save Ollama settings');
    } finally {
      setSaveBusy(false);
    }
  };

  useEffect(() => {
    if (!editorOpen) return;
    firstEditorFieldRef.current?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        if (discardAction) {
          setDiscardAction(null);
        } else {
          requestCloseEditor();
        }
      }
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [discardAction, editorOpen, editorDirty, saveBusy]);

  useEffect(() => {
    if (discardAction) {
      discardCancelRef.current?.focus();
    }
  }, [discardAction]);

  useEffect(() => {
    if (!editorDirty) return;
    const warnBeforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
    };
    window.addEventListener('beforeunload', warnBeforeUnload);
    return () => window.removeEventListener('beforeunload', warnBeforeUnload);
  }, [editorDirty]);

  if (loading && !integration && !status) {
    return (
      <div className="flex h-64 items-center justify-center">
        <div className="h-8 w-8 animate-spin rounded-full border-2 border-blue-500 border-t-transparent" />
      </div>
    );
  }

  if (!integration || !status) {
    return (
      <div className="space-y-3 p-6">
        <div
          role="alert"
          className="rounded-lg border border-red-700 bg-red-900/30 p-4 text-red-300"
        >
          {error ?? 'Ollama settings are not available yet.'}
        </div>
        <button
          type="button"
          onClick={() => void loadData()}
          className="inline-flex min-h-10 items-center gap-2 rounded-lg bg-blue-600 px-4 text-sm font-medium text-white hover:bg-blue-700"
        >
          <RefreshCw className="h-4 w-4" aria-hidden="true" />
          Retry
        </button>
      </div>
    );
  }

  return (
    <div className="space-y-6 p-6">
      <div className="flex items-start justify-between gap-4">
        <div>
          <div className="flex items-center gap-2">
            <Puzzle className="h-5 w-5 text-blue-400" />
            <h2 className="text-base font-semibold text-white">Ollama Runtime</h2>
          </div>
          <p className="mt-2 max-w-2xl text-sm text-gray-400">
            Switch models live, point LlamaFarm at a different Ollama endpoint, and keep the
            local runtime focused on one Ollama surface.
          </p>
        </div>
        <button
          type="button"
          onClick={requestRefresh}
          disabled={refreshing}
          className="inline-flex items-center gap-2 rounded-lg border border-gray-700 px-3 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
        >
          <RefreshCw
            className={`h-4 w-4 ${refreshing ? 'animate-spin' : ''}`}
            aria-hidden="true"
          />
          {refreshing ? 'Refreshing…' : 'Refresh'}
        </button>
      </div>

      {error && (
        <div
          role="alert"
          className="flex flex-wrap items-center gap-3 rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300"
        >
          <span className="flex-1">Refresh failed: {error}. Showing the last loaded values.</span>
          <button
            type="button"
            onClick={requestRefresh}
            className="rounded-md border border-red-600/60 px-2.5 py-1.5 text-xs hover:bg-red-900/50"
          >
            Retry
          </button>
        </div>
      )}

      {saveSuccess && (
        <div className="rounded-lg border border-green-700 bg-green-900/30 p-3 text-sm text-green-300">
          {saveSuccess}
        </div>
      )}

      {saveError && !editorOpen && (
        <div className="rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300">
          {saveError}
        </div>
      )}

      <div className="grid grid-cols-1 gap-4 xl:grid-cols-3">
        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5 xl:col-span-2">
          <div className="flex items-start justify-between gap-4">
            <div>
              <div className="flex items-center gap-2">
                <Server className="h-5 w-5 text-emerald-400" />
                <h3 className="text-base font-semibold text-white">Live Model Switch</h3>
              </div>
              <p className="mt-2 text-sm text-gray-400">
                Apply a different installed model without restarting the gateway.
              </p>
            </div>
            <span className="inline-flex items-center gap-1 rounded-full border border-green-700/60 bg-green-900/30 px-2.5 py-1 text-xs font-medium text-green-300">
              <CheckCircle2 className="h-3.5 w-3.5" />
              Ollama only
            </span>
          </div>

          <div className="mt-5 grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-4">
            <div className="rounded-lg border border-gray-800 bg-gray-950/50 p-4">
              <span className="text-xs uppercase tracking-[0.2em] text-gray-500">
                Effective model
              </span>
              <p className="mt-2 break-all text-sm font-semibold text-white">{currentModel}</p>
              <p className="mt-2 text-xs text-gray-500">
                {status.ollama.active_model_loaded ? 'Loaded now' : 'Configured, waiting to load'}
              </p>
              <p className="mt-1 break-all text-xs text-gray-600">
                Configured: {configuredModel || 'not set'}
              </p>
            </div>

            <div className="rounded-lg border border-gray-800 bg-gray-950/50 p-4">
              <span className="text-xs uppercase tracking-[0.2em] text-gray-500">
                Effective endpoint
              </span>
              <p className="mt-2 break-all text-sm font-semibold text-white">{currentEndpoint}</p>
              <p className="mt-2 text-xs text-gray-500">{installedModels.length} installed models</p>
              <p className="mt-1 break-all text-xs text-gray-600">
                Configured: {configuredEndpoint}
              </p>
            </div>

            <div className="rounded-lg border border-gray-800 bg-gray-950/50 p-4">
              <span className="text-xs uppercase tracking-[0.2em] text-gray-500">Temperature</span>
              <p className="mt-2 text-sm font-semibold text-white">{currentTemperatureText}</p>
              <p className="mt-2 text-xs text-gray-500">Live runtime sampling setting</p>
            </div>

            <div className="rounded-lg border border-gray-800 bg-gray-950/50 p-4">
              <span className="text-xs uppercase tracking-[0.2em] text-gray-500">Loaded models</span>
              <p className="mt-2 text-sm font-semibold text-white">{currentLoadedModels.length}</p>
              <p className="mt-2 text-xs text-gray-500">Reported by the active Ollama runtime</p>
            </div>
          </div>

          {status.ollama.model_environment_override && (
            <div className="mt-4 rounded-lg border border-amber-700/50 bg-amber-950/30 px-3 py-2 text-sm text-amber-200">
              Effective model is controlled by the{' '}
              <code className="font-mono">{status.ollama.model_environment_override}</code>{' '}
              environment override. Saved model choices remain configured but will not become
              effective until that override is removed.
            </div>
          )}

          <div className="mt-5 flex flex-col gap-3 rounded-lg border border-gray-800 bg-gray-950/50 p-4 lg:flex-row lg:items-center">
            <select
              aria-label="Effective model"
              value={quickModel}
              onChange={(event) => setQuickModel(event.target.value)}
              disabled={quickSaveBusy}
              className="min-w-0 flex-1 rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
            >
              {[
                ...(currentModel && !installedModels.includes(currentModel) ? [currentModel] : []),
                ...installedModels,
              ].map((model) => (
                <option key={model} value={model}>
                  {model}
                </option>
              ))}
            </select>
            <input
              aria-label="Runtime temperature"
              type="number"
              min="0"
              max="2"
              step="0.1"
              value={quickTemperature}
              onChange={(event) => setQuickTemperature(event.target.value)}
              disabled={quickSaveBusy}
              className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500 lg:w-32"
            />
            <button
              onClick={() => void saveQuickRuntime()}
              disabled={
                quickSaveBusy ||
                !quickModel ||
                quickTemperatureState.error !== null ||
                !quickSettingsChanged
              }
              className="rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
            >
              {quickSaveBusy ? 'Applying...' : 'Apply Live Settings'}
            </button>
            <button
              ref={editButtonRef}
              type="button"
              onClick={openEditor}
              className="inline-flex items-center justify-center gap-2 rounded-lg border border-gray-700 px-4 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 hover:text-white"
            >
              <Pencil className="h-4 w-4" />
              Edit Connection
            </button>
          </div>
        </div>

        <div className="rounded-xl border border-gray-800 bg-gray-900 p-5">
          <div className="flex items-center gap-2">
            <Package className="h-5 w-5 text-orange-400" />
            <h3 className="text-base font-semibold text-white">Runtime Notes</h3>
          </div>
          <ul className="mt-4 space-y-3 text-sm text-gray-400">
            <li>Installed model choices come from the configured Ollama endpoint.</li>
            <li>Quick Apply writes `default_model` and `default_temperature` into the live runtime snapshot.</li>
            <li>Use Edit Connection to change endpoint, temperature, or set an Ollama API key for a remote host.</li>
          </ul>
        </div>
      </div>

      <div className="grid grid-cols-1 gap-6 xl:grid-cols-2">
        <div className="rounded-xl border border-gray-800 bg-gray-900">
          <div className="border-b border-gray-800 px-5 py-4">
            <h3 className="text-base font-semibold text-white">Installed Models</h3>
          </div>
          {installedModels.length === 0 ? (
            <div className="p-8 text-center text-sm text-gray-500">
              No models were reported by the current Ollama endpoint.
            </div>
          ) : (
            <div className="divide-y divide-gray-800">
              {installedModels.map((model) => (
                <div key={model} className="flex items-center justify-between gap-3 px-5 py-3 text-sm">
                  <span className="break-all text-white">{model}</span>
                  <span
                    className={`rounded-full px-2.5 py-1 text-xs font-medium ${
                      currentLoadedModels.includes(model)
                        ? 'bg-emerald-900/40 text-emerald-300'
                        : 'bg-gray-800 text-gray-400'
                    }`}
                  >
                    {currentLoadedModels.includes(model) ? 'Loaded' : 'Installed'}
                  </span>
                </div>
              ))}
            </div>
          )}
        </div>

        <div className="rounded-xl border border-gray-800 bg-gray-900">
          <div className="border-b border-gray-800 px-5 py-4">
            <h3 className="text-base font-semibold text-white">Loaded Models</h3>
          </div>
          {currentLoadedModels.length === 0 ? (
            <div className="p-8 text-center text-sm text-gray-500">
              Ollama is not currently reporting any loaded models.
            </div>
          ) : (
            <div className="divide-y divide-gray-800">
              {currentLoadedModels.map((model) => (
                <div key={model} className="flex items-center gap-3 px-5 py-3 text-sm">
                  <Radio className="h-4 w-4 text-blue-400" />
                  <span className="break-all text-white">{model}</span>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      <OllamaGpuPlacementPanel models={installedModels} currentModel={currentModel} />

      {editorOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="ollama-editor-title"
          aria-describedby="ollama-editor-description"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 p-4"
          onMouseDown={(event) => {
            if (event.target === event.currentTarget) {
              requestCloseEditor();
            }
          }}
        >
          <div className="w-full max-w-xl rounded-xl border border-gray-800 bg-gray-900 shadow-xl">
            <div className="flex items-center justify-between gap-3 border-b border-gray-800 px-5 py-4">
              <div>
                <h3 id="ollama-editor-title" className="text-sm font-semibold text-white">
                  Edit Ollama Settings
                </h3>
                <p id="ollama-editor-description" className="mt-1 text-xs text-gray-400">
                  Save here to update the live runtime without restarting LlamaFarm.
                </p>
              </div>
              <button
                type="button"
                onClick={requestCloseEditor}
                disabled={saveBusy}
                className="text-gray-400 transition-colors hover:text-white disabled:opacity-50"
                aria-label="Close Ollama settings editor"
              >
                <X className="h-4 w-4" aria-hidden="true" />
              </button>
            </div>

            <div className="space-y-4 p-5">
              <div>
                <label
                  htmlFor="ollama-model-choice"
                  className="mb-1.5 block text-sm font-medium text-gray-300"
                >
                  Installed Model
                </label>
                <select
                  ref={firstEditorFieldRef}
                  id="ollama-model-choice"
                  value={formModelChoice}
                  onChange={(event) => setFormModelChoice(event.target.value)}
                  className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
                >
                  {modelOptions.map((model) => (
                    <option key={model} value={model}>
                      {model}
                    </option>
                  ))}
                  <option value={CUSTOM_MODEL}>Custom model ID...</option>
                </select>
                {formModelChoice === CUSTOM_MODEL && (
                  <>
                    <label htmlFor="ollama-custom-model" className="sr-only">
                      Custom model ID
                    </label>
                    <input
                      id="ollama-custom-model"
                      type="text"
                      value={formCustomModel}
                      onChange={(event) => setFormCustomModel(event.target.value)}
                      placeholder="example: llama3.1:8b"
                      className="mt-2 w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
                    />
                  </>
                )}
              </div>

              <div>
                <label
                  htmlFor="ollama-endpoint"
                  className="mb-1.5 block text-sm font-medium text-gray-300"
                >
                  Ollama Endpoint
                </label>
                <input
                  id="ollama-endpoint"
                  type="text"
                  value={formEndpoint}
                  onChange={(event) => setFormEndpoint(event.target.value)}
                  placeholder="http://localhost:11434"
                  className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>

              <div>
                <label
                  htmlFor="ollama-temperature"
                  className="mb-1.5 block text-sm font-medium text-gray-300"
                >
                  Temperature
                </label>
                <input
                  id="ollama-temperature"
                  type="number"
                  min="0"
                  max="2"
                  step="0.1"
                  value={formTemperature}
                  onChange={(event) => setFormTemperature(event.target.value)}
                  className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
              </div>

              <div>
                <label
                  htmlFor="ollama-api-key"
                  className="mb-1.5 flex items-center gap-2 text-sm font-medium text-gray-300"
                >
                  <span>API Key</span>
                  {apiKeyField?.has_value && (
                    <span className="rounded border border-green-800 bg-green-900/30 px-1.5 py-0.5 text-[11px] text-green-400">
                      Configured
                    </span>
                  )}
                </label>
                <input
                  id="ollama-api-key"
                  type="password"
                  value={formApiKey}
                  onChange={(event) => setFormApiKey(event.target.value)}
                  placeholder={
                    apiKeyField?.has_value
                      ? 'Leave blank to keep the current API key'
                      : 'Optional for local Ollama'
                  }
                  className="w-full rounded-lg border border-gray-700 bg-gray-950 px-3 py-2 text-sm text-gray-200 focus:outline-none focus:ring-2 focus:ring-blue-500"
                />
                {apiKeyField?.masked_value && (
                  <p className="mt-2 text-xs text-gray-500">
                    Current value: <span className="font-mono text-gray-300">{apiKeyField.masked_value}</span>
                  </p>
                )}
              </div>

              {saveError && (
                <div
                  role="alert"
                  className="rounded-lg border border-red-700 bg-red-900/30 p-3 text-sm text-red-300"
                >
                  {saveError}
                </div>
              )}
            </div>

            {discardAction ? (
              <div
                role="alert"
                className="border-t border-amber-800/60 bg-amber-950/30 px-5 py-4"
              >
                <p className="text-sm font-medium text-amber-200">Discard unsaved changes?</p>
                <p className="mt-1 text-xs text-amber-300/80">
                  {discardAction === 'refresh'
                    ? 'The latest settings will be loaded after your edits are discarded.'
                    : 'Your edits have not been saved.'}
                </p>
                <div className="mt-4 flex justify-end gap-2">
                  <button
                    ref={discardCancelRef}
                    type="button"
                    onClick={() => setDiscardAction(null)}
                    className="rounded-lg border border-gray-700 px-4 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800"
                  >
                    Keep editing
                  </button>
                  <button
                    type="button"
                    onClick={discardEditorChanges}
                    className="rounded-lg bg-amber-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-amber-700"
                  >
                    {discardAction === 'refresh' ? 'Discard and refresh' : 'Discard changes'}
                  </button>
                </div>
              </div>
            ) : (
              <div className="flex items-center justify-end gap-2 border-t border-gray-800 px-5 py-4">
                <button
                  type="button"
                  onClick={requestCloseEditor}
                  disabled={saveBusy}
                  className="rounded-lg border border-gray-700 px-4 py-2 text-sm text-gray-300 transition-colors hover:bg-gray-800 disabled:opacity-50"
                >
                  Cancel
                </button>
                <button
                  type="button"
                  onClick={() => void saveSettings()}
                  disabled={saveBusy}
                  className="inline-flex items-center gap-2 rounded-lg bg-blue-600 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-blue-700 disabled:opacity-50"
                >
                  <KeyRound className="h-4 w-4" aria-hidden="true" />
                  {saveBusy ? 'Saving...' : 'Save Ollama Settings'}
                </button>
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}


export default function Integrations() {
  return <IntegrationsPanel />;
}
