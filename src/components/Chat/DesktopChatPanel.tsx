import React, { useEffect } from 'react';
import { useLocation } from 'react-router-dom';
import { useChatManager } from '../../contexts/ChatManagerContext';
import { useTabManager } from '../../contexts/TabManagerContext';
import { useAssistantStore, assistantClient } from '../../assistant';
import AssistantChat from '../AssistantChat/AssistantChat';
import styles from './DesktopChatPanel.module.css';

// Loose shapes for the still-untyped tab/context plumbing. Tightened when
// TabManagerContext is converted under P2-1.
interface McpServerSelection {
  attachedServerIds?: string[];
  selectedServerIds?: string[];
  disabledServerIds?: string[];
}
interface TabContext {
  mcpServers?: McpServerSelection;
}
interface Tab {
  id: string;
  context?: TabContext;
}

const normalizeIdList = (ids?: string[] | null): string[] => [...(ids || [])].sort();
const getEnabledMcpServerIds = (context?: TabContext | null): string[] => {
  const attached =
    context?.mcpServers?.attachedServerIds || context?.mcpServers?.selectedServerIds || [];
  const disabled = new Set(context?.mcpServers?.disabledServerIds || []);
  return attached.filter((id) => !disabled.has(id));
};

/**
 * DesktopChatPanel - Chat panel container
 *
 * This component provides a full-height, fixed-position panel
 * that appears on the right side of the screen.
 *
 * On tab routes: renders AssistantChat for the active tab.
 * On workspace routes: renders AssistantChat for the workspace session
 * (bridged via a synthetic "workspace:{id}" tab key).
 */
const DesktopChatPanel = () => {
  const location = useLocation();
  const { isCurrentChatOpen } = useChatManager() as {
    isCurrentChatOpen: () => boolean;
  };
  const { activeTabId, tabs } = useTabManager() as {
    activeTabId: string | null;
    tabs: Tab[];
  };

  const workspaceRouteMatch = location.pathname.match(/^\/workspace(?:\/([^/]+))?\/?$/);
  const isWorkspaceRoute = Boolean(workspaceRouteMatch);
  const workspaceId = workspaceRouteMatch?.[1]
    ? decodeURIComponent(workspaceRouteMatch[1])
    : 'default';

  // On workspace routes, use a synthetic tab key; otherwise use the real active tab.
  const effectiveTabId = isWorkspaceRoute
    ? `workspace:${workspaceId}`
    : activeTabId;

  const assistantSessionId = useAssistantStore(
    (state) => (effectiveTabId ? state.activeSessionByTab[effectiveTabId] : undefined)
  );
  const sessionKind = useAssistantStore(
    (state) => assistantSessionId ? state.sessions[assistantSessionId]?.session?.kind : null
  );
  const isOpen = isCurrentChatOpen();
  const activeTab = tabs.find((tab) => tab.id === activeTabId);

  // General workspaces embed chat in the page — don't show the side panel.
  // Agent workspaces (background_job) use the side panel for chat. The
  // auto-close happens after the hooks below to keep hook order stable.
  const isGeneralWorkspace = isWorkspaceRoute && sessionKind && sessionKind !== 'background_job';

  // Restore existing assistant session from DB on tab change or when the
  // panel is opened after a background run attached a session later.
  useEffect(() => {
    // Skip restore on workspace routes — the terminal wrapper handles session binding.
    if (isWorkspaceRoute) return;
    if (!activeTabId || assistantSessionId || !isOpen) return;

    let cancelled = false;

    const restore = async () => {
      try {
        const sessions = await assistantClient.listSessions(activeTabId);
        if (cancelled || sessions.length === 0) return;

        const contextMcpServerIds = normalizeIdList(getEnabledMcpServerIds(activeTab?.context));

        const session =
          sessions.find((candidate) =>
            candidate &&
            JSON.stringify(normalizeIdList(candidate.context?.mcpServerIds || [])) ===
              JSON.stringify(contextMcpServerIds)
          ) || sessions[0];
        const [messages, runs, toolCalls] = await Promise.all([
          assistantClient.loadSessionMessages(session.id),
          assistantClient.listRuns(session.id),
          assistantClient.listToolCalls(session.id),
        ]);
        if (cancelled) return;

        const store = useAssistantStore.getState();
        const currentActiveSessionId = store.getActiveSessionForTab(activeTabId);
        if (currentActiveSessionId && currentActiveSessionId !== session.id) {
          return;
        }

        store.loadSessionData(session.id, session, messages, runs, toolCalls);
        store.setActiveSessionForTab(activeTabId, session.id);
      } catch {
        // No session exists yet — that's fine
      }
    };
    restore();

    return () => {
      cancelled = true;
    };
  }, [activeTab, activeTabId, assistantSessionId, isOpen, isWorkspaceRoute]);

  if (isGeneralWorkspace && isOpen) {
    return null;
  }

  return (
    <div
      id="desktop-chat-panel"
      className={`${styles.desktopChatPanel} ${isOpen ? styles.open : ''}`}
      role="complementary"
      aria-label="Chat panel"
      aria-hidden={!isOpen}
    >
      <div className={styles.chatContainer}>
        {effectiveTabId && <AssistantChat tabId={effectiveTabId} />}
      </div>
    </div>
  );
};

export default DesktopChatPanel;
