import React, { memo, useCallback, useEffect, useMemo, useState } from 'react';
import ReactDOM from 'react-dom';
import { getMcpServers } from '../../api/client';
import { assistantClient } from '../../assistant';
import ContextBadge from '../../components/ContextPanel/ContextBadge';
import McpServerAvatar from '../../components/ContextPanel/McpServerAvatar';
import McpServerSelector from '../../components/ContextPanel/McpServerSelector';
import { getWorkspaceSnapshot, updateWorkspaceSessionMcp, setWorkspaceProvider } from '../client';
import styles from './WorkspaceContextBar.module.css';

const MCP_SERVERS_CHANGED_EVENT = 'mcp-servers-changed';
const CONNECTIONS_CHANGED_EVENT = 'assistant-provider-connections-changed';
const SNAPSHOT_OPTIONS = {
  includeSessionPayload: false,
  includeFiles: false,
};

/**
 * WorkspaceContextBar — shows MCP server badges and provider info for workspaces.
 *
 * Self-loading: only needs workspaceId, fetches its own data from the snapshot API.
 *
 * Agent workspaces: read-only display (MCP configured via agent settings in Fleet).
 * General workspace: editable — user can add/remove/toggle MCP servers.
 */
const WorkspaceContextBar = memo(({ workspaceId }) => {
  const [showMcpSelector, setShowMcpSelector] = useState(false);
  const [availableMcpServers, setAvailableMcpServers] = useState([]);
  const [providerConnections, setProviderConnections] = useState([]);
  const [localMcpServerIds, setLocalMcpServerIds] = useState([]);
  const [localDisabledIds, setLocalDisabledIds] = useState([]);
  const [isAgent, setIsAgent] = useState(false);
  const [agentMcpServerIds, setAgentMcpServerIds] = useState([]);
  const [selectedProviderId, setSelectedProviderId] = useState('');

  // Load workspace snapshot to determine type and MCP config
  useEffect(() => {
    if (!workspaceId) return;
    let cancelled = false;

    const loadSnapshot = async () => {
      try {
        const snap = await getWorkspaceSnapshot(workspaceId, SNAPSHOT_OPTIONS);
        if (cancelled) return;
        const agent = snap?.kind === 'agent';
        setIsAgent(agent);
        if (agent) {
          setAgentMcpServerIds(snap?.selectedMcpServerIds || []);
        } else {
          setLocalMcpServerIds(snap?.session?.context?.mcpServerIds || []);
        }
        // Pick up the preferred provider from the snapshot binding
        if (!agent && snap?.providerConnectionIds?.length > 0) {
          setSelectedProviderId((prev) => prev || snap.providerConnectionIds[0]);
        }
      } catch {
        // Snapshot not available yet — fine
      }
    };

    loadSnapshot();
    return () => { cancelled = true; };
  }, [workspaceId]);

  // Load available MCP servers and provider connections
  useEffect(() => {
    let cancelled = false;

    const load = async () => {
      try {
        const [servers, connections] = await Promise.all([
          getMcpServers(),
          assistantClient.listProviderConnections().catch(() => []),
        ]);
        if (!cancelled) {
          setAvailableMcpServers(servers || []);
          setProviderConnections(connections || []);
        }
      } catch {
        if (!cancelled) {
          setAvailableMcpServers([]);
          setProviderConnections([]);
        }
      }
    };

    load();
    window.addEventListener(MCP_SERVERS_CHANGED_EVENT, load);
    window.addEventListener(CONNECTIONS_CHANGED_EVENT, load);

    return () => {
      cancelled = true;
      window.removeEventListener(MCP_SERVERS_CHANGED_EVENT, load);
      window.removeEventListener(CONNECTIONS_CHANGED_EVENT, load);
    };
  }, []);

  const configuredMcpServers = useMemo(
    () => availableMcpServers.filter((s) => s.enabled),
    [availableMcpServers]
  );

  const displayMcpServerIds = isAgent ? agentMcpServerIds : localMcpServerIds;

  const displayMcpServers = useMemo(
    () => displayMcpServerIds
      .map((id) => availableMcpServers.find((s) => s.id === id))
      .filter(Boolean),
    [displayMcpServerIds, availableMcpServers]
  );

  const persistMcpChange = useCallback(
    async (nextIds) => {
      setLocalMcpServerIds(nextIds);
      try {
        await updateWorkspaceSessionMcp(workspaceId, nextIds);
      } catch (err) {
        console.error('[WorkspaceContextBar] Failed to persist MCP change:', err);
      }
    },
    [workspaceId]
  );

  const handleAddMcp = useCallback(
    (serverId) => {
      if (isAgent) return;
      const nextIds = localMcpServerIds.includes(serverId)
        ? localMcpServerIds
        : [...localMcpServerIds, serverId];
      persistMcpChange(nextIds);
    },
    [isAgent, localMcpServerIds, persistMcpChange]
  );

  const handleRemoveMcp = useCallback(
    (serverId) => {
      if (isAgent) return;
      persistMcpChange(localMcpServerIds.filter((id) => id !== serverId));
    },
    [isAgent, localMcpServerIds, persistMcpChange]
  );

  const handleToggleMcp = useCallback(
    (serverId) => {
      if (isAgent) return;
      const isDisabled = localDisabledIds.includes(serverId);
      setLocalDisabledIds(
        isDisabled
          ? localDisabledIds.filter((id) => id !== serverId)
          : [...localDisabledIds, serverId]
      );
    },
    [isAgent, localDisabledIds]
  );

  const enabledProviders = useMemo(
    () => providerConnections.filter((c) => c.enabled),
    [providerConnections]
  );

  const handleProviderChange = useCallback(
    async (e) => {
      const id = e.target.value;
      setSelectedProviderId(id);
      try {
        await setWorkspaceProvider(workspaceId, id);
      } catch (err) {
        console.error('[WorkspaceContextBar] Failed to set provider:', err);
      }
    },
    [workspaceId]
  );

  const hasConfiguredServers = configuredMcpServers.length > 0;
  const hasProviders = !isAgent && enabledProviders.length > 0;
  const hasBadges = displayMcpServers.length > 0 || (!isAgent && hasConfiguredServers) || hasProviders;

  if (!hasBadges) return null;

  return (
    <>
      <div className={styles.contextBar}>
        {hasProviders && (
          <label className={styles.providerPicker}>
            <svg className={styles.providerIcon} width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 2L2 7l10 5 10-5-10-5z" />
              <path d="M2 17l10 5 10-5" />
              <path d="M2 12l10 5 10-5" />
            </svg>
            <select
              className={styles.providerSelect}
              value={selectedProviderId}
              onChange={handleProviderChange}
            >
              {enabledProviders.map((conn) => (
                <option key={conn.id} value={conn.id}>
                  {conn.name} — {conn.modelId}
                </option>
              ))}
            </select>
            <svg className={styles.providerChevron} width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="6 9 12 15 18 9" />
            </svg>
          </label>
        )}

        {displayMcpServers.map((server) => {
          const isDisabled = localDisabledIds.includes(server.id);
          return (
            <ContextBadge
              key={server.id}
              type="mcp"
              label={server.name}
              value={server.name}
              variant={isDisabled ? 'disabled' : undefined}
              iconElement={<McpServerAvatar server={server} disabled={isDisabled} />}
              onClick={isAgent ? undefined : () => handleToggleMcp(server.id)}
              clickable={!isAgent}
              titleOverride={
                isAgent
                  ? `${server.name}: configured in agent settings`
                  : `${server.name}: click to ${isDisabled ? 'enable' : 'disable'}`
              }
            />
          );
        })}

        {!isAgent && hasConfiguredServers && (
          <ContextBadge
            type="mcp"
            label="Add MCP"
            value="Add MCP"
            variant="add"
            onClick={() => setShowMcpSelector(true)}
            clickable={true}
          />
        )}
      </div>

      {showMcpSelector && ReactDOM.createPortal(
        <McpServerSelector
          servers={configuredMcpServers}
          attachedIds={localMcpServerIds}
          disabledIds={localDisabledIds}
          onAdd={handleAddMcp}
          onRemove={handleRemoveMcp}
          onClose={() => setShowMcpSelector(false)}
        />,
        document.body
      )}
    </>
  );
});

WorkspaceContextBar.displayName = 'WorkspaceContextBar';

export default WorkspaceContextBar;
