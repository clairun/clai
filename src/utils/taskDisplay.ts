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
  input_required: 'Needs an answer',
  stopping: 'Stopping…',
  stopped: 'Stopped',
  completed: 'Completed',
  failed: 'Failed',
  // Tool results recorded before `blocked` became a failure reason.
  blocked: 'Blocked',
};

export const taskStatusLabel = (status: string): string => TASK_STATUS_LABEL[status] || status;

const FAILED_LABEL: Record<string, string> = {
  restart: 'Failed · interrupted',
  connection_unavailable: 'Failed · no connection',
};

/**
 * How a live task's status reads: `key` picks the pill style, `label` its
 * text. A blocked task is a failure with reason `blocked`, shown as its own pill.
 */
export const taskDisplayStatus = (task: {
  status: string;
  failureReason?: string | null;
}): { key: string; label: string } => {
  if (task.status === 'failed' && task.failureReason === 'blocked') {
    return { key: 'blocked', label: 'Blocked' };
  }
  if (task.status === 'failed') {
    return { key: 'failed', label: FAILED_LABEL[task.failureReason ?? ''] ?? 'Failed' };
  }
  return { key: task.status, label: taskStatusLabel(task.status) };
};

/** A task waiting on someone: it failed (or was blocked) or asked a question, and is not yet reviewed. */
export const isTaskAttention = (
  task: Pick<WorkspaceTaskResponse, 'attentionAcknowledgedAt'> & { status: string }
): boolean =>
  (task.status === 'failed' || task.status === 'input_required') && !task.attentionAcknowledgedAt;

export const isTaskActive = (task: { status: string }): boolean =>
  task.status === 'running' || task.status === 'queued' || task.status === 'input_required';

/** A task whose current cycle can still be stopped. */
export const canStopTask = (task: { status: string }): boolean => isTaskActive(task);
