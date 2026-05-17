import { invoke } from '@tauri-apps/api/core';

export async function getWorkspaceSnapshot(workspaceId = 'default') {
  return invoke('workspace_get_snapshot', { workspaceId });
}

export async function getOrCreateWorkspaceSession(workspaceId = 'default') {
  return invoke('workspace_get_or_create_session', { workspaceId });
}

export async function readWorkspaceFile(workspaceId, path) {
  return invoke('workspace_read_file', {
    request: {
      workspaceId,
      path,
    },
  });
}

export async function writeWorkspaceFile(workspaceId, path, content) {
  return invoke('workspace_write_file', {
    request: {
      workspaceId,
      path,
      content,
    },
  });
}

export async function downloadWorkspaceFile(workspaceId, path, destination) {
  return invoke('workspace_download_file', {
    request: {
      workspaceId,
      path,
      destination,
    },
  });
}

export async function updateWorkspaceSessionMcp(workspaceId, mcpServerIds) {
  return invoke('workspace_update_session_mcp', {
    request: {
      workspaceId,
      mcpServerIds,
    },
  });
}

export async function setWorkspaceProvider(workspaceId, providerConnectionId) {
  return invoke('workspace_set_provider', { workspaceId, providerConnectionId });
}

export async function listWorkspaceAgents(workspaceId) {
  return invoke('workspace_list_agents', { workspaceId });
}

export async function assignWorkspaceAgent(workspaceId, agentDefinitionId, options = {}) {
  return invoke('workspace_assign_agent', {
    request: {
      workspaceId,
      agentDefinitionId,
      displayName: options.displayName || null,
      role: options.role || 'member',
    },
  });
}

export async function unassignWorkspaceAgent(workspaceAgentId) {
  return invoke('workspace_unassign_agent', { workspaceAgentId });
}

export async function setWorkspaceDefaultAgent(workspaceId, workspaceAgentId) {
  return invoke('workspace_set_default_agent', { workspaceId, workspaceAgentId });
}

export async function acknowledgeWorkspaceTask(workspaceId, taskId) {
  return invoke('workspace_acknowledge_task', {
    request: {
      workspaceId,
      taskId,
    },
  });
}

export async function submitWorkspaceTaskFeedback(workspaceId, taskId, response) {
  return invoke('workspace_submit_task_feedback', {
    request: {
      workspaceId,
      taskId,
      response,
    },
  });
}

export async function createWorkspace(title) {
  return invoke('workspace_create', { title: title || null });
}

export async function listWorkspaces() {
  return invoke('workspace_list');
}

export async function deleteWorkspace(workspaceId) {
  return invoke('workspace_delete', { workspaceId });
}
