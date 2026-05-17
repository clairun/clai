/**
 * useAgentBridge Hook
 *
 * This hook initializes the agent bridge and registers tool handlers
 * that interact with the TabManager, Dashboard, Canvas, and Chat.
 *
 * Tool Categories:
 * - agent.* - Agent lifecycle (setup)
 * - dashboard.* - Chart management (addChart, removeChart, etc.)
 * - tabs.* - Tile layout management (splitTile, removeTile, etc.)
 * - canvas.* - Node-based canvas (addChart, addStatusBadge, addText, addEdge, etc.)
 * - chat.* - Agent text communication (message)
 *
 * Usage:
 * ```jsx
 * // In App.jsx or a top-level component
 * function App() {
 *   useAgentBridge();
 *   return <...>;
 * }
 * ```
 */

import { useEffect, useRef } from 'react';
import { getMcpServers } from '../api/client';
import { useTabManager } from '../contexts/TabManagerContext';
import { useCommand } from '../contexts/CommandContext';
import { useWorkspaceStore } from '../stores/workspaceStore';
import { writeWorkspaceFile } from '../workspace/client';
import {
  initAgentBridge,
  cleanupAgentBridge,
  registerToolHandler,
  unregisterToolHandler,
  setAgentTab,
  getAgentTab,
  clearAgentTab,
  getTabCreationLock,
  setTabCreationLock,
  clearTabCreationLock,
  getAgentTabId,
} from './bridge';

const MCP_SERVERS_CHANGED_EVENT = 'mcp-servers-changed';
const DEFAULT_CANVAS_STATE = { nodes: [], edges: [] };
const DEFAULT_DASHBOARD_STATE = { elements: [], timeRange: '1h' };

const getCommandState = (commandId, fallbackState) => {
  const cmd = useWorkspaceStore.getState().commands[commandId];
  return cmd?.state || fallbackState;
};

const getCanvasState = (commandId) => getCommandState(commandId, DEFAULT_CANVAS_STATE);
const getDashboardState = (commandId) => getCommandState(commandId, DEFAULT_DASHBOARD_STATE);

const getCanvasArtifactPath = (commandId) => `visualizations/canvas-${commandId}.canvas`;
const getDashboardArtifactPath = (commandId) => `visualizations/dashboard-${commandId}.dashboard.json`;

const serializeCanvasArtifact = (state = DEFAULT_CANVAS_STATE) => JSON.stringify({
  nodes: state.nodes || [],
  edges: state.edges || [],
}, null, 2);

const serializeDashboardArtifact = (state = DEFAULT_DASHBOARD_STATE) => JSON.stringify({
  elements: state.elements || [],
  timeRange: state.timeRange || state.selectedInterval?.label || DEFAULT_DASHBOARD_STATE.timeRange,
}, null, 2);

const createCanvasNode = (id, type, position, data) => {
  const defaultDimensions = {
    chart: { width: data?.width || 450, height: data?.height || 350 },
    markdown: { width: data?.width || 400, height: data?.height || 200 },
    statusBadge: { width: data?.width || 200, height: data?.height || 120 },
  };
  const dims = defaultDimensions[type] || { width: 300, height: 200 };

  return {
    id,
    type,
    position,
    data,
    style: { width: dims.width, height: dims.height },
  };
};

const createCanvasEdge = (id, sourceId, targetId, options = {}) => ({
  id,
  source: sourceId,
  target: targetId,
  type: 'smoothstep',
  animated: options.animated === true,
  label: options.label || undefined,
});

const createDashboardElement = (id, config) => ({
  id,
  type: 'context-chart',
  config,
});

const persistVisualizationArtifact = async (workspaceId, commandType, commandId) => {
  if (!workspaceId || !commandId) {
    return;
  }

  if (commandType === 'canvas') {
    await writeWorkspaceFile(
      workspaceId,
      getCanvasArtifactPath(commandId),
      serializeCanvasArtifact(getCanvasState(commandId))
    );
    return;
  }

  if (commandType === 'dashboard') {
    await writeWorkspaceFile(
      workspaceId,
      getDashboardArtifactPath(commandId),
      serializeDashboardArtifact(getDashboardState(commandId))
    );
  }
};

/**
 * Create a store-backed fallback canvas API for when the Canvas component
 * isn't mounted (e.g., agent tab not active).  Manipulates Zustand state
 * directly — the Canvas component will pick up changes on next mount.
 */
const createStoreBackedCanvasApi = (commandId) => {
  const generateNodeId = () => `node_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
  const generateEdgeId = () => `edge_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

  const atomicUpdate = (updater) => {
    useWorkspaceStore.getState().updateCommandStateAtomic(commandId, updater);
  };

  const getState = () => {
    return getCanvasState(commandId);
  };

  return {
    addNode: (type, position, data) => {
      const id = generateNodeId();
      atomicUpdate((state) => ({
        nodes: [...(state.nodes || []), createCanvasNode(id, type, position, data)],
      }));
      return id;
    },
    removeNode: (nodeId) => {
      atomicUpdate((state) => ({
        nodes: (state.nodes || []).filter((n) => n.id !== nodeId),
        edges: (state.edges || []).filter((e) => e.source !== nodeId && e.target !== nodeId),
      }));
    },
    updateNode: (nodeId, updates) => {
      atomicUpdate((state) => ({
        nodes: (state.nodes || []).map((n) =>
          n.id === nodeId
            ? {
                ...n,
                ...(updates.position ? { position: updates.position } : {}),
                ...(updates.data ? { data: { ...n.data, ...updates.data } } : {}),
              }
            : n
        ),
      }));
    },
    addEdge: (sourceId, targetId, options = {}) => {
      const id = generateEdgeId();
      atomicUpdate((state) => ({
        edges: [...(state.edges || []), { id, source: sourceId, target: targetId, label: options.label, animated: options.animated || false }],
      }));
      return id;
    },
    removeEdge: (edgeId) => {
      atomicUpdate((state) => ({
        edges: (state.edges || []).filter((e) => e.id !== edgeId),
      }));
    },
    getNodes: () => getState().nodes || [],
    getEdges: () => getState().edges || [],
    clear: () => {
      atomicUpdate(() => ({ nodes: [], edges: [] }));
    },
  };
};

