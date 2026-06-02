import { invoke } from '@tauri-apps/api/core';
import type {
  McpServerResponse,
  SkillCatalogResponse,
  SkillDefinition,
  SkillSourceResponse,
  WorkspaceAgentResponse,
} from '../generated/bindings';

/**
 * CLAI backend API client.
 *
 * This module provides a TypeScript interface to the Tauri backend.
 * All API calls are routed through Rust via invoke handlers.
 *
 * Return types use generated bindings where available; surfaces without a
 * binding yet are typed `unknown` and shaped by their (typed) consumers.
 * Request payloads are typed `unknown` — callers build them and the Rust
 * side validates; tighten to binding request types when those consumers
 * are revisited.
 */

// ============================================================================
// Error Handling
// ============================================================================

/**
 * Handle API errors. Always throws, so callers can treat the catch path as
 * unreachable (`never`).
 */
const handleApiError = (error: unknown, operation: string): never => {
  const errorMessage = String(error);
  throw new Error(`${operation}: ${errorMessage}`);
};

// ============================================================================
// AI Provider Functions
// ============================================================================

/**
 * Get the currently configured AI provider.
 */
export const getAiProvider = async (): Promise<unknown> => {
  try {
    return await invoke('get_ai_provider');
  } catch (error) {
    return handleApiError(error, 'Failed to get AI provider');
  }
};

/**
 * Set the AI provider (e.g., { type: 'claude' }).
 */
export const setAiProvider = async (provider: unknown): Promise<unknown> => {
  try {
    return await invoke('set_ai_provider', { provider });
  } catch (error) {
    throw new Error(`Failed to set AI provider: ${error}`);
  }
};

/**
 * Clear the AI provider configuration.
 */
export const clearAiProvider = async (): Promise<void> => {
  try {
    await invoke('clear_ai_provider');
  } catch (error) {
    throw new Error(`Failed to clear AI provider: ${error}`);
  }
};

/**
 * Get all available AI providers on the system.
 */
export const getAvailableAiProviders = async (): Promise<unknown> => {
  try {
    return await invoke('get_available_ai_providers');
  } catch (error) {
    return handleApiError(error, 'Failed to get available AI providers');
  }
};

/**
 * Validate a specific AI provider.
 * @throws If the provider is not available
 */
export const validateAiProvider = async (provider: unknown): Promise<unknown> => {
  try {
    return await invoke('validate_ai_provider', { provider });
  } catch (error) {
    throw new Error(`Provider not available: ${error}`);
  }
};

/**
 * Get available models for a provider type ('claude', 'gemini', 'codex').
 */
export const getProviderModels = async (providerType: string): Promise<unknown> => {
  try {
    return await invoke('get_provider_models', { providerType });
  } catch (error) {
    return handleApiError(error, 'Failed to get provider models');
  }
};

// ============================================================================
// Agent Management
// ============================================================================

// Legacy global-agent CRUD removed — agents are workspace-local now.
// Use the workspace_* wrappers below.

export const getAgentTemplates = async (): Promise<unknown> => {
  try {
    return await invoke('agent_templates_list');
  } catch (error) {
    return handleApiError(error, 'Failed to get agent templates');
  }
};

// ============================================================================
// Workspace-Scoped Agent CRUD
// ============================================================================

export const workspaceGetAgent = async (
  workspaceId: string,
  agentId: string
): Promise<WorkspaceAgentResponse> => {
  try {
    return await invoke('workspace_get_agent', { workspaceId, agentId });
  } catch (error) {
    return handleApiError(error, 'Failed to load workspace agent');
  }
};

export const workspaceCreateAgent = async (
  request: unknown
): Promise<WorkspaceAgentResponse> => {
  try {
    return await invoke('workspace_create_agent', { request });
  } catch (error) {
    return handleApiError(error, 'Failed to create workspace agent');
  }
};

