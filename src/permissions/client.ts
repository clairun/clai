import { invoke } from '@tauri-apps/api/core';
import type { AskFirstDecision, PermissionRequest } from '../generated/bindings';

/**
 * Send the user's decision on a pending ask-first request. For "Always
 * allow" the backend removes the matched rules from the agent's ask-first
 * list before resolving the awaiting bash tool, so the removal survives a
 * crash between the click and the command.
 */
export async function submitPermissionDecision(
  requestId: string,
  decision: AskFirstDecision,
): Promise<void> {
  return invoke('submit_permission_decision', {
    requestId,
    decision,
  });
}

/**
 * Returns any currently-pending permission requests for the given
 * workspace. Used by the inline approval card to discover requests
 * that were registered before it mounted (e.g., the user navigates
 * to the workspace after the original event fired).
 */
export async function listPendingPermissionRequests(
  workspaceId: string,
): Promise<PermissionRequest[]> {
  return invoke('list_pending_permission_requests', { workspaceId });
}

/**
 * Returns the current per-workspace pending-approval count. Used by
 * attention listeners that need a snapshot — the event stream alone
 * only reports transitions, so a listener that mounted after a request
 * fired would otherwise miss it.
 */
export async function listPendingPermissionCounts(): Promise<Record<string, number>> {
  return invoke('list_pending_permission_counts');
}

/**
 * Symmetric to `listPendingPermissionCounts` for filesystem path-grant
 * requests.
 */
export async function listPendingPathGrantCounts(): Promise<Record<string, number>> {
  return invoke('list_pending_path_grant_counts');
}
