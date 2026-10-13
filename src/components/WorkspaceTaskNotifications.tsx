import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useNavigate } from 'react-router';
import type { AssistantEventEnvelope, WorkspaceTaskResponse } from '../generated/bindings';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import styles from './WorkspaceTaskNotifications.module.css';

const ASSISTANT_EVENT = 'assistant://event';
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

/** The statuses that toast, and how they read; null for the rest. */
const attentionStatus = (task: WorkspaceTaskResponse): NotificationItem['status'] | null => {
  if (task.status === 'input_required') return { key: 'input_required', label: 'Needs an answer' };
  if (task.status !== 'failed') return null;
  return task.failureReason === 'blocked'
    ? { key: 'blocked', label: 'Blocked' }
    : { key: 'failed', label: 'Failed' };
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
    // already in attention when the app starts emit nothing, so they never
    // toast; the workspace banner shows them.
    const seen = seenRef.current;
    const unlistenPromise = listen<AssistantEventEnvelope>(ASSISTANT_EVENT, (event) => {
      if (event.payload.event.type !== 'task_updated') return;
      const task = event.payload.event.payload.task;
      const status = attentionStatus(task);
      if (!status) return;
      const id = `${task.id}:${task.cycleNo}:${task.status}`;
      if (seen.has(id)) return;
      seen.add(id);

      const item: NotificationItem = {
        id,
        taskId: task.id,
        workspaceId: task.workspaceId,
        title: task.title || 'Workspace task',
        status,
        text: task.error || task.resultSummary || 'Open the task to see what it needs.',
      };

      setNotifications((current) => [
        item,
        ...current.filter((existing) => existing.taskId !== item.taskId),
      ].slice(0, MAX_NOTIFICATIONS));
      timersRef.current.set(id, window.setTimeout(() => dismiss(id), AUTO_DISMISS_MS));
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
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
