/**
 * Workspace Client
 *
 * Thin wrapper around Tauri invoke calls for workspace operations.
 */

import { invoke } from '@tauri-apps/api/core';
import type {
  WorkspaceAgentResponse,
  WorkspaceFileContent,
  WorkspaceListEntry,
  WorkspaceSessionBinding,
  WorkspaceSnapshot,
} from '../generated/bindings';

interface SnapshotOptions {
  includeSessionPayload?: boolean;
  includeFiles?: boolean;
}

export async function getWorkspaceSnapshot(
  workspaceId: string = 'default',
  options: SnapshotOptions | null = null
): Promise<WorkspaceSnapshot> {
  return invoke('workspace_get_snapshot', { workspaceId, options });
}

export async function getOrCreateWorkspaceSession(
  workspaceId: string = 'default'
): Promise<WorkspaceSessionBinding> {
  return invoke('workspace_get_or_create_session', { workspaceId });
}

export async function readWorkspaceFile(
  workspaceId: string,
  path: string
): Promise<WorkspaceFileContent> {
  return invoke('workspace_read_file', {
    request: { workspaceId, path },
  });
}

export async function writeWorkspaceFile(
  workspaceId: string,
  path: string,
  content: string
): Promise<string> {
  return invoke('workspace_write_file', {
    request: { workspaceId, path, content },
  });
}

export async function downloadWorkspaceFile(
  workspaceId: string,
  path: string,
  destination: string
): Promise<string> {
  return invoke('workspace_download_file', {
    request: { workspaceId, path, destination },
  });
}

export async function updateWorkspaceSessionMcp(
  workspaceId: string,
  mcpServerIds: string[]
): Promise<void> {
  return invoke('workspace_update_session_mcp', {
    request: { workspaceId, mcpServerIds },
  });
}

export async function setWorkspaceProvider(
  workspaceId: string,
  providerConnectionId: string
): Promise<void> {
  return invoke('workspace_set_provider', { workspaceId, providerConnectionId });
}

export async function listWorkspaceAgents(workspaceId: string): Promise<WorkspaceAgentResponse[]> {
  return invoke('workspace_list_agents', { workspaceId });
}

// assignWorkspaceAgent / unassignWorkspaceAgent: removed. Agents are
// workspace-local; use workspaceCreateAgent / workspaceDeleteAgent from
// `../api/client.js` instead.

export async function setWorkspaceDefaultAgent(
  workspaceId: string,
  workspaceAgentId: string
): Promise<void> {
  return invoke('workspace_set_default_agent', { workspaceId, workspaceAgentId });
}

export async function acknowledgeWorkspaceTask(workspaceId: string, taskId: string): Promise<void> {
  return invoke('workspace_acknowledge_task', {
    request: { workspaceId, taskId },
  });
}

export async function createWorkspace(title?: string | null): Promise<string> {
  return invoke('workspace_create', { title: title || null });
}

/**
 * Clone a workspace's configuration (agents, skills, MCP, providers, sandbox,
 * schedule cadence) into a new empty workspace — no sessions/messages/tasks/
 * memory/artifacts. Returns the new workspace id.
 */
export async function cloneWorkspaceConfig(workspaceId: string): Promise<string> {
  return invoke('workspace_clone_config', { workspaceId });
}

export async function listWorkspaces(): Promise<WorkspaceListEntry[]> {
  return invoke('workspace_list');
}

export async function deleteWorkspace(workspaceId: string): Promise<void> {
  return invoke('workspace_delete', { workspaceId });
}

export async function runWorkspaceNow(workspaceId: string): Promise<void> {
  return invoke('workspace_run_now', { workspaceId });
}

export async function setWorkspaceSchedulePaused(
  workspaceId: string,
  paused: boolean
): Promise<void> {
  return invoke('workspace_set_schedule_paused', { workspaceId, paused });
}

export async function setWorkspaceTitle(workspaceId: string, title: string): Promise<void> {
  return invoke('workspace_set_title', { workspaceId, title });
}
