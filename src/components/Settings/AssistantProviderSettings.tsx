import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import ReactDOM from 'react-dom';
import { assistantClient } from '../../assistant';
import type {
  AuthMode,
  ModelInfo,
  ProviderCatalogEntry,
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
  /** Wire/execution protocol adapter key. */
  protocolId: string;
  /** Brand/catalog id (drives logo + preset). */
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
  protocolId: 'openai',
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

/** Brand logo with a monogram fallback so a missing SVG never breaks the UI. */
const ProviderLogo = ({ providerId, size = 28 }: { providerId: string; size?: number }) => {
  const [failed, setFailed] = useState(false);
  const letter = (providerId || '?').trim().charAt(0).toUpperCase() || '?';
  const box: React.CSSProperties = {
    width: size,
    height: size,
    flexShrink: 0,
    borderRadius: '6px',
    display: 'flex',
    alignItems: 'center',
    justifyContent: 'center',
    objectFit: 'contain',
  };
  if (failed || !providerId) {
    return (
      <div
        style={{
          ...box,
          background: 'var(--color-bg-elevated)',
          border: '1px solid var(--color-border-light)',
          color: 'var(--color-text-secondary)',
          fontSize: size * 0.5,
          fontWeight: 700,
        }}
        aria-hidden="true"
      >
        {letter}
      </div>
    );
  }
  return (
    <img
      src={`/provider-catalog/${providerId}.svg`}
      alt=""
      style={box}
      onError={() => setFailed(true)}
    />
  );
};

interface AssistantProviderSettingsProps {
  // 'new' opens the "Add Connection" flow immediately — used by first-run
  // deep links (e.g. the "Configure a provider first" badge in the chat).
  initialAction?: 'new' | null;
}

const AssistantProviderSettings = ({ initialAction = null }: AssistantProviderSettingsProps) => {
  const [connections, setConnections] = useState<ProviderConnection[]>([]);
  const [adapters, setAdapters] = useState<ProviderDescriptor[]>([]);
  const [catalog, setCatalog] = useState<ProviderCatalogEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [deletingId, setDeletingId] = useState<string | null>(null);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [formOpen, setFormOpen] = useState(false);
  const [form, setForm] = useState<ConnectionForm>(initialForm);
  // The catalog entry backing the current form (null for CLI picks and for
  // editing a legacy/custom connection with no matching entry).
  const [selectedEntry, setSelectedEntry] = useState<ProviderCatalogEntry | null>(null);
  const [showAdvancedUrl, setShowAdvancedUrl] = useState(false);
  const [probeModels, setProbeModels] = useState<ModelInfo[]>([]);
  const [probing, setProbing] = useState(false);
  // Page-level error (load/test/delete) vs. form-level error (validation,
  // save) — the latter renders inside the form modal where the user is.
  const [error, setError] = useState<string | null>(null);
  const [formError, setFormError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [descriptorModels, setDescriptorModels] = useState<ModelInfo[]>([]);

  const selectedAdapter = useMemo(
    () => adapters.find((adapter) => adapter.id === form.protocolId) || null,
    [adapters, form.protocolId],
  );
  const isCliAdapter = selectedAdapter?.isCliBacked === true;
  const requiresKey = selectedEntry ? selectedEntry.requiresApiKey : true;
  const baseUrlLocked = selectedEntry?.baseUrlLocked === true;

  const cliAdapters = useMemo(
    () => adapters.filter((adapter) => adapter.isCliBacked),
    [adapters],
  );
  const hostedEntries = useMemo(() => catalog.filter((e) => e.category === 'hosted'), [catalog]);
  const selfHostedEntries = useMemo(
    () => catalog.filter((e) => e.category === 'self_hosted'),
    [catalog],
  );
  const customEntries = useMemo(() => catalog.filter((e) => e.category === 'custom'), [catalog]);

  const loadData = useCallback(async () => {
    setLoading(true);
    try {
      const [nextConnections, nextAdapters, nextCatalog] = await Promise.all([
        assistantClient.listProviderConnections(),
        assistantClient.listAvailableProviderAdapters().catch(() => []),
        assistantClient.listProviderCatalog().catch(() => []),
      ]);
      setConnections(nextConnections || []);
      setAdapters(nextAdapters || []);
      setCatalog(nextCatalog || []);
      setError(null);
    } catch (err) {
      console.error('[AssistantProviderSettings] Failed to load:', err);
      setError('Failed to load provider connections.');
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    // eslint-disable-next-line react-hooks/set-state-in-effect -- initial-load + cross-tab event listener: the first mount fetches provider data once, and CONNECTIONS_CHANGED_EVENT triggers a reload when another view mutates connections. Effect is required for addEventListener/removeEventListener pairing; loadData is a stable useCallback([]).
    loadData();
    window.addEventListener(CONNECTIONS_CHANGED_EVENT, loadData);
    return () => window.removeEventListener(CONNECTIONS_CHANGED_EVENT, loadData);
  }, [loadData]);

  useEffect(() => {
    if (!isCliAdapter) {
      // eslint-disable-next-line react-hooks/set-state-in-effect -- CLI adapter model fetch keyed on isCliAdapter/form.protocolId: clears CLI models when the selected provider is not CLI-backed, otherwise fetches its static model list with a cancellation guard. Effect is required for the async fetch + cleanup; the rule cannot model the isCliAdapter branch.
      setDescriptorModels([]);
      return undefined;
    }
    let cancelled = false;
    (async () => {
      try {
        const models = await assistantClient.listProviderDescriptorModels(form.protocolId);
        if (cancelled) return;
        setDescriptorModels(models || []);
      } catch (err) {
        console.error('[AssistantProviderSettings] Failed to load CLI models:', err);
        if (!cancelled) setDescriptorModels([]);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [isCliAdapter, form.protocolId]);

  const beginCreate = useCallback(() => {
    setEditingId(null);
    setSelectedEntry(null);
    setFormError(null);
    setSuccess(null);
    setProbeModels([]);
    setShowAdvancedUrl(false);
    setPickerOpen(true);
  }, []);

  const chooseCatalogEntry = useCallback((entry: ProviderCatalogEntry) => {
    setEditingId(null);
    setSelectedEntry(entry);
    setProbeModels([]);
    setShowAdvancedUrl(false);
    setForm({
      ...initialForm,
      name: entry.displayName,
      protocolId: entry.protocolId,
      providerId: entry.id,
      baseUrl: entry.defaultBaseUrl || '',
      authMode: 'developer_api_key',
    });
    setFormError(null);
    setSuccess(null);
    setPickerOpen(false);
    setFormOpen(true);
  }, []);

  const chooseCliAdapter = useCallback((adapter: ProviderDescriptor) => {
    setEditingId(null);
    setSelectedEntry(null);
    setProbeModels([]);
    setShowAdvancedUrl(false);
    setForm({
      ...initialForm,
      name: adapter.displayName,
      protocolId: adapter.id,
      providerId: adapter.id,
      baseUrl: '',
      authMode: 'subscription_login',
    });
    setFormError(null);
    setSuccess(null);
    setPickerOpen(false);
    setFormOpen(true);
  }, []);

  const beginEdit = useCallback(
    (connection: ProviderConnection) => {
      setEditingId(connection.id);
      setSelectedEntry(catalog.find((e) => e.id === connection.providerId) || null);
      setProbeModels([]);
      setShowAdvancedUrl(Boolean(connection.baseUrl));
      setForm({
        id: connection.id,
        name: connection.name,
        protocolId: connection.protocolId,
        providerId: connection.providerId || connection.protocolId,
        apiKey: '',
        baseUrl: connection.baseUrl || '',
        modelId: connection.modelId,
        enabled: connection.enabled,
        authMode: connection.authMode || null,
      });
      setFormError(null);
      setSuccess(null);
      setFormOpen(true);
    },
    [catalog],
  );

  const closePicker = useCallback(() => setPickerOpen(false), []);
  const closeForm = useCallback(() => {
    if (saving) return;
    setFormOpen(false);
    setEditingId(null);
    setSelectedEntry(null);
    setProbeModels([]);
  }, [saving]);

  // Consume a 'new' deep link once per mount, after the catalog has loaded.
  const consumedInitialActionRef = useRef(false);
  useEffect(() => {
    if (initialAction !== 'new' || loading || consumedInitialActionRef.current) return;
    consumedInitialActionRef.current = true;
    beginCreate();
  }, [initialAction, loading, beginCreate]);

  const handleLoadModels = useCallback(async () => {
    setProbing(true);
    setFormError(null);
    try {
      const models = await assistantClient.probeCatalogModels({
        protocolId: form.protocolId,
        providerId: selectedEntry?.id || form.providerId || null,
        baseUrl: form.baseUrl.trim() || selectedEntry?.defaultBaseUrl || null,
        apiKey: form.apiKey.trim() || null,
      });
      setProbeModels(models || []);
      if (!models || models.length === 0) {
        setFormError('No models returned — you can still type a model id manually.');
      }
    } catch (err) {
      console.error('[AssistantProviderSettings] Model probe failed:', err);
      setProbeModels([]);
      setFormError(
        typeof err === 'string'
          ? err
          : 'Could not list models — check the key/endpoint, or type a model id manually.',
      );
    } finally {
      setProbing(false);
    }
  }, [form.protocolId, form.baseUrl, form.apiKey, selectedEntry]);

  const handleSubmit = useCallback(async () => {
    if (!form.name.trim()) {
      setFormError('Connection name is required.');
      return;
    }
    if (!isCliAdapter && !form.modelId.trim()) {
      setFormError('Model ID is required.');
      return;
    }
    if (!editingId && !isCliAdapter && requiresKey && !form.apiKey.trim()) {
      setFormError('API key is required for new connections.');
      return;
    }

    setSaving(true);
    setFormError(null);
    setSuccess(null);

    const authMode: AuthMode | null = isCliAdapter ? 'subscription_login' : form.authMode ?? null;
    const brandId = form.providerId || form.protocolId;

    try {
      if (editingId) {
        await assistantClient.updateProviderConnection({
          id: editingId,
          name: form.name.trim(),
          protocolId: form.protocolId,
          providerId: brandId,
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
          protocolId: form.protocolId,
          providerId: brandId,
          apiKey: isCliAdapter ? null : form.apiKey.trim() || null,
          authMode,
          baseUrl: form.baseUrl.trim() || null,
          modelId: form.modelId.trim(),
          accountLabel: null,
        });
        setSuccess('Connection created.');
      }

      setFormOpen(false);
      setEditingId(null);
      setSelectedEntry(null);
      await loadData();
      window.dispatchEvent(new CustomEvent(CONNECTIONS_CHANGED_EVENT));
    } catch (err) {
      console.error('[AssistantProviderSettings] Save failed:', err);
      setFormError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to save provider connection.');
    } finally {
      setSaving(false);
    }
  }, [editingId, form, isCliAdapter, requiresKey, loadData]);

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
        setFormOpen(false);
        setEditingId(null);
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
  }, [editingId, loadData]);

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

  // Model quick-pick list: live probe > catalog curated > CLI static list.
  const quickPickModels: ModelInfo[] =
    probeModels.length > 0
      ? probeModels
      : selectedEntry && selectedEntry.curatedModels.length > 0
        ? selectedEntry.curatedModels
        : isCliAdapter
          ? descriptorModels
          : [];

  const providerHeaderName = selectedEntry?.displayName || selectedAdapter?.displayName || form.providerId;

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

  const renderPickerCard = (
    key: string,
    providerId: string,
    title: string,
    subtitle: string,
    onClick: () => void,
  ) => (
    <button
      key={key}
      type="button"
      onClick={onClick}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '10px',
        textAlign: 'left',
        padding: '10px 12px',
        borderRadius: '10px',
        border: '1px solid var(--color-border-light)',
        background: 'var(--color-bg-elevated)',
        cursor: 'pointer',
        width: '100%',
      }}
    >
      <ProviderLogo providerId={providerId} size={32} />
      <span style={{ display: 'flex', flexDirection: 'column', minWidth: 0 }}>
        <span style={{ fontSize: '13px', fontWeight: 600, color: 'var(--color-text-primary)' }}>{title}</span>
        <span
          style={{
            fontSize: '11px',
            color: 'var(--color-text-secondary)',
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }}
        >
          {subtitle}
        </span>
      </span>
    </button>
  );

  const gridStyle: React.CSSProperties = {
    display: 'grid',
    gridTemplateColumns: 'repeat(auto-fill, minmax(210px, 1fr))',
    gap: '8px',
    marginBottom: '16px',
  };
  const groupLabelStyle: React.CSSProperties = {
    fontSize: '11px',
    fontWeight: 700,
    textTransform: 'uppercase',
    letterSpacing: '0.04em',
    color: 'var(--color-text-secondary)',
    margin: '4px 0 8px',
  };

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
          Pick a provider from the list — endpoint and models are prefilled — or configure a local
          CLI agent (Claude Code, Codex, OpenCode). Click a connection to edit it.
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
              <div style={{ display: 'flex', alignItems: 'center', gap: '10px', minWidth: 0, flex: 1 }}>
                <ProviderLogo providerId={connection.providerId || connection.protocolId} size={28} />
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
                        ? (connection.baseUrl || CLI_BINARY_PLACEHOLDERS[connection.protocolId] || connection.protocolId)
                        : (connection.baseUrl || 'api.openai.com/v1')}
                    </code>
                  </span>
                </div>
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

      {/* Step 1 — catalog picker */}
      {pickerOpen && ReactDOM.createPortal(
        <div className={modalStyles.overlay} onClick={(event) => event.target === event.currentTarget && closePicker()}>
          <div className={modalStyles.modal} style={{ width: '680px', maxWidth: '92vw' }} onClick={(event) => event.stopPropagation()}>
            <div className={modalStyles.header}>
              <h2 className={modalStyles.title}>Choose a provider</h2>
              <button className={modalStyles.closeButton} onClick={closePicker} title="Close">
                <CloseIcon />
              </button>
            </div>
            <div style={{ padding: '4px 20px 20px', overflowY: 'auto', maxHeight: '70vh' }}>
              {hostedEntries.length > 0 && (
                <>
                  <div style={groupLabelStyle}>Hosted</div>
                  <div style={gridStyle}>
                    {hostedEntries.map((entry) =>
                      renderPickerCard(entry.id, entry.id, entry.displayName, entry.description, () => chooseCatalogEntry(entry)),
                    )}
                  </div>
                </>
              )}
              {cliAdapters.length > 0 && (
                <>
                  <div style={groupLabelStyle}>Local CLI (uses your subscription)</div>
                  <div style={gridStyle}>
                    {cliAdapters.map((adapter) =>
                      renderPickerCard(adapter.id, adapter.id, adapter.displayName, 'Runs through the local CLI', () => chooseCliAdapter(adapter)),
                    )}
                  </div>
                </>
              )}
              {selfHostedEntries.length > 0 && (
                <>
                  <div style={groupLabelStyle}>Self-hosted / local</div>
                  <div style={gridStyle}>
                    {selfHostedEntries.map((entry) =>
                      renderPickerCard(entry.id, entry.id, entry.displayName, entry.description, () => chooseCatalogEntry(entry)),
                    )}
                  </div>
                </>
              )}
              {customEntries.length > 0 && (
                <>
                  <div style={groupLabelStyle}>Custom</div>
                  <div style={gridStyle}>
                    {customEntries.map((entry) =>
                      renderPickerCard(entry.id, entry.id, entry.displayName, entry.description, () => chooseCatalogEntry(entry)),
                    )}
                  </div>
                </>
              )}
            </div>
          </div>
        </div>,
        document.body,
      )}

      {/* Step 2 — preset-aware form */}
      {formOpen && ReactDOM.createPortal(
        <div className={modalStyles.overlay} onClick={(event) => event.target === event.currentTarget && closeForm()}>
          <div className={modalStyles.modal} style={{ width: '560px' }} onClick={(event) => event.stopPropagation()}>
            <div className={modalStyles.header}>
              <h2 className={modalStyles.title} style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
                <ProviderLogo providerId={form.providerId} size={24} />
                {editingId ? 'Edit Connection' : `Add ${providerHeaderName}`}
              </h2>
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

              {isCliAdapter && (
                <div className={modalStyles.quickConnect}>
                  <p className={modalStyles.sectionDescription}>
                    This provider runs through your local <strong>{providerHeaderName}</strong> CLI
                    using its own authentication (typically a paid subscription). Make sure the binary is
                    installed and you have signed in (e.g. <code>claude /login</code>, <code>codex login</code>, or <code>opencode auth login</code>) in your terminal
                    before testing this connection. No API key is stored.
                  </p>
                </div>
              )}

              {!isCliAdapter && requiresKey && (
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
                  {selectedEntry?.docsUrl && (
                    <a
                      href={selectedEntry.docsUrl}
                      target="_blank"
                      rel="noreferrer"
                      style={{ fontSize: '11px', color: 'var(--color-primary)', marginTop: '4px', display: 'inline-block' }}
                    >
                      Where do I get an API key? ↗
                    </a>
                  )}
                </div>
              )}

              {!isCliAdapter && !requiresKey && (
                <p className={modalStyles.sectionDescription}>
                  This provider is keyless — no API key required. Make sure the server is running at the endpoint below.
                </p>
              )}

              <div className={modalStyles.field}>
                <label className={modalStyles.label} htmlFor="provider-conn-model">
                  Model ID{' '}
                  {isCliAdapter && <span style={{ fontWeight: 400, opacity: 0.7 }}>(optional)</span>}
                </label>
                {quickPickModels.length > 0 && (
                  <select
                    className={modalStyles.select}
                    style={{ marginBottom: '6px' }}
                    value={quickPickModels.some((m) => m.id === form.modelId) ? form.modelId : ''}
                    onChange={(e) => setForm((current) => ({ ...current, modelId: e.target.value }))}
                    disabled={saving}
                  >
                    <option value="">
                      {isCliAdapter ? "Default (CLI's configured model)" : 'Pick a model…'}
                    </option>
                    {quickPickModels.map((model) => (
                      <option key={model.id} value={model.id}>
                        {model.displayName === model.id ? model.id : `${model.displayName} (${model.id})`}
                      </option>
                    ))}
                  </select>
                )}
                <div style={{ display: 'flex', gap: '8px' }}>
                  <input
                    id="provider-conn-model"
                    className={modalStyles.input}
                    style={{ flex: 1 }}
                    type="text"
                    value={form.modelId}
                    onChange={(e) => setForm((current) => ({ ...current, modelId: e.target.value }))}
                    placeholder={isCliAdapter ? 'Leave blank to use the CLI default' : 'e.g. gpt-4o-mini'}
                    disabled={saving}
                  />
                  {!isCliAdapter && (
                    <button
                      type="button"
                      style={{ ...secondaryButtonStyle, whiteSpace: 'nowrap' }}
                      onClick={handleLoadModels}
                      disabled={saving || probing}
                    >
                      {probing ? 'Loading…' : 'Load models'}
                    </button>
                  )}
                </div>
              </div>

              {/* Endpoint: CLI binary path, editable base URL, or locked-with-override */}
              {isCliAdapter ? (
                <div className={modalStyles.field}>
                  <label className={modalStyles.label} htmlFor="provider-conn-base-url">CLI binary path (optional)</label>
                  <input
                    id="provider-conn-base-url"
                    className={modalStyles.input}
                    type="text"
                    value={form.baseUrl}
                    onChange={(e) => setForm((current) => ({ ...current, baseUrl: e.target.value }))}
                    placeholder={CLI_BINARY_PLACEHOLDERS[form.protocolId] || 'claude'}
                    disabled={saving}
                  />
                </div>
              ) : baseUrlLocked && !showAdvancedUrl ? (
                <div className={modalStyles.field}>
                  <label className={modalStyles.label}>Endpoint</label>
                  <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                    <code style={{ fontSize: '12px', color: 'var(--color-text-secondary)' }}>
                      {selectedEntry?.defaultBaseUrl}
                    </code>
                    <button
                      type="button"
                      style={secondaryButtonStyle}
                      onClick={() => {
                        setShowAdvancedUrl(true);
                        setForm((current) => ({
                          ...current,
                          baseUrl: current.baseUrl || selectedEntry?.defaultBaseUrl || '',
                        }));
                      }}
                      disabled={saving}
                    >
                      Advanced: override
                    </button>
                  </div>
                </div>
              ) : (
                <div className={modalStyles.field}>
                  <label className={modalStyles.label} htmlFor="provider-conn-base-url">Base URL</label>
                  <input
                    id="provider-conn-base-url"
                    className={modalStyles.input}
                    type="text"
                    value={form.baseUrl}
                    onChange={(e) => setForm((current) => ({ ...current, baseUrl: e.target.value }))}
                    placeholder={selectedEntry?.defaultBaseUrl || 'https://api.openai.com/v1'}
                    disabled={saving}
                  />
                </div>
              )}

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
