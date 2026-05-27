/**
 * TabManagerContext
 *
 * This context manages tabs and their tile layouts.
 */

import React, { createContext, useContext, useState, useCallback, useEffect, useRef, useMemo } from 'react';
import { useCommand } from './CommandContext';
import { handleTabCommand } from '../utils/tabCommandHandler';
import { CommandRegistry } from '../commands/CommandRegistry';
import { isContentCommand, isLayoutCommand } from '../utils/commandTypes';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { useShallow } from 'zustand/react/shallow';

const TabManagerContext = createContext(null);

/**
 * Hook to use the TabManagerContext
 * @throws {Error} If used outside of TabManagerProvider
 */
export const useTabManager = () => {
  const context = useContext(TabManagerContext);
  if (!context) {
    throw new Error('useTabManager must be used within a TabManagerProvider');
  }
  return context;
};

/**
 * Generate a unique tab ID
 */
const generateTabId = () => `tab_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

/**
 * Generate a unique tile ID
 */
const generateTileId = () => `tile_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

const DEFAULT_TAB_CONTEXT = {
  mcpServers: {
    attachedServerIds: [],
    disabledServerIds: [],
  },
  assistantConnectionId: null,
  customContext: {},
};

const uniqueIds = (ids = []) => [...new Set((ids || []).filter(Boolean))];