/**
 * Create a store-backed fallback dashboard API for when the Dashboard
 * component isn't mounted.
 */
const createStoreBackedDashboardApi = (commandId) => {
  const generateElementId = () => `el_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

  const atomicUpdate = (updater) => {
    useWorkspaceStore.getState().updateCommandStateAtomic(commandId, updater);
  };

  const getState = () => {
    return getDashboardState(commandId);
  };

  return {
    addChart: (config) => {
      const elementId = generateElementId();
      atomicUpdate((state) => ({
        elements: [...(state.elements || []), createDashboardElement(elementId, config)],
      }));
      return elementId;
    },
    removeChart: (chartId) => {
      atomicUpdate((state) => ({
        elements: (state.elements || []).filter((el) => el.id !== chartId),
      }));
      return true;
    },
    getCharts: () => (getState().elements || []).filter((el) => el.type === 'context-chart'),
    getChartsDetailed: () => (getState().elements || []).filter((el) => el.type === 'context-chart'),
    hasElement: (elementId) => (getState().elements || []).some((el) => el.id === elementId),
    setTimeRange: (range) => {
      atomicUpdate(() => ({ timeRange: range }));
      return true;
    },
    clearCharts: () => {
      atomicUpdate(() => ({ elements: [] }));
    },
  };
};

/**
 * Generate a unique chart ID
 */
const generateChartId = () => `chart_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

const hasValidTileStructure = (tab) => Boolean(
  tab?.rootTile?.id && (tab.rootTile.type === 'leaf' || tab.rootTile.type === 'split')
);

const getAnomaliesTarget = (command) => {
  const args = command?.args || {};
  const options = args.options || {};
  return {
    spaceId: args.spaceId || options.spaceId || options['space-id'] || '',
    roomId: args.roomId || options.roomId || options['room-id'] || '',
    mcpServerId: args.mcpServerId || options.mcpServerId || options['mcp-server-id'] || null,
  };
};

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * Hook to initialize and connect the agent bridge to TabManager
 */
export const useAgentBridge = () => {
  const tabManager = useTabManager();
  const { executeCommand } = useCommand();
  const initializedRef = useRef(false);

  // Get TabManager functions we need
  const {
    tabs,
    createTab,
    splitTile,
    closeTile,
    getTile,
    getActiveTab,
    getCommandFromTab,
  } = tabManager;

  // Store tabManager ref for handlers (avoids stale closure issues)
  const tabManagerRef = useRef(tabManager);
  tabManagerRef.current = tabManager;

  // Store executeCommand ref for handlers
  const executeCommandRef = useRef(executeCommand);
  executeCommandRef.current = executeCommand;
  const mcpServersRef = useRef([]);

  useEffect(() => {
    let cancelled = false;

    const loadMcpServers = async () => {
      try {
        const servers = await getMcpServers();
        if (!cancelled) {
          mcpServersRef.current = servers || [];
        }
      } catch (error) {
        console.error('[AgentBridge] Failed to load MCP servers:', error);
        if (!cancelled) {
          mcpServersRef.current = [];
        }
      }
    };

    loadMcpServers();
    window.addEventListener(MCP_SERVERS_CHANGED_EVENT, loadMcpServers);

    return () => {
      cancelled = true;
      window.removeEventListener(MCP_SERVERS_CHANGED_EVENT, loadMcpServers);
    };
  }, []);

  const waitForTab = async (tabId, timeoutMs = 2000, intervalMs = 50) => {
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      const candidate = tabManagerRef.current.tabs.find((tab) => tab.id === tabId);
      if (hasValidTileStructure(candidate)) {
        return candidate;
      }
      await sleep(intervalMs);
    }
    return null;
  };

  const resolveNetdataMcpServerId = async (agentTab, explicitMcpServerId = null) => {
    if (mcpServersRef.current.length === 0) {
      try {
        mcpServersRef.current = (await getMcpServers()) || [];
      } catch (error) {
        console.error('[AgentBridge] Failed to refresh MCP servers:', error);
      }
    }

    const enabledServerIds = agentTab?.mcpServerIds || [];
    const enabledServers = mcpServersRef.current.filter((server) => (
      server.enabled && enabledServerIds.includes(server.id)
    ));
    const netdataServers = enabledServers.filter((server) => server.integrationType === 'netdata_cloud');

    if (explicitMcpServerId) {
      const selectedServer = netdataServers.find((server) => server.id === explicitMcpServerId);
      if (!selectedServer) {
        throw new Error(`Netdata MCP server not enabled for this tab/session: ${explicitMcpServerId}`);
      }
      return selectedServer.id;
    }

    if (netdataServers.length === 0) {
      if (enabledServers.length === 1) {
        return enabledServers[0].id;
      }
      throw new Error('No Netdata MCP server is enabled for this tab/session');
    }

    if (netdataServers.length > 1) {
      throw new Error('Multiple Netdata MCP servers are enabled for this tab/session. Attach only one Netdata server to use chart tools.');
    }

    return netdataServers[0].id;
  };

  const getEnabledMcpServerIdsForTab = (tabId, agentTab = null) => {
    const fromAgentTab = agentTab?.mcpServerIds || [];
    const tab = tabManagerRef.current.tabs.find((candidate) => candidate.id === tabId);
    const attachedFromTab = tab?.context?.mcpServers?.attachedServerIds
      || tab?.context?.mcpServers?.selectedServerIds
      || [];
    const disabledFromTab = new Set(tab?.context?.mcpServers?.disabledServerIds || []);
    const enabledFromTab = attachedFromTab.filter((id) => !disabledFromTab.has(id));

    return [...new Set([...fromAgentTab, ...enabledFromTab])];
  };

  const resolveNetdataMcpServerIdForTab = async (tabId, agentTab, explicitMcpServerId = null) => {
    const agentTabWithFallback = {
      ...agentTab,
      mcpServerIds: getEnabledMcpServerIdsForTab(tabId, agentTab),
    };

    return resolveNetdataMcpServerId(agentTabWithFallback, explicitMcpServerId);
  };

  const normalizeGroupBy = (groupBy) => {
    if (!groupBy) {
      return [];
    }
    if (Array.isArray(groupBy)) {
      return groupBy.filter((value) => typeof value === 'string' && value.trim() !== '');
    }
    if (typeof groupBy === 'string' && groupBy.trim() !== '') {
      return [groupBy.trim()];
    }
    return [];
  };

  const normalizeFilterBy = (filterBy) => {
    const convertFilterEntriesToMap = (entries) => {
      if (!Array.isArray(entries)) {
        return null;
      }

      const result = {};
      let foundStructuredEntry = false;

      for (const entry of entries) {
        if (!entry || typeof entry !== 'object' || Array.isArray(entry)) {
          continue;
        }

        const label = typeof entry.label === 'string' ? entry.label.trim() : '';
        const value = typeof entry.value === 'string' ? entry.value.trim() : '';
        if (!label || !value) {
          continue;
        }

        foundStructuredEntry = true;
        if (!Array.isArray(result[label])) {
          result[label] = [];
        }
        if (!result[label].includes(value)) {
          result[label].push(value);
        }
      }

      return foundStructuredEntry ? result : null;
    };

    if (!filterBy) {
      return {};
    }
    if (typeof filterBy === 'string') {
      try {
        const parsed = JSON.parse(filterBy);
        if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
          return parsed;
        }
        const normalizedEntries = convertFilterEntriesToMap(parsed);
        if (normalizedEntries) {
          return normalizedEntries;
        }
      } catch {
        return {};
      }
      return {};
    }
    const normalizedEntries = convertFilterEntriesToMap(filterBy);
    if (normalizedEntries) {
      return normalizedEntries;
    }
    if (typeof filterBy === 'object' && !Array.isArray(filterBy)) {
      return filterBy;
    }
    return {};
  };

  /**
   * Setup an agent's tab with dashboard command.
   * Called BEFORE CLI starts to avoid race conditions.
   * Does NOT switch to the new tab - avoids interrupting user activity.
   *
   * @param {string} agentId - Agent identifier
   * @param {string} agentName - Human-readable agent name
   * @param {string} spaceId - Netdata space ID
   * @param {string} roomId - Netdata room ID
   * @returns {{ tabId: string }} The created/existing tab ID
   */
  const setupAgentTab = (agentId, agentName, spaceId, roomId, mcpServerIds = []) => {
    // Check if we already have a tab for this agent in the Map
    const tabInfo = getAgentTab(agentId, spaceId, roomId);

    if (tabInfo?.tabId) {
      // Verify the tab still exists
      const existingTab = tabManagerRef.current.tabs.find(t => t.id === tabInfo.tabId);
      if (existingTab) {
        // Tab already exists, no need to create
        return { tabId: tabInfo.tabId };
      }
      // Tab was removed, clear the mapping
      clearAgentTab(agentId, spaceId, roomId);
    }

    // Also check for an existing dedicated agent tab by persisted agent context
    // (handles app reload where the in-memory Map is lost).
    const existingAgentTab = tabManagerRef.current.tabs.find(
      (t) => t.context?.agent?.agentId === agentId
    );

    if (existingAgentTab) {
      // Found existing tab, restore the mapping (with agent name)
      tabManagerRef.current.updateTabContext(existingAgentTab.id, {
        mcpServers: {
          attachedServerIds: mcpServerIds,
          disabledServerIds: [],
        },
        agent: {
          agentId,
          agentName,
        },
      });
      setAgentTab(agentId, spaceId, roomId, existingAgentTab.id, agentName, mcpServerIds);
      return { tabId: existingAgentTab.id };
    }

    // Create a new tab for this agent with bot icon
    // TODO: Redesign to avoid switching to the new tab (don't interrupt user)
    // Current limitation: createTab always switches, and executeCommand assigns
    // to the active tab. Need TabManagerContext changes to support creating
    // a tab with a command without switching.
    const title = `🤖 ${agentName}`;
    const newTab = tabManagerRef.current.createTab(title);

    // Update the tab's context with the automation binding
    tabManagerRef.current.updateTabContext(newTab.id, {
      mcpServers: {
        attachedServerIds: mcpServerIds,
        disabledServerIds: [],
      },
      agent: {
        agentId,
        agentName,
      },
    });

    // Execute canvas command - agents communicate visually through canvas
    executeCommandRef.current('canvas');

    // Store the mapping (with agent name for future recreation)
    setAgentTab(agentId, spaceId, roomId, newTab.id, agentName, mcpServerIds);

    return { tabId: newTab.id, rootTileId: newTab.rootTile?.id || null };
  };

  /**
   * Get an agent's tab (if exists).
   * Used by tool handlers after setup.
   *
   * @param {string} agentId - Agent identifier
   * @param {string} spaceId - Netdata space ID
   * @param {string} roomId - Netdata room ID
   * @returns {string|null} Tab ID or null
   */
  const getAgentTabId = (agentId, spaceId, roomId) => {
    const tabInfo = getAgentTab(agentId, spaceId, roomId);
    if (tabInfo?.tabId) {
      // Verify tab still exists
      const existingTab = tabManagerRef.current.tabs.find(t => t.id === tabInfo.tabId);
      if (existingTab) {
        return tabInfo.tabId;
      }
      // Tab was removed but we still have the mapping - don't clear it yet
      // (ensureAgentTab will recreate the tab using the stored agentName)
    }
    return null;
  };

  /**
   * Ensure an agent has a tab, recreating it if the user closed it.
   * This allows agents to continue working even if their tab was closed.
   * Uses a lock to prevent duplicate tab creation from rapid tool calls.
   *
   * @param {string} agentId - Agent identifier
   * @param {string} spaceId - Netdata space ID
   * @param {string} roomId - Netdata room ID
   * @returns {string} Tab ID (existing or newly created)
   */
  const ensureAgentTab = (agentId, spaceId, roomId, fallbackTabId = null, fallbackMcpServerIds = []) => {
    // First check if tab exists (fast path)
    const existingTabId = getAgentTabId(agentId, spaceId, roomId);
    if (existingTabId) {
      return existingTabId;
    }

    if (fallbackTabId) {
      const fallbackTab = tabManagerRef.current.tabs.find((tab) => tab.id === fallbackTabId);
      if (hasValidTileStructure(fallbackTab)) {
        setAgentTab(agentId, spaceId, roomId, fallbackTabId, agentId, fallbackMcpServerIds);
        return fallbackTabId;
      }
    }

    // Check if another call is already creating this tab
    // Since setupAgentTab is synchronous, if we see a lock, creation is done
    // and we should re-check the mapping
    if (getTabCreationLock(agentId, spaceId, roomId)) {
      // Creation was in progress - by now it should be done (sync code)
      // Re-check the mapping
      const tabIdAfterLock = getAgentTabId(agentId, spaceId, roomId);
      if (tabIdAfterLock) {
        return tabIdAfterLock;
      }
      // Still no tab? Fall through to create (lock holder may have failed)
    }

    // Tab doesn't exist - recreate it
    const tabInfo = getAgentTab(agentId, spaceId, roomId);
    const agentName = tabInfo?.agentName || agentId;
    const mcpServerIds = tabInfo?.mcpServerIds?.length ? tabInfo.mcpServerIds : fallbackMcpServerIds;

    console.log(`[AgentBridge] Recreating closed tab for agent: ${agentId}`);

    // Set lock before creating (value doesn't matter, just presence)
    setTabCreationLock(agentId, spaceId, roomId, true);

    try {
      // Use setupAgentTab to create a new tab (synchronous)
      const result = setupAgentTab(agentId, agentName, spaceId, roomId, mcpServerIds);
      return result.tabId;
    } finally {
      // Clear lock - mapping is already updated by setupAgentTab
      clearTabCreationLock(agentId, spaceId, roomId);
    }
  };

  /**
   * Helper to get a command from a request.
   * Handles tab lookup, command lookup, and validation.
   * Automatically recreates the tab if it was closed by the user.
   *
   * @param {string} commandType - Expected command type (canvas, dashboard, etc.)
   * @param {Object} request - Tool request object
   * @param {Object} options - Options
   * @param {boolean} options.required - If true, throws on failure. If false, returns null.
   * @returns {Object|null} Command entry with api, or null/throws
   */
  const getCommandByType = (commandType, request, { required = true } = {}) => {
    const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
    const { commandId } = params;

    // Use ensureAgentTab to recreate tab if user closed it
    const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);
    if (!tabId) {
      if (required) {
        throw new Error('No tab found for this agent. Call agent.setup first.');
      }
      return null;
    }

    const command = tabManagerRef.current.getCommandFromTab(tabId, commandId);
    if (!command) {
      if (required) {
        throw new Error(`${commandType} not found: ${commandId}`);
      }
      return null;
    }
    if (command.type !== commandType) {
      if (required) {
        throw new Error(`Command is not a ${commandType}: ${commandId} (type: ${command.type})`);
      }
      return null;
    }

    if (!command.api) {
      // Component not mounted — use store-backed fallback for canvas
      if (commandType === 'canvas') {
        return { ...command, api: createStoreBackedCanvasApi(commandId), isStoreBacked: true };
      }
      if (required) {
        throw new Error(`${commandType} not ready: ${commandId}`);
      }
      return null;
    }

    return command;
  };

  // Convenience wrapper for canvas commands
  const getCanvasCommand = (request, options) => getCommandByType('canvas', request, options);

  useEffect(() => {
    // Prevent double initialization (React StrictMode)
    if (initializedRef.current) {
      return;
    }
    initializedRef.current = true;

    // Initialize the bridge
    initAgentBridge();

    // Register agent setup handler (called BEFORE CLI starts)
    // For scheduled agents: creates a new tab
    // For on-demand agents: uses an existing tabId if it has valid tile structure
    registerToolHandler('agent.setup', async (request) => {
      const { agentId, spaceId, roomId, params } = request;
      const {
        agentName,
        tabId: existingTabId,
        mcpServerIds = [],
        managedAgentTab = true,
      } = params;

      // If tabId is provided (on-demand agent), check if it has a valid tile structure
      if (existingTabId) {
        let existingTab = tabManagerRef.current.tabs.find(t => t.id === existingTabId);

        if (!hasValidTileStructure(existingTab)) {
          existingTab = await waitForTab(existingTabId);
        }

        if (hasValidTileStructure(existingTab)) {
          const nextContext = {
            agent: managedAgentTab
              ? {
                agentId,
                agentName: agentName || 'Clai',
              }
              : null,
          };
          if (managedAgentTab) {
            nextContext.mcpServers = {
              attachedServerIds: mcpServerIds,
              disabledServerIds: [],
            };
          }
          tabManagerRef.current.updateTabContext(existingTabId, nextContext);
          setAgentTab(agentId, spaceId, roomId, existingTabId, agentName || 'Clai', mcpServerIds);
          console.log(`[AgentBridge] Agent tab set for on-demand: ${existingTabId}, rootTile: ${existingTab.rootTile.id}`);
          return { tabId: existingTabId, rootTileId: existingTab.rootTile.id };
        }

        // Tab doesn't have valid tile structure - log details for debugging
        console.warn(`[AgentBridge] Tab ${existingTabId} has invalid tile structure:`, {
          hasTab: !!existingTab,
          hasRootTile: !!existingTab?.rootTile,
          rootTileId: existingTab?.rootTile?.id,
          rootTileType: existingTab?.rootTile?.type,
        });
      }

      // Create a new tab for the agent (scheduled agents or tabs without valid tiles)
      const result = setupAgentTab(agentId, agentName || 'Clai', spaceId, roomId, mcpServerIds);

      console.log(`[AgentBridge] Agent tab setup complete: ${result.tabId}`);
      return result;
    });

    // Helper to get dashboard command from agent's tab
    const getDashboardFromTab = (tabId, commandId = null) => {
      let command;
      if (commandId) {
        command = tabManagerRef.current.getCommandFromTab(tabId, commandId);
      if (!command || command.type !== 'dashboard') return null;
      } else {
        const dashboards = tabManagerRef.current.getCommandsByTypeFromTab(tabId, 'dashboard');
        command = dashboards.length > 0 ? dashboards[0] : null;
      }
      if (!command) return null;
      // Provide store-backed fallback when component isn't mounted
      if (!command.api) {
        return { ...command, api: createStoreBackedDashboardApi(command.id), isStoreBacked: true };
      }
      return command;
    };

    const getAnomaliesFromTab = (tabId, commandId = null) => {
      if (commandId) {
        const command = tabManagerRef.current.getCommandFromTab(tabId, commandId);
        if (!command || command.type !== 'anomalies') {
          return null;
        }
        return command;
      }

      const anomalies = tabManagerRef.current.getCommandsByTypeFromTab(tabId, 'anomalies');
      return anomalies.length > 0 ? anomalies[0] : null;
    };

    const findTileInTree = (tile, tileId) => {
      if (!tile) {
        return null;
      }
      if (tile.id === tileId) {
        return tile;
      }
      if (!Array.isArray(tile.children)) {
        return null;
      }
      for (const child of tile.children) {
        const match = findTileInTree(child, tileId);
        if (match) {
          return match;
        }
      }
      return null;
    };

    registerToolHandler('anomalies.open', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);
      const requestedSpaceId = typeof params?.spaceId === 'string' ? params.spaceId.trim() : '';
      const requestedRoomId = typeof params?.roomId === 'string' ? params.roomId.trim() : '';

      if (!requestedSpaceId || !requestedRoomId) {
        throw new Error('anomalies.open requires spaceId and roomId');
      }

      const tab = tabManagerRef.current.tabs.find((candidate) => candidate.id === tabId);
      if (!tab) {
        throw new Error(`Tab not found: ${tabId}`);
      }

      const targetMatches = (command) => {
        const target = command?.api?.getTarget?.() || getAnomaliesTarget(command);
        return target.spaceId === requestedSpaceId && target.roomId === requestedRoomId;
      };

      if (params?.commandId) {
        const command = getAnomaliesFromTab(tabId, params.commandId);
        if (!command) {
          throw new Error(`Anomalies command not found: ${params.commandId}`);
        }
        if (!targetMatches(command)) {
          throw new Error('Requested anomalies command is bound to a different target');
        }
        return { commandId: command.id, tileId: command.tileId, reused: true };
      }

      const matchingCommand = tabManagerRef.current
        .getCommandsByTypeFromTab(tabId, 'anomalies')
        .find((command) => targetMatches(command));

      if (matchingCommand) {
        return { commandId: matchingCommand.id, tileId: matchingCommand.tileId, reused: true };
      }

      const splitType = params?.splitType === 'horizontal' ? 'horizontal' : 'vertical';
      const parentTileId = params?.parentTileId || tab.rootTile?.id;
      const parentTile = findTileInTree(tab.rootTile, parentTileId);
      if (!parentTile) {
        throw new Error(`Parent tile not found: ${parentTileId}`);
      }

      let targetTileId = null;
      if (parentTile.type === 'leaf' && !parentTile.commandId) {
        targetTileId = parentTile.id;
      } else {
        const splitResult = tabManagerRef.current.splitTileInTab(tabId, parentTile.id, splitType);
        if (!splitResult.success) {
          throw new Error(splitResult.message || 'Failed to create anomalies tile');
        }
        targetTileId = splitResult.newTileId;
      }

      const command = tabManagerRef.current.createCommandInTab(tabId, 'anomalies', {
        spaceId: requestedSpaceId,
        roomId: requestedRoomId,
      });

      if (!command) {
        throw new Error('Failed to create anomalies command');
      }

      tabManagerRef.current.assignCommandToTileInTab(tabId, command.id, targetTileId);

      return { commandId: command.id, tileId: targetTileId, reused: false };
    });

    // Register dashboard tool handlers
    registerToolHandler('dashboard.addChart', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);
      const agentTab = getAgentTab(agentId, spaceId, roomId);

      if (!params?.spaceId || !params?.roomId) {
        throw new Error('dashboard.addChart requires spaceId and roomId');
      }
      const mcpServerId = await resolveNetdataMcpServerIdForTab(tabId, agentTab, params.mcpServerId);

      const dashboard = getDashboardFromTab(tabId, params.commandId);
      if (!dashboard) {
        throw new Error('No dashboard found. Create one with tabs.splitTile({ commandType: "dashboard" })');
      }

      const normalizedGroupBy = normalizeGroupBy(params.groupBy);
      const normalizedFilterBy = normalizeFilterBy(params.filterBy);
      const chartConfig = {
        mcpServerId,
        spaceId: params.spaceId,
        roomId: params.roomId,
        context: params.context,
        groupBy: normalizedGroupBy,
        filterBy: normalizedFilterBy,
      };

      const chartId = dashboard.api.addChart(chartConfig);
      if (!chartId) {
        throw new Error('Failed to add dashboard chart');
      }

      if (!dashboard.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(dashboard.id, (state) => ({
          elements: [...(state.elements || []), createDashboardElement(chartId, chartConfig)],
        }));
      }
      await persistVisualizationArtifact(agentId, 'dashboard', dashboard.id);

      return { chartId };
    });

    registerToolHandler('dashboard.removeChart', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);

      const dashboard = getDashboardFromTab(tabId, params.commandId);
      if (!dashboard) {
        throw new Error('Dashboard not found');
      }

      dashboard.api.removeChart(params.chartId);
      if (!dashboard.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(dashboard.id, (state) => ({
          elements: (state.elements || []).filter((el) => el.id !== params.chartId),
        }));
      }
      await persistVisualizationArtifact(agentId, 'dashboard', dashboard.id);
      return { success: true };
    });

    registerToolHandler('dashboard.clearCharts', async (request) => {
      const { agentId, spaceId, roomId, params } = request;
      const tabId = getAgentTabId(agentId, spaceId, roomId);

      if (!tabId) {
        return { success: true };
      }

      const dashboard = getDashboardFromTab(tabId, params?.commandId);
      if (!dashboard) {
        return { success: true };
      }

      dashboard.api.clearCharts();
      if (!dashboard.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(dashboard.id, () => ({
          elements: [],
        }));
      }
      await persistVisualizationArtifact(agentId, 'dashboard', dashboard.id);
      return { success: true };
    });

    registerToolHandler('dashboard.setTimeRange', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);

      const dashboard = getDashboardFromTab(tabId, params?.commandId);
      if (!dashboard) {
        throw new Error('Dashboard not found');
      }

      const success = dashboard.api.setTimeRange(params.range);
      if (!success) {
        throw new Error(`Invalid time range: ${params.range}`);
      }

      // Update tab context with time range (for backwards compatibility)
      // The dashboard component will read this from context
      tabManagerRef.current.updateTabContext(tabId, {
        dashboard: {
          ...tabManagerRef.current.getTabContext(tabId)?.dashboard,
          timeRange: params.range,
        },
      });

      if (!dashboard.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(dashboard.id, () => ({
          timeRange: params.range,
        }));
      }
      await persistVisualizationArtifact(agentId, 'dashboard', dashboard.id);

      return { success: true, range: params.range };
    });

    // Register tabs tool handlers
    registerToolHandler('tabs.splitTile', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);

      // Get the parent tile ID - use the root tile if not specified or if "root" is passed
      let parentTileId = params.parentTileId;
      const tab = tabManagerRef.current.tabs.find(t => t.id === tabId);

      if (!tab) {
        throw new Error(`Tab not found: ${tabId}`);
      }

      // Handle special cases: no parentTileId, or "root" as a keyword
      if (!parentTileId || parentTileId === 'root') {
        // Get the root tile of the tab - validate it exists and has a valid structure
        if (!tab.rootTile?.id) {
          throw new Error(`Tab ${tabId} has no valid rootTile. Structure: ${JSON.stringify({
            hasRootTile: !!tab.rootTile,
            rootTileId: tab.rootTile?.id,
            rootTileType: tab.rootTile?.type,
          })}`);
        }
        parentTileId = tab.rootTile.id;
        console.log(`[AgentBridge] Using rootTile as parent: ${parentTileId}`);
      }

      // Split the tile in the agent's tab
      // 'vertical' = side by side, 'horizontal' = stacked
      console.log(`[AgentBridge] Splitting tile: tabId=${tabId}, parentTileId=${parentTileId}, splitType=${params.splitType}`);
      const result = tabManagerRef.current.splitTileInTab(
        tabId,
        parentTileId,
        params.splitType
      );

      if (!result.success) {
        // Add more context to the error for debugging
        console.error(`[AgentBridge] splitTile failed:`, {
          tabId,
          parentTileId,
          splitType: params.splitType,
          rootTileId: tab.rootTile?.id,
          rootTileType: tab.rootTile?.type,
          errorMessage: result.message,
        });
        throw new Error(`${result.message}. Tab: ${tabId}, Tile: ${parentTileId}`);
      }

      const response = { tileId: result.newTileId };

      // If commandType is provided, create a command in the new tile
      if (params.commandType) {
        const command = tabManagerRef.current.createCommandInTab(
          tabId,
          params.commandType,
          {}
        );

        if (command) {
          // Assign the command to the new tile
          tabManagerRef.current.assignCommandToTileInTab(tabId, command.id, result.newTileId);
          response.commandId = command.id;
        }
      }

      return response;
    });

    registerToolHandler('tabs.removeTile', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);

      const result = tabManagerRef.current.closeTile(params.tileId);

      if (!result.success) {
        throw new Error(result.message);
      }

      return { success: true };
    });

    registerToolHandler('tabs.getTileLayout', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [] } = request;
      // Use ensureAgentTab to recreate tab if user closed it
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);

      const tab = tabManagerRef.current.tabs.find(t => t.id === tabId);
      if (!tab) {
        throw new Error('Tab not found');
      }

      // Helper to get content details based on command type
      // Returns enough info for agent to decide if content can be reused
      const getContentDetails = (command) => {
        if (command.type === 'canvas') {
          const state = getCanvasState(command.id);
          // Get all canvas nodes with their data
          const nodes = state.nodes || [];
          return {
            nodeCount: nodes.length,
            nodes: nodes.map(n => {
              const summary = { nodeId: n.id, nodeType: n.type };

              // Include type-specific info so agent can evaluate content
              if (n.type === 'chart') {
                summary.context = n.data?.context || null; // e.g., "system.cpu"
                summary.title = n.data?.title || null;
              } else if (n.type === 'statusBadge') {
                summary.status = n.data?.status || null; // e.g., "healthy"
                summary.title = n.data?.title || null;
                summary.message = n.data?.message?.substring(0, 100) || null;
              } else if (n.type === 'markdown') {
                // First 150 chars of content to understand what it's about
                summary.contentPreview = n.data?.content?.substring(0, 150) || null;
              } else {
                summary.title = n.data?.title || null;
              }

              return summary;
            }),
          };
        }

        if (command.type === 'dashboard') {
          const state = getDashboardState(command.id);
          // Get all dashboard charts
          const charts = (state.elements || []).filter((el) => el.type === 'context-chart');
          return {
            chartCount: charts.length,
            charts: charts.map(c => ({
              chartId: c.id,
              context: c.config?.context,
            })),
          };
        }

        if (!command?.api) return null;

        if (command.type === 'anomalies') {
          const target = command.api?.getTarget?.() || getAnomaliesTarget(command);
          const items = command.api?.getItems?.() || [];
          return {
            spaceId: target.spaceId || null,
            roomId: target.roomId || null,
            itemCount: items.length,
          };
        }

        return null;
      };

      // Convert tile tree to layout format (includes commandId AND content for discovery)
      const convertTile = (tile) => {
        if (tile.type === 'leaf') {
          // Look up the command to get its type and content
          const command = tile.commandId
            ? tabManagerRef.current.getCommandFromTab(tabId, tile.commandId)
            : null;

          const content = command ? getContentDetails(command) : null;

          return {
            tileId: tile.id,
            commandId: tile.commandId || null,
            command: command?.type || null,
            content, // Include actual content details!
            splitType: null,
            children: [],
          };
        }

        return {
          tileId: tile.id,
          commandId: null,
          command: null,
          content: null,
          splitType: tile.direction,
          children: tile.children.map(convertTile),
        };
      };

      // Build the tree
      const root = convertTile(tab.rootTile);

      // Collect all canvases and dashboards for easy access
      const collectCommands = (tile, result = { canvases: [], dashboards: [], anomalies: [] }) => {
        if (tile.command === 'canvas' && tile.commandId) {
          result.canvases.push({
            commandId: tile.commandId,
            nodeCount: tile.content?.nodeCount || 0,
            nodes: tile.content?.nodes || [],
          });
        } else if (tile.command === 'dashboard' && tile.commandId) {
          result.dashboards.push({
            commandId: tile.commandId,
            chartCount: tile.content?.chartCount || 0,
            charts: tile.content?.charts || [],
          });
        } else if (tile.command === 'anomalies' && tile.commandId) {
          result.anomalies.push({
            commandId: tile.commandId,
            spaceId: tile.content?.spaceId || null,
            roomId: tile.content?.roomId || null,
            itemCount: tile.content?.itemCount || 0,
          });
        }
        if (tile.children) {
          tile.children.forEach(child => collectCommands(child, result));
        }
        return result;
      };

      const available = collectCommands(root);

      // Return flat lists with rootTileId for splitting
      // The agent needs rootTileId to call tabs.splitTile
      return {
        rootTileId: tab.rootTile?.id || null,
        canvasCount: available.canvases.length,
        canvases: available.canvases,
        dashboardCount: available.dashboards.length,
        dashboards: available.dashboards,
        anomaliesCount: available.anomalies.length,
        anomalies: available.anomalies,
      };
    });

    // Get full content details for any command by ID
    registerToolHandler('tabs.getCommandContent', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      const { commandId } = params;

      if (!commandId) {
        throw new Error('commandId is required');
      }

      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);
      const command = tabManagerRef.current.getCommandFromTab(tabId, commandId);

      if (!command) {
        throw new Error(`Command not found: ${commandId}`);
      }

      const result = {
        commandId,
        commandType: command.type,
        content: null,
      };

      if (command.type === 'canvas') {
        const nodes = getCanvasState(command.id).nodes || [];
        result.content = {
          nodeCount: nodes.length,
          nodes: nodes.map(n => ({
            nodeId: n.id,
            nodeType: n.type,
            x: n.position?.x || 0,
            y: n.position?.y || 0,
            data: n.data || {},
          })),
        };
      } else if (command.type === 'dashboard') {
        const charts = (getDashboardState(command.id).elements || []).filter((el) => el.type === 'context-chart');
        result.content = {
          chartCount: charts.length,
          charts: charts.map(c => ({
            chartId: c.id,
            context: c.config?.context,
            groupBy: c.config?.groupBy || null,
            filterBy: c.config?.filterBy || null,
          })),
        };
      } else if (command.type === 'anomalies') {
        const target = command.api?.getTarget?.() || getAnomaliesTarget(command);
        const items = command.api?.getItems?.() || [];
        result.content = {
          spaceId: target.spaceId || null,
          roomId: target.roomId || null,
          itemCount: items.length,
          contexts: items.slice(0, 50).map((item) => ({
            context: item.context,
            anomalyRate: item.anomalyRate,
          })),
        };
      }

      return result;
    });

    // =========================================================================
    // Canvas Tool Handlers
    // =========================================================================

    registerToolHandler('canvas.addChart', async (request) => {
      const { agentId, spaceId, roomId, tabId: fallbackTabId, mcpServerIds: fallbackMcpServerIds = [], params } = request;
      const { x, y, context, title, groupBy, filterBy, timeRange, width, height } = params;
      const tabId = ensureAgentTab(agentId, spaceId, roomId, fallbackTabId, fallbackMcpServerIds);
      const agentTab = getAgentTab(agentId, spaceId, roomId);

      if (!params?.spaceId || !params?.roomId) {
        throw new Error('canvas.addChart requires spaceId and roomId');
      }
      const mcpServerId = await resolveNetdataMcpServerIdForTab(tabId, agentTab, params.mcpServerId);

      const normalizedGroupBy = normalizeGroupBy(groupBy);
      const normalizedFilterBy = normalizeFilterBy(filterBy);

      const command = getCanvasCommand(request);
      const nodeData = {
        mcpServerId,
        spaceId: params.spaceId,
        roomId: params.roomId,
        context,
        title: title || null,
        groupBy: normalizedGroupBy,
        filterBy: normalizedFilterBy,
        timeRange: timeRange || '15m',
        width: width || 400,
        height: height || 300,
      };
      const nodeId = command.api.addNode('chart', { x, y }, nodeData);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          nodes: [...(state.nodes || []), createCanvasNode(nodeId, 'chart', { x, y }, nodeData)],
        }));
      }
      await persistVisualizationArtifact(agentId, 'canvas', command.id);

      return { nodeId };
    });

    registerToolHandler('canvas.addStatusBadge', async (request) => {
      const { params } = request;
      const { x, y, status, message, title } = params;

      const command = getCanvasCommand(request);
      const nodeData = {
        status,
        message,
        title: title || null,
        showTimestamp: false,
        timestamp: new Date().toISOString(),
      };
      const nodeId = command.api.addNode('statusBadge', { x, y }, nodeData);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          nodes: [...(state.nodes || []), createCanvasNode(nodeId, 'statusBadge', { x, y }, nodeData)],
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      return { nodeId };
    });

    registerToolHandler('canvas.addMarkdown', async (request) => {
      const { params } = request;
      const { x, y, content, width, maxHeight } = params;

      const command = getCanvasCommand(request);
      const nodeData = {
        content,
        width: width || 400,
        maxHeight: maxHeight || null,
        showHandles: true,
      };
      const nodeId = command.api.addNode('markdown', { x, y }, nodeData);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          nodes: [...(state.nodes || []), createCanvasNode(nodeId, 'markdown', { x, y }, nodeData)],
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      return { nodeId };
    });

    registerToolHandler('canvas.addEdge', async (request) => {
      const { params } = request;
      const { sourceId, targetId, label, animated } = params;

      const command = getCanvasCommand(request);
      const edgeOptions = { label, animated };
      const edgeId = command.api.addEdge(sourceId, targetId, edgeOptions);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          edges: [...(state.edges || []), createCanvasEdge(edgeId, sourceId, targetId, edgeOptions)],
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      return { edgeId };
    });

    registerToolHandler('canvas.removeNode', async (request) => {
      const { params } = request;
      const { nodeId } = params;

      const command = getCanvasCommand(request);
      command.api.removeNode(nodeId);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          nodes: (state.nodes || []).filter((n) => n.id !== nodeId),
          edges: (state.edges || []).filter((e) => e.source !== nodeId && e.target !== nodeId),
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      return { success: true };
    });

    registerToolHandler('canvas.removeEdge', async (request) => {
      const { params } = request;
      const { edgeId } = params;

      const command = getCanvasCommand(request);
      command.api.removeEdge(edgeId);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          edges: (state.edges || []).filter((e) => e.id !== edgeId),
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      return { success: true };
    });

    registerToolHandler('canvas.clearCanvas', async (request) => {
      const command = getCanvasCommand(request, { required: false });
      if (!command) {
        return { success: true };
      }

      command.api.clear();
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, () => ({
          nodes: [],
          edges: [],
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);
      return { success: true };
    });

    registerToolHandler('canvas.updateNode', async (request) => {
      const { params } = request;
      const { nodeId, x, y, data } = params;

      const command = getCanvasCommand(request);

      // Build the updates object
      const updates = {};
      if (x !== undefined || y !== undefined) {
        updates.position = { x, y };
      }
      if (data) {
        updates.data = data;
      }

      command.api.updateNode(nodeId, updates);
      if (!command.isStoreBacked) {
        useWorkspaceStore.getState().updateCommandStateAtomic(command.id, (state) => ({
          nodes: (state.nodes || []).map((node) =>
            node.id === nodeId
              ? {
                  ...node,
                  ...(updates.position ? { position: updates.position } : {}),
                  ...(updates.data ? { data: { ...(node.data || {}), ...updates.data } } : {}),
                }
              : node
          ),
        }));
      }
      await persistVisualizationArtifact(request.agentId, 'canvas', command.id);

      // Get the updated node to return
      const nodes = getCanvasState(command.id).nodes || [];
      const updatedNode = nodes.find(n => n.id === nodeId);

      return {
        nodeId: updatedNode?.id || nodeId,
        nodeType: updatedNode?.type,
        x: updatedNode?.position?.x || 0,
        y: updatedNode?.position?.y || 0,
        data: updatedNode?.data || {},
      };
    });

    // Cleanup on unmount
    return () => {
      // Reset the ref so handlers can be re-registered on next mount
      initializedRef.current = false;

      // Unregister all handlers
      unregisterToolHandler('agent.setup');
      unregisterToolHandler('dashboard.addChart');
      unregisterToolHandler('dashboard.removeChart');
      unregisterToolHandler('dashboard.clearCharts');
      unregisterToolHandler('dashboard.setTimeRange');
      unregisterToolHandler('tabs.splitTile');
      unregisterToolHandler('tabs.removeTile');
      unregisterToolHandler('tabs.getTileLayout');
      unregisterToolHandler('tabs.getCommandContent');
      // Canvas handlers
      unregisterToolHandler('canvas.addChart');
      unregisterToolHandler('canvas.addStatusBadge');
      unregisterToolHandler('canvas.addMarkdown');
      unregisterToolHandler('canvas.addEdge');
      unregisterToolHandler('canvas.removeNode');
      unregisterToolHandler('canvas.removeEdge');
      unregisterToolHandler('canvas.updateNode');
      unregisterToolHandler('canvas.clearCanvas');

      // Note: We don't call cleanupAgentBridge here because
      // other components might still be using it. It should be
      // cleaned up at app shutdown.
    };
  }, []); // Empty deps - only run once

  // Return nothing - this hook is for side effects only
};

export default useAgentBridge;
