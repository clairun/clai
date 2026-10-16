import { useState } from 'react';
import type { WorkspaceTaskResponse } from '../../generated/bindings';
import { canStopTask } from '../../utils/taskDisplay';
import { stopWorkspaceTask } from '../../workspace/client';
import { useTaskStore } from '../../workspace/taskStore';
import styles from './taskStatus.module.css';

interface TaskStopButtonProps {
  task: WorkspaceTaskResponse;
  label: string;
  className?: string;
}

const errorText = (err: unknown): string =>
  typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to stop.';

/**
 * Stop for one task's current cycle. Disabled while the request is in flight,
 * then while the task is `stopping`, so the state survives a reload. A task
 * that had already ended comes back unchanged, which is not an error.
 */
export default function TaskStopButton({ task, label, className = '' }: TaskStopButtonProps) {
  const upsertTask = useTaskStore((state) => state.upsertTask);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (!canStopTask(task) && task.status !== 'stopping') return null;
  const busy = pending || task.status === 'stopping';
  const stop = async () => {
    setPending(true);
    setError(null);
    try {
      upsertTask(await stopWorkspaceTask(task.workspaceId, task.id));
    } catch (err) {
      setError(errorText(err));
    } finally {
      setPending(false);
    }
  };
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
