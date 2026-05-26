import { invoke } from '@tauri-apps/api/core';
import type { PathGrantDecision, PathGrantRequest } from '../generated/bindings';

/**
 * Resolve a pending fs_request_grant request.
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
export async function submitPathGrantDecision(
  requestId: string,
  decision: PathGrantDecision,
): Promise<void> {
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
export async function listPendingPathGrantRequests(
  workspaceId: string,
): Promise<PathGrantRequest[]> {
  return invoke('list_pending_path_grant_requests', { workspaceId });
}
