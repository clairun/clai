/**
 * TerminalEmulatorWrapper
 *
 * Wraps the TerminalEmulator and provides it with access to the active tab's context.
 * Routes free-text terminal prompts into the assistant engine.
 */

import React, { useCallback } from 'react';
import { useLocation } from 'react-router-dom';
import { useTabManager } from '../../contexts/TabManagerContext';
import { TabContextProvider } from '../../contexts/TabContext';
import { useChatManager } from '../../contexts/ChatManagerContext';
import { useFleet } from '../../contexts/FleetContext';
import { useAssistantSession, useAssistantStore, assistantClient } from '../../assistant';
import { getOrCreateWorkspaceSession } from '../../workspace/client';
import TerminalEmulator from './TerminalEmulator';

const getEnabledMcpServerIds = (tab) => {
  const attached = tab.context?.mcpServers?.attachedServerIds || tab.context?.mcpServers?.selectedServerIds || [];
  const disabled = new Set(tab.context?.mcpServers?.disabledServerIds || []);
  return attached.filter((id) => !disabled.has(id));
};

const TerminalEmulatorWrapper = () => {
  const location = useLocation();
  const { tabs, activeTabId, updateTabContext } = useTabManager();
  const { openChat } = useChatManager();
  const { isFleetRoute, selectedAgent, refresh: refreshFleet } = useFleet();
  const { ensureSession } = useAssistantSession(activeTabId);
  const workspaceRouteMatch = location.pathname.match(/^\/workspace(?:\/([^/]+))?\/?$/);
  const isWorkspaceRoute = Boolean(workspaceRouteMatch);
  const currentWorkspaceId = workspaceRouteMatch?.[1]
    ? decodeURIComponent(workspaceRouteMatch[1])
    : 'default';

  // Get active tab
  const activeTab = tabs.find(t => t.id === activeTabId);

  // Handle context changes from the terminal
  const handleContextChange = (context) => {
    if (activeTab) {
      updateTabContext(activeTab.id, context);
    }
  };

  /**
   * Check if a provider session is configured.
   * Always fetches fresh — the invoke is fast and the user may have
   * connected/disconnected a provider since the last check.
   */
  const getEnabledProviderConnections = useCallback(async () => {
    try {
      const sessions = await assistantClient.listProviderConnections();
      return (sessions || []).filter((connection) => connection.enabled);
    } catch {
      return [];
    }
  }, []);

  const resolveTabConnectionId = useCallback(async (tab) => {
    if (!tab) {
      return null;
    }

    // Legacy agent-derived tabs no longer exist; fall straight through to the
    // generic provider-connection picker.
    const connections = await getEnabledProviderConnections();
    if (connections.length === 0) {
      return null;
    }

    const existingId = tab.context?.assistantConnectionId;
    const selected = connections.find((connection) => connection.id === existingId) || connections[0];
    if (selected.id !== existingId) {
      updateTabContext(tab.id, { assistantConnectionId: selected.id });
    }
    return selected.id;
  }, [getEnabledProviderConnections, updateTabContext]);

  /**
   * Handle sending a query through the assistant engine.
   */
  const handleSendToAgent = useCallback(
    async (query) => {
      if (location.pathname === '/fleet' && isFleetRoute) {
        if (!selectedAgent) {
          return {
            error:
              'Select a workspace in Fleet to chat with its default agent. (If the workspace has no default agent set, open it from the card and configure one.)',
          };
        }

        const connectionId = selectedAgent.providerConnectionIds?.[0] || null;
        if (!connectionId) {
          return {
            error: 'Select a provider connection for this agent in Settings before sending prompts.',
          };
        }

        try {
          let sessionId = selectedAgent.sessionId;

          if (!sessionId) {
            const createdSession = await assistantClient.createSession({
              tabId: selectedAgent.tabId || null,
              kind: 'background_job',
              title: selectedAgent.name,
              context: {
                // `workspaceId` opens the per-workspace DB on the backend;
                // `automationId` / `agentWorkspaceId` identify the agent
                // within it. These are distinct ids — keep them straight.
                workspaceId: selectedAgent.workspaceId,
                tabId: selectedAgent.tabId || null,
                mcpServerIds: selectedAgent.selectedMcpServerIds || [],
                execution: selectedAgent.execution || undefined,
                automationId: selectedAgent.agentId,
                agentWorkspaceId: selectedAgent.workspaceId,
                automationName: selectedAgent.name,
                automationDescription: selectedAgent.description || null,
              },
            });
            sessionId = createdSession.id;
            useAssistantStore.getState().initSession(createdSession);
            await refreshFleet().catch(() => {});
          }

          const result = await assistantClient.sendMessage(sessionId, query, connectionId);
          const store = useAssistantStore.getState();
          store.addMessage(sessionId, result.message);
          return {};
        } catch (err) {
          console.error('[TerminalEmulatorWrapper] Fleet assistant error:', err);
          return {
            error: typeof err === 'string' ? err : (err?.message || 'Assistant request failed.'),
          };
        }
      }

      if (isWorkspaceRoute) {
        try {
          const binding = await getOrCreateWorkspaceSession(currentWorkspaceId);
          const connectionId = binding.providerConnectionId;
          if (!connectionId) {
            return {
              error: 'Add an enabled assistant provider connection before sending prompts from this workspace.',
            };
          }

          const store = useAssistantStore.getState();
          store.initSession(binding.session);

          // Bridge workspace session to the chat panel via a synthetic tab key
          const workspaceTabKey = `workspace:${currentWorkspaceId}`;
          store.setActiveSessionForTab(workspaceTabKey, binding.session.id);

          // Only open the side panel for agent workspaces — general workspaces
          // embed the chat directly in the page.
          if (binding.session.kind === 'background_job') {
            openChat();
          }

          const result = await assistantClient.sendMessage(binding.session.id, query, connectionId);
          store.addMessage(binding.session.id, result.message);
          return {};
        } catch (err) {
          console.error('[TerminalEmulatorWrapper] Workspace assistant error:', err);
          return {
            error: typeof err === 'string' ? err : (err?.message || 'Assistant request failed.'),
          };
        }
      }

      if (!activeTab) {
        console.warn('[TerminalEmulatorWrapper] No active tab');
        return { error: 'No active tab available.' };
      }

      const connectionId = await resolveTabConnectionId(activeTab);
      if (!connectionId) {
        openChat();
        return {
          error: 'Add an enabled assistant provider connection in Settings before sending prompts.',
        };
      }

      openChat();

      try {
        const mcpServerIds = getEnabledMcpServerIds(activeTab);
        const sessionId = await ensureSession({ mcpServerIds });
        const result = await assistantClient.sendMessage(sessionId, query, connectionId);
        const store = useAssistantStore.getState();
        store.addMessage(sessionId, result.message);
        return {};
      } catch (err) {
        console.error('[TerminalEmulatorWrapper] Assistant error:', err);
        return {
          error: typeof err === 'string' ? err : (err?.message || 'Assistant request failed.'),
        };
      }
    },
    [
      activeTab,
      ensureSession,
      resolveTabConnectionId,
      isFleetRoute,
      location.pathname,
      openChat,
      refreshFleet,
      selectedAgent,
      currentWorkspaceId,
      isWorkspaceRoute,
    ]
  );

  // If no active tab, render terminal without context
  if (!activeTab) {
    return <TerminalEmulator onSendToChat={handleSendToAgent} />;
  }

  // Wrap terminal with the active tab's context
  return (
    <TabContextProvider
      tabId={activeTab.id}
      initialContext={activeTab.context}
      onContextChange={handleContextChange}
    >
      <TerminalEmulator onSendToChat={handleSendToAgent} />
    </TabContextProvider>
  );
};

export default TerminalEmulatorWrapper;
