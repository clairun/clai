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
 * is gone. A minimal `rootTile` leaf is still created and persisted because
 * the workspace-state persist shape (and the vestigial, stubbed Rust
 * `save_workspace_state` command) still expects it.
 */

import React, { createContext, useContext, useState, useCallback, useEffect, useRef, useMemo } from 'react';
import { useWorkspaceStore } from '../stores/workspaceStore';
import type { LeafTileNode, TabContext, WorkspaceTab } from '../stores/workspaceStore';
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

const generateTileId = () => `tile_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

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
  spaceRoom?: unknown;
  [key: string]: unknown;
}

const uniqueIds = (ids: string[] = []): string[] => [...new Set((ids || []).filter(Boolean))];

const normalizeTabContext = (context: RawTabContext | null | undefined = {}): TabContext => {
  const {
    mcpServers: rawMcpContext = {},
    customContext: rawCustomContext = {},
    spaceRoom: _spaceRoom,
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

/** Create the vestigial root leaf tile a tab still carries for persistence. */
const createTile = (commandId: string | null = null): LeafTileNode => ({
  id: generateTileId(),
  type: 'leaf',
  commandId: commandId ?? undefined,
});

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
   * Falls back to localStorage for one-time migration of old persisted tabs.
   */
  useEffect(() => {
    if (!workspaceState.initialized) return;
    if (hasLoadedTabs.current) return;
    hasLoadedTabs.current = true;

    let tabsLoaded = false;
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
      setTabs(tabsFromStore);
      setActiveTabId(storedActiveTabId || tabsFromStore[0]!.id);
      tabsLoaded = true;
    }

    // One-time migration from the old localStorage tab store.
    if (!tabsLoaded) {
      try {
        const savedTabs = localStorage.getItem('netdata_tabs');
        const savedActiveTabId = localStorage.getItem('netdata_active_tab_id');

        if (savedTabs) {
          const parsed = JSON.parse(savedTabs);
          if (Array.isArray(parsed) && parsed.length > 0) {
            const migratedTabs: WorkspaceTab[] = parsed.map((tab) => {
              const migratedTab = { ...tab } as WorkspaceTab & { rootTile?: WorkspaceTab['rootTile'] };
              if (!migratedTab.rootTile) {
                migratedTab.rootTile = createTile(null);
              } else if (!migratedTab.rootTile.id || !migratedTab.rootTile.type) {
                migratedTab.rootTile = createTile(
                  (migratedTab.rootTile as LeafTileNode).commandId || null
                );
              }
              migratedTab.context = normalizeTabContext(migratedTab.context as RawTabContext);
              return migratedTab as WorkspaceTab;
            });

            setTabs(migratedTabs);
            setActiveTabId(savedActiveTabId || migratedTabs[0]!.id);

            localStorage.removeItem('netdata_tabs');
            localStorage.removeItem('netdata_active_tab_id');
            localStorage.removeItem('netdata_selected_space');
            localStorage.removeItem('netdata_selected_room');

            tabsLoaded = true;
          }
        }
      } catch (err) {
        console.error('Error loading tabs from localStorage:', err);
        localStorage.removeItem('netdata_tabs');
        localStorage.removeItem('netdata_active_tab_id');
        localStorage.removeItem('netdata_selected_space');
        localStorage.removeItem('netdata_selected_room');
      }
    }
    // If no tabs were loaded, start with none. Fresh installs run with zero
    // tabs: the /tab command that once created them is gone (legacy tabs/
    // tiles UI), so tabs only exist in stores persisted by old versions.
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