export const workspaceUpdateAgent = async (
  request: unknown
): Promise<WorkspaceAgentResponse> => {
  try {
    return await invoke('workspace_update_agent', { request });
  } catch (error) {
    return handleApiError(error, 'Failed to update workspace agent');
  }
};

export const workspaceDeleteAgent = async (
  workspaceId: string,
  agentId: string
): Promise<void> => {
  try {
    return await invoke('workspace_delete_agent', { workspaceId, agentId });
  } catch (error) {
    return handleApiError(error, 'Failed to delete workspace agent');
  }
};

export const workspaceSetAgentEnabled = async (
  workspaceId: string,
  agentId: string,
  enabled: boolean
): Promise<unknown> => {
  try {
    return await invoke('workspace_set_agent_enabled', {
      request: { workspaceId, agentId, enabled },
    });
  } catch (error) {
    return handleApiError(error, 'Failed to update workspace agent status');
  }
};

export const workspaceAgentDefaultExecution = async (): Promise<unknown> => {
  try {
    return await invoke('workspace_agent_default_execution');
  } catch (error) {
    return handleApiError(error, 'Failed to load agent defaults');
  }
};

// ============================================================================
// Skill Catalog
// ============================================================================

export const getSkillSources = async (): Promise<SkillSourceResponse[]> => {
  try {
    return await invoke('skill_sources_list');
  } catch (error) {
    return handleApiError(error, 'Failed to get skill sources');
  }
};

export const getSkills = async (): Promise<SkillDefinition[]> => {
  try {
    return await invoke('skills_list');
  } catch (error) {
    return handleApiError(error, 'Failed to get skills');
  }
};

export const getSkillsCatalog = async (): Promise<SkillCatalogResponse> => {
  try {
    return await invoke('skills_catalog');
  } catch (error) {
    return handleApiError(error, 'Failed to get skill catalog');
  }
};

export const addSkillSource = async (request: unknown): Promise<SkillSourceResponse> => {
  try {
    return await invoke('skill_source_add', { request });
  } catch (error) {
    return handleApiError(error, 'Failed to add skill source');
  }
};

export const refreshSkillSource = async (id: string): Promise<unknown> => {
  try {
    return await invoke('skill_source_refresh', { id });
  } catch (error) {
    return handleApiError(error, 'Failed to refresh skill source');
  }
};

export const setSkillSourceEnabled = async (
  id: string,
  enabled: boolean
): Promise<unknown> => {
  try {
    return await invoke('skill_source_set_enabled', { request: { id, enabled } });
  } catch (error) {
    return handleApiError(error, 'Failed to update skill source');
  }
};

export const deleteSkillSource = async (id: string): Promise<void> => {
  try {
    return await invoke('skill_source_delete', { id });
  } catch (error) {
    return handleApiError(error, 'Failed to delete skill source');
  }
};

// ============================================================================
// MCP Server Management
// ============================================================================

export const getMcpServers = async (): Promise<McpServerResponse[]> => {
  try {
    return await invoke('get_mcp_servers');
  } catch (error) {
    return handleApiError(error, 'Failed to get MCP servers');
  }
};

export const getMcpServer = async (id: string): Promise<McpServerResponse> => {
  try {
    return await invoke('get_mcp_server', { id });
  } catch (error) {
    return handleApiError(error, 'Failed to get MCP server');
  }
};

export const createMcpServer = async (request: unknown): Promise<McpServerResponse> => {
  try {
    return await invoke('create_mcp_server', { request });
  } catch (error) {
    return handleApiError(error, 'Failed to create MCP server');
  }
};

export const updateMcpServer = async (request: unknown): Promise<McpServerResponse> => {
  try {
    return await invoke('update_mcp_server', { request });
  } catch (error) {
    return handleApiError(error, 'Failed to update MCP server');
  }
};

export const deleteMcpServer = async (id: string): Promise<void> => {
  try {
    return await invoke('delete_mcp_server', { id });
  } catch (error) {
    return handleApiError(error, 'Failed to delete MCP server');
  }
};
