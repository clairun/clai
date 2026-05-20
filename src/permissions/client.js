import { invoke } from '@tauri-apps/api/core';

/**
 * Send the user's per-segment decisions for a pending shell-permission
 * approval. The backend persists any AllowAlways/DenyAlways entries to
 * disk before resolving the awaiting bash tool, so grants are durable
 * across crashes between user click and command execution.
 *
 * decisions: Array of one of:
 *   { kind: 'allowOnce' }
 *   { kind: 'allowAlways', scope: 'agent', prefix: string }
 *   { kind: 'denyOnce' }
 *   { kind: 'denyAlways', scope: 'agent', prefix: string }
 */
export async function submitPermissionDecision(requestId, decisions) {
  return invoke('submit_permission_decision', {
    requestId,
    decisions,
  });
}

/**
 * Returns any currently-pending permission requests for the given
 * workspace. Used by the inline approval card to discover requests
 * that were registered before it mounted (e.g., the user navigates
 * to the workspace after the original event fired).
 */
export async function listPendingPermissionRequests(workspaceId) {
  return invoke('list_pending_permission_requests', { workspaceId });
}
