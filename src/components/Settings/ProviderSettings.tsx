/**
 * ProviderSettings Component
 *
 * Displays and allows selection of AI providers and models for CLAI.
 */

import React, { useState, useEffect } from 'react';
import {
  getAvailableAiProviders,
  setAiProvider,
  getAiProvider,
  getProviderModels,
} from '../../api/client';
import styles from './ProviderSettings.module.css';

// Legacy AI-CLI provider shapes (the `ai_provider` commands, distinct
// from the typed provider_connections surface). No generated binding;
// declared locally.
interface AiProviderSpec {
  type: string;
  model?: string | null;
}

interface AiProviderInfo {
  name: string;
  command: string;
  version?: string | null;
  available: boolean;
  error?: string | null;
  provider: AiProviderSpec;
}

interface CurrentAiProvider {
  provider: AiProviderSpec;
  is_configured?: boolean;
}

interface AiModel {
  id: string;
  name: string;
  recommended?: boolean;
  description?: string;
}

/**
 * Loading spinner icon
 */
const LoadingIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className={styles.spinner}>
    <circle cx="12" cy="12" r="10" opacity="0.25" />
    <path d="M12 2a10 10 0 0 1 10 10" />
  </svg>
);

/**
 * Check icon for selected provider
 */
const CheckIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="20 6 9 17 4 12" />
  </svg>
);

/**
 * Warning icon for unavailable providers
 */
const WarningIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <circle cx="12" cy="12" r="10" />
    <line x1="12" y1="8" x2="12" y2="12" />
    <line x1="12" y1="16" x2="12.01" y2="16" />
  </svg>
);

/**
 * ProviderSettings - AI Provider and Model selection interface
 */
