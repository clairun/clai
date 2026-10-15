import React from 'react';
import { useNavigate } from 'react-router';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import styles from './WorkspaceTaskNotifications.module.css';

const WorkspaceTaskNotifications = () => {
  const navigate = useNavigate();
  const runNowSkipped = useFleetActivityStore((state) => state.runNowSkipped);
  const dismissRunNowSkipped = useFleetActivityStore((state) => state.dismissRunNowSkipped);

  if (runNowSkipped.length === 0) {
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
    </div>
  );
};

export default WorkspaceTaskNotifications;
