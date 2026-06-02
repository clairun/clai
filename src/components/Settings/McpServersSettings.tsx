import React, { useEffect, useState } from 'react';
import {
  createMcpServer,
  deleteMcpServer,
  getMcpServers,
  updateMcpServer,
} from '../../api/client';
import McpServerFormModal from './McpServerFormModal';
import type {
  McpServerAuthResponse,
  McpServerResponse,
  McpServerTransport,
} from '../../generated/bindings';
import styles from './McpServersSettings.module.css';

const MCP_SERVERS_CHANGED_EVENT = 'mcp-servers-changed';

const PlusIcon = () => (
  <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
    <line x1="12" y1="5" x2="12" y2="19" />
    <line x1="5" y1="12" x2="19" y2="12" />
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
  return 'No auth';
};

const McpServersSettings = () => {
  const [servers, setServers] = useState<McpServerResponse[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [isFormOpen, setIsFormOpen] = useState(false);
  const [editingServer, setEditingServer] = useState<McpServerResponse | null>(null);

  useEffect(() => {
    loadServers();
  }, []);

  const loadServers = async () => {
    setLoading(true);
    setError(null);

    try {
      const result = await getMcpServers();
      setServers(result || []);
    } catch (loadError) {
      console.error('[McpServersSettings] Failed to load servers:', loadError);
      setError('Failed to load MCP servers. Please try again.');
    } finally {
      setLoading(false);
    }
  };

  const handleCreate = () => {
    setEditingServer(null);
    setIsFormOpen(true);
  };

  const handleEdit = (server: McpServerResponse) => {
    setEditingServer(server);
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

  const handleSubmit = async (formData: Record<string, unknown>) => {
    setError(null);
    try {
      if (editingServer) {
        const updated = await updateMcpServer({
          id: editingServer.id,
          ...formData,
        });
        setServers((current) => current.map((server) => (
          server.id === updated.id ? updated : server
        )));
      } else {
        const created = await createMcpServer(formData);
        setServers((current) => [...current, created]);
      }
      window.dispatchEvent(new CustomEvent(MCP_SERVERS_CHANGED_EVENT));
      setIsFormOpen(false);
      setEditingServer(null);
    } catch (submitError) {
      console.error('[McpServersSettings] Failed to save server:', submitError);
      throw submitError;
    }
  };

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
          <span>Add Server</span>
        </button>
      </div>

      {error && <div className={styles.errorBanner}>{error}</div>}

      {loading ? (
        <div className={styles.loadingState}>Loading MCP servers...</div>
      ) : servers.length === 0 ? (
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

      <div className={styles.hint}>
        <p>
          This slice stores MCP server definitions and agent assignments. External tool discovery and execution are wired into the backend foundation, but transport-level calls are still a follow-up step.
        </p>
      </div>

      <McpServerFormModal
        isOpen={isFormOpen}
        onClose={() => {
          setIsFormOpen(false);
          setEditingServer(null);
        }}
        onSubmit={handleSubmit}
        server={editingServer}
      />
    </div>
  );
};

export default McpServersSettings;
