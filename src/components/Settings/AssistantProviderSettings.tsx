import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import ReactDOM from 'react-dom';
import { assistantClient } from '../../assistant';
import type {
  AuthMode,
  ModelInfo,
  ProviderConnection,
  ProviderDescriptor,
} from '../../generated/bindings';
import styles from './ProviderSettings.module.css';
// The add/edit form opens in a portal modal over the settings modal —
// same pattern (and stylesheet) as the MCP server form, so the two item
// editors look and behave identically.
import modalStyles from './McpServerFormModal.module.css';

const CONNECTIONS_CHANGED_EVENT = 'assistant-provider-connections-changed';

interface ConnectionForm {
  id: string | null;
  name: string;
  providerId: string;
  apiKey: string;
  baseUrl: string;
  modelId: string;
  enabled: boolean;
  authMode: AuthMode | null;
}

const LoadingIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" className={styles.spinner}>
    <circle cx="12" cy="12" r="10" opacity="0.25" />
    <path d="M12 2a10 10 0 0 1 10 10" />
  </svg>
);

const CheckIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="20 6 9 17 4 12" />
  </svg>
);

const CloseIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

const secondaryButtonStyle: React.CSSProperties = {
  appearance: 'none',
  border: '1px solid var(--color-border-medium)',
  background: 'var(--color-bg-elevated)',
  color: 'var(--color-text-secondary)',
  borderRadius: '8px',
  padding: '6px 10px',
  fontSize: '12px',
  fontWeight: 600,
  cursor: 'pointer',
};

const initialForm: ConnectionForm = {
  id: null,
  name: '',
  providerId: 'openai',
  apiKey: '',
  baseUrl: '',
  modelId: '',
  enabled: true,
  authMode: null,
};

const CLI_BINARY_PLACEHOLDERS: Record<string, string> = {
  'claude-code': 'claude',
  codex: 'codex',
  opencode: 'opencode',
};

interface AssistantProviderSettingsProps {
  // 'new' opens the "Add Connection" form immediately — used by first-run
  // deep links (e.g. the "Configure a provider first" badge in the chat).
  initialAction?: 'new' | null;
}

