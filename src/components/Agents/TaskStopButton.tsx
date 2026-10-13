import { useCallback } from 'react';
import type { WorkspaceTaskResponse } from '../../generated/bindings';
import { useCancelRun } from '../../assistant/useCancelRun';
import { canStopTask } from '../../utils/taskDisplay';
import { stopWorkspaceTask } from '../../workspace/client';
import { useTaskStore } from '../../workspace/taskStore';
import styles from './taskStatus.module.css';

interface TaskStopButtonProps {
  task: WorkspaceTaskResponse;
  label: string;
  className?: string;
}

/**
 * Stop for one task's current cycle. Stays disabled while the task is
 * `stopping`, so the state survives a reload. A task that had already ended
 * comes back unchanged, which is not an error.
 */
export default function TaskStopButton({ task, label, className = '' }: TaskStopButtonProps) {
  const upsertTask = useTaskStore((state) => state.upsertTask);
  const { workspaceId, id } = task;
  const cancel = useCallback(
    async () => upsertTask(await stopWorkspaceTask(workspaceId, id)),
    [upsertTask, workspaceId, id]
  );
  const { stop, stopping, error } = useCancelRun({
    cancel,
    isActive: canStopTask(task),
    key: `${id}:${task.cycleNo}`,
  });
  if (!canStopTask(task) && task.status !== 'stopping') return null;
  const busy = stopping || task.status === 'stopping';
  return (
    <>
      <button
        type="button"
        className={`${styles.stopButton} ${className}`}
        aria-label={label}
        title={busy ? 'Stopping…' : 'Stop'}
        disabled={busy}
        onClick={stop}
      >
        <span aria-hidden="true">{busy ? '■ …' : '■'}</span>
      </button>
      {error && (
        <span role="alert" className={styles.stopError}>
          {error}
        </span>
      )}
    </>
  );
}
