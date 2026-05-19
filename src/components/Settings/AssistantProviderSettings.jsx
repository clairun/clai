import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { assistantClient } from '../../assistant';
import styles from './ProviderSettings.module.css';

const CONNECTIONS_CHANGED_EVENT = 'assistant-provider-connections-changed';

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

const inputStyle = {
  width: '100%',
  padding: '10px 12px',
  fontSize: '14px',
  fontFamily: "'Monaco', 'Menlo', 'Consolas', monospace",
  color: 'var(--color-text-primary)',
  background: 'var(--color-bg-primary)',
  border: '1px solid var(--color-border-light)',
  borderRadius: '8px',
  outline: 'none',
  boxSizing: 'border-box',
};

const labelStyle = {
  display: 'block',
  fontSize: '13px',
  fontWeight: 500,
  color: 'var(--color-text-secondary)',
  marginBottom: '6px',
};

const secondaryButtonStyle = {
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

const initialForm = {
  id: null,
  name: '',
  providerId: 'openai',
  apiKey: '',
  baseUrl: '',
  modelId: '',
  enabled: true,
};

const AssistantProviderSettings = () => {
  const [connections, setConnections] = useState([]);
  const [adapters, setAdapters] = useState([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [testingId, setTestingId] = useState(null);
  const [deletingId, setDeletingId] = useState(null);
  const [editingId, setEditingId] = useState(null);
  const [form, setForm] = useState(initialForm);
  const [error, setError] = useState(null);
  const [success, setSuccess] = useState(null);

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

  const beginEdit = useCallback((connection) => {
    setEditingId(connection.id);
    setForm({
      id: connection.id,
      name: connection.name,
      providerId: connection.providerId,
      apiKey: '',
      baseUrl: connection.baseUrl || '',
      modelId: connection.modelId,
      enabled: connection.enabled,
    });
    setError(null);
    setSuccess(null);
  }, []);

  const handleSubmit = useCallback(async () => {
    if (!form.name.trim()) {
      setError('Connection name is required.');
      return;
    }
    if (!form.modelId.trim()) {
      setError('Model ID is required.');
      return;
    }
    if (!editingId && !form.apiKey.trim()) {
      setError('API key is required for new connections.');
      return;
    }

    setSaving(true);
    setError(null);
    setSuccess(null);

    try {
      if (editingId) {
        await assistantClient.updateProviderConnection({
          id: form.id,
          name: form.name.trim(),
          providerId: form.providerId,
          apiKey: form.apiKey.trim() || null,
          baseUrl: form.baseUrl.trim() || null,
          modelId: form.modelId.trim(),
          enabled: form.enabled,
        });
        setSuccess('Connection updated.');
      } else {
        await assistantClient.createProviderConnection({
          name: form.name.trim(),
          providerId: form.providerId,
          apiKey: form.apiKey.trim(),
          baseUrl: form.baseUrl.trim() || null,
          modelId: form.modelId.trim(),
        });
        setSuccess('Connection created.');
      }

      resetForm();
      await loadData();
      window.dispatchEvent(new CustomEvent(CONNECTIONS_CHANGED_EVENT));
    } catch (err) {
      console.error('[AssistantProviderSettings] Save failed:', err);
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to save provider connection.'));
    } finally {
      setSaving(false);
    }
  }, [editingId, form, loadData, resetForm]);

  const handleDelete = useCallback(async (connection) => {
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
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to delete provider connection.'));
    } finally {
      setDeletingId(null);
    }
  }, [editingId, loadData, resetForm]);

  const handleTest = useCallback(async (connectionId) => {
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
      setError(typeof err === 'string' ? err : (err?.message || 'Failed to test provider connection.'));
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
        <h3 className={styles.sectionTitle}>Assistant Provider Connections</h3>
        <p className={styles.sectionDescription}>
          Configure one or more OpenAI-compatible connections for the assistant runtime and scheduled agents.
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
              </div>
              <span className={styles.providerCommand}>
                <code>{connection.modelId}</code> • <code>{connection.baseUrl || 'api.openai.com/v1'}</code>
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

      <div style={{ display: 'flex', flexDirection: 'column', gap: '12px', padding: '16px', border: '1px solid var(--color-border-light)', borderRadius: '10px', background: 'var(--color-bg-primary)' }}>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center', gap: '12px' }}>
          <div>
            <div className={styles.sectionTitle} style={{ fontSize: '14px', marginBottom: '4px' }}>
              {editingId ? 'Edit Connection' : 'Add Connection'}
            </div>
            <div className={styles.sectionDescription}>
              {editingId ? 'Update the selected provider connection.' : 'Create a new provider connection.'}
            </div>
          </div>
          {editingId && (
            <button type="button" style={secondaryButtonStyle} onClick={resetForm} disabled={saving}>
              Cancel
            </button>
          )}
        </div>

        <div>
          <label style={labelStyle}>Connection Name</label>
          <input
            type="text"
            value={form.name}
            onChange={(e) => setForm((current) => ({ ...current, name: e.target.value }))}
            placeholder="e.g. Personal OpenAI"
            style={inputStyle}
          />
        </div>

        <div>
          <label style={labelStyle}>Provider Adapter</label>
          <select
            className={styles.select}
            value={form.providerId}
            onChange={(e) => setForm((current) => ({ ...current, providerId: e.target.value }))}
            disabled={saving || editingId !== null}
          >
            {(adapters.length > 0 ? adapters : [{ id: 'openai', displayName: 'OpenAI-Compatible' }]).map((adapter) => (
              <option key={adapter.id} value={adapter.id}>
                {adapter.displayName}
              </option>
            ))}
          </select>
        </div>

        <div>
          <label style={labelStyle}>
            API Key {!editingId && <span style={{ color: 'var(--color-critical, #DC2626)' }}>*</span>}
          </label>
          <input
            type="password"
            value={form.apiKey}
            onChange={(e) => setForm((current) => ({ ...current, apiKey: e.target.value }))}
            placeholder={editingId ? 'Leave blank to keep existing key' : 'sk-...'}
            style={inputStyle}
          />
        </div>

        <div>
          <label style={labelStyle}>
            Model ID <span style={{ color: 'var(--color-critical, #DC2626)' }}>*</span>
          </label>
          <input
            type="text"
            value={form.modelId}
            onChange={(e) => setForm((current) => ({ ...current, modelId: e.target.value }))}
            placeholder="e.g. gpt-4o-mini"
            style={inputStyle}
          />
        </div>

        <div>
          <label style={labelStyle}>Base URL</label>
          <input
            type="text"
            value={form.baseUrl}
            onChange={(e) => setForm((current) => ({ ...current, baseUrl: e.target.value }))}
            placeholder="https://api.openai.com/v1"
            style={inputStyle}
          />
        </div>

        {editingId && (
          <label style={{ display: 'flex', alignItems: 'center', gap: '8px', fontSize: '13px', color: 'var(--color-text-secondary)' }}>
            <input
              type="checkbox"
              checked={form.enabled}
              onChange={(e) => setForm((current) => ({ ...current, enabled: e.target.checked }))}
            />
            Enabled
          </label>
        )}

        <button
          type="button"
          onClick={handleSubmit}
          disabled={saving}
          style={{
            width: '100%',
            padding: '10px 16px',
            fontSize: '14px',
            fontWeight: 500,
            fontFamily: 'inherit',
            color: '#fff',
            background: saving ? 'var(--color-text-tertiary)' : 'var(--color-primary, #6366f1)',
            border: 'none',
            borderRadius: '8px',
            cursor: saving ? 'not-allowed' : 'pointer',
          }}
        >
          {saving ? 'Saving…' : editingId ? 'Save Connection' : 'Add Connection'}
        </button>
      </div>

      <div className={styles.hint}>
        <p>
          Provider credentials are stored securely in your OS keychain. The first enabled connection becomes the default for non-agent tabs until the user switches it from the tab context.
        </p>
      </div>
    </div>
  );
};

export default AssistantProviderSettings;
