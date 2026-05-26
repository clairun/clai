import React, { useEffect, useState } from 'react';
import ReactDOM from 'react-dom';
import type { McpServerResponse } from '../../generated/bindings';
import styles from './McpServerFormModal.module.css';

interface McpServerFormModalProps {
  isOpen: boolean;
  onClose: () => void;
  // Receives the assembled form payload (name/enabled/integrationType/
  // transport/auth). Loosely typed to match McpServersSettings' handler.
  onSubmit: (data: Record<string, unknown>) => Promise<void>;
  server?: McpServerResponse | null;
}

const CloseIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

const McpServerFormModal = ({ isOpen, onClose, onSubmit, server }: McpServerFormModalProps) => {
  const isEditing = !!server;
  const [name, setName] = useState('');
  const [enabled, setEnabled] = useState(true);
  const [transportType, setTransportType] = useState<'stdio' | 'http'>('stdio');
  const [integrationType, setIntegrationType] = useState('generic');
  const [authType, setAuthType] = useState('none');
  const [bearerToken, setBearerToken] = useState('');
  const [hasStoredSecret, setHasStoredSecret] = useState(false);
  const [command, setCommand] = useState('');
  const [argsText, setArgsText] = useState('');
  const [url, setUrl] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) {
      return;
    }

    if (server) {
      setName(server.name || '');
      setEnabled(server.enabled !== false);
      setIntegrationType(server.integrationType || 'generic');
      if (server.transport?.type === 'http') {
        setTransportType('http');
        setUrl(server.transport.url || '');
        setAuthType(server.auth?.type || 'none');
        // Binding field is snake_case (`has_secret`) and only exists on
        // the bearer_token variant; narrow before reading.
        setHasStoredSecret(server.auth?.type === 'bearer_token' ? server.auth.has_secret : false);
        setBearerToken('');
        setCommand('');
        setArgsText('');
      } else {
        setTransportType('stdio');
        setCommand(server.transport?.command || '');
        setArgsText((server.transport?.args || []).join('\n'));
        setAuthType('none');
        setHasStoredSecret(false);
        setBearerToken('');
        setUrl('');
      }
    } else {
      setName('');
      setEnabled(true);
      setTransportType('stdio');
      setIntegrationType('generic');
      setAuthType('none');
      setHasStoredSecret(false);
      setBearerToken('');
      setCommand('');
      setArgsText('');
      setUrl('');
    }

    setError(null);
  }, [isOpen, server]);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isOpen && !saving) {
        onClose();
      }
    };

    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [isOpen, saving, onClose]);

  useEffect(() => {
    if (isOpen) {
      document.body.style.overflow = 'hidden';
    } else {
      document.body.style.overflow = '';
    }

    return () => {
      document.body.style.overflow = '';
    };
  }, [isOpen]);

  if (!isOpen) {
    return null;
  }

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    setError(null);

    const trimmedName = name.trim();
    if (!trimmedName) {
      setError('Server name is required');
      return;
    }

    let transport: Record<string, unknown>;
    if (transportType === 'http') {
      const trimmedUrl = url.trim();
      if (!trimmedUrl) {
        setError('HTTP transport requires a URL');
        return;
      }
      transport = { type: 'http', url: trimmedUrl };
    } else {
      const trimmedCommand = command.trim();
      if (!trimmedCommand) {
        setError('Stdio transport requires a command');
        return;
      }
      transport = {
        type: 'stdio',
        command: trimmedCommand,
        args: argsText
          .split('\n')
          .map((arg) => arg.trim())
          .filter(Boolean),
      };
    }

    let auth: Record<string, unknown> = { type: 'none' };
    if (transportType === 'http' && authType === 'bearer_token') {
      auth = {
        type: 'bearer_token',
        token: bearerToken.trim() || undefined,
      };
    }

    setSaving(true);
    try {
      await onSubmit({
        name: trimmedName,
        enabled,
        integrationType,
        transport,
        auth,
      });
    } catch (submitError) {
      setError(submitError instanceof Error ? submitError.message : 'Failed to save MCP server');
    } finally {
      setSaving(false);
    }
  };

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={(event) => event.target === event.currentTarget && !saving && onClose()}>
      <div className={styles.modal} onClick={(event) => event.stopPropagation()}>
        <div className={styles.header}>
          <h2 className={styles.title}>{isEditing ? 'Edit MCP Server' : 'Add MCP Server'}</h2>
          <button className={styles.closeButton} onClick={onClose} disabled={saving} title="Close">
            <CloseIcon />
          </button>
        </div>

        <form className={styles.form} onSubmit={handleSubmit}>
          {error && <div className={styles.errorBanner}>{error}</div>}

          <div className={styles.field}>
            <label className={styles.label} htmlFor="mcp-server-name">Name</label>
            <input
              id="mcp-server-name"
              className={styles.input}
              type="text"
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="e.g., Filesystem Tools"
              disabled={saving}
              autoFocus
            />
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="mcp-integration-type">Integration</label>
            <select
              id="mcp-integration-type"
              className={styles.select}
              value={integrationType}
              onChange={(event) => setIntegrationType(event.target.value)}
              disabled={saving}
            >
              <option value="generic">Generic MCP</option>
              <option value="netdata_cloud">Netdata Cloud</option>
            </select>
          </div>

          <div className={styles.field}>
            <label className={styles.label} htmlFor="mcp-transport-type">Transport</label>
            <select
              id="mcp-transport-type"
              className={styles.select}
              value={transportType}
              onChange={(event) => setTransportType(event.target.value as 'stdio' | 'http')}
              disabled={saving}
            >
              <option value="stdio">Stdio</option>
              <option value="http">HTTP</option>
            </select>
          </div>

          {transportType === 'stdio' ? (
            <>
              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-command">Command</label>
                <input
                  id="mcp-command"
                  className={styles.input}
                  type="text"
                  value={command}
                  onChange={(event) => setCommand(event.target.value)}
                  placeholder="npx"
                  disabled={saving}
                />
              </div>

              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-args">Arguments</label>
                <textarea
                  id="mcp-args"
                  className={styles.textarea}
                  value={argsText}
                  onChange={(event) => setArgsText(event.target.value)}
                  placeholder="@modelcontextprotocol/server-filesystem&#10;/path/to/root"
                  rows={4}
                  disabled={saving}
                />
                <span className={styles.hint}>One argument per line.</span>
              </div>
            </>
          ) : (
            <>
              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-url">URL</label>
                <input
                  id="mcp-url"
                  className={styles.input}
                  type="url"
                  value={url}
                  onChange={(event) => setUrl(event.target.value)}
                  placeholder="https://example.com/mcp"
                  disabled={saving}
                />
              </div>

              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-auth-type">Authentication</label>
                <select
                  id="mcp-auth-type"
                  className={styles.select}
                  value={authType}
                  onChange={(event) => setAuthType(event.target.value)}
                  disabled={saving}
                >
                  <option value="none">None</option>
                  <option value="bearer_token">Bearer Token</option>
                </select>
              </div>

              {authType === 'bearer_token' && (
                <div className={styles.field}>
                  <label className={styles.label} htmlFor="mcp-bearer-token">Bearer Token</label>
                  <input
                    id="mcp-bearer-token"
                    className={styles.input}
                    type="password"
                    value={bearerToken}
                    onChange={(event) => setBearerToken(event.target.value)}
                    placeholder={isEditing ? 'Leave blank to keep current token' : 'Paste bearer token'}
                    disabled={saving}
                  />
                  <span className={styles.hint}>
                    {hasStoredSecret
                      ? 'A token is already stored securely. Enter a new value only to rotate it.'
                      : 'The token is stored securely in your OS keyring and is never written to config.json.'}
                  </span>
                </div>
              )}
            </>
          )}

          <div className={styles.field}>
            <label className={styles.checkboxOption}>
              <input
                type="checkbox"
                checked={enabled}
                onChange={(event) => setEnabled(event.target.checked)}
                disabled={saving}
              />
              <span>Enabled</span>
            </label>
            <span className={styles.hint}>
              Disabled servers stay in config but are hidden from agent selection and future tool discovery.
            </span>
          </div>

          <div className={styles.actions}>
            <button type="button" className={styles.cancelButton} onClick={onClose} disabled={saving}>
              Cancel
            </button>
            <button type="submit" className={styles.submitButton} disabled={saving}>
              {saving ? 'Saving...' : isEditing ? 'Save Changes' : 'Create Server'}
            </button>
          </div>
        </form>
      </div>
    </div>,
    document.body
  );
};

export default McpServerFormModal;
