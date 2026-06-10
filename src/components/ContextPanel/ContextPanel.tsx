import React, { useMemo, useState, useEffect } from 'react';
import ReactDOM from 'react-dom';
import { useTabManager } from '../../contexts/TabManagerContext';
import { getMcpServers } from '../../api/client';
import { assistantClient } from '../../assistant';
import type { McpServerResponse, ProviderConnection } from '../../generated/bindings';
import ContextBadge from './ContextBadge';
import McpServerAvatar from './McpServerAvatar';
import McpServerSelector from './McpServerSelector';
import ProviderSetupBadge from './ProviderSetupBadge';
import styles from './ContextPanel.module.css';

const MCP_SERVERS_CHANGED_EVENT = 'mcp-servers-changed';
const CONNECTIONS_CHANGED_EVENT = 'assistant-provider-connections-changed';

// Loose shapes for the still-untyped TabManagerContext. Tightened when that
// context is converted under P2-1.
interface TabMcpServers {
  attachedServerIds?: string[];
  selectedServerIds?: string[];
  disabledServerIds?: string[];
}
interface TabContext {
  customContext?: Record<string, unknown>;
  mcpServers?: TabMcpServers;
  agent?: { agentId?: string | null } | null;
  assistantConnectionId?: string | null;
}
interface Tab {
  id: string;
  context?: TabContext | null;
}
interface TabManager {
  getActiveTab: () => Tab | null;
  updateTabContext: (tabId: string, patch: Partial<TabContext>) => void;
}

const getAttachedMcpServerIds = (context?: TabContext | null): string[] =>
  context?.mcpServers?.attachedServerIds || context?.mcpServers?.selectedServerIds || [];

const getDisabledMcpServerIds = (context?: TabContext | null): string[] =>
  context?.mcpServers?.disabledServerIds || [];

const getEnabledMcpServerIds = (context?: TabContext | null): string[] => {
  const disabled = new Set(getDisabledMcpServerIds(context));
  return getAttachedMcpServerIds(context).filter((id) => !disabled.has(id));
};

