import { invoke } from '@tauri-apps/api/core';

/**
 * CLAI backend API client.
 *
 * This module provides a JavaScript interface to the Tauri backend.
 * All API calls are routed through Rust via invoke handlers.
 */

// ============================================================================
// Authentication Functions
// ============================================================================

/**
 * Store the API token securely in the OS keychain
 * @param {string} token - The bearer token to store
 * @returns {Promise<void>}
 * @throws {Error} If token storage fails
 */
export const setToken = async (token) => {
  try {
    await invoke('set_token', { token });
  } catch (error) {
    throw new Error(`Failed to store token: ${error}`);
  }
};

/**
 * Check if a token is stored (user is authenticated)
 * @returns {Promise<boolean>} True if token exists
 */
export const hasToken = async () => {
  try {
    return await invoke('has_token');
  } catch (error) {
    console.error('Failed to check token:', error);
    return false;
  }
};

/**
 * Clear the stored token (logout)
 * @returns {Promise<void>}
 */
export const clearToken = async () => {
  try {
    await invoke('clear_token');
  } catch (error) {
    console.error('Failed to clear token:', error);
  }
};

/**
 * Set the API base URL
 * @param {string} url - The base URL (e.g., "https://app.netdata.cloud")
 * @returns {Promise<void>}
 */
export const setBaseUrl = async (url) => {
  try {
    await invoke('set_base_url', { url });
  } catch (error) {
    throw new Error(`Failed to set base URL: ${error}`);
  }
};

/**
 * Get the current API base URL
 * @returns {Promise<string>} The base URL
 */
export const getBaseUrl = async () => {
  try {
    return await invoke('get_base_url');
  } catch (error) {
    return 'https://app.netdata.cloud';
  }
};

// ============================================================================
// Error Handling
// ============================================================================

/**
 * Handle API errors
 * @param {Error} error - The error from invoke
 * @param {string} operation - Description of the operation that failed
 * @throws {Error} Re-throws the error after handling
 */
const handleApiError = (error, operation) => {
  const errorMessage = error.toString();
  throw new Error(`${operation}: ${errorMessage}`);
};

// ============================================================================
// API Functions
// ============================================================================

/**
 * Get data from Netdata Cloud with complex aggregation and filtering options
 * @param {string} spaceId - Space ID
 * @param {string} roomId - Room ID
 * @param {Object} params - Data query parameters (see Rust DataQuery struct)
 * @returns {Promise<Object>} Data response
 * @throws {Error} If the request fails
 */
export const getData = async (spaceId, roomId, params) => {
  try {
    // Validate required parameters
    if (!params.scope || !params.scope.contexts || !params.scope.nodes) {
      throw new Error('scope.contexts and scope.nodes are required');
    }
    if (!params.window || params.window.after === undefined || params.window.before === undefined) {
      throw new Error('window.after and window.before are required');
    }
    if (!params.aggregations || !params.aggregations.metrics || !params.aggregations.time) {
      throw new Error('aggregations.metrics and aggregations.time are required');
    }

    // Build the query object to match Rust's DataQuery struct
    const query = {
      format: params.format || 'json2',
      options: params.options || ['jsonwrap', 'nonzero', 'flip', 'ms', 'jw-anomaly-rates', 'minify'],
      scope: {
        contexts: params.scope.contexts,
        nodes: params.scope.nodes,
        instances: params.scope.instances,
        dimensions: params.scope.dimensions,
        labels: params.scope.labels
      },
      selectors: {
        contexts: params.selectors?.contexts || ['*'],
        nodes: params.selectors?.nodes || ['*'],
        instances: params.selectors?.instances || ['*'],
        dimensions: params.selectors?.dimensions || ['*'],
        labels: params.selectors?.labels || ['*'],
        alerts: params.selectors?.alerts
      },
      aggregations: {
        metrics: params.aggregations.metrics.map(metric => ({
          aggregation: metric.aggregation,
          group_by: metric.group_by || [],
          group_by_label: metric.group_by_label || []
        })),
        time: {
          time_group: params.aggregations.time.time_group,
          time_resampling: params.aggregations.time.time_resampling,
          time_group_options: params.aggregations.time.time_group_options
        }
      },
      window: {
        after: params.window.after,
        before: params.window.before,
        points: params.window.points,
        duration: params.window.duration,
        tier: params.window.tier,
        baseline: params.window.baseline
      },
      timeout: params.timeout || 10000
    };

    return await invoke('api_get_data', { spaceId, roomId, query });
  } catch (error) {
    handleApiError(error, 'Failed to get data');
  }
};

