import { invoke } from '@tauri-apps/api/core';
import type { NetworkAccessDecision, NetworkAccessRequest } from '../generated/bindings';

/**
 * Answer a pending net_request_access request. For “Always” the backend
 * checks that the entry is the requested host or `*.` and a parent domain of
 * it, and saves it on the agent before the waiting tool resumes; a refused or
 * failed save leaves the request pending and rejects with the reason.
 */
export async function submitNetworkAccessDecision(
  requestId: string,
  decision: NetworkAccessDecision,
): Promise<void> {
  return invoke('submit_network_access_decision', { requestId, decision });
}

/** The pending network access requests of a workspace, for views that mount late. */
export async function listPendingNetworkAccessRequests(
  workspaceId: string,
): Promise<NetworkAccessRequest[]> {
  return invoke('list_pending_network_access_requests', { workspaceId });
}

/** The pending network access count per workspace. */
export async function listPendingNetworkAccessCounts(): Promise<Record<string, number>> {
  return invoke('list_pending_network_access_counts');
}
