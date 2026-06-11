/**
 * Workspace State Store
 *
 * Centralized state management for tabs, commands, and layout using
 * Zustand. Persists state to SQLite via the `save_workspace_state` and
 * `load_workspace_state` Tauri commands.
 */

import { create } from 'zustand';
import { devtools } from 'zustand/middleware';
import { immer } from 'zustand/middleware/immer';
import { invoke } from '@tauri-apps/api/core';

// ── Domain types ───────────────────────────────────────────────────

export interface TabMcpServers {
  attachedServerIds: string[];
  disabledServerIds: string[];
}

export interface TabContext {
  mcpServers: TabMcpServers;
  /** Arbitrary, command-specific context blob. */
  customContext: Record<string, unknown>;
  /** Future fields land here without breaking persisted state. */
  [key: string]: unknown;
}

export type CommandType = 'dashboard' | 'canvas' | 'help' | 'echo' | string;

export type CommandArgs = Record<string, unknown>;
export type CommandPersistentState = Record<string, unknown>;

export interface WorkspaceCommand {
  id: string;
  type: CommandType;
  args: CommandArgs;
  tabId: string;
  tileId: string;
  createdAt: number;
  state: CommandPersistentState;
}

export interface LeafTileNode {
  id: string;
  type: 'leaf';
  commandId?: string;
}

export interface SplitTileNode {
  id: string;
  type: 'split';
  direction: 'horizontal' | 'vertical';
  sizes: number[];
  children: TileNode[];
}

export type TileNode = LeafTileNode | SplitTileNode;

export interface WorkspaceTab {
  id: string;
  title: string;
  createdAt: number;
  rootTile: TileNode;
  context: TabContext;
}

/** Shape persisted to SQLite + sent back through `load_workspace_state`. */
interface PersistedWorkspaceState {
  activeTabId: string | null;
  tabOrder: string[];
  tabs: Record<string, WorkspaceTab>;
  commands: Record<string, WorkspaceCommand>;
}

// ── Normalization helpers ──────────────────────────────────────────

const DEFAULT_CONTEXT: TabContext = {
  mcpServers: {
    attachedServerIds: [],
    disabledServerIds: [],
  },
  customContext: {},
};

interface RawMcpServers {
  attachedServerIds?: string[];
  selectedServerIds?: string[]; // legacy alias from older persisted state
  disabledServerIds?: string[];
}

interface RawTabContext {
  mcpServers?: RawMcpServers;
  customContext?: Record<string, unknown>;
  /** Legacy field intentionally dropped during normalize. */
  spaceRoom?: unknown;
  [key: string]: unknown;
}

function normalizeTabContext(context: RawTabContext | null | undefined = {}): TabContext {
  const {
    mcpServers: rawMcpServers = {},
    customContext: rawCustomContext = {},
    spaceRoom: _spaceRoom,
    ...restContext
  } = context || {};

  const attachedServerIds = [
    ...new Set(
      (rawMcpServers.attachedServerIds || rawMcpServers.selectedServerIds || []).filter(Boolean),
    ),
  ];
  const disabledServerIds = [
    ...new Set((rawMcpServers.disabledServerIds || []).filter(Boolean)),
  ].filter((id) => attachedServerIds.includes(id));

  return {
    ...DEFAULT_CONTEXT,
    ...restContext,
    mcpServers: {
      attachedServerIds,
      disabledServerIds,
    },
    customContext: {
      ...DEFAULT_CONTEXT.customContext,
      ...rawCustomContext,
    },
  };
}

function sanitizeTileCommandReferences(
  tile: TileNode | null | undefined,
  validCommandIds: Set<string>,
): TileNode | null | undefined {
  if (!tile) return tile;

  if (tile.type === 'leaf') {
    return {
      ...tile,
      commandId: tile.commandId && validCommandIds.has(tile.commandId) ? tile.commandId : undefined,
    };
  }

  if (tile.type === 'split' && Array.isArray(tile.children)) {
    return {
      ...tile,
      children: tile.children.map(
        (child) => sanitizeTileCommandReferences(child, validCommandIds) as TileNode,
      ),
    };
  }

  return tile;
}

function sanitizeWorkspaceState(
  state: PersistedWorkspaceState | null | undefined,
): PersistedWorkspaceState | null | undefined {
  if (!state) return state;

  const commands = { ...(state.commands || {}) };
  const validCommandIds = new Set(Object.keys(commands));
  const tabs = Object.fromEntries(
    Object.entries(state.tabs || {}).map(([tabId, tab]) => [
      tabId,
      {
        ...tab,
        context: normalizeTabContext(tab.context as RawTabContext),
        rootTile: sanitizeTileCommandReferences(tab.rootTile, validCommandIds) as TileNode,
      },
    ]),
  );

  return {
    ...state,
    tabs,
    commands,
  };
}