/**
 * Get contexts from Netdata Cloud
 * @param {string} spaceId - Space ID
 * @param {string} roomId - Room ID
 * @param {Object} params - Contexts query parameters
 * @returns {Promise<Object>} Contexts response
 * @throws {Error} If the request fails
 */
export const getContexts = async (spaceId, roomId, params) => {
  try {
    // Build the query object to match Rust's ContextsQuery struct
    const query = {
      format: params.format || 'json2',
      scope: {
        contexts: params.scope?.contexts || ['*'],
        nodes: params.scope?.nodes || []
      },
      selectors: {
        contexts: params.selectors?.contexts || ['*'],
        nodes: params.selectors?.nodes || ['*']
      },
      window: {
        after: params.window.after,
        before: params.window.before
      },
      timeout: params.timeout || 20000
    };

    return await invoke('api_get_contexts', { spaceId, roomId, query });
  } catch (error) {
    handleApiError(error, 'Failed to get contexts');
  }
};

// ============================================================================
// AI Provider Functions
// ============================================================================

/**
 * Get the currently configured AI provider
 * @returns {Promise<Object>} Provider info
 * @property {Object|null} provider - The configured provider
 * @property {string|null} name - Human-readable provider name
 * @property {boolean} is_configured - Whether a provider is set
 * @throws {Error} If the request fails
 */
export const getAiProvider = async () => {
  try {
    return await invoke('get_ai_provider');
  } catch (error) {
    handleApiError(error, 'Failed to get AI provider');
  }
};

/**
 * Set the AI provider
 * @param {Object} provider - The provider config (e.g., { type: 'claude' })
 * @returns {Promise<Object>} Validated provider info
 * @throws {Error} If the provider is not available
 */
export const setAiProvider = async (provider) => {
  try {
    return await invoke('set_ai_provider', { provider });
  } catch (error) {
    throw new Error(`Failed to set AI provider: ${error}`);
  }
};

/**
 * Clear the AI provider configuration
 * @returns {Promise<void>}
 * @throws {Error} If the request fails
 */
export const clearAiProvider = async () => {
  try {
    await invoke('clear_ai_provider');
  } catch (error) {
    throw new Error(`Failed to clear AI provider: ${error}`);
  }
};

/**
 * Get all available AI providers on the system
 * @returns {Promise<Array>} Array of provider info objects
 * @property {Object} provider - The provider type
 * @property {string} name - Human-readable name
 * @property {string} command - CLI command
 * @property {string|null} version - Version string if detected
 * @property {boolean} available - Whether provider is working
 * @property {string|null} error - Error message if not available
 */
export const getAvailableAiProviders = async () => {
  try {
    return await invoke('get_available_ai_providers');
  } catch (error) {
    handleApiError(error, 'Failed to get available AI providers');
  }
};

/**
 * Validate a specific AI provider
 * @param {Object} provider - The provider config to validate
 * @returns {Promise<Object>} Validated provider info
 * @throws {Error} If the provider is not available
 */
export const validateAiProvider = async (provider) => {
  try {
    return await invoke('validate_ai_provider', { provider });
  } catch (error) {
    throw new Error(`Provider not available: ${error}`);
  }
};

/**
 * Get available models for a provider type
 * @param {string} providerType - Provider type ('claude', 'gemini', 'codex')
 * @returns {Promise<Array>} Array of model info objects
 * @property {string} id - Model identifier to pass to CLI
 * @property {string} name - Human-readable model name
 * @property {string} description - Brief model description
 * @property {boolean} recommended - Whether this is the recommended model
 */
export const getProviderModels = async (providerType) => {
  try {
    return await invoke('get_provider_models', { providerType });
  } catch (error) {
    handleApiError(error, 'Failed to get provider models');
  }
};

// ============================================================================
// Agent Management
// ============================================================================

/**
 * Get all agents
 * @returns {Promise<Array>} List of agents
 */
export const getAgents = async () => {
  try {
    return await invoke('get_agents');
  } catch (error) {
    handleApiError(error, 'Failed to get agents');
  }
};

/**
 * Get a single agent by ID
 * @param {string} id - Agent ID
 * @returns {Promise<Object|null>} Agent or null if not found
 */
export const getAgent = async (id) => {
  try {
    return await invoke('get_agent', { id });
  } catch (error) {
    handleApiError(error, 'Failed to get agent');
  }
};

