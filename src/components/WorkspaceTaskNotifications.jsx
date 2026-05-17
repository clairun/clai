import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useNavigate } from 'react-router-dom';
import { useFleet } from '../contexts/FleetContext';
import styles from './WorkspaceTaskNotifications.module.css';

const WORKSPACE_TASK_ATTENTION_EVENT = 'workspace://task-attention';
const MAX_NOTIFICATIONS = 4;
const AUTO_DISMISS_MS = 10000;

const STATUS_LABEL = {
  blocked: 'Blocked',
  failed: 'Failed',
  needs_user_input: 'Needs input',
};

const notificationText = (payload) => (
  payload?.error || payload?.summary || 'Open the workspace to inspect the task.'
);

const WorkspaceTaskNotifications = () => {
  const navigate = useNavigate();
  const { refresh } = useFleet();
  const [notifications, setNotifications] = useState([]);
  const timersRef = useRef(new Map());

  const dismiss = useCallback((id) => {
    const timer = timersRef.current.get(id);
    if (timer) {
      window.clearTimeout(timer);
      timersRef.current.delete(id);
    }
    setNotifications((current) => current.filter((item) => item.id !== id));
  }, []);

  useEffect(() => {
    const unlistenPromise = listen(WORKSPACE_TASK_ATTENTION_EVENT, (event) => {
      const payload = event.payload;
      if (!payload?.taskId || !payload?.workspaceId) {
        return;
      }

      const id = `${payload.taskId}:${payload.updatedAt || Date.now()}`;
      const item = {
        id,
        taskId: payload.taskId,
        workspaceId: payload.workspaceId,
        title: payload.title || 'Workspace task',
        status: payload.status || 'blocked',
        text: notificationText(payload),
      };

      setNotifications((current) => [
        item,
        ...current.filter((existing) => existing.taskId !== item.taskId),
      ].slice(0, MAX_NOTIFICATIONS));

      const existingTimer = timersRef.current.get(id);
      if (existingTimer) {
        window.clearTimeout(existingTimer);
      }
      timersRef.current.set(id, window.setTimeout(() => dismiss(id), AUTO_DISMISS_MS));

      refresh().catch(() => {});
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
      for (const timer of timersRef.current.values()) {
        window.clearTimeout(timer);
      }
      timersRef.current.clear();
    };
  }, [dismiss, refresh]);

  if (notifications.length === 0) {
    return null;
  }

  return (
    <div className={styles.stack} aria-live="polite" aria-label="Workspace notifications">
      {notifications.map((item) => (
        <div key={item.id} className={styles.toast}>
          <div className={styles.toastHeader}>
            <span className={styles.title}>{item.title}</span>
            <span className={`${styles.status} ${styles[`status_${item.status}`] || ''}`}>
              {STATUS_LABEL[item.status] || item.status}
            </span>
          </div>
          <p className={styles.body}>{item.text}</p>
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.openButton}
              onClick={() => {
                navigate(`/workspace/${item.workspaceId}`);
                dismiss(item.id);
              }}
            >
              Open workspace
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
