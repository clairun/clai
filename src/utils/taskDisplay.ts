/**
 * How tasks read on screen: status labels, "needs attention", relative
 * times. Shared by the workspace page, the tasks drawer and the transcript
 * panel so the three never disagree on what a blocked task is called.
 */
import type { WorkspaceTaskResponse } from '../generated/bindings';

export type NumericTimestamp = number | bigint | null | undefined;

export const toNumber = (value: NumericTimestamp): number | null => {
  if (value === null || value === undefined) return null;
  return typeof value === 'bigint' ? Number(value) : value;
};

export const formatRelativeTime = (timestamp: NumericTimestamp): string => {
  const value = toNumber(timestamp);
  if (!value) return 'Never';
  const diffMs = Date.now() - value;
  const diffSec = Math.max(0, Math.floor(diffMs / 1000));
  if (diffSec < 60) return `${diffSec}s ago`;
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)}m ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)}h ago`;
  return `${Math.floor(diffSec / 86400)}d ago`;
};

export const TASK_STATUS_LABEL: Record<string, string> = {
  queued: 'Queued',
  running: 'Running',
  completed: 'Completed',
  failed: 'Failed',
  blocked: 'Blocked',
  cancelled: 'Cancelled',
};

export const taskStatusLabel = (status: string): string => TASK_STATUS_LABEL[status] || status;

/** A task the user has not yet looked at after it stopped on a problem. */
export const isTaskAttention = (
  task: Pick<WorkspaceTaskResponse, 'status' | 'attentionAcknowledgedAt' | 'userResponseAt'>
): boolean =>
  (task.status === 'blocked' || task.status === 'failed') &&
  !task.attentionAcknowledgedAt &&
  !task.userResponseAt;

export const isTaskActive = (task: Pick<WorkspaceTaskResponse, 'status'>): boolean =>
  task.status === 'running' || task.status === 'queued';
