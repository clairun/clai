/**
 * TabManagerContext
 *
 * Manages the application's tabs and their per-tab capability context
 * (MCP selection, custom key/value data). Tabs are the live data model
 * read by the terminal and the MCP context bar.
 *
 * History: this context used to also own a tile-grid layout and a per-tab
 * command registry that fed the pre-workspace Home UI. That UI was removed
 * (see Routes.tsx / the P2-0 dead-code sweep), so the tile/command machinery
 * is gone. Tabs can no longer be created (the /tab command went with that
 * UI) and the Rust persistence commands are stubs, so in practice this
 * provider always runs with zero tabs; it survives only because the
 * terminal and MCP context bar still read it. The `rootTile` field on
 * persisted tabs is a vestige of the old persist shape.
 */

import React, { createContext, useContext, useState, useCallback, useEffect, useRef, useMemo } from 'react';
import { useWorkspaceStore } from '../stores/workspaceStore';
import type { TabContext, WorkspaceTab } from '../stores/workspaceStore';
import { useShallow } from 'zustand/react/shallow';

interface TabManagerContextValue {
  tabs: WorkspaceTab[];
  activeTabId: string | null;

  getActiveTab: () => WorkspaceTab | null;

  updateTabContext: (tabId: string, context: Record<string, unknown>) => void;
}

const TabManagerContext = createContext<TabManagerContextValue | null>(null);

/**
 * Hook to use the TabManagerContext
 * @throws If used outside of TabManagerProvider
 */
export const useTabManager = (): TabManagerContextValue => {
  const context = useContext(TabManagerContext);
  if (!context) {
    throw new Error('useTabManager must be used within a TabManagerProvider');
  }
  return context;
};

const DEFAULT_TAB_CONTEXT: TabContext = {
  mcpServers: {
    attachedServerIds: [],
    disabledServerIds: [],
  },
  customContext: {},
};

interface RawMcpServers {
  attachedServerIds?: string[];
  selectedServerIds?: string[];
  disabledServerIds?: string[];
}
interface RawTabContext {
  mcpServers?: RawMcpServers;
  customContext?: Record<string, unknown>;
  [key: string]: unknown;
}

const uniqueIds = (ids: string[] = []): string[] => [...new Set((ids || []).filter(Boolean))];

const normalizeTabContext = (context: RawTabContext | null | undefined = {}): TabContext => {
  const {
    mcpServers: rawMcpContext = {},
    customContext: rawCustomContext = {},
    ...restContext
  } = context || {};
  const legacySelectedIds = uniqueIds(rawMcpContext.selectedServerIds || []);
  const attachedServerIds = uniqueIds(rawMcpContext.attachedServerIds || legacySelectedIds);
  const disabledServerIds = uniqueIds(rawMcpContext.disabledServerIds || []).filter(
    (id) => attachedServerIds.includes(id)
  );

  return {
    ...DEFAULT_TAB_CONTEXT,
    ...restContext,
    mcpServers: {
      attachedServerIds,
      disabledServerIds,
    },
    customContext: {
      ...DEFAULT_TAB_CONTEXT.customContext,
      ...rawCustomContext,
    },
  };
};

/**
 * TabManagerProvider component
 */
