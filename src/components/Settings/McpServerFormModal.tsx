import React, { useCallback, useEffect, useState } from 'react';
import ReactDOM from 'react-dom';
import {
  cancelMcpOAuthLogin,
  finishMcpOAuthLogin,
  startMcpOAuthLogin,
} from '../../api/client';
import type { McpCatalogEntry, McpServerResponse } from '../../generated/bindings';
import { openExternal } from '../../utils/openExternal';
import styles from './McpServerFormModal.module.css';

interface McpServerFormModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSubmit: (data: McpServerFormPayload) => Promise<McpServerResponse>;
  onServerSaved: (server: McpServerResponse) => void;
  server?: McpServerResponse | null;
  catalogEntry?: McpCatalogEntry | null;
}

type TransportType = 'stdio' | 'http';
type AuthType = 'none' | 'bearer_token' | 'oauth';

interface OAuthLoginState {
  loginId: string;
  authorizationUrl: string;
  expiresAt: string;
}

export interface McpServerFormPayload {
  name: string;
  enabled: boolean;
  transport: Record<string, unknown>;
  auth: Record<string, unknown>;
}

const CloseIcon = () => (
  <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="18" y1="6" x2="6" y2="18" />
    <line x1="6" y1="6" x2="18" y2="18" />
  </svg>
);

