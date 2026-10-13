import React, { useCallback, useEffect, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import type { WorkspaceTaskResponse } from '../generated/bindings';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { isTaskAttention, taskDisplayStatus } from '../utils/taskDisplay';
import { useTaskStore } from '../workspace/taskStore';
import styles from './WorkspaceTaskNotifications.module.css';

const MAX_NOTIFICATIONS = 4;
const AUTO_DISMISS_MS = 10000;

interface NotificationItem {
  id: string;
  taskId: string;
  workspaceId: string;
  title: string;
  status: { key: string; label: string };
  text: string;
}

/**
 * Tasks whose status or cycle changed between two snapshots. Once a workspace
 * is loaded, a task with no earlier copy is a transition only if it was
 * created after that load; an older one was fetched because someone opened it.
 */
const transitionedTasks = (
  state: ReturnType<typeof useTaskStore.getState>,
  prev: ReturnType<typeof useTaskStore.getState>
): WorkspaceTaskResponse[] => {
  const changed = [];
  for (const [workspaceId, tasks] of Object.entries(state.tasks)) {
    const before = prev.tasks[workspaceId];
    if (tasks === before || state.seeded[workspaceId] !== prev.seeded[workspaceId]) continue;
    for (const task of Object.values(tasks)) {
      const old = before?.[task.id];
      const seededAt = prev.seeded[workspaceId];
      const transitioned = old
        ? old.status !== task.status || old.cycleNo !== task.cycleNo
        : seededAt === undefined || task.createdAt > BigInt(seededAt);
      if (transitioned) changed.push(task);
    }
  }
  return changed;
};

const WorkspaceTaskNotifications = () => {
  const navigate = useNavigate();
  const [notifications, setNotifications] = useState<NotificationItem[]>([]);
  const timersRef = useRef(new Map<string, number>());
  const seenRef = useRef(new Set<string>());
  const runNowSkipped = useFleetActivityStore((state) => state.runNowSkipped);
  const dismissRunNowSkipped = useFleetActivityStore((state) => state.dismissRunNowSkipped);

  const dismiss = useCallback((id: string) => {
    const timer = timersRef.current.get(id);
    if (timer) {
      window.clearTimeout(timer);
      timersRef.current.delete(id);
    }
    setNotifications((current) => current.filter((item) => item.id !== id));
  }, []);

  useEffect(() => {
    // Capture the timers map at effect setup so the cleanup clears exactly
    // the timers this effect set up, even if the ref is later swapped out
    // (e.g. by switching to useState/useReducer).
    const timers = timersRef.current;
    // A task reaching attention toasts once per cycle and status. Tasks
    // already in attention at their workspace's first details load never
    // toast; the workspace banner shows them.
    const seen = seenRef.current;
    const notify = (task: WorkspaceTaskResponse, id: string) => {
      const item: NotificationItem = {
        id,
        taskId: task.id,
        workspaceId: task.workspaceId,
        title: task.title || 'Workspace task',
        status: taskDisplayStatus(task),
        text: task.error || task.resultSummary || 'Open the task to see what it needs.',
      };

      setNotifications((current) => [
        item,
        ...current.filter((existing) => existing.taskId !== item.taskId),
      ].slice(0, MAX_NOTIFICATIONS));
      timersRef.current.set(id, window.setTimeout(() => dismiss(id), AUTO_DISMISS_MS));
    };

    const unsubscribe = useTaskStore.subscribe((state, prev) => {
      for (const task of transitionedTasks(state, prev)) {
        if (!isTaskAttention(task)) continue;
        const id = `${task.id}:${task.cycleNo}:${task.status}`;
        if (seen.has(id)) continue;
        seen.add(id);
        notify(task, id);
      }
    });

    return () => {
      unsubscribe();
      for (const timer of timers.values()) {
        window.clearTimeout(timer);
      }
      timers.clear();
    };
  }, [dismiss]);

  if (notifications.length === 0 && runNowSkipped.length === 0) {
    return null;
  }

  return (
    <div className={styles.stack} aria-live="polite" aria-label="Workspace notifications">
      {runNowSkipped.map((workspaceId) => (
        <div
          key={`run-now-skipped:${workspaceId}`}
          role="status"
          className={`${styles.toast} ${styles.toastNeutral}`}
        >
          <p className={styles.body}>Run now skipped: session busy</p>
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.openButton}
              onClick={() => {
                navigate(`/workspace/${workspaceId}`);
                dismissRunNowSkipped(workspaceId);
              }}
            >
              Open workspace
            </button>
            <button
              type="button"
              className={styles.dismissButton}
              onClick={() => dismissRunNowSkipped(workspaceId)}
              aria-label="Dismiss notification"
            >
              Dismiss
            </button>
          </div>
        </div>
      ))}
      {notifications.map((item) => (
        <div key={item.id} className={styles.toast}>
          <div className={styles.toastHeader}>
            <span className={styles.title}>{item.title}</span>
            <span className={`${styles.status} ${styles[`status_${item.status.key}`] || ''}`}>
              {item.status.label}
            </span>
          </div>
          <p className={styles.body}>{item.text}</p>
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.openButton}
              aria-label={`Open task ${item.title}`}
              onClick={() => {
                navigate(`/workspace/${item.workspaceId}?task=${encodeURIComponent(item.taskId)}`);
                dismiss(item.id);
              }}
            >
              Open task
            </button>
            <button
              type="button"
              className={styles.dismissButton}
              onClick={() => dismiss(item.id)}
              aria-label="Dismiss notification"
            >
              Dismiss
            </button>
          </div>
        </div>
      ))}
    </div>
  );
};

export default WorkspaceTaskNotifications;