export const TabManagerProvider = ({ children }: { children: React.ReactNode }) => {
  const [tabs, setTabs] = useState<WorkspaceTab[]>([]);
  const [activeTabId, setActiveTabId] = useState<string | null>(null);

  // Zustand store (backed by the — currently stubbed — SQLite persistence).
  const workspaceState = useWorkspaceStore(
    useShallow((state) => ({
      storedTabs: state.tabs,
      storedTabOrder: state.tabOrder,
      storedActiveTabId: state.activeTabId,
      initialized: state.initialized,
    }))
  );

  const hasLoadedTabs = useRef(false);
  const skipNextSync = useRef(false);

  /**
   * Load tabs from the Zustand store (backed by SQLite) on mount.
   *
   * This is a genuine one-shot hydrate: the `hasLoadedTabs` ref + the
   * `workspaceState.initialized` gate guarantee it fires at most once,
   * and `skipNextSync` blocks the downstream sync effect from echoing
   * the just-loaded tabs back into the store. The setState-in-effect
   * is the whole point of the effect (move persisted data into React
   * state); the lint rule cannot model the one-shot guards and warns
   * anyway. A single disable on the first setState silences the whole
   * block (the rule reports only the first setState per effect).
   */
  useEffect(() => {
    if (!workspaceState.initialized) return;
    if (hasLoadedTabs.current) return;
    hasLoadedTabs.current = true;

    const { storedTabs, storedTabOrder, storedActiveTabId } = workspaceState;

    let tabsFromStore: WorkspaceTab[];
    if (storedTabOrder && storedTabOrder.length > 0) {
      tabsFromStore = storedTabOrder
        .map((id) => storedTabs[id])
        .filter((t): t is WorkspaceTab => Boolean(t));
    } else {
      tabsFromStore = Object.values(storedTabs);
    }

    if (tabsFromStore.length > 0) {
      // Skip the next sync to avoid immediately rewriting what we just loaded.
      skipNextSync.current = true;
      // eslint-disable-next-line react-hooks/set-state-in-effect -- one-shot hydrate from persisted store; hasLoadedTabs ref + skipNextSync prevent the echo the rule warns about.
      setTabs(tabsFromStore);
      setActiveTabId(storedActiveTabId || tabsFromStore[0]!.id);
    }
    // If the store had no tabs, start with none. Fresh installs run with
    // zero tabs: the /tab command that once created them is gone (legacy
    // tabs/tiles UI), so tabs only exist in stores persisted by old versions.
  }, [workspaceState]);

  /**
   * Sync tabs to the Zustand store whenever they change.
   * Zustand handles debounced persistence.
   */
  useEffect(() => {
    if (tabs.length === 0) return;

    if (skipNextSync.current) {
      skipNextSync.current = false;
      return;
    }

    const store = useWorkspaceStore.getState();

    tabs.forEach((tab) => {
      const existingTab = store.tabs[tab.id];
      if (!existingTab) {
        useWorkspaceStore.setState((state) => {
          state.tabs[tab.id] = {
            id: tab.id,
            title: tab.title,
            createdAt: tab.createdAt,
            rootTile: tab.rootTile,
            context: tab.context,
          };
          if (!state.tabOrder.includes(tab.id)) {
            state.tabOrder.push(tab.id);
          }
        });
      } else {
        if (JSON.stringify(existingTab.rootTile) !== JSON.stringify(tab.rootTile)) {
          store.updateTabRootTile(tab.id, tab.rootTile);
        }
        if (existingTab.title !== tab.title) {
          store.renameTab(tab.id, tab.title);
        }
        if (JSON.stringify(existingTab.context) !== JSON.stringify(tab.context)) {
          store.updateTabContext(tab.id, tab.context);
        }
      }
    });

    // Remove tabs from the store that no longer exist locally.
    Object.keys(store.tabs).forEach((tabId) => {
      if (!tabs.find((t) => t.id === tabId)) {
        store.closeTab(tabId);
      }
    });

    if (activeTabId && store.activeTabId !== activeTabId) {
      store.setActiveTab(activeTabId);
    }

    const localTabOrder = tabs.map((t) => t.id);
    const storeTabOrder = useWorkspaceStore.getState().tabOrder;
    if (JSON.stringify(localTabOrder) !== JSON.stringify(storeTabOrder)) {
      useWorkspaceStore.setState((state) => {
        state.tabOrder = localTabOrder;
      });
    }

    useWorkspaceStore.getState().triggerSave();
  }, [tabs, activeTabId]);

  const getActiveTab = useCallback((): WorkspaceTab | null => {
    return tabs.find((t) => t.id === activeTabId) || null;
  }, [tabs, activeTabId]);

  const updateTabContext = useCallback((tabId: string, context: Record<string, unknown>) => {
    setTabs((prev) =>
      prev.map((tab) =>
        tab.id === tabId
          ? {
              ...tab,
              context: normalizeTabContext({
                ...tab.context,
                ...context,
              }),
            }
          : tab
      )
    );
  }, []);

  const value = useMemo<TabManagerContextValue>(() => ({
    tabs,
    activeTabId,
    getActiveTab,
    updateTabContext,
  }), [
    tabs,
    activeTabId,
    getActiveTab,
    updateTabContext,
  ]);

  return (
    <TabManagerContext.Provider value={value}>
      {children}
    </TabManagerContext.Provider>
  );
};

export default TabManagerContext;