// ── Save plumbing ──────────────────────────────────────────────────

function debounce<TArgs extends unknown[]>(fn: (...args: TArgs) => unknown, ms: number) {
  let timeoutId: ReturnType<typeof setTimeout> | undefined;
  return (...args: TArgs) => {
    if (timeoutId) clearTimeout(timeoutId);
    timeoutId = setTimeout(() => fn(...args), ms);
  };
}

const generateId = (prefix: string): string => {
  return `${prefix}_${Date.now()}_${Math.random().toString(36).slice(2, 9)}`;
};

let dbReady = false;

const persistShape = (state: WorkspaceStoreState): PersistedWorkspaceState => ({
  activeTabId: state.activeTabId,
  tabOrder: state.tabOrder,
  tabs: state.tabs,
  commands: state.commands,
});

const debouncedSave = debounce(async (state: WorkspaceStoreState) => {
  if (!dbReady) {
    console.debug('Database not ready, skipping save');
    return;
  }
  try {
    await invoke('save_workspace_state', { workspaceState: persistShape(state) });
    console.debug('Workspace state saved');
  } catch (error) {
    console.error('Failed to save workspace state:', error);
  }
}, 300);

export const saveWorkspaceStateNow = async (state: WorkspaceStoreState): Promise<void> => {
  if (!dbReady) {
    console.debug('Database not ready, skipping immediate save');
    return;
  }
  try {
    await invoke('save_workspace_state', { workspaceState: persistShape(state) });
    console.debug('Workspace state saved immediately');
  } catch (error) {
    console.error('Failed to save workspace state immediately:', error);
  }
};

// ── Store ─────────────────────────────────────────────────────────

export interface WorkspaceStoreState {
  activeTabId: string | null;
  tabOrder: string[];
  tabs: Record<string, WorkspaceTab>;
  commands: Record<string, WorkspaceCommand>;
  initialized: boolean;

  initialize: () => Promise<void>;

  createTab: (title?: string) => string;
  closeTab: (tabId: string) => void;
  setActiveTab: (tabId: string) => void;
  updateTabRootTile: (tabId: string, rootTile: TileNode) => void;
  updateTabContext: (tabId: string, contextUpdate: Partial<TabContext>) => void;
  renameTab: (tabId: string, title: string) => void;
  reorderTabs: (fromIndex: number, toIndex: number) => void;
  triggerSave: () => void;

  createCommand: (
    tabId: string,
    tileId: string,
    type: CommandType,
    args?: CommandArgs,
    initialState?: CommandPersistentState,
  ) => string;
  removeCommand: (commandId: string) => void;
  updateCommandState: (commandId: string, partialState: CommandPersistentState) => void;
  updateCommandStateAtomic: (
    commandId: string,
    updater: (current: CommandPersistentState) => CommandPersistentState,
  ) => void;
  updateCommandArgs: (commandId: string, args: CommandArgs) => void;
  moveCommand: (commandId: string, newTileId: string) => void;

  getCommandByTile: (tabId: string, tileId: string) => WorkspaceCommand | null;
}

