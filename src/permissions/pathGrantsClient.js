import { invoke } from '@tauri-apps/api/core';

/**
 * Resolve a pending fs_request_grant request.
 *
 * decision is one of:
 *   { kind: 'deny' }
 *   { kind: 'allowOnce', path: string, access: 'read_only' | 'read_write' }
 *   { kind: 'allowAlways', path: string, access: 'read_only' | 'read_write', scope: 'agent' }
 *
 * Narrowing rules (enforced backend-side; the UI should mirror them so
 * the user never sees a confusing rejection):
 *   - `path` must be the requested path or a descendant. The modal can
 *     narrow ~/.config → ~/.config/gh but cannot widen ~/.config/gh → ~/.
 *   - `access` may downgrade RW → RO but never upgrade RO → RW.
 *
 * AllowAlways is persisted to the agent's execution.filesystem.extraPaths
 * in the workspace_agents DB row before delivery, with origin tagged as
 * {kind: 'approval', reason, grantedAtUnixMs}.
 */
export async function submitPathGrantDecision(requestId, decision) {
  return invoke('submit_path_grant_decision', {
    requestId,
    decision,
  });
}

/**
 * Returns any currently-pending path-grant requests for the given
 * workspace. Used by the inline card to discover requests registered
 * before it mounted.
 */
export async function listPendingPathGrantRequests(workspaceId) {
  return invoke('list_pending_path_grant_requests', { workspaceId });
}