const parseList = (value: string): string[] => value
  .split(/[\s,]+/)
  .map((item) => item.trim())
  .filter(Boolean);

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const McpServerFormModal = ({
  isOpen,
  onClose,
  onSubmit,
  onServerSaved,
  server,
  catalogEntry,
}: McpServerFormModalProps) => {
  const isEditing = !!server;
  const [name, setName] = useState('');
  const [enabled, setEnabled] = useState(true);
  const [transportType, setTransportType] = useState<TransportType>('stdio');
  const [authType, setAuthType] = useState<AuthType>('none');
  const [bearerToken, setBearerToken] = useState('');
  const [hasStoredSecret, setHasStoredSecret] = useState(false);
  const [command, setCommand] = useState('');
  const [argsText, setArgsText] = useState('');
  const [url, setUrl] = useState('');
  const [scopesText, setScopesText] = useState('');
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [clientMetadataUrl, setClientMetadataUrl] = useState('');
  const [oauthLogin, setOauthLogin] = useState<OAuthLoginState | null>(null);
  const [saving, setSaving] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const existingConnectedOAuth = server?.auth?.type === 'oauth' && server.auth.connected;
  const formDisabled = saving || !!oauthLogin;

  // Catalog entries are pre-seeded with everything OAuth needs (name, URL,
  // suggested scopes, dynamic client registration). For a fresh connect we
  // collapse the full form to a single "Connect with OAuth" action and tuck
  // the editable fields behind an "Advanced settings" disclosure.
  const isCatalogQuickConnect =
    !!catalogEntry && !isEditing && transportType === 'http' && authType === 'oauth';
  const showFormFields = !isCatalogQuickConnect || showAdvanced;

  // This effect re-initialises the 16+ form fields from the incoming
  // `server` / `catalogEntry` props whenever the modal is (re)opened with
  // a different editing target. The form is mounted-once-per-edit and the
  // user can switch between "edit existing", "new from catalog", and
  // "new empty" modes, so we cannot rely on `useState` lazy initialisers
  // alone. The parent (`McpServersSettings.tsx`) could in principle
  // remount the modal via `key={server?.id ?? catalogEntry?.id ?? 'new'}`
  // to avoid this effect; tracked as a future refactor.
  useEffect(() => {
    if (!isOpen) {
      return;
    }

    // eslint-disable-next-line react-hooks/set-state-in-effect
    setOauthLogin(null);
    setClientSecret('');
    setShowAdvanced(false);

    if (server) {
      setName(server.name || '');
      setEnabled(server.enabled !== false);
      if (server.transport?.type === 'http') {
        setTransportType('http');
        setUrl(server.transport.url || catalogEntry?.endpointUrl || '');
        setAuthType((server.auth?.type || 'none') as AuthType);
        setHasStoredSecret(server.auth?.type === 'bearer_token' ? server.auth.has_secret : false);
        setBearerToken('');
        setCommand('');
        setArgsText('');
        if (server.auth?.type === 'oauth') {
          setScopesText((server.auth.scopes || []).join('\n'));
          setClientMetadataUrl(server.auth.client_metadata_url || '');
        } else {
          setScopesText((catalogEntry?.suggestedScopes || []).join('\n'));
          setClientMetadataUrl('');
        }
        setClientId('');
      } else {
        setTransportType('stdio');
        setCommand(server.transport?.command || '');
        setArgsText((server.transport?.args || []).join('\n'));
        setAuthType('none');
        setHasStoredSecret(false);
        setBearerToken('');
        setUrl(catalogEntry?.endpointUrl || '');
        setScopesText((catalogEntry?.suggestedScopes || []).join('\n'));
        setClientId('');
        setClientMetadataUrl('');
      }
    } else if (catalogEntry) {
      setName(catalogEntry.displayName);
      setEnabled(true);
      setTransportType('http');
      setAuthType('oauth');
      setHasStoredSecret(false);
      setBearerToken('');
      setCommand('');
      setArgsText('');
      setUrl(catalogEntry.endpointUrl);
      setScopesText((catalogEntry.suggestedScopes || []).join('\n'));
      setClientId('');
      setClientMetadataUrl('');
    } else {
      setName('');
      setEnabled(true);
      setTransportType('stdio');
      setAuthType('none');
      setHasStoredSecret(false);
      setBearerToken('');
      setCommand('');
      setArgsText('');
      setUrl('');
      setScopesText('');
      setClientId('');
      setClientMetadataUrl('');
    }

    setError(null);
  }, [isOpen, server, catalogEntry]);

  const cancelPendingOAuth = useCallback(async () => {
    if (!oauthLogin) {
      return;
    }
    try {
      await cancelMcpOAuthLogin(oauthLogin.loginId);
    } catch (cancelError) {
      console.warn('[McpServerFormModal] Failed to cancel OAuth login:', cancelError);
    } finally {
      setOauthLogin(null);
    }
  }, [oauthLogin]);

  const requestClose = useCallback(() => {
    if (saving) {
      return;
    }
    void cancelPendingOAuth();
    onClose();
  }, [cancelPendingOAuth, onClose, saving]);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (event.key === 'Escape' && isOpen && !saving) {
        requestClose();
      }
    };

    document.addEventListener('keydown', handleEscape);
    return () => document.removeEventListener('keydown', handleEscape);
  }, [isOpen, requestClose, saving]);

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

  const buildPayload = (): McpServerFormPayload | null => {
    const trimmedName = name.trim();
    if (!trimmedName) {
      setError('Server name is required');
      return null;
    }

    let transport: Record<string, unknown>;
    if (transportType === 'http') {
      const trimmedUrl = url.trim();
      if (!trimmedUrl) {
        setError('HTTP transport requires a URL');
        return null;
      }
      transport = { type: 'http', url: trimmedUrl };
    } else {
      const trimmedCommand = command.trim();
      if (!trimmedCommand) {
        setError('Stdio transport requires a command');
        return null;
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
      if (!bearerToken.trim() && !hasStoredSecret) {
        setError('Bearer token is required');
        return null;
      }
      auth = {
        type: 'bearer_token',
        token: bearerToken.trim() || undefined,
      };
    } else if (transportType === 'http' && authType === 'oauth') {
      auth = {
        type: 'oauth',
        scopes: parseList(scopesText),
        client_id: clientId.trim() || undefined,
        client_secret: clientSecret.trim() || undefined,
        client_metadata_url: clientMetadataUrl.trim() || undefined,
      };
    }

    return {
      name: trimmedName,
      enabled,
      transport,
      auth,
    };
  };

  const saveConfig = async () => {
    const payload = buildPayload();
    if (!payload) {
      return;
    }

    setSaving(true);
    setError(null);
    try {
      await onSubmit(payload);
      onClose();
    } catch (submitError) {
      setError(errText(submitError, 'Failed to save MCP server'));
    } finally {
      setSaving(false);
    }
  };

  const startOAuth = async () => {
    const payload = buildPayload();
    if (!payload || payload.transport.type !== 'http') {
      return;
    }

    setSaving(true);
    setError(null);
    try {
      const started = await startMcpOAuthLogin({
        serverId: server?.id || null,
        name: payload.name,
        enabled: payload.enabled,
        url: String(payload.transport.url),
        scopes: parseList(scopesText),
        clientId: clientId.trim() || null,
        clientSecret: clientSecret.trim() || null,
        clientMetadataUrl: clientMetadataUrl.trim() || null,
      });
      setOauthLogin({
        loginId: started.loginId,
        authorizationUrl: started.authorizationUrl,
        expiresAt: started.expiresAt,
      });
      await openExternal(started.authorizationUrl);
    } catch (oauthError) {
      setError(errText(oauthError, 'Failed to start MCP OAuth login'));
    } finally {
      setSaving(false);
    }
  };

  const finishOAuth = async () => {
    if (!oauthLogin) {
      return;
    }

    setSaving(true);
    setError(null);
    try {
      const saved = await finishMcpOAuthLogin(oauthLogin.loginId);
      setOauthLogin(null);
      onServerSaved(saved);
      onClose();
    } catch (oauthError) {
      setOauthLogin(null);
      setError(errText(oauthError, 'Failed to finish MCP OAuth login'));
    } finally {
      setSaving(false);
    }
  };

  const handleSubmit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (transportType === 'http' && authType === 'oauth') {
      if (oauthLogin) {
        await finishOAuth();
      } else if (existingConnectedOAuth) {
        await saveConfig();
      } else {
        await startOAuth();
      }
      return;
    }
    await saveConfig();
  };

  const submitLabel = (() => {
    if (saving) {
      return oauthLogin ? 'Finishing...' : 'Saving...';
    }
    if (oauthLogin) {
      return 'Finish Connection';
    }
    if (transportType === 'http' && authType === 'oauth' && !existingConnectedOAuth) {
      return 'Connect with OAuth';
    }
    return isEditing ? 'Save Changes' : 'Create Server';
  })();

  return ReactDOM.createPortal(
    <div className={styles.overlay} onClick={(event) => event.target === event.currentTarget && requestClose()}>
      <div className={styles.modal} onClick={(event) => event.stopPropagation()}>
        <div className={styles.header}>
          <h2 className={styles.title}>{isEditing ? 'Edit MCP Server' : catalogEntry ? `Connect ${catalogEntry.displayName}` : 'Add MCP Server'}</h2>
          <button className={styles.closeButton} onClick={requestClose} disabled={saving} title="Close">
            <CloseIcon />
          </button>
        </div>

        <form className={styles.form} onSubmit={handleSubmit}>
          {error && <div className={styles.errorBanner}>{error}</div>}

          {catalogEntry && (
            <div className={styles.templatePanel}>
              <div className={styles.templateHeader}>
                <div>
                  <div className={styles.templateMeta}>{catalogEntry.category}</div>
                  <div className={styles.sectionTitle}>{catalogEntry.displayName}</div>
                </div>
              </div>
              <div className={styles.sectionDescription}>{catalogEntry.description}</div>
              {catalogEntry.notes && (
                <div className={styles.sectionDescription}>{catalogEntry.notes}</div>
              )}
            </div>
          )}

          {oauthLogin && (
            <div className={styles.pendingPanel}>
              <div className={styles.sectionTitle}>OAuth Login Started</div>
              <div className={styles.sectionDescription}>
                Complete the browser authorization, then finish the connection here.
              </div>
              <div className={styles.oauthActions}>
                <button
                  type="button"
                  className={styles.cancelButton}
                  onClick={() => openExternal(oauthLogin.authorizationUrl)}
                  disabled={saving}
                >
                  Open Browser
                </button>
                <button
                  type="button"
                  className={styles.cancelButton}
                  onClick={cancelPendingOAuth}
                  disabled={saving}
                >
                  Cancel Login
                </button>
              </div>
            </div>
          )}

          {isCatalogQuickConnect && !oauthLogin && (
            <div className={styles.quickConnect}>
              <p className={styles.sectionDescription}>
                CLAI will open your browser to authorize access using OAuth. No extra setup is
                required — adjust the connection details only if this provider needs custom scopes
                or a pre-registered client.
              </p>
              <button
                type="button"
                className={styles.advancedToggle}
                aria-expanded={showAdvanced}
                aria-controls="mcp-advanced-fields"
                onClick={() => setShowAdvanced((value) => !value)}
                disabled={saving}
              >
                {showAdvanced ? 'Hide advanced settings' : 'Advanced settings'}
              </button>
            </div>
          )}

          {showFormFields && (
            <div id="mcp-advanced-fields" className={styles.advancedFields}>
              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-server-name">Name</label>
                <input
                  id="mcp-server-name"
                  className={styles.input}
                  type="text"
                  value={name}
                  onChange={(event) => setName(event.target.value)}
                  placeholder="e.g., Filesystem Tools"
                  disabled={formDisabled}
                  autoFocus
                />
              </div>

              <div className={styles.field}>
                <label className={styles.label} htmlFor="mcp-transport-type">Transport</label>
                <select
                  id="mcp-transport-type"
                  className={styles.select}
                  value={transportType}
                  onChange={(event) => {
                    const next = event.target.value as TransportType;
                    setTransportType(next);
                    if (next === 'stdio') {
                      setAuthType('none');
                    }
                  }}
                  disabled={formDisabled}
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
                      disabled={formDisabled}
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
                      disabled={formDisabled}
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
                      disabled={formDisabled}
                    />
                  </div>

                  <div className={styles.field}>
                    <label className={styles.label} htmlFor="mcp-auth-type">Authentication</label>
                    <select
                      id="mcp-auth-type"
                      className={styles.select}
                      value={authType}
                      onChange={(event) => setAuthType(event.target.value as AuthType)}
                      disabled={formDisabled}
                    >
                      <option value="none">None</option>
                      <option value="bearer_token">Bearer Token</option>
                      <option value="oauth">OAuth</option>
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
                        disabled={formDisabled}
                      />
                      <span className={styles.hint}>
                        {hasStoredSecret
                          ? 'A token is already stored securely. Enter a new value only to rotate it.'
                          : 'The token is stored securely in your OS keyring and is never written to config.json.'}
                      </span>
                    </div>
                  )}

                  {authType === 'oauth' && (
                    <div className={styles.section}>
                      <div className={styles.sectionTitle}>OAuth</div>
                      {server?.auth?.type === 'oauth' && (
                        <div className={`${styles.authStatus} ${server.auth.connected ? styles.authConnected : styles.authDisconnected}`}>
                          {server.auth.connected ? 'Connected' : 'Reconnect required'}
                        </div>
                      )}
                      <div className={styles.field}>
                        <label className={styles.label} htmlFor="mcp-oauth-scopes">Scopes</label>
                        <textarea
                          id="mcp-oauth-scopes"
                          className={styles.textarea}
                          value={scopesText}
                          onChange={(event) => setScopesText(event.target.value)}
                          placeholder="Leave blank to use server defaults"
                          rows={3}
                          disabled={formDisabled}
                        />
                        <span className={styles.hint}>Separate scopes with spaces, commas, or new lines.</span>
                      </div>
                      <div className={styles.gridTwo}>
                        <div className={styles.field}>
                          <label className={styles.label} htmlFor="mcp-oauth-client-id">Client ID</label>
                          <input
                            id="mcp-oauth-client-id"
                            className={styles.input}
                            type="text"
                            value={clientId}
                            onChange={(event) => setClientId(event.target.value)}
                            placeholder={server?.auth?.type === 'oauth' && server.auth.client_id_configured ? 'Configured' : 'Dynamic registration'}
                            disabled={formDisabled}
                          />
                        </div>
                        <div className={styles.field}>
                          <label className={styles.label} htmlFor="mcp-oauth-client-secret">Client Secret</label>
                          <input
                            id="mcp-oauth-client-secret"
                            className={styles.input}
                            type="password"
                            value={clientSecret}
                            onChange={(event) => setClientSecret(event.target.value)}
                            placeholder={server?.auth?.type === 'oauth' && server.auth.client_secret_configured ? 'Stored' : 'Optional'}
                            disabled={formDisabled}
                          />
                        </div>
                      </div>
                      <div className={styles.field}>
                        <label className={styles.label} htmlFor="mcp-oauth-client-metadata-url">Client Metadata URL</label>
                        <input
                          id="mcp-oauth-client-metadata-url"
                          className={styles.input}
                          type="url"
                          value={clientMetadataUrl}
                          onChange={(event) => setClientMetadataUrl(event.target.value)}
                          placeholder="Use CLAI default metadata"
                          disabled={formDisabled || !!clientId.trim()}
                        />
                      </div>
                      {existingConnectedOAuth && !oauthLogin && (
                        <div className={styles.oauthActions}>
                          <button
                            type="button"
                            className={styles.cancelButton}
                            onClick={startOAuth}
                            disabled={saving}
                          >
                            Reconnect OAuth
                          </button>
                        </div>
                      )}
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
                    disabled={formDisabled}
                  />
                  <span>Enabled</span>
                </label>
                <span className={styles.hint}>
                  Disabled servers stay in config but are hidden from agent selection and future tool discovery.
                </span>
              </div>
            </div>
          )}

          <div className={styles.actions}>
            <button type="button" className={styles.cancelButton} onClick={requestClose} disabled={saving}>
              Cancel
            </button>
            <button type="submit" className={styles.submitButton} disabled={saving}>
              {submitLabel}
            </button>
          </div>
        </form>
      </div>
    </div>,
    document.body
  );
};

export default McpServerFormModal;