const ProviderSettings = () => {
  const [providers, setProviders] = useState<AiProviderInfo[]>([]);
  const [currentProvider, setCurrentProvider] = useState<CurrentAiProvider | null>(null);
  const [models, setModels] = useState<AiModel[]>([]);
  const [selectedModel, setSelectedModel] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [loadingModels, setLoadingModels] = useState(false);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Fetch providers and current selection on mount
  useEffect(() => {
    const fetchData = async () => {
      setLoading(true);
      setError(null);

      try {
        // Fetch available providers and current selection in parallel.
        // The api/client.js helpers are untyped JS; cast their results.
        const [providersResult, currentResult] = await Promise.all([
          getAvailableAiProviders() as Promise<AiProviderInfo[]>,
          getAiProvider().catch(() => null) as Promise<CurrentAiProvider | null>,
        ]);

        setProviders(providersResult);
        setCurrentProvider(currentResult);

        // If a provider is already selected, fetch its models
        if (currentResult?.provider?.type) {
          const providerType = currentResult.provider.type;
          const modelsResult = (await getProviderModels(providerType)) as AiModel[];
          setModels(modelsResult);
          // Set the currently selected model
          setSelectedModel(currentResult.provider.model || null);
        }
      } catch (err) {
        console.error('[ProviderSettings] Failed to fetch providers:', err);
        setError('Failed to load providers. Please try again.');
      } finally {
        setLoading(false);
      }
    };

    fetchData();
  }, []);

  // Handle provider selection
  const handleSelect = async (providerInfo: AiProviderInfo) => {
    if (!providerInfo.available || saving) return;

    setSaving(true);
    setError(null);

    try {
      // When selecting a new provider, don't set a model yet
      const providerWithNoModel = { ...providerInfo.provider, model: null };
      await setAiProvider(providerWithNoModel);
      setCurrentProvider({ provider: providerWithNoModel, is_configured: true });

      // Notify other components that the provider changed
      window.dispatchEvent(new CustomEvent('ai-provider-changed'));

      // Fetch models for the newly selected provider
      setLoadingModels(true);
      const providerType = providerInfo.provider.type;
      const modelsResult = await getProviderModels(providerType);
      setModels(modelsResult);
      setSelectedModel(null); // Reset model selection
    } catch (err) {
      console.error('[ProviderSettings] Failed to set provider:', err);
      setError('Failed to save provider. Please try again.');
    } finally {
      setSaving(false);
      setLoadingModels(false);
    }
  };

  // Handle model selection
  const handleModelChange = async (modelId: string) => {
    if (saving || !currentProvider?.provider) return;

    setSaving(true);
    setError(null);

    try {
      // Create provider with selected model (or null for default)
      const providerWithModel = {
        ...currentProvider.provider,
        model: modelId || null,
      };
      await setAiProvider(providerWithModel);
      setCurrentProvider({ provider: providerWithModel, is_configured: true });
      setSelectedModel(modelId || null);

      // Notify other components that the provider changed
      window.dispatchEvent(new CustomEvent('ai-provider-changed'));
    } catch (err) {
      console.error('[ProviderSettings] Failed to set model:', err);
      setError('Failed to save model. Please try again.');
    } finally {
      setSaving(false);
    }
  };

  // Loading state
  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.loadingState}>
          <LoadingIcon />
          <span>Detecting AI providers...</span>
        </div>
      </div>
    );
  }

  // Error state
  if (error && providers.length === 0) {
    return (
      <div className={styles.container}>
        <div className={styles.errorState}>
          <WarningIcon />
          <span>{error}</span>
        </div>
      </div>
    );
  }

  const availableCount = providers.filter(p => p.available).length;

  return (
    <div className={styles.container}>
      <div className={styles.sectionHeader}>
        <h3 className={styles.sectionTitle}>AI Provider</h3>
        <p className={styles.sectionDescription}>
          Select which AI CLI to use for chat and scheduled agents.
          {availableCount === 0 && (
            <span className={styles.noProviders}>
              {' '}No providers available. Install an AI CLI to get started.
            </span>
          )}
        </p>
      </div>

      {error && (
        <div className={styles.errorBanner}>
          <WarningIcon />
          <span>{error}</span>
        </div>
      )}

      <div className={styles.providerList}>
        {providers.map((provider) => {
          const isSelected = currentProvider?.provider?.type === provider.provider.type;
          const isAvailable = provider.available;

          return (
            <button
              key={provider.command}
              className={`${styles.providerItem} ${isSelected ? styles.selected : ''} ${!isAvailable ? styles.unavailable : ''}`}
              onClick={() => handleSelect(provider)}
              disabled={!isAvailable || saving}
            >
              <div className={styles.providerInfo}>
                <div className={styles.providerMain}>
                  <span className={styles.providerName}>{provider.name}</span>
                  {provider.version && (
                    <span className={styles.providerVersion}>{provider.version}</span>
                  )}
                </div>
                {!isAvailable && provider.error && (
                  <span className={styles.providerError}>
                    <WarningIcon />
                    {provider.error}
                  </span>
                )}
                {isAvailable && (
                  <span className={styles.providerCommand}>
                    <code>{provider.command}</code>
                  </span>
                )}
              </div>
              {isSelected && (
                <span className={styles.checkIcon}>
                  <CheckIcon />
                </span>
              )}
              {saving && isSelected && (
                <span className={styles.savingIcon}>
                  <LoadingIcon />
                </span>
              )}
            </button>
          );
        })}
      </div>

      {/* Model Selection - Only show when a provider is selected */}
      {currentProvider?.provider && models.length > 0 && (
        <div className={styles.modelSection}>
          <div className={styles.sectionHeader}>
            <h3 className={styles.sectionTitle}>Model</h3>
            <p className={styles.sectionDescription}>
              Select which model to use. Faster models reduce response time.
            </p>
          </div>

          {loadingModels ? (
            <div className={styles.loadingState}>
              <LoadingIcon />
              <span>Loading models...</span>
            </div>
          ) : (
            <div className={styles.modelSelect}>
              <select
                value={selectedModel || ''}
                onChange={(e) => handleModelChange(e.target.value)}
                disabled={saving}
                className={styles.select}
              >
                <option value="">Default (CLI decides)</option>
                {models.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.name}
                    {model.recommended ? ' (Recommended)' : ''}
                  </option>
                ))}
              </select>
              {selectedModel && (
                <p className={styles.modelDescription}>
                  {models.find(m => m.id === selectedModel)?.description}
                </p>
              )}
            </div>
          )}
        </div>
      )}

      <div className={styles.hint}>
        <p>
          Scheduled agents use your selected AI CLI to run monitoring and orchestration tasks.
          Make sure the CLI is installed and configured with the credentials it needs.
        </p>
      </div>
    </div>
  );
};

export default ProviderSettings;
