import React, { useEffect, useRef, useState } from 'react';
import {
  createMcpServer,
  deleteMcpServer,
  disconnectMcpOAuth,
  getMcpServerCatalog,
  getMcpServers,
  updateMcpServer,
} from '../../api/client';
import McpServerFormModal from './McpServerFormModal';
import type { McpServerFormPayload } from './McpServerFormModal';
import type {
  McpCatalogEntry,
  McpServerAuthResponse,
  McpServerResponse,
  McpServerTransport,
} from '../../generated/bindings';
import styles from './McpServersSettings.module.css';

const MCP_SERVERS_CHANGED_EVENT = 'mcp-servers-changed';

type McpSection = 'catalog' | 'configured';

const PlusIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
  </svg>
);

const LinkIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <path d="M10 13a5 5 0 0 0 7.07 0l2.83-2.83a5 5 0 0 0-7.07-7.07L11.5 4.43" />
    <path d="M14 11a5 5 0 0 0-7.07 0L4.1 13.83a5 5 0 0 0 7.07 7.07l1.33-1.33" />
  </svg>
);

const transportSummary = (transport: McpServerTransport | null | undefined): string => {
  if (!transport) return 'Unknown transport';
  if (transport.type === 'http') {
    return transport.url;
  }
  return `${transport.command}${transport.args?.length ? ` ${transport.args.join(' ')}` : ''}`;
};

const authSummary = (auth: McpServerAuthResponse | null | undefined): string => {
  if (!auth || auth.type === 'none') {
    return 'No auth';
  }
  if (auth.type === 'bearer_token') {
    // Binding field is snake_case (`has_secret`); the old .jsx read
    // `auth.hasSecret` and always rendered "missing".
    return auth.has_secret ? 'Bearer token configured' : 'Bearer token missing';
  }
  if (auth.type === 'oauth') {
    return auth.connected ? 'OAuth connected' : 'OAuth reconnect required';
  }
  return 'Unknown auth';
};

const normalizeUrl = (value: string): string => value.replace(/\/+$/, '').toLowerCase();

const configuredCatalogServer = (
  entry: McpCatalogEntry,
  servers: McpServerResponse[]
): McpServerResponse | undefined => servers.find((server) => (
  server.transport?.type === 'http'
    && normalizeUrl(server.transport.url) === normalizeUrl(entry.endpointUrl)
));

const initials = (name: string): string => name
  .split(/\s+/)
  .filter(Boolean)
  .slice(0, 2)
  .map((part) => part[0]?.toUpperCase())
  .join('') || 'M';

const errText = (err: unknown, fallback: string): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : fallback;