const AssistantProviderSettings = ({ initialAction = null }: AssistantProviderSettingsProps) => {
  const [connections, setConnections] = useState<ProviderConnection[]>([]);
  const [adapters, setAdapters] = useState<ProviderDescriptor[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formOpen, setFormOpen] = useState(false);
  const [form, setForm] = useState<ConnectionForm>(initialForm);
  // Page-level error (load/test/delete) vs. form-level error (validation,
  // save) — the latter renders inside the form modal where the user is.
  const [error, setError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [descriptorModels, setDescriptorModels] = useState<ModelInfo[]>([]);

  const selectedAdapter = useMemo(
    () => adapters.find((adapter) => adapter.id === form.providerId) || null,
    [adapters, form.providerId],
  );
  const isCliAdapter = selectedAdapter?.isCliBacked === true;

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [nextConnections, nextAdapters] = await Promise.all([
        assistantClient.listProviderConnections(),
        assistantClient.listAvailableProviderAdapters().catch(() => []),
      ]);
      setConnections(nextConnections || []);
      setAdapters(nextAdapters || []);
      setError(null);
    } catch (err) {
      console.error('[AssistantProviderSettings] Failed to load:', err);
      setError('Failed to load provider connections.');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
    window.addEventListener(CONNECTIONS_CHANGED_EVENT, loadData);
    return () => window.removeEventListener(CONNECTIONS_CHANGED_EVENT, loadData);
  }, [loadData]);

  useEffect(() => {
    if (!isCliAdapter) {
      setDescriptorModels([]);
      return undefined;
    }
    let cancelled = false;
    (async () => {
      try {
        const models = await assistantClient.listProviderDescriptorModels(form.providerId);
        if (cancelled) return;
        const list = models || [];
        setDescriptorModels(list);
        // Don't auto-select a model for CLI adapters: an empty model lets the
        // CLI fall back to whatever model it is configured to use itself.
      } catch (err) {
        console.error('[AssistantProviderSettings] Failed to load CLI models:', err);
        if (!cancelled) setDescriptorModels([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isCliAdapter, form.providerId]);

  // Provider-connection dependents (workspace agents that reference a
  // connection) are no longer enumerated client-side — the backend
  // `provider_connection_delete` refuses deletion with a clear message
  // including the count when dependents exist.
  const dependencyCounts = useMemo(() => new Map(), []);

  const resetForm = useCallback(() => {
    setEditingId(null);
    setForm({
      ...initialForm,
      providerId: adapters[0]?.id || 'openai',
    });
  }, [adapters]);

  const beginCreate = useCallback(() => {
    resetForm();
    setFormError(null);
    setSuccess(null);
    setFormOpen(true);
  }, [resetForm]);

  const beginEdit = useCallback((connection: ProviderConnection) => {
    setEditingId(connection.id);
    setForm({
      id: connection.id,
      name: connection.name,
      providerId: connection.providerId,
      apiKey: '',
      baseUrl: connection.baseUrl || '',
      modelId: connection.modelId,
      enabled: connection.enabled,
      authMode: connection.authMode || null,
    });
    setFormError(null);
    setSuccess(null);
    setFormOpen(true);
  }, []);

  const closeForm = useCallback(() => {
    if (saving) return;
    setFormOpen(false);
    resetForm();
  }, [resetForm, saving]);

  // Consume a 'new' deep link once per mount, after the adapter list has
  // loaded so the form's default provider is the real first adapter.
  const consumedInitialActionRef = useRef(false);
  useEffect(() => {
    if (initialAction !== 'new' || loading || consumedInitialActionRef.current) return;
    consumedInitialActionRef.current = true;
    beginCreate();
  }, [initialAction, loading, beginCreate]);

  const handleSubmit = useCallback(async () => {
    if (!form.name.trim()) {
      setFormError('Connection name is required.');
      return;
    }
    if (!isCliAdapter && !form.modelId.trim()) {
      setFormError('Model ID is required.');
      return;
    }
    if (!editingId && !isCliAdapter && !form.apiKey.trim()) {
      setFormError('API key is required for new connections.');
      return;
    }

    setSaving(true);
    setFormError(null);
    setSuccess(null);

    const authMode: AuthMode | null = isCliAdapter
      ? 'subscription_login'
      : form.authMode ?? null;

    try {
      if (editingId) {
        await assistantClient.updateProviderConnection({
          id: editingId,
          name: form.name.trim(),
          providerId: form.providerId,
          apiKey: isCliAdapter ? null : form.apiKey.trim() || null,
          authMode,
          baseUrl: form.baseUrl.trim() || null,
          modelId: form.modelId.trim(),
          accountLabel: null,
          enabled: form.enabled,
        });
        setSuccess('Connection updated.');
      } else {
        await assistantClient.createProviderConnection({
          name: form.name.trim(),
          providerId: form.providerId,
          apiKey: isCliAdapter ? null : form.apiKey.trim(),
          authMode,
          baseUrl: form.baseUrl.trim() || null,
          modelId: form.modelId.trim(),
          accountLabel: null,
        });
        setSuccess('Connection created.');
      }

      setFormOpen(false);
      resetForm();
      await loadData();
      window.dispatchEvent(new CustomEvent(CONNECTIONS_CHANGED_EVENT));
    } catch (err) {
      console.error('[AssistantProviderSettings] Save failed:', err);
      setFormError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save provider connection.');
    } finally {
      setSaving(false);
    }
  }, [editingId, form, isCliAdapter, loadData, resetForm]);

  const handleDelete = useCallback(async (connection: ProviderConnection) => {
    if (!window.confirm(`Delete provider connection "${connection.name}"?`)) {
      return;
    }

    setDeletingId(connection.id);
    setError(null);
    setSuccess(null);
    try {
      await assistantClient.deleteProviderConnection(connection.id);
      if (editingId === connection.id) {
        resetForm();
      }
      await loadData();
      window.dispatchEvent(new CustomEvent(CONNECTIONS_CHANGED_EVENT));
      setSuccess('Connection deleted.');
    } catch (err) {
      console.error('[AssistantProviderSettings] Delete failed:', err);
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to delete provider connection.');
    } finally {
      setDeletingId(null);
    }
  }, [editingId, loadData, resetForm]);

  const handleTest = useCallback(async (connectionId: string) => {
    setTestingId(connectionId);
    setError(null);
    setSuccess(null);
    try {
      const result = await assistantClient.testProviderConnection(connectionId);
      if (result.success) {
        setSuccess('Connection test succeeded.');
      } else {
        setError(result.error || 'Connection test failed.');
      }
    } catch (err) {
      console.error('[AssistantProviderSettings] Test failed:', err);
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to test provider connection.');
    } finally {
      setTestingId(null);
    }
  }, []);

  if (loading) {
    return (
      <div className={styles.container}>
        <div className={styles.loadingState}>
          <LoadingIcon />
          <span>Loading provider connections...</span>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.container}>
      <div className={styles.sectionHeader}>
        <div style={{ display: 'flex', alignItems: 'flex-start', justifyContent: 'space-between', gap: '12px' }}>
          <h3 className={styles.sectionTitle}>Assistant Provider Connections</h3>
          <button
            type="button"
            className={modalStyles.addButton}
            style={{ padding: '8px 14px', flexShrink: 0 }}
            onClick={beginCreate}
          >
            + Add Connection
          </button>
        </div>
        <p className={styles.sectionDescription}>
          Configure API providers (OpenAI / Anthropic) or local CLI agents (Claude Code, Codex,
          OpenCode) for the assistant runtime and scheduled agents. Click a connection to edit it.
        </p>
      </div>

      {error && (
        <div className={styles.errorBanner}>
          <span>{error}</span>
        </div>
      )}

      {success && (
        <div style={{
          display: 'flex',
          alignItems: 'center',
          gap: '8px',
          padding: '12px 16px',
          background: 'rgba(16, 185, 129, 0.1)',
          border: '1px solid rgba(16, 185, 129, 0.3)',
          borderRadius: '8px',
          color: 'var(--color-success, #10b981)',
          fontSize: '13px',
        }}>
          <CheckIcon />
          <span>{success}</span>
        </div>
      )}

      {connections.length === 0 ? (
        <div className={styles.noProviders}>
          No provider connections yet. Click &ldquo;Add Connection&rdquo; to set up your first one.
        </div>
      ) : (
        <div className={styles.providerList}>
          {connections.map((connection) => (
            <div
              key={connection.id}
              className={`${styles.providerItem} ${editingId === connection.id ? styles.selected : ''} ${!connection.enabled ? styles.unavailable : ''}`}
              onClick={() => beginEdit(connection)}
              role="button"
              tabIndex={0}
              onKeyDown={(event) => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  beginEdit(connection);
                }
              }}
            >
              <div className={styles.providerInfo}>
                <div className={styles.providerMain}>
                  <span className={styles.providerName}>{connection.name}</span>
                  <span className={styles.providerVersion}>{connection.enabled ? 'enabled' : 'disabled'}</span>
                  {connection.authMode === 'subscription_login' && (
                    <span className={styles.providerVersion}>via CLI</span>
                  )}
                </div>
                <span className={styles.providerCommand}>
                  <code>{connection.modelId.trim() || 'default model'}</code> • <code>
                    {connection.authMode === 'subscription_login'
                      ? (connection.baseUrl || CLI_BINARY_PLACEHOLDERS[connection.providerId] || connection.providerId)
                      : (connection.baseUrl || 'api.openai.com/v1')}
                  </code>
                </span>
                <span className={styles.providerCommand}>
                  used by {dependencyCounts.get(connection.id) || 0} agent(s)
                </span>
              </div>
              <div style={{ display: 'flex', gap: '8px', marginLeft: '12px' }}>
                <button
                  type="button"
                  style={secondaryButtonStyle}
                  onClick={(event) => {
                    event.stopPropagation();
                    handleTest(connection.id);
                  }}
                  disabled={testingId === connection.id}
                >
                  {testingId === connection.id ? 'Testing…' : 'Test'}
                </button>
                <button
                  type="button"
                  style={secondaryButtonStyle}
                  onClick={(event) => {
                    event.stopPropagation();
                    handleDelete(connection);
                  }}
                  disabled={deletingId === connection.id}
                >
                  {deletingId === connection.id ? 'Deleting…' : 'Delete'}
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      <div className={styles.hint}>
        <p>
          Provider credentials are stored securely in your OS keychain. The first enabled connection becomes the default for non-agent tabs until the user switches it from the tab context.
        </p>
      </div>

      {formOpen && ReactDOM.createPortal(
        <div className={modalStyles.overlay} onClick={(event) => event.target === event.currentTarget && closeForm()}>
          <div className={modalStyles.modal} style={{ width: '560px' }} onClick={(event) => event.stopPropagation()}>
            <div className={modalStyles.header}>
              <h2 className={modalStyles.title}>{editingId ? 'Edit Connection' : 'Add Connection'}</h2>
              <button className={modalStyles.closeButton} onClick={closeForm} disabled={saving} title="Close">
                <CloseIcon />
              </button>
            </div>

            <form
              className={modalStyles.form}
              onSubmit={(event) => {
                event.preventDefault();
                handleSubmit();
              }}
            >
              {formError && <div className={modalStyles.errorBanner}>{formError}</div>}

              <div className={modalStyles.field}>
                <label className={modalStyles.label} htmlFor="provider-conn-name">Connection Name</label>
                <input
                  id="provider-conn-name"
                  className={modalStyles.input}
                  type="text"
                  value={form.name}
                  onChange={(e) => setForm((current) => ({ ...current, name: e.target.value }))}
                  placeholder="e.g. Personal OpenAI"
                  disabled={saving}
                  autoFocus
                />
              </div>

              <div className={modalStyles.field}>
                <label className={modalStyles.label} htmlFor="provider-conn-adapter">Provider Adapter</label>
                <select
                  id="provider-conn-adapter"
                  className={modalStyles.select}
                  value={form.providerId}
                  onChange={(e) =>
                    // Reset the model when switching providers: a model id valid for
                    // one CLI (e.g. `sonnet`) is meaningless for another (Codex), and
                    // a controlled <select> would otherwise keep the stale value in
                    // state while visually showing the new provider's first option.
                    setForm((current) => ({ ...current, providerId: e.target.value, modelId: '' }))
                  }
                  disabled={saving || editingId !== null}
                >
                  {(adapters.length > 0 ? adapters : [{ id: 'openai', displayName: 'OpenAI-Compatible' }]).map((adapter) => (
                    <option key={adapter.id} value={adapter.id}>
                      {adapter.displayName}
                    </option>
                  ))}
                </select>
              </div>

              {isCliAdapter && (
                <div className={modalStyles.quickConnect}>
                  <p className={modalStyles.sectionDescription}>
                    This provider runs through your local <strong>{selectedAdapter?.displayName}</strong> CLI
                    using its own authentication (typically a paid subscription). Make sure the binary is
                    installed and you have signed in (e.g. <code>claude /login</code>, <code>codex login</code>, or <code>opencode auth login</code>) in your terminal
                    before testing this connection. No API key is stored.
                  </p>
                </div>
              )}

              {!isCliAdapter && (
                <div className={modalStyles.field}>
                  <label className={modalStyles.label} htmlFor="provider-conn-api-key">
                    API Key {!editingId && <span className={modalStyles.required}>*</span>}
                  </label>
                  <input
                    id="provider-conn-api-key"
                    className={modalStyles.input}
                    type="password"
                    value={form.apiKey}
                    onChange={(e) => setForm((current) => ({ ...current, apiKey: e.target.value }))}
                    placeholder={editingId ? 'Leave blank to keep existing key' : 'sk-...'}
                    disabled={saving}
                  />
                </div>
              )}

              <div className={modalStyles.field}>
                <label className={modalStyles.label} htmlFor="provider-conn-model">
                  Model ID{' '}
                  {isCliAdapter ? (
                    <span style={{ fontWeight: 400, color: 'var(--color-text-tertiary)' }}>(optional)</span>
                  ) : (
                    <span className={modalStyles.required}>*</span>
                  )}
                </label>
                {isCliAdapter && descriptorModels.length > 0 ? (
                  <select
                    id="provider-conn-model"
                    className={modalStyles.select}
                    value={form.modelId}
                    onChange={(e) => setForm((current) => ({ ...current, modelId: e.target.value }))}
                    disabled={saving}
                  >
                    <option value="">Default (use the CLI&apos;s configured model)</option>
                    {descriptorModels.map((model) => (
                      <option key={model.id} value={model.id}>
                        {model.displayName} ({model.id})
                      </option>
                    ))}
                    {form.modelId.trim() &&
                      !descriptorModels.some((model) => model.id === form.modelId) && (
                        // Surface a stale/unknown stored value (e.g. a model saved
                        // for a different CLI) so the select reflects state honestly
                        // instead of silently displaying a non-matching option.
                        <option value={form.modelId}>{form.modelId} (unrecognized)</option>
                      )}
                  </select>
                ) : (
                  <input
                    id="provider-conn-model"
                    className={modalStyles.input}
                    type="text"
                    value={form.modelId}
                    onChange={(e) => setForm((current) => ({ ...current, modelId: e.target.value }))}
                    placeholder={isCliAdapter ? 'Leave blank to use the CLI default' : 'e.g. gpt-4o-mini'}
                    disabled={saving}
                  />
                )}
              </div>

              <div className={modalStyles.field}>
                <label className={modalStyles.label} htmlFor="provider-conn-base-url">
                  {isCliAdapter ? 'CLI binary path (optional)' : 'Base URL'}
                </label>
                <input
                  id="provider-conn-base-url"
                  className={modalStyles.input}
                  type="text"
                  value={form.baseUrl}
                  onChange={(e) => setForm((current) => ({ ...current, baseUrl: e.target.value }))}
                  placeholder={
                    isCliAdapter
                      ? CLI_BINARY_PLACEHOLDERS[form.providerId] || 'claude'
                      : 'https://api.openai.com/v1'
                  }
                  disabled={saving}
                />
              </div>

              {editingId && (
                <label className={modalStyles.checkboxOption}>
                  <input
                    type="checkbox"
                    checked={form.enabled}
                    onChange={(e) => setForm((current) => ({ ...current, enabled: e.target.checked }))}
                    disabled={saving}
                  />
                  Enabled
                </label>
              )}

              <div className={modalStyles.actions}>
                <button type="button" className={modalStyles.cancelButton} onClick={closeForm} disabled={saving}>
                  Cancel
                </button>
                <button type="submit" className={modalStyles.submitButton} disabled={saving}>
                  {saving ? 'Saving…' : editingId ? 'Save Connection' : 'Add Connection'}
                </button>
              </div>
            </form>
          </div>
        </div>,
        document.body
      )}
    </div>
  );
};

export default AssistantProviderSettings;