export const useWorkspaceStore = create<WorkspaceStoreState>()(
  devtools(
    immer((set, get) => ({
      activeTabId: null,
      tabOrder: [],
      tabs: {},
      commands: {},
      initialized: false,

      initialize: async () => {
        try {
          const loadedState = (await invoke('load_workspace_state')) as
            | PersistedWorkspaceState
            | null
            | undefined;
          const state = sanitizeWorkspaceState(loadedState);
          if (state && Object.keys(state.tabs).length > 0) {
            const tabOrder =
              state.tabOrder && state.tabOrder.length > 0
                ? state.tabOrder
                : Object.keys(state.tabs);
            set({
              activeTabId: state.activeTabId,
              tabOrder,
              tabs: state.tabs,
              commands: state.commands,
              initialized: true,
            });
            console.debug('Workspace state loaded:', Object.keys(state.tabs).length, 'tabs');
          } else {
            set({ initialized: true });
            console.debug('Workspace state initialized (no saved tabs)');
          }
          dbReady = true;
        } catch (error) {
          console.error('Failed to load workspace state:', error);
          set({ initialized: true });
          dbReady = true;
        }
      },

      // ── Tab actions ─────────────────────────────────────────────

      createTab: (title: string = 'New Tab') => {
        const id = generateId('tab');
        const rootTileId = generateId('tile');
        set((state) => {
          state.tabs[id] = {
            id,
            title,
            createdAt: Date.now(),
            rootTile: { id: rootTileId, type: 'leaf', commandId: undefined },
            context: { ...DEFAULT_CONTEXT },
          };
          state.tabOrder.push(id);
          state.activeTabId = id;
        });
        debouncedSave(get());
        return id;
      },

      closeTab: (tabId) => {
        set((state) => {
          for (const cmdId of Object.keys(state.commands)) {
            if (state.commands[cmdId]!.tabId === tabId) {
              delete state.commands[cmdId];
            }
          }
          const orderIndex = state.tabOrder.indexOf(tabId);
          if (orderIndex !== -1) {
            state.tabOrder.splice(orderIndex, 1);
          }
          delete state.tabs[tabId];
          if (state.activeTabId === tabId) {
            state.activeTabId = state.tabOrder.length > 0 ? state.tabOrder[0]! : null;
          }
        });
        debouncedSave(get());
      },

      setActiveTab: (tabId) => {
        set((state) => {
          if (state.tabs[tabId]) state.activeTabId = tabId;
        });
        debouncedSave(get());
      },

      updateTabRootTile: (tabId, rootTile) => {
        set((state) => {
          if (state.tabs[tabId]) state.tabs[tabId].rootTile = rootTile;
        });
        debouncedSave(get());
      },

      updateTabContext: (tabId, contextUpdate) => {
        set((state) => {
          const tab = state.tabs[tabId];
          if (tab) {
            tab.context = { ...tab.context, ...contextUpdate };
          }
        });
        debouncedSave(get());
      },

      renameTab: (tabId, title) => {
        set((state) => {
          if (state.tabs[tabId]) state.tabs[tabId].title = title;
        });
        debouncedSave(get());
      },

      reorderTabs: (fromIndex, toIndex) => {
        set((state) => {
          if (fromIndex < 0 || fromIndex >= state.tabOrder.length) return;
          if (toIndex < 0 || toIndex >= state.tabOrder.length) return;
          if (fromIndex === toIndex) return;
          const [movedId] = state.tabOrder.splice(fromIndex, 1);
          if (movedId !== undefined) state.tabOrder.splice(toIndex, 0, movedId);
        });
        debouncedSave(get());
      },

      triggerSave: () => {
        debouncedSave(get());
      },

      // ── Command actions ─────────────────────────────────────────

      createCommand: (tabId, tileId, type, args = {}, initialState = {}) => {
        const id = generateId('cmd');
        set((state) => {
          state.commands[id] = {
            id,
            type,
            args,
            tabId,
            tileId,
            createdAt: Date.now(),
            state: initialState,
          };
        });
        debouncedSave(get());
        return id;
      },

      removeCommand: (commandId) => {
        set((state) => {
          delete state.commands[commandId];
        });
        debouncedSave(get());
      },

      updateCommandState: (commandId, partialState) => {
        set((state) => {
          const cmd = state.commands[commandId];
          if (cmd) cmd.state = { ...(cmd.state || {}), ...partialState };
        });
        debouncedSave(get());
      },

      /**
       * Atomically update command state using a function that receives current
       * state and returns the new state. Safe for concurrent read-modify-write.
       */
      updateCommandStateAtomic: (commandId, updater) => {
        set((state) => {
          const cmd = state.commands[commandId];
          if (cmd) {
            const current = cmd.state || {};
            cmd.state = { ...current, ...updater(current) };
          }
        });
        debouncedSave(get());
      },

      updateCommandArgs: (commandId, args) => {
        set((state) => {
          if (state.commands[commandId]) {
            state.commands[commandId].args = {
              ...state.commands[commandId].args,
              ...args,
            };
          }
        });
        debouncedSave(get());
      },

      moveCommand: (commandId, newTileId) => {
        set((state) => {
          if (state.commands[commandId]) {
            state.commands[commandId].tileId = newTileId;
          }
        });
        debouncedSave(get());
      },

      // ── Helpers ─────────────────────────────────────────────────

      getCommandByTile: (tabId, tileId) => {
        const commands = get().commands;
        return (
          Object.values(commands).find((cmd) => cmd.tabId === tabId && cmd.tileId === tileId) ||
          null
        );
      },
    })),
    { name: 'WorkspaceStore' },
  ),
);