const McpServersSettings = () => {
  const [servers, setServers] = useState<McpServerResponse[]>([]);
  const [catalog, setCatalog] = useState<McpCatalogEntry[]>([]);
  const [failedLogos, setFailedLogos] = useState<Set<string>>(() => new Set());
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingServer, setEditingServer] = useState<McpServerResponse | null>(null);
  const [selectedCatalogEntry, setSelectedCatalogEntry] = useState<McpCatalogEntry | null>(null);
  const [disconnectingId, setDisconnectingId] = useState<string | null>(null);
  const [activeSection, setActiveSection] = useState<McpSection>('catalog');

  useEffect(() => {
    // eslint-disable-next-line react-hooks/immutability -- One-shot async bootstrap: loadServers is declared below with `const` so the linter cannot prove the closure value at effect registration; the function only reads the initial state via setServers/setLoading/setError, so the TDZ is benign here.
    loadServers();
  }, []);

  const loadServers = async () => {
    setLoading(true);
    setError(null);

    try {
      const [serverResult, catalogResult] = await Promise.all([
        getMcpServers(),
        getMcpServerCatalog(),
      ]);
      setServers(serverResult || []);
      setCatalog(catalogResult || []);
    } catch (loadError) {
      console.error('[McpServersSettings] Failed to load servers:', loadError);
      setError(errText(loadError, 'Failed to load MCP servers. Please try again.'));
    } finally {
      setLoading(false);
    }
  };

  const handleCreate = () => {
    setEditingServer(null);
    setSelectedCatalogEntry(null);
    setIsFormOpen(true);
  };

  const handleConnectCatalogEntry = (entry: McpCatalogEntry) => {
    const configured = configuredCatalogServer(entry, servers);
    setEditingServer(configured || null);
    setSelectedCatalogEntry(entry);
    setIsFormOpen(true);
  };

  const handleEdit = (server: McpServerResponse) => {
    setEditingServer(server);
    setSelectedCatalogEntry(null);
    setIsFormOpen(true);
  };

  const handleDelete = async (serverId: string) => {
    setError(null);
    try {
      await deleteMcpServer(serverId);
      setServers((current) => current.filter((server) => server.id !== serverId));
      window.dispatchEvent(new CustomEvent(MCP_SERVERS_CHANGED_EVENT));
    } catch (deleteError) {
      console.error('[McpServersSettings] Failed to delete server:', deleteError);
      setError(deleteError instanceof Error ? deleteError.message : 'Failed to delete MCP server.');
    }
  };

  const handleDisconnectOAuth = async (server: McpServerResponse) => {
    if (disconnectingId) {
      return;
    }
    setDisconnectingId(server.id);
    setError(null);
    try {
      const updated = await disconnectMcpOAuth(server.id);
      setServers((current) => current.map((item) => (
        item.id === updated.id ? updated : item
      )));
      window.dispatchEvent(new CustomEvent(MCP_SERVERS_CHANGED_EVENT));
    } catch (disconnectError) {
      console.error('[McpServersSettings] Failed to disconnect OAuth:', disconnectError);
      setError(errText(disconnectError, 'Failed to disconnect MCP OAuth.'));
    } finally {
      setDisconnectingId(null);
    }
  };

  const upsertServer = (saved: McpServerResponse) => {
    setServers((current) => {
      if (current.some((server) => server.id === saved.id)) {
        return current.map((server) => (server.id === saved.id ? saved : server));
      }
      return [...current, saved];
    });
    window.dispatchEvent(new CustomEvent(MCP_SERVERS_CHANGED_EVENT));
  };

  const handleSubmit = async (formData: McpServerFormPayload): Promise<McpServerResponse> => {
    setError(null);
    try {
      let saved: McpServerResponse;
      if (editingServer) {
        saved = await updateMcpServer({
          id: editingServer.id,
          ...formData,
        });
      } else {
        saved = await createMcpServer(formData);
      }
      upsertServer(saved);
      return saved;
    } catch (submitError) {
      console.error('[McpServersSettings] Failed to save server:', submitError);
      throw submitError;
    }
  };

  const handleFormClose = () => {
    setIsFormOpen(false);
    setEditingServer(null);
    setSelectedCatalogEntry(null);
  };

  // When the catalog is empty there is only one thing to show, so force the
  // configured view regardless of the last-selected tab.
  const hasCatalog = catalog.length > 0;
  const effectiveSection: McpSection = hasCatalog ? activeSection : 'configured';
  const catalogTabRef = useRef<HTMLButtonElement>(null);
  const configuredTabRef = useRef<HTMLButtonElement>(null);
  // The configured section doubles as a tabpanel only when the sub-nav is
  // shown (i.e. the catalog is non-empty); otherwise it stands alone.
  const configuredPanelProps = hasCatalog
    ? {
        role: 'tabpanel' as const,
        id: 'mcp-panel-configured',
        'aria-labelledby': 'mcp-tab-configured',
        tabIndex: 0,
      }
    : {};

  const handleTabKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
      return;
    }
    event.preventDefault();
    const next: McpSection =
      event.key === 'Home'
        ? 'catalog'
        : event.key === 'End'
          ? 'configured'
          : effectiveSection === 'catalog'
            ? 'configured'
            : 'catalog';
    setActiveSection(next);
    (next === 'catalog' ? catalogTabRef : configuredTabRef).current?.focus();
  };

  const renderCatalogSection = () => (
    <section className={styles.section} role="tabpanel" id="mcp-panel-catalog" aria-labelledby="mcp-tab-catalog" tabIndex={0}>
      <div className={styles.sectionHeader}>
        <div>
          <h4 className={styles.sectionTitle}>Hosted OAuth Servers</h4>
          <p className={styles.sectionDescription}>Connect commonly used hosted MCP servers with the browser OAuth flow.</p>
        </div>
      </div>
      <div className={styles.catalogGrid}>
        {catalog.map((entry) => {
          const configured = configuredCatalogServer(entry, servers);
          const logoSrc = entry.logoAsset ? `/${entry.logoAsset}` : '';
          const showLogo = Boolean(logoSrc) && !failedLogos.has(entry.id);
          return (
            <div key={entry.id} className={styles.catalogCard}>
              <div className={styles.catalogLogo} aria-hidden="true">
                {showLogo ? (
                  <img
                    className={styles.catalogLogoImage}
                    src={logoSrc}
                    alt=""
                    onError={() => {
                      setFailedLogos((current) => {
                        const next = new Set(current);
                        next.add(entry.id);
                        return next;
                      });
                    }}
                  />
                ) : (
                  initials(entry.displayName)
                )}
              </div>
              <div className={styles.catalogMain}>
                <div className={styles.catalogNameRow}>
                  <span className={styles.catalogName}>{entry.displayName}</span>
                  {configured && (
                    <span className={`${styles.statusBadge} ${configured.enabled ? styles.enabled : styles.disabled}`}>
                      {configured.auth?.type === 'oauth' && configured.auth.connected ? 'Connected' : 'Configured'}
                    </span>
                  )}
                </div>
                <div className={styles.catalogCategory}>{entry.category}</div>
                <p className={styles.catalogDescription}>{entry.description}</p>
              </div>
              <button
                type="button"
                className={styles.catalogButton}
                onClick={() => handleConnectCatalogEntry(entry)}
              >
                <LinkIcon />
                <span>{configured ? 'Edit' : 'Connect'}</span>
              </button>
            </div>
          );
        })}
      </div>
    </section>
  );

  const renderConfiguredSection = () => (
    <section className={styles.section} {...configuredPanelProps}>
      <div className={styles.sectionHeader}>
        <div>
          <h4 className={styles.sectionTitle}>Configured Servers</h4>
          <p className={styles.sectionDescription}>Servers listed here can be selected in workspace and agent context.</p>
        </div>
        <span className={styles.count}>{servers.length}</span>
      </div>
      {servers.length === 0 ? (
        <div className={styles.emptyState}>
          No MCP servers configured yet.
        </div>
      ) : (
        <div className={styles.serverList}>
          {servers.map((server) => (
            <div key={server.id} className={styles.serverCard}>
              <div className={styles.serverMain}>
                <div className={styles.serverNameRow}>
                  <span className={styles.serverName}>{server.name}</span>
                  <span className={`${styles.statusBadge} ${server.enabled ? styles.enabled : styles.disabled}`}>
                    {server.enabled ? 'Enabled' : 'Disabled'}
                  </span>
                  {server.auth?.type === 'oauth' && (
                    <span className={`${styles.statusBadge} ${server.auth.connected ? styles.connected : styles.needsLogin}`}>
                      {server.auth.connected ? 'OAuth' : 'Reconnect'}
                    </span>
                  )}
                </div>
                <div className={styles.serverMeta}>
                  {server.transport?.type === 'http' ? 'HTTP' : 'Stdio'} · {authSummary(server.auth)}
                </div>
                <div className={styles.serverTransport}>{transportSummary(server.transport)}</div>
              </div>
              <div className={styles.serverActions}>
                <button className={styles.actionButton} onClick={() => handleEdit(server)}>
                  Edit
                </button>
                {server.auth?.type === 'oauth' && (
                  <button
                    className={styles.actionButton}
                    onClick={() => handleDisconnectOAuth(server)}
                    disabled={disconnectingId === server.id}
                  >
                    {disconnectingId === server.id ? 'Disconnecting...' : 'Disconnect'}
                  </button>
                )}
                <button
                  className={`${styles.actionButton} ${styles.deleteButton}`}
                  onClick={() => handleDelete(server.id)}
                >
                  Delete
                </button>
              </div>
            </div>
          ))}
        </div>
      )}
    </section>
  );

  return (
    <div className={styles.container}>
      <div className={styles.header}>
        <div className={styles.headerText}>
          <h3 className={styles.title}>MCP Servers</h3>
          <p className={styles.description}>
            Register local or remote MCP servers once, then assign them explicitly to the agents that should be allowed to use them.
          </p>
        </div>
        <button className={styles.addButton} onClick={handleCreate}>
          <PlusIcon />
          <span>Add Custom</span>
        </button>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      {loading ? (
        <div className={styles.loadingState}>Loading MCP servers...</div>
      ) : (
        <>
          {hasCatalog && (
            <div className={styles.subNav} role="tablist" aria-label="MCP server sections">
              <button
                type="button"
                role="tab"
                id="mcp-tab-catalog"
                aria-controls="mcp-panel-catalog"
                tabIndex={effectiveSection === 'catalog' ? 0 : -1}
                ref={catalogTabRef}
                onKeyDown={handleTabKeyDown}
                aria-selected={effectiveSection === 'catalog'}
                className={`${styles.subNavItem} ${effectiveSection === 'catalog' ? styles.subNavItemActive : ''}`}
                onClick={() => setActiveSection('catalog')}
              >
                <span>Catalog</span>
                <span className={styles.subNavCount}>{catalog.length}</span>
              </button>
              <button
                type="button"
                role="tab"
                id="mcp-tab-configured"
                aria-controls="mcp-panel-configured"
                tabIndex={effectiveSection === 'configured' ? 0 : -1}
                ref={configuredTabRef}
                onKeyDown={handleTabKeyDown}
                aria-selected={effectiveSection === 'configured'}
                className={`${styles.subNavItem} ${effectiveSection === 'configured' ? styles.subNavItemActive : ''}`}
                onClick={() => setActiveSection('configured')}
              >
                <span>Configured</span>
                <span className={styles.subNavCount}>{servers.length}</span>
              </button>
            </div>
          )}

          {effectiveSection === 'catalog' ? renderCatalogSection() : renderConfiguredSection()}
        </>
      )}

      <div className={styles.hint}>
        <p>
          OAuth credentials and bearer tokens are stored in the OS keyring. Config files keep only server definitions and secret references.
        </p>
      </div>

      <McpServerFormModal
        isOpen={isFormOpen}
        onClose={handleFormClose}
        onSubmit={handleSubmit}
        onServerSaved={upsertServer}
        server={editingServer}
        catalogEntry={selectedCatalogEntry}
      />
    </div>
  );
};

export default McpServersSettings;