/**
 * Create a new agent
 * @param {Object} request - Agent creation request
 * @param {string} request.name - Agent name
 * @param {string} request.description - Agent description (supports markdown)
 * @param {number} request.intervalMinutes - Check interval in minutes
 * @param {Array<string>} request.selectedMcpServerIds - Enabled MCP servers for this agent
 * @param {Array<string>} request.selectedSkillIds - Selected reusable skill IDs for this agent
 * @param {Object} request.execution - Local execution capability policy
 * @returns {Promise<Object>} Created agent
 */
export const createAgent = async (request) => {
  try {
    return await invoke('create_agent', { request });
  } catch (error) {
    handleApiError(error, 'Failed to create agent');
  }
};

/**
 * Update an existing agent
 * @param {Object} request - Agent update request
 * @param {string} request.id - Agent ID
 * @param {string} request.name - Agent name
 * @param {string} request.description - Agent description
 * @param {number} request.intervalMinutes - Check interval in minutes
 * @param {Array<string>} request.selectedMcpServerIds - Enabled MCP servers for this agent
 * @param {Array<string>} request.selectedSkillIds - Selected reusable skill IDs for this agent
 * @param {Object} request.execution - Local execution capability policy
 * @returns {Promise<Object>} Updated agent
 */
export const updateAgent = async (request) => {
  try {
    return await invoke('update_agent', { request });
  } catch (error) {
    handleApiError(error, 'Failed to update agent');
  }
};

/**
 * Delete an agent
 * @param {string} id - Agent ID
 * @returns {Promise<void>}
 */
export const deleteAgent = async (id) => {
  try {
    return await invoke('delete_agent', { id });
  } catch (error) {
    handleApiError(error, 'Failed to delete agent');
  }
};

/**
 * Enable or disable an agent globally
 * @param {string} id - Agent ID
 * @param {boolean} enabled - Whether the agent should run
 * @returns {Promise<Object>} Updated agent
 */
export const setAgentEnabled = async (id, enabled) => {
  try {
    return await invoke('set_agent_enabled', {
      request: { id, enabled },
    });
  } catch (error) {
    handleApiError(error, 'Failed to update agent status');
  }
};

// ============================================================================
// Skill Catalog
// ============================================================================

export const getSkillSources = async () => {
  try {
    return await invoke('skill_sources_list');
  } catch (error) {
    handleApiError(error, 'Failed to get skill sources');
  }
};

export const getSkills = async () => {
  try {
    return await invoke('skills_list');
  } catch (error) {
    handleApiError(error, 'Failed to get skills');
  }
};

export const getSkillsCatalog = async () => {
  try {
    return await invoke('skills_catalog');
  } catch (error) {
    handleApiError(error, 'Failed to get skill catalog');
  }
};

export const addSkillSource = async (request) => {
  try {
    return await invoke('skill_source_add', { request });
  } catch (error) {
    handleApiError(error, 'Failed to add skill source');
  }
};

export const refreshSkillSource = async (id) => {
  try {
    return await invoke('skill_source_refresh', { id });
  } catch (error) {
    handleApiError(error, 'Failed to refresh skill source');
  }
};

export const setSkillSourceEnabled = async (id, enabled) => {
  try {
    return await invoke('skill_source_set_enabled', {
      request: { id, enabled },
    });
  } catch (error) {
    handleApiError(error, 'Failed to update skill source');
  }
};

export const deleteSkillSource = async (id) => {
  try {
    return await invoke('skill_source_delete', { id });
  } catch (error) {
    handleApiError(error, 'Failed to delete skill source');
  }
};

// ============================================================================
// MCP Server Management
// ============================================================================

export const getMcpServers = async () => {
  try {
    return await invoke('get_mcp_servers');
  } catch (error) {
    handleApiError(error, 'Failed to get MCP servers');
  }
};

export const getMcpServer = async (id) => {
  try {
    return await invoke('get_mcp_server', { id });
  } catch (error) {
    handleApiError(error, 'Failed to get MCP server');
  }
};

export const createMcpServer = async (request) => {
  try {
    return await invoke('create_mcp_server', { request });
  } catch (error) {
    handleApiError(error, 'Failed to create MCP server');
  }
};

export const updateMcpServer = async (request) => {
  try {
    return await invoke('update_mcp_server', { request });
  } catch (error) {
    handleApiError(error, 'Failed to update MCP server');
  }
};

export const deleteMcpServer = async (id) => {
  try {
    return await invoke('delete_mcp_server', { id });
  } catch (error) {
    handleApiError(error, 'Failed to delete MCP server');
  }
};