const ContextPanel = () => {
  const { getActiveTab, updateTabContext } = useTabManager() as TabManager;
  const [showMcpSelector, setShowMcpSelector] = useState(false);
  const [availableMcpServers, setAvailableMcpServers] = useState<McpServerResponse[]>([]);
  const [providerConnections, setProviderConnections] = useState<ProviderConnection[]>([]);
  // Distinguishes "no providers configured" from "providers not loaded yet",
  // so the setup call-to-action doesn't flash on every startup.
  const [connectionsLoaded, setConnectionsLoaded] = useState(false);

  const activeTab = getActiveTab();
  const tabContext = activeTab?.context;
  const customContext = tabContext?.customContext || {};
  const attachedMcpServerIds = getAttachedMcpServerIds(tabContext);
  const disabledMcpServerIds = getDisabledMcpServerIds(tabContext);
  const enabledMcpServerIds = getEnabledMcpServerIds(tabContext);
  const isAgentManagedTab = Boolean(tabContext?.agent?.agentId);
  const enabledProviderConnections = useMemo(
    () => providerConnections.filter((connection) => connection.enabled),
    [providerConnections]
  );
  const selectedAssistantConnectionId = tabContext?.assistantConnectionId || enabledProviderConnections[0]?.id || '';

  useEffect(() => {
    let cancelled = false;

    const loadContextData = async () => {
      try {
        const [servers, connections] = await Promise.all([
          getMcpServers() as Promise<McpServerResponse[]>,
          assistantClient.listProviderConnections().catch(() => [] as ProviderConnection[]),
        ]);
        if (!cancelled) {
          setAvailableMcpServers(servers || []);
          setProviderConnections(connections || []);
          setConnectionsLoaded(true);
        }
      } catch {
        if (!cancelled) {
          setAvailableMcpServers([]);
          setProviderConnections([]);
          setConnectionsLoaded(true);
        }
      }
    };

    loadContextData();
    window.addEventListener(MCP_SERVERS_CHANGED_EVENT, loadContextData);
    window.addEventListener(CONNECTIONS_CHANGED_EVENT, loadContextData);

    return () => {
      cancelled = true;
      window.removeEventListener(MCP_SERVERS_CHANGED_EVENT, loadContextData);
      window.removeEventListener(CONNECTIONS_CHANGED_EVENT, loadContextData);
    };
  }, []);

  const configuredMcpServers = useMemo(
    () => availableMcpServers.filter((server) => server.enabled),
    [availableMcpServers]
  );

  // `attachedMcpServers` is intentionally derived inline rather than
  // wrapped in `useMemo`. The work is O(n*m) over the user's attached
  // MCP server IDs (typically 0-5) and the catalog of available
  // servers (typically 0-30) — a few hundred operations per render at
  // most — and there are no memoized downstream consumers, so the
  // memoization would only add bookkeeping without a runtime win.
  // Wrapping it in `useMemo` also triggers
  // `react-hooks/preserve-manual-memoization` because the compiler
  // treats `attachedMcpServerIds` (the return of a helper call) as a
  // "may be mutated later" value it cannot statically reason about.
  const attachedMcpServers = attachedMcpServerIds
    .map((id) => availableMcpServers.find((server) => server.id === id))
    .filter((server): server is McpServerResponse => Boolean(server));

  const hasCustomContext = Object.keys(customContext).length > 0;
  const hasMcpContext = attachedMcpServerIds.length > 0 || configuredMcpServers.length > 0;
  const hasAssistantContext = !isAgentManagedTab && enabledProviderConnections.length > 0;
  // First-run state: nothing to select. Show the setup call-to-action where
  // the picker would be, so a fresh install has an obvious next step.
  const needsProvider = !isAgentManagedTab && connectionsLoaded && enabledProviderConnections.length === 0;

  useEffect(() => {
    if (!activeTab || isAgentManagedTab || enabledProviderConnections.length === 0) {
      return;
    }

    if (!tabContext?.assistantConnectionId) {
      updateTabContext(activeTab.id, {
        assistantConnectionId: enabledProviderConnections[0]!.id,
      });
    }
  }, [activeTab, enabledProviderConnections, isAgentManagedTab, tabContext?.assistantConnectionId, updateTabContext]);

  // `handleAddMcpServer` / `handleRemoveMcpServer` / `handleToggleMcpServer`
  // are intentionally plain (non-memoized) functions. The React Compiler
  // treats manual `useCallback` deps that capture values from a function
  // call (e.g. `getActiveTab()`, the helper-returned `attachedMcpServerIds`)
  // as "may be mutated later" and refuses to compile the surrounding
  // `useMemo`/`useCallback`. The callbacks have no memoized consumers
  // (McpServerSelector is not wrapped in React.memo), so dropping the
  // manual memoization is both safe at runtime and the canonical fix
  // recommended by the React Compiler docs.
  const handleAddMcpServer = (serverId: string) => {
    if (!activeTab || isAgentManagedTab) return;

    const nextAttachedIds = attachedMcpServerIds.includes(serverId)
      ? attachedMcpServerIds
      : [...attachedMcpServerIds, serverId];

    updateTabContext(activeTab.id, {
      mcpServers: {
        attachedServerIds: nextAttachedIds,
        disabledServerIds: disabledMcpServerIds.filter((id) => id !== serverId),
      },
    });
  };

  const handleRemoveMcpServer = (serverId: string) => {
    if (!activeTab || isAgentManagedTab) return;

    updateTabContext(activeTab.id, {
      mcpServers: {
        attachedServerIds: attachedMcpServerIds.filter((id) => id !== serverId),
        disabledServerIds: disabledMcpServerIds.filter((id) => id !== serverId),
      },
    });
  };

  const handleToggleMcpServer = (serverId: string) => {
    if (!activeTab || isAgentManagedTab || !attachedMcpServerIds.includes(serverId)) return;

    const isDisabled = disabledMcpServerIds.includes(serverId);
    const nextDisabledIds = isDisabled
      ? disabledMcpServerIds.filter((id) => id !== serverId)
      : [...disabledMcpServerIds, serverId];

    updateTabContext(activeTab.id, {
      mcpServers: {
        attachedServerIds: attachedMcpServerIds,
        disabledServerIds: nextDisabledIds,
      },
    });
  };

  if (!hasCustomContext && !hasMcpContext && !hasAssistantContext && !needsProvider) {
    return null;
  }

  return (
    <>
      <div className={styles.contextPanel}>
        <div className={styles.contextContainer}>
          {hasAssistantContext && (
            <label className={styles.providerPicker}>
              <svg className={styles.providerIcon} width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 2L2 7l10 5 10-5-10-5z" />
                <path d="M2 17l10 5 10-5" />
                <path d="M2 12l10 5 10-5" />
              </svg>
              <select
                className={styles.providerSelect}
                value={selectedAssistantConnectionId}
                onChange={(event) => {
                  if (!activeTab) return;
                  updateTabContext(activeTab.id, {
                    assistantConnectionId: event.target.value,
                  });
                }}
              >
                {enabledProviderConnections.map((connection) => (
                  <option key={connection.id} value={connection.id}>
                    {connection.name} — {connection.modelId}
                  </option>
                ))}
              </select>
              <svg className={styles.providerChevron} width="8" height="8" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <polyline points="6 9 12 15 18 9" />
              </svg>
            </label>
          )}

          {needsProvider && <ProviderSetupBadge />}

          {hasMcpContext && !isAgentManagedTab && (
            <ContextBadge
              type="mcp"
              label="Add MCP"
              value="Add MCP"
              variant="add"
              onClick={() => configuredMcpServers.length > 0 && setShowMcpSelector(true)}
              clickable={configuredMcpServers.length > 0}
            />
          )}

          {attachedMcpServers.map((server) => {
            const isDisabled = disabledMcpServerIds.includes(server.id);
            const isEnabled = enabledMcpServerIds.includes(server.id);

            return (
              <ContextBadge
                key={server.id}
                type="mcp"
                label={server.name}
                value={server.name}
                variant={isDisabled ? 'disabled' : undefined}
                iconElement={<McpServerAvatar server={server} disabled={isDisabled} />}
                onClick={() => handleToggleMcpServer(server.id)}
                clickable={!isAgentManagedTab}
                titleOverride={
                  isAgentManagedTab
                    ? `${server.name}: managed by scheduled agent`
                    : `${server.name}: ${isEnabled ? 'click to disable for this tab' : 'click to enable for this tab'}`
                }
              />
            );
          })}

          {hasCustomContext &&
            Object.entries(customContext).map(([key, value]) => (
              <ContextBadge
                key={key}
                type="custom"
                label={key}
                value={String(value)}
              />
            ))}
        </div>
      </div>

      {showMcpSelector && ReactDOM.createPortal(
        <McpServerSelector
          servers={configuredMcpServers}
          attachedIds={attachedMcpServerIds}
          disabledIds={disabledMcpServerIds}
          onAdd={handleAddMcpServer}
          onRemove={handleRemoveMcpServer}
          onClose={() => setShowMcpSelector(false)}
        />,
        document.body
      )}
    </>
  );
};

export default ContextPanel;
