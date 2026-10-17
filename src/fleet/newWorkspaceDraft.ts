import { setWorkspaceProvider, updateWorkspaceSessionMcp } from '../workspace/client';

/**
 * Model and MCP choices made in the Fleet terminal with no card selected,
 * applied to the workspace the next prompt creates. The empty provider means
 * the default a new workspace gets (the first enabled connection).
 */
export const newWorkspaceDraft = {
  providerConnectionId: '',
  mcpServerIds: [] as string[],
  disabledMcpServerIds: [] as string[],
};

export async function applyNewWorkspaceDraft(workspaceId: string): Promise<void> {
  const { providerConnectionId, mcpServerIds, disabledMcpServerIds } = newWorkspaceDraft;
  if (providerConnectionId) await setWorkspaceProvider(workspaceId, providerConnectionId);
  await updateWorkspaceSessionMcp(workspaceId, mcpServerIds, disabledMcpServerIds);
}