const normalizeTabContext = (context = {}) => {
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

/**
 * Create a new leaf tile (contains a command)
 * @param {string|null} commandId - Command ID to display in this tile
 * @returns {Object} Leaf tile structure
 */
const createLeafTile = (commandId = null) => ({
  id: generateTileId(),
  type: 'leaf',
  commandId,
});

/**
 * Create a new split tile (contains child tiles)
 * @param {string} direction - 'horizontal' or 'vertical'
 * @param {Array} children - Array of child tiles
 * @param {Array} sizes - Array of percentage sizes for children (e.g., [50, 50])
 * @returns {Object} Split tile structure
 */
const createSplitTile = (direction, children, sizes = null) => {
  // Auto-calculate equal sizes if not provided
  const tileCount = children.length;
  const defaultSizes = Array(tileCount).fill(100 / tileCount);

  return {
    id: generateTileId(),
    type: 'split',
    direction, // 'horizontal' | 'vertical'
    children,
    sizes: sizes || defaultSizes,
  };
};

/**
 * Create a new tile structure (defaults to leaf)
 * For backward compatibility
 */
const createTile = (commandId = null) => createLeafTile(commandId);

/**
 * Extract the number from a tab title (e.g., "Tab 3" -> 3)
 * @param {string} title - Tab title
 * @returns {number|null} The extracted number or null if not found
 */
const extractTabNumber = (title) => {
  const match = title.match(/^Tab (\d+)$/);
  return match ? parseInt(match[1], 10) : null;
};

/**
 * Find the next available tab number based on existing tabs
 * @param {Array} tabs - Array of existing tabs
 * @returns {number} The next available tab number
 */
const getNextTabNumber = (tabs) => {
  if (tabs.length === 0) return 1;

  // Extract all tab numbers from existing tabs
  const tabNumbers = tabs
    .map(tab => extractTabNumber(tab.title))
    .filter(num => num !== null);

  // If no numbered tabs exist, start with 1
  if (tabNumbers.length === 0) return 1;

  // Find the maximum number and add 1
  return Math.max(...tabNumbers) + 1;
};

/**
 * Create a new tab structure
 */
const createTab = (title = null, commandId = null, initialContext = null) => ({
  id: generateTabId(),
  title: title || `Tab ${Date.now()}`,
  createdAt: Date.now(),
  rootTile: createTile(commandId),
  // Tab-specific context
  context: normalizeTabContext(initialContext),
});

/**
 * Recursively find a tile by ID in a tile tree
 * @param {Object} tile - Root tile to search from
 * @param {string} tileId - Tile ID to find
 * @returns {Object|null} Found tile or null
 */
const findTileById = (tile, tileId) => {
  if (tile.id === tileId) {
    return tile;
  }

  if (tile.type === 'split' && tile.children) {
    for (const child of tile.children) {
      const found = findTileById(child, tileId);
      if (found) return found;
    }
  }

  return null;
};

/**
 * Recursively find the parent of a tile in a tile tree
 * @param {Object} tile - Root tile to search from
 * @param {string} tileId - Tile ID to find parent of
 * @returns {Object|null} Parent tile or null
 */
const findParentTile = (tile, tileId) => {
  if (tile.type === 'split' && tile.children) {
    // Check if any child matches the target ID
    if (tile.children.some(child => child.id === tileId)) {
      return tile;
    }

    // Recursively search in children
    for (const child of tile.children) {
      const found = findParentTile(child, tileId);
      if (found) return found;
    }
  }

  return null;
};

/**
 * Split a tile into two tiles
 * @param {Object} tile - Tile to split
 * @param {string} direction - 'horizontal' or 'vertical'
 * @param {string|null} newCommandId - Command ID for the new tile
 * @returns {Object} New split tile
 */
const splitTileInternal = (tile, direction, newCommandId = null) => {
  // Create a new tile for the existing content
  const existingTile = { ...tile };

  // Create a new empty tile
  const newTile = createLeafTile(newCommandId);

  // Return a split tile containing both
  return createSplitTile(direction, [existingTile, newTile], [50, 50]);
};

/**
 * Remove a tile from a split tile's children
 * @param {Object} splitTile - Parent split tile
 * @param {string} tileIdToRemove - ID of tile to remove
 * @returns {Object|null} Updated split tile or the remaining child if only one left
 */
const removeTileFromSplit = (splitTile, tileIdToRemove) => {
  if (splitTile.type !== 'split') {
    return splitTile;
  }

  // Filter out the tile to remove
  const remainingChildren = splitTile.children.filter(
    child => child.id !== tileIdToRemove
  );

  // If only one child remains, return it directly (collapse the split)
  if (remainingChildren.length === 1) {
    return remainingChildren[0];
  }

  // If multiple children remain, update the split tile
  if (remainingChildren.length > 1) {
    // Recalculate sizes to distribute evenly
    const newSizes = remainingChildren.map(() => 100 / remainingChildren.length);
    return {
      ...splitTile,
      children: remainingChildren,
      sizes: newSizes,
    };
  }

  // If no children remain (shouldn't happen), return null
  return null;
};

/**
 * Update a tile's size in a split tile
 * @param {Object} splitTile - Parent split tile
 * @param {string} tileId - ID of tile to resize
 * @param {number} newSize - New size percentage (0-100)
 * @returns {Object} Updated split tile
 */
const updateTileSize = (splitTile, tileId, newSize) => {
  if (splitTile.type !== 'split') {
    return splitTile;
  }

  const tileIndex = splitTile.children.findIndex(child => child.id === tileId);
  if (tileIndex === -1) {
    return splitTile;
  }

  // Calculate new sizes
  const newSizes = [...splitTile.sizes];
  const oldSize = newSizes[tileIndex];
  const sizeDiff = newSize - oldSize;

  // Adjust the target tile size
  newSizes[tileIndex] = newSize;

  // Distribute the difference among other tiles proportionally
  const otherIndices = newSizes
    .map((_, idx) => idx)
    .filter(idx => idx !== tileIndex);

  if (otherIndices.length > 0) {
    const totalOtherSize = otherIndices.reduce((sum, idx) => sum + newSizes[idx], 0);

    otherIndices.forEach(idx => {
      const proportion = newSizes[idx] / totalOtherSize;
      newSizes[idx] = newSizes[idx] - (sizeDiff * proportion);
    });
  }

  return {
    ...splitTile,
    sizes: newSizes,
  };
};

/**
 * Recursively clone a tile tree with new IDs
 * @param {Object} tile - Tile to clone
 * @returns {Object} Cloned tile with new IDs
 */
const cloneTileTree = (tile) => {
  if (tile.type === 'leaf') {
    return {
      ...tile,
      id: generateTileId(),
    };
  }

  if (tile.type === 'split') {
    return {
      ...tile,
      id: generateTileId(),
      children: tile.children.map(cloneTileTree),
    };
  }

  return tile;
};

/**
 * TabManagerProvider component
 * Provides tab management state and methods to the application
 */
export const TabManagerProvider = ({ children }) => {
  // All tabs
  const [tabs, setTabs] = useState([]);

  // Active tab ID
  const [activeTabId, setActiveTabId] = useState(null);

  // Active tile ID (within active tab)
  const [activeTileId, setActiveTileId] = useState(null);

  // Get current command and executeCommand from CommandContext
  const { currentCommand, executeCommand } = useCommand();

  // Track the last processed command ID to prevent re-processing on tab switches
  const lastProcessedCommandId = useRef(null);

  // CommandRegistry instances per tab (Map<tabId, CommandRegistry>)
  // Stored in ref because APIs are mutable and shouldn't trigger re-renders
  const registriesRef = useRef(new Map());

  /**
   * Get or create a CommandRegistry for a tab
   * @param {string} tabId - Tab ID
   * @returns {CommandRegistry} Registry for the tab
   */
  const getRegistry = useCallback((tabId) => {
    if (!registriesRef.current.has(tabId)) {
      registriesRef.current.set(tabId, new CommandRegistry());
    }
    return registriesRef.current.get(tabId);
  }, []);

  /**
   * Remove a CommandRegistry when a tab is closed
   * @param {string} tabId - Tab ID
   */
  const removeRegistry = useCallback((tabId) => {
    registriesRef.current.delete(tabId);
  }, []);

  // Get Zustand store state using useShallow for proper snapshot caching
  const workspaceState = useWorkspaceStore(
    useShallow((state) => ({
      storedTabs: state.tabs,
      storedTabOrder: state.tabOrder,
      storedCommands: state.commands,
      storedActiveTabId: state.activeTabId,
      initialized: state.initialized,
    }))
  );

  // Track if we've already loaded tabs to prevent re-running
  const hasLoadedTabs = useRef(false);

  // Track if we should skip the next sync (to prevent race condition during initial load)
  const skipNextSync = useRef(false);

  /**
   * Load tabs from Zustand store (backed by SQLite) on mount
   * Falls back to localStorage for migration, then to /help command
   */
  useEffect(() => {
    // Wait for Zustand store to be initialized from SQLite
    if (!workspaceState.initialized) {
      return;
    }

    // Only load once
    if (hasLoadedTabs.current) {
      return;
    }
    hasLoadedTabs.current = true;

    let tabsLoaded = false;

    // First, try to load from Zustand store (populated from SQLite)
    const { storedTabs, storedTabOrder, storedActiveTabId, storedCommands } = workspaceState;

    // Build tabs array in the correct order using tabOrder
    let tabsFromStore;
    if (storedTabOrder && storedTabOrder.length > 0) {
      // Use tabOrder to maintain the correct order
      tabsFromStore = storedTabOrder
        .map(id => storedTabs[id])
        .filter(Boolean); // Filter out any undefined (in case of stale IDs)
    } else {
      // Fallback to Object.values if no tabOrder (migration case)
      tabsFromStore = Object.values(storedTabs);
    }

    if (tabsFromStore.length > 0) {
      console.log('[TabManagerContext] Loading tabs from Zustand store');

      // Skip the next sync to prevent race condition - commands being restored to registry
      skipNextSync.current = true;

      // Restore tabs from store
      setTabs(tabsFromStore);
      setActiveTabId(storedActiveTabId || tabsFromStore[0].id);

      // Set active tile to the root tile of active tab
      const activeTab = tabsFromStore.find(t => t.id === (storedActiveTabId || tabsFromStore[0].id));
      if (activeTab) {
        setActiveTileId(activeTab.rootTile.id);
      }

      // Rebuild CommandRegistries from stored commands
      Object.values(storedCommands).forEach(cmd => {
        const registry = getRegistry(cmd.tabId);
        // Restore command entry (api gets registered when component mounts, state is in Zustand)
        registry.restoreCommand(cmd.id, cmd.type, cmd.args, cmd.tileId, cmd.createdAt);
      });

      tabsLoaded = true;
    }

    // Fallback: try localStorage for migration
    if (!tabsLoaded) {
      try {
        const savedTabs = localStorage.getItem('netdata_tabs');
        const savedActiveTabId = localStorage.getItem('netdata_active_tab_id');

        if (savedTabs) {
          const parsed = JSON.parse(savedTabs);
          if (Array.isArray(parsed) && parsed.length > 0) {
            console.log('[TabManagerContext] Migrating tabs from localStorage');

            // Migrate old tabs to new structure with context field and rootTile
            const migratedTabs = parsed.map(tab => {
              let migratedTab = { ...tab };

              // Migration 1: Add rootTile if missing (pre-tiling system tabs)
              if (!migratedTab.rootTile) {
                migratedTab.rootTile = createTile(null);
              }
              // Also ensure rootTile has required fields (partial migration)
              else if (!migratedTab.rootTile.id || !migratedTab.rootTile.type) {
                migratedTab.rootTile = createTile(migratedTab.rootTile.commandId || null);
              }

              // Migration 2: Add context if missing
              if (!migratedTab.context) {
                migratedTab = {
                  ...migratedTab,
                  context: normalizeTabContext(),
                };
              }

              migratedTab.context = normalizeTabContext(migratedTab.context);

              return migratedTab;
            });

            setTabs(migratedTabs);
            setActiveTabId(savedActiveTabId || migratedTabs[0].id);

            // Set active tile to the root tile of active tab
            const activeTab = migratedTabs.find(t => t.id === (savedActiveTabId || migratedTabs[0].id));
            if (activeTab) {
              setActiveTileId(activeTab.rootTile.id);
            }

            // Clear localStorage after successful migration (data is now in Zustand/SQLite)
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

    // If no tabs were loaded, create initial tab with /help command
    if (!tabsLoaded) {
      // Use setTimeout to ensure CommandContext is ready
      setTimeout(() => {
        executeCommand('help'); // Pass as string so parseCommand generates proper id
      }, 0);
    }
  }, [executeCommand, getRegistry, workspaceState.initialized]); // Runs when initialized becomes true

  /**
   * Sync tabs to Zustand store whenever they change.
   * Zustand handles debounced persistence to SQLite.
   */
  useEffect(() => {
    // Skip initial render and when loading from store
    if (tabs.length === 0) return;

    // Skip sync during initial load from store - commands are being restored to registry
    // This prevents the sync from removing commands before they're fully restored
    if (skipNextSync.current) {
      skipNextSync.current = false;
      return;
    }

    // Get direct access to Zustand store for bulk updates
    const store = useWorkspaceStore.getState();

    // Sync each tab to the store
    tabs.forEach(tab => {
      const existingTab = store.tabs[tab.id];
      if (!existingTab) {
        // Tab doesn't exist in store - this is a new tab created locally
        // We need to update the store directly since we're managing IDs locally
        useWorkspaceStore.setState((state) => {
          state.tabs[tab.id] = {
            id: tab.id,
            title: tab.title,
            createdAt: tab.createdAt,
            rootTile: tab.rootTile,
            context: tab.context,
          };
          // Also add to tabOrder if not already present
          if (!state.tabOrder.includes(tab.id)) {
            state.tabOrder.push(tab.id);
          }
        });
      } else {
        // Tab exists - update it if changed
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

      // Sync commands for this tab from the registry
      const registry = registriesRef.current.get(tab.id);
      if (registry) {
        const registryCommands = registry.getAll();
        const registryCommandIds = new Set(registryCommands.map(c => c.id));

        // Add/update commands
        registryCommands.forEach(cmd => {
          const existingCmd = store.commands[cmd.id];
          if (!existingCmd) {
            // Create command in store
            useWorkspaceStore.setState((state) => {
              state.commands[cmd.id] = {
                id: cmd.id,
                type: cmd.type,
                args: cmd.args,
                tabId: tab.id,
                tileId: cmd.tileId,
                createdAt: cmd.createdAt,
                state: {}, // Component state starts empty, updated by components
              };
            });
          } else if (existingCmd.tileId !== cmd.tileId) {
            // Update tileId if changed
            store.moveCommand(cmd.id, cmd.tileId);
          }
        });

        // Remove commands from store that no longer exist in registry
        Object.values(store.commands)
          .filter(cmd => cmd.tabId === tab.id && !registryCommandIds.has(cmd.id))
          .forEach(cmd => store.removeCommand(cmd.id));
      }
    });

    // Remove tabs from store that no longer exist locally
    Object.keys(store.tabs).forEach(tabId => {
      if (!tabs.find(t => t.id === tabId)) {
        store.closeTab(tabId);
      }
    });

    // Sync active tab
    if (activeTabId && store.activeTabId !== activeTabId) {
      store.setActiveTab(activeTabId);
    }

    // Sync tab order to match local tabs array order
    const localTabOrder = tabs.map(t => t.id);
    const storeTabOrder = useWorkspaceStore.getState().tabOrder;
    if (JSON.stringify(localTabOrder) !== JSON.stringify(storeTabOrder)) {
      useWorkspaceStore.setState((state) => {
        state.tabOrder = localTabOrder;
      });
    }

    // Trigger save for any direct state modifications made above
    useWorkspaceStore.getState().triggerSave();
  }, [tabs, activeTabId]);

  /**
   * When a new command is executed, add it to the active tile
   * If no tabs exist, create the first tab automatically
   *
   * For content commands (canvas, dashboard, etc.):
   * - Creates an entry in the CommandRegistry
   * - Assigns the registry's commandId to the tile
   *
   * For layout commands (tab, tile):
   * - Skipped here, handled by handleLayoutCommand
   */
  useEffect(() => {
    // Only process if we have a command and it's different from the last one we processed
    if (currentCommand && currentCommand.id !== lastProcessedCommandId.current) {
      // Mark this command as processed
      lastProcessedCommandId.current = currentCommand.id;

      // Skip layout commands - they're handled by handleLayoutCommand
      if (isLayoutCommand(currentCommand)) {
        return;
      }

      // If no tabs exist, create the first tab
      if (tabs.length === 0) {
        const newTab = createTab('Tab 1', null); // Create tab without command
        setTabs([newTab]);
        setActiveTabId(newTab.id);
        setActiveTileId(newTab.rootTile.id);

        // For content commands, create registry entry and assign to tile
        if (isContentCommand(currentCommand)) {
          const registry = getRegistry(newTab.id);
          const commandId = registry.create(currentCommand.type, currentCommand.args || {});
          registry.assignToTile(commandId, newTab.rootTile.id);

          // Update the tile with the new commandId
          setTabs(prev => prev.map(tab => {
            if (tab.id === newTab.id) {
              return {
                ...tab,
                rootTile: { ...tab.rootTile, commandId },
              };
            }
            return tab;
          }));
        }
        return;
      }

      // If tabs exist, add command to active tile
      if (activeTabId && activeTileId) {
        // For content commands, create registry entry
        if (isContentCommand(currentCommand)) {
          const registry = getRegistry(activeTabId);

          // Delete existing command in this tile before creating new one
          const existingCommand = registry.getByTile(activeTileId);
          if (existingCommand) {
            registry.delete(existingCommand.id);
          }

          const commandId = registry.create(currentCommand.type, currentCommand.args || {});
          registry.assignToTile(commandId, activeTileId);

          setTabs(prev => {
            return prev.map(tab => {
              if (tab.id === activeTabId) {
                // Recursively update the active tile in the tree
                const updateTileTree = (tile) => {
                  if (tile.id === activeTileId) {
                    return { ...tile, commandId };
                  }
                  if (tile.type === 'split' && tile.children) {
                    return {
                      ...tile,
                      children: tile.children.map(updateTileTree),
                    };
                  }
                  return tile;
                };

                return { ...tab, rootTile: updateTileTree(tab.rootTile) };
              }
              return tab;
            });
          });
        } else {
          // Non-content commands (fallback to old behavior for compatibility)
          setTabs(prev => {
            const activeTab = prev.find(t => t.id === activeTabId);
            if (activeTab) {
              const tileHasCommand = (tile, cmdId) => {
                if (tile.commandId === cmdId) return true;
                if (tile.type === 'split' && tile.children) {
                  return tile.children.some(child => tileHasCommand(child, cmdId));
                }
                return false;
              };

              if (tileHasCommand(activeTab.rootTile, currentCommand.id)) {
                return prev;
              }
            }

            return prev.map(tab => {
              if (tab.id === activeTabId) {
                const updateTileTree = (tile) => {
                  if (tile.id === activeTileId) {
                    return { ...tile, commandId: currentCommand.id };
                  }
                  if (tile.type === 'split' && tile.children) {
                    return {
                      ...tile,
                      children: tile.children.map(updateTileTree),
                    };
                  }
                  return tile;
                };

                return { ...tab, rootTile: updateTileTree(tab.rootTile) };
              }
              return tab;
            });
          });
        }
      }
    }
  }, [currentCommand, tabs.length, activeTabId, activeTileId, getRegistry]);

  /**
   * Create a new tab
   * @param {string} title - Optional tab title
   * @param {string} commandId - Optional command ID to add to the tab
   * @returns {Object} The created tab
   */
  const createNewTab = useCallback((title = null, commandId = null) => {
    // Calculate next available tab number if no title provided
    const tabTitle = title || `Tab ${getNextTabNumber(tabs)}`;

    const tabContext = activeTabId
      ? (() => {
        const activeTab = tabs.find(t => t.id === activeTabId);
        if (!activeTab?.context) return null;

        return {
          mcpServers: {
            attachedServerIds: [],
            disabledServerIds: [],
          },
          customContext: { ...activeTab.context.customContext },
        };
      })()
      : null;

    const newTab = createTab(tabTitle, commandId, tabContext);

    setTabs(prev => [...prev, newTab]);
    setActiveTabId(newTab.id);
    setActiveTileId(newTab.rootTile.id);

    return newTab;
  }, [tabs, activeTabId]);

  /**
   * Close a tab
   * @param {string} tabId - Tab ID to close
   */
  const closeTab = useCallback((tabId) => {
    // Clean up the CommandRegistry for this tab
    removeRegistry(tabId);

    setTabs(prev => {
      const filtered = prev.filter(t => t.id !== tabId);

      // If closing active tab, switch to another tab
      if (tabId === activeTabId) {
        if (filtered.length > 0) {
          const newActiveTab = filtered[filtered.length - 1];
          setActiveTabId(newActiveTab.id);
          setActiveTileId(newActiveTab.rootTile.id);
        } else {
          setActiveTabId(null);
          setActiveTileId(null);
        }
      }

      return filtered;
    });
  }, [activeTabId]);

  /**
   * Switch to a tab
   * @param {string} tabId - Tab ID to switch to
   */
  const switchToTab = useCallback((tabId) => {
    const tab = tabs.find(t => t.id === tabId);
    if (tab) {
      setActiveTabId(tabId);
      setActiveTileId(tab.rootTile.id);
    }
  }, [tabs]);

  /**
   * Switch to tab by index (1-based)
   * @param {number} index - Tab index (1-based)
   */
  const switchToTabByIndex = useCallback((index) => {
    if (index > 0 && index <= tabs.length) {
      const tab = tabs[index - 1];
      switchToTab(tab.id);
    }
  }, [tabs, switchToTab]);

  /**
   * Switch to next tab
   * @returns {string|null} The ID of the tab switched to, or null if no tabs
   */
  const switchToNextTab = useCallback(() => {
    if (tabs.length === 0) return null;

    const currentIndex = tabs.findIndex(t => t.id === activeTabId);
    const nextIndex = (currentIndex + 1) % tabs.length;

    const nextTabId = tabs[nextIndex].id;
    switchToTab(nextTabId);
    return nextTabId;
  }, [tabs, activeTabId, switchToTab]);

  /**
   * Switch to previous tab
   * @returns {string|null} The ID of the tab switched to, or null if no tabs
   */
  const switchToPrevTab = useCallback(() => {
    if (tabs.length === 0) return null;

    const currentIndex = tabs.findIndex(t => t.id === activeTabId);
    const prevIndex = currentIndex === 0 ? tabs.length - 1 : currentIndex - 1;

    const prevTabId = tabs[prevIndex].id;
    switchToTab(prevTabId);
    return prevTabId;
  }, [tabs, activeTabId, switchToTab]);

  /**
   * Rename a tab
   * @param {string} tabId - Tab ID to rename
   * @param {string} newTitle - New tab title
   */
  const renameTab = useCallback((tabId, newTitle) => {
    setTabs(prev =>
      prev.map(tab =>
        tab.id === tabId ? { ...tab, title: newTitle } : tab
      )
    );
  }, []);

  /**
   * Move/reorder a tab
   * @param {number} fromIndex - Source index
   * @param {number} toIndex - Destination index
   */
  const moveTab = useCallback((fromIndex, toIndex) => {
    setTabs(prev => {
      const newTabs = [...prev];
      const [movedTab] = newTabs.splice(fromIndex, 1);
      newTabs.splice(toIndex, 0, movedTab);
      return newTabs;
    });
    // Sync with workspace store for persistence
    useWorkspaceStore.getState().reorderTabs(fromIndex, toIndex);
  }, []);

  /**
   * Add a command to the active tile
   * @param {Object} command - Command object from CommandContext
   */
  const addCommandToActiveTile = useCallback((command) => {
    if (!activeTabId || !activeTileId) return;

    setTabs(prev =>
      prev.map(tab => {
        if (tab.id === activeTabId) {
          return {
            ...tab,
            rootTile: {
              ...tab.rootTile,
              commandId: command.id,
            },
          };
        }
        return tab;
      })
    );
  }, [activeTabId, activeTileId]);

  /**
   * Get the active tab
   * @returns {Object|null} Active tab or null
   */
  const getActiveTab = useCallback(() => {
    return tabs.find(t => t.id === activeTabId) || null;
  }, [tabs, activeTabId]);

  /**
   * Get a tab by ID
   * @param {string} tabId - Tab ID
   * @returns {Object|null} Tab or null
   */
  const getTab = useCallback((tabId) => {
    return tabs.find(t => t.id === tabId) || null;
  }, [tabs]);

  /**
   * Clear all tabs
   */
  const clearAllTabs = useCallback(() => {
    // Clear all CommandRegistries
    registriesRef.current.clear();

    setTabs([]);
    setActiveTabId(null);
    setActiveTileId(null);
  }, []);

  /**
   * Update tab context
   * @param {string} tabId - Tab ID
   * @param {Object} context - Partial context update
   */
  const updateTabContext = useCallback((tabId, context) => {
    setTabs(prev =>
      prev.map(tab =>
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

  /**
   * Get tab context
   * @param {string} tabId - Tab ID
   * @returns {Object|null} Tab context or null
   */
  const getTabContext = useCallback((tabId) => {
    const tab = tabs.find(t => t.id === tabId);
    return tab?.context || null;
  }, [tabs]);

  /**
   * Get active tab context
   * @returns {Object|null} Active tab context or null
   */
  const getActiveTabContext = useCallback(() => {
    if (!activeTabId) return null;
    return getTabContext(activeTabId);
  }, [activeTabId, getTabContext]);

  /**
   * Split a tile in the active tab
   * @param {string} tileId - Tile ID to split
   * @param {string} direction - 'horizontal' or 'vertical'
   * @param {string} newCommandId - Optional command ID for the new tile
   * @returns {Object|null} Result with success status and new tile ID
   */
  const splitTile = useCallback((tileId, direction, newCommandId = null) => {
    if (!activeTabId) {
      return { success: false, message: 'No active tab' };
    }

    // Use a result object that will be populated inside setTabs callback
    let result = { success: false, message: 'Unknown error', newTileId: null };

    // Compute everything inside setTabs to use latest state (prev)
    setTabs(prev => {
      const activeTab = prev.find(t => t.id === activeTabId);
      if (!activeTab) {
        result = { success: false, message: 'Active tab not found', newTileId: null };
        return prev;
      }

      // Find the tile to split using prev state
      const tileToSplit = findTileById(activeTab.rootTile, tileId);
      if (!tileToSplit) {
        result = { success: false, message: 'Tile not found', newTileId: null };
        return prev;
      }

      // Create the split tile
      const splitTileResult = splitTileInternal(tileToSplit, direction, newCommandId);

      // Update the tile tree
      const updateTileTree = (tile) => {
        if (tile.id === tileId) {
          return splitTileResult;
        }
        if (tile.type === 'split' && tile.children) {
          return {
            ...tile,
            children: tile.children.map(updateTileTree),
          };
        }
        return tile;
      };

      const updatedRootTile = updateTileTree(activeTab.rootTile);

      // Set result with new tile ID
      const newTileId = splitTileResult.children[1].id;
      result = {
        success: true,
        message: `Tile split ${direction}`,
        newTileId,
      };

      return prev.map(tab =>
        tab.id === activeTabId
          ? { ...tab, rootTile: updatedRootTile }
          : tab
      );
    });

    // Set the new tile as active (second child of the split)
    if (result.success && result.newTileId) {
      setActiveTileId(result.newTileId);
    }

    return result;
  }, [activeTabId]);

  /**
   * Split a tile in a specific tab (for agent bridge)
   * @param {string} tabId - Tab ID
   * @param {string} tileId - Tile ID to split
   * @param {string} direction - 'horizontal' or 'vertical'
   * @param {string} newCommandId - Optional command ID for the new tile
   * @returns {Object} Result with success status and new tile ID
   */
  const splitTileInTab = useCallback((tabId, tileId, direction, newCommandId = null) => {
    // Use a result object that will be populated inside setTabs callback
    let result = { success: false, message: 'Unknown error', newTileId: null };

    // Compute everything inside setTabs to use latest state (prev)
    setTabs(prev => {
      const tab = prev.find(t => t.id === tabId);
      if (!tab) {
        result = { success: false, message: 'Tab not found', newTileId: null };
        return prev;
      }

      // Find the tile to split using prev state
      const tileToSplit = findTileById(tab.rootTile, tileId);
      if (!tileToSplit) {
        result = { success: false, message: 'Tile not found', newTileId: null };
        return prev;
      }

      // Create the split tile
      const splitTileResult = splitTileInternal(tileToSplit, direction, newCommandId);

      // Update the tile tree
      const updateTileTree = (tile) => {
        if (tile.id === tileId) {
          return splitTileResult;
        }
        if (tile.type === 'split' && tile.children) {
          return {
            ...tile,
            children: tile.children.map(updateTileTree),
          };
        }
        return tile;
      };

      const updatedRootTile = updateTileTree(tab.rootTile);

      // Set result with new tile ID
      const newTileId = splitTileResult.children[1].id;
      result = {
        success: true,
        message: `Tile split ${direction}`,
        newTileId,
      };

      return prev.map(t =>
        t.id === tabId
          ? { ...t, rootTile: updatedRootTile }
          : t
      );
    });

    return result;
  }, []);

  /**
   * Close a tile in the active tab
   * @param {string} tileId - Tile ID to close
   * @returns {Object} Result with success status
   */
  const closeTile = useCallback((tileId) => {
    if (!activeTabId) {
      return { success: false, message: 'No active tab' };
    }

    const activeTab = tabs.find(t => t.id === activeTabId);
    if (!activeTab) {
      return { success: false, message: 'Active tab not found' };
    }

    // Can't close the root tile if it's the only one
    if (activeTab.rootTile.id === tileId && activeTab.rootTile.type === 'leaf') {
      return { success: false, message: 'Cannot close the only tile in a tab' };
    }

    // Find the parent of the tile to close
    const parent = findParentTile(activeTab.rootTile, tileId);

    if (!parent) {
      // If no parent, this is the root tile
      return { success: false, message: 'Cannot close root tile' };
    }

    // Delete the command in this tile from the registry before closing
    const registry = getRegistry(activeTabId);
    const existingCommand = registry.getByTile(tileId);
    if (existingCommand) {
      registry.delete(existingCommand.id);
    }

    // Remove the tile from its parent
    const updatedParent = removeTileFromSplit(parent, tileId);

    // Update the tile tree
    const updateTileTree = (tile) => {
      if (tile.id === parent.id) {
        return updatedParent;
      }
      if (tile.type === 'split' && tile.children) {
        return {
          ...tile,
          children: tile.children.map(updateTileTree),
        };
      }
      return tile;
    };

    let updatedRootTile = updateTileTree(activeTab.rootTile);

    // If the root was updated and it's now a leaf (collapsed), use it directly
    if (updatedRootTile.id === parent.id && updatedParent.type === 'leaf') {
      updatedRootTile = updatedParent;
    }

    // Update the tab
    setTabs(prev =>
      prev.map(tab =>
        tab.id === activeTabId
          ? { ...tab, rootTile: updatedRootTile }
          : tab
      )
    );

    // If the closed tile was active, set a new active tile
    if (tileId === activeTileId) {
      // Find the first leaf tile in the updated tree
      const findFirstLeaf = (tile) => {
        if (tile.type === 'leaf') return tile.id;
        if (tile.type === 'split' && tile.children.length > 0) {
          return findFirstLeaf(tile.children[0]);
        }
        return null;
      };

      const newActiveTileId = findFirstLeaf(updatedRootTile);
      if (newActiveTileId) {
        setActiveTileId(newActiveTileId);
      }
    }

    return { success: true, message: 'Tile closed' };
  }, [activeTabId, activeTileId, tabs, getRegistry]);

  /**
   * Resize a tile in the active tab
   * @param {string} tileId - Tile ID to resize
   * @param {number} newSize - New size percentage (0-100)
   * @returns {Object} Result with success status
   */
  const resizeTile = useCallback((tileId, newSize) => {
    if (!activeTabId) {
      return { success: false, message: 'No active tab' };
    }

    const activeTab = tabs.find(t => t.id === activeTabId);
    if (!activeTab) {
      return { success: false, message: 'Active tab not found' };
    }

    // Find the parent of the tile to resize
    const parent = findParentTile(activeTab.rootTile, tileId);

    if (!parent || parent.type !== 'split') {
      return { success: false, message: 'Tile cannot be resized (no parent split)' };
    }

    // Update the tile sizes
    const updatedParent = updateTileSize(parent, tileId, newSize);

    // Update the tile tree
    const updateTileTree = (tile) => {
      if (tile.id === parent.id) {
        return updatedParent;
      }
      if (tile.type === 'split' && tile.children) {
        return {
          ...tile,
          children: tile.children.map(updateTileTree),
        };
      }
      return tile;
    };

    const updatedRootTile = updateTileTree(activeTab.rootTile);

    // Update the tab
    setTabs(prev =>
      prev.map(tab =>
        tab.id === activeTabId
          ? { ...tab, rootTile: updatedRootTile }
          : tab
      )
    );

    return { success: true, message: 'Tile resized' };
  }, [activeTabId, tabs]);

  /**
   * Get a tile by ID in the active tab
   * @param {string} tileId - Tile ID
   * @returns {Object|null} Tile or null
   */
  const getTile = useCallback((tileId) => {
    if (!activeTabId) return null;

    const activeTab = tabs.find(t => t.id === activeTabId);
    if (!activeTab) return null;

    return findTileById(activeTab.rootTile, tileId);
  }, [activeTabId, tabs]);

  /**
   * Get all leaf tiles in the active tab
   * @returns {Array} Array of leaf tiles
   */
  const getLeafTiles = useCallback(() => {
    if (!activeTabId) return [];

    const activeTab = tabs.find(t => t.id === activeTabId);
    if (!activeTab) return [];

    const leafTiles = [];
    const collectLeafTiles = (tile) => {
      if (tile.type === 'leaf') {
        leafTiles.push(tile);
      } else if (tile.type === 'split' && tile.children) {
        tile.children.forEach(collectLeafTiles);
      }
    };

    collectLeafTiles(activeTab.rootTile);
    return leafTiles;
  }, [activeTabId, tabs]);

  /**
   * Duplicate a tab
   * @param {string} tabId - Tab ID to duplicate
   * @returns {Object} The duplicated tab
   */
  const duplicateTab = useCallback((tabId) => {
    const tab = tabs.find(t => t.id === tabId);
    if (!tab) return null;

    const newTab = {
      ...tab,
      id: generateTabId(),
      title: `${tab.title} (Copy)`,
      createdAt: Date.now(),
      rootTile: {
        ...tab.rootTile,
        id: generateTileId(),
      },
      // Deep clone context to prevent reference sharing
      context: {
        mcpServers: {
          attachedServerIds: [...(tab.context?.mcpServers?.attachedServerIds || [])],
          disabledServerIds: [...(tab.context?.mcpServers?.disabledServerIds || [])],
        },
        customContext: { ...tab.context?.customContext || {} },
      },
    };

    setTabs(prev => [...prev, newTab]);
    setActiveTabId(newTab.id);
    setActiveTileId(newTab.rootTile.id);

    return newTab;
  }, [tabs]);

  /**
   * Handle tab layout commands from terminal
   * @param {Object} command - Parsed command object
   * @returns {Object} Result object with success status and message
   */
  const handleLayoutCommand = useCallback((command) => {
    const { type, args } = command;

    try {
      switch (type) {
        case 'tab': {
          // Delegate to handleTabCommand with tabManager context
          const tabManager = {
            tabs,
            activeTabId,
            createTab: createNewTab,
            closeTab,
            switchToTab,
            switchToTabByIndex,
            switchToNextTab,
            switchToPrevTab,
            renameTab,
            duplicateTab,
            resetTab: (tabId) => {
              // Placeholder for Phase 3
              return { success: true, message: 'Tab reset (Phase 3)' };
            },
          };
          return handleTabCommand(command, tabManager);
        }

        case 'reset-all': {
          // reset-all - Clear all tabs
          clearAllTabs();
          return {
            success: true,
            message: 'All tabs cleared'
          };
        }

        // NOTE: the `/tile` command was removed alongside the tile-grid UI
        // (TabContent/TileView). It now falls through to the default branch.

        default:
          return {
            success: false,
            message: `Unknown layout command: ${type}`
          };
      }
    } catch (error) {
      console.error('Error handling layout command:', error);
      return {
        success: false,
        message: `Error: ${error.message}`
      };
    }
  }, [tabs, activeTabId, activeTileId, createNewTab, switchToTab, switchToTabByIndex, switchToNextTab, switchToPrevTab, closeTab, renameTab, duplicateTab, clearAllTabs, splitTile, closeTile, resizeTile, getLeafTiles]);

  // Extract inline functions to useCallback for proper memoization
  const focusNextTile = useCallback(() => {
    const leafTiles = getLeafTiles();
    if (leafTiles.length === 0) return false;
    const currentIndex = leafTiles.findIndex(t => t.id === activeTileId);
    const nextIndex = (currentIndex + 1) % leafTiles.length;
    setActiveTileId(leafTiles[nextIndex].id);
    return true;
  }, [activeTileId, getLeafTiles]);

  const focusPrevTile = useCallback(() => {
    const leafTiles = getLeafTiles();
    if (leafTiles.length === 0) return false;
    const currentIndex = leafTiles.findIndex(t => t.id === activeTileId);
    const prevIndex = currentIndex === 0 ? leafTiles.length - 1 : currentIndex - 1;
    setActiveTileId(leafTiles[prevIndex].id);
    return true;
  }, [activeTileId, getLeafTiles]);

  // ============================================
  // CommandRegistry Methods
  // ============================================

  /**
   * Create a new command in the active tab's registry
   * @param {string} type - Command type (canvas, dashboard, etc.)
   * @param {object} args - Command arguments
   * @returns {string|null} Command ID or null if no active tab
   */
  const createCommand = useCallback((type, args = {}) => {
    if (!activeTabId) return null;
    const registry = getRegistry(activeTabId);
    return registry.create(type, args);
  }, [activeTabId, getRegistry]);

  /**
   * Delete a command from the active tab's registry
   * @param {string} commandId - Command ID to delete
   * @returns {boolean} True if deleted
   */
  const deleteCommand = useCallback((commandId) => {
    if (!activeTabId) return false;
    const registry = getRegistry(activeTabId);
    return registry.delete(commandId);
  }, [activeTabId, getRegistry]);

  /**
   * Get a command from the active tab's registry
   * @param {string} commandId - Command ID
   * @returns {Object|undefined} Command entry
   */
  const getCommand = useCallback((commandId) => {
    if (!activeTabId) return undefined;
    const registry = getRegistry(activeTabId);
    return registry.get(commandId);
  }, [activeTabId, getRegistry]);

  /**
   * Get all commands from the active tab's registry
   * @returns {Array} Array of command entries
   */
  const getCommands = useCallback(() => {
    if (!activeTabId) return [];
    const registry = getRegistry(activeTabId);
    return registry.getAll();
  }, [activeTabId, getRegistry]);

  /**
   * Get commands by type from the active tab's registry
   * @param {string} type - Command type to filter by
   * @returns {Array} Array of matching command entries
   */
  const getCommandsByType = useCallback((type) => {
    if (!activeTabId) return [];
    const registry = getRegistry(activeTabId);
    return registry.getByType(type);
  }, [activeTabId, getRegistry]);

  /**
   * Get the command assigned to a specific tile
   * @param {string} tileId - Tile ID
   * @returns {Object|undefined} Command entry
   */
  const getCommandByTile = useCallback((tileId) => {
    if (!activeTabId) return undefined;
    const registry = getRegistry(activeTabId);
    return registry.getByTile(tileId);
  }, [activeTabId, getRegistry]);

  /**
   * Assign a command to a tile
   * Updates both the registry and the tile's commandId in state
   * @param {string} commandId - Command ID
   * @param {string} tileId - Tile ID
   */
  const assignCommandToTile = useCallback((commandId, tileId) => {
    if (!activeTabId) return;
    const registry = getRegistry(activeTabId);
    registry.assignToTile(commandId, tileId);

    // Also update the tile's commandId in the tab structure
    setTabs(prev => prev.map(tab => {
      if (tab.id !== activeTabId) return tab;

      const updateTileCommandId = (tile) => {
        if (tile.id === tileId) {
          return { ...tile, commandId };
        }
        if (tile.children) {
          return { ...tile, children: tile.children.map(updateTileCommandId) };
        }
        return tile;
      };

      return {
        ...tab,
        rootTile: updateTileCommandId(tab.rootTile),
      };
    }));
  }, [activeTabId, getRegistry]);

  /**
   * Register a component API with the registry (called by useCommandRegistration hook)
   * @param {string} commandId - Command ID
   * @param {object} api - API object with methods
   */
  const registerCommandApi = useCallback((commandId, api) => {
    if (!activeTabId) return;
    const registry = getRegistry(activeTabId);
    registry.registerApi(commandId, api);
  }, [activeTabId, getRegistry]);

  /**
   * Unregister a component API (called by useCommandRegistration hook on unmount)
   * @param {string} commandId - Command ID
   */
  const unregisterCommandApi = useCallback((commandId) => {
    if (!activeTabId) return;
    const registry = getRegistry(activeTabId);
    registry.unregisterApi(commandId);
  }, [activeTabId, getRegistry]);

  /**
   * Register a component API in a specific tab's registry
   * @param {string} tabId - Tab ID where the component lives
   * @param {string} commandId - Command ID
   * @param {object} api - API object with methods
   */
  const registerCommandApiInTab = useCallback((tabId, commandId, api) => {
    if (!tabId) return;
    const registry = getRegistry(tabId);
    if (registry) {
      registry.registerApi(commandId, api);
    }
  }, [getRegistry]);

  /**
   * Unregister a component API from a specific tab's registry
   * @param {string} tabId - Tab ID where the component lives
   * @param {string} commandId - Command ID
   */
  const unregisterCommandApiInTab = useCallback((tabId, commandId) => {
    if (!tabId) return;
    const registry = getRegistry(tabId);
    if (registry) {
      registry.unregisterApi(commandId);
    }
  }, [getRegistry]);

  /**
   * Get command from a specific tab's registry (for agent bridge)
   * @param {string} tabId - Tab ID
   * @param {string} commandId - Command ID
   * @returns {Object|undefined} Command entry
   */
  const getCommandFromTab = useCallback((tabId, commandId) => {
    const registry = getRegistry(tabId);
    return registry.get(commandId);
  }, [getRegistry]);

  /**
   * Get commands by type from a specific tab's registry
   * @param {string} tabId - Tab ID
   * @param {string} type - Command type
   * @returns {Array} Array of matching commands
   */
  const getCommandsByTypeFromTab = useCallback((tabId, type) => {
    const registry = getRegistry(tabId);
    return registry.getByType(type);
  }, [getRegistry]);

  /**
   * Create a command in a specific tab's registry (for agent bridge)
   * @param {string} tabId - Tab ID
   * @param {string} type - Command type
   * @param {object} args - Command arguments
   * @returns {Object|null} Command entry or null if tab not found
   */
  const createCommandInTab = useCallback((tabId, type, args = {}) => {
    const registry = getRegistry(tabId);
    if (!registry) return null;
    const commandId = registry.create(type, args);
    return registry.get(commandId);
  }, [getRegistry]);

  /**
   * Assign a command to a tile in a specific tab (for agent bridge)
   * @param {string} tabId - Tab ID
   * @param {string} commandId - Command ID
   * @param {string} tileId - Tile ID
   */
  const assignCommandToTileInTab = useCallback((tabId, commandId, tileId) => {
    const registry = getRegistry(tabId);
    if (!registry) return;
    registry.assignToTile(commandId, tileId);

    // Update the tile's commandId in the tab structure
    setTabs(prev => prev.map(tab => {
      if (tab.id !== tabId) return tab;

      const updateTileCommandId = (tile) => {
        if (tile.id === tileId) {
          return { ...tile, commandId };
        }
        if (tile.children) {
          return { ...tile, children: tile.children.map(updateTileCommandId) };
        }
        return tile;
      };

      return {
        ...tab,
        rootTile: updateTileCommandId(tab.rootTile),
      };
    }));
  }, [getRegistry]);

  const value = useMemo(() => ({
    // State
    tabs,
    activeTabId,
    activeTileId,

    // Tab Management
    createTab: createNewTab,
    closeTab,
    switchToTab,
    switchToTabByIndex,
    switchToNextTab,
    switchToPrevTab,
    renameTab,
    moveTab,
    duplicateTab,
    clearAllTabs,

    // Getters
    getActiveTab,
    getTab,

    // Context Management (NEW)
    updateTabContext,
    getTabContext,
    getActiveTabContext,

    // Command Integration
    addCommandToActiveTile,
    handleLayoutCommand,

    // Tile Management (Phase 3)
    setActiveTile: setActiveTileId,
    splitTile,
    closeTile,
    resizeTile,
    getTile,
    getLeafTiles,

    // Tile Navigation
    focusNextTile,
    focusPrevTile,

    // CommandRegistry Methods
    createCommand,
    deleteCommand,
    getCommand,
    getCommands,
    getCommandsByType,
    getCommandByTile,
    assignCommandToTile,
    registerCommandApi,
    unregisterCommandApi,
    registerCommandApiInTab,
    unregisterCommandApiInTab,
    getCommandFromTab,
    getCommandsByTypeFromTab,
    createCommandInTab,
    assignCommandToTileInTab,
    splitTileInTab,
  }), [
    tabs,
    activeTabId,
    activeTileId,
    createNewTab,
    closeTab,
    switchToTab,
    switchToTabByIndex,
    switchToNextTab,
    switchToPrevTab,
    renameTab,
    moveTab,
    duplicateTab,
    clearAllTabs,
    getActiveTab,
    getTab,
    updateTabContext,
    getTabContext,
    getActiveTabContext,
    addCommandToActiveTile,
    handleLayoutCommand,
    splitTile,
    closeTile,
    resizeTile,
    getTile,
    getLeafTiles,
    focusNextTile,
    focusPrevTile,
    createCommand,
    deleteCommand,
    getCommand,
    getCommands,
    getCommandsByType,
    getCommandByTile,
    assignCommandToTile,
    registerCommandApi,
    unregisterCommandApi,
    registerCommandApiInTab,
    unregisterCommandApiInTab,
    getCommandFromTab,
    getCommandsByTypeFromTab,
    createCommandInTab,
    assignCommandToTileInTab,
    splitTileInTab,
  ]);

  return (
    <TabManagerContext.Provider value={value}>
      {children}
    </TabManagerContext.Provider>
  );
};

export default TabManagerContext;
