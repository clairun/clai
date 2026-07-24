import React, { useCallback, useState } from 'react';
import {
  LATEST_RELEASE_URL,
  installAppUpdate,
  installEventText,
  updateErrorText,
} from '../utils/appUpdates';
import { useAvailableAppUpdate } from '../hooks/useAvailableAppUpdate';
import { openExternal } from '../utils/openExternal';
import styles from './WorkspaceTaskNotifications.module.css';

interface InstallState {
  error: string;
  progress: string;
  installing: boolean;
}

const IDLE_INSTALL: InstallState = { error: '', progress: '', installing: false };

/**
 * Dismissible toast shown when an update becomes available. Dismissal is
 * keyed by version: dismissing v1 keeps the toast hidden for v1 but a later
 * v2 re-surfaces it. The persistent top-bar badge (AppUpdateBadge) is the
 * always-visible counterpart and is not affected by dismissal here.
 */
const AppUpdateNotifications = () => {
  const update = useAvailableAppUpdate();
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  const [install, setInstall] = useState<InstallState>(IDLE_INSTALL);

  // A different version arriving should not inherit a stale error or
  // progress line from a previous install attempt. Render-phase state
  // adjustment (React's recommended pattern for derived resets).
  const [seenVersion, setSeenVersion] = useState<string | null>(null);
  const version = update?.version ?? null;
  if (version !== seenVersion) {
    setSeenVersion(version);
    setInstall(IDLE_INSTALL);
  }

  const dismiss = useCallback(() => {
    setDismissedVersion(update?.version ?? null);
    setInstall(IDLE_INSTALL);
  }, [update?.version]);

  const startInstall = useCallback(async () => {
    setInstall({ error: '', progress: 'Starting download...', installing: true });
    try {
      await installAppUpdate((event) => {
        setInstall((current) => ({ ...current, progress: installEventText(event) }));
      });
    } catch (error) {
      setInstall({
        error: updateErrorText(error, 'Failed to install update.'),
        progress: '',
        installing: false,
      });
    }
  }, []);

  const viewRelease = useCallback(() => {
    openExternal(LATEST_RELEASE_URL).catch((error) => {
      console.error('[AppUpdateNotifications] Failed to open release page:', error);
    });
  }, []);

  if (!update || update.version === dismissedVersion) return null;

  const installable = update.installable;
  const body = install.error
    ? install.error
    : install.progress ||
      (installable
        ? `CLAI v${update.version} is ready to install.`
        : `CLAI v${update.version} is available. This build updates outside CLAI — get it from GitHub Releases.`);

  return (
    <div
      className={styles.stack}
      style={{ top: 'auto', bottom: 18 }}
      aria-live="polite"
      aria-label="App update notification"
    >
      <div className={styles.toast}>
        <div className={styles.toastHeader}>
          <span className={styles.title}>Update available</span>
          <span className={styles.status}>v{update.version}</span>
        </div>
        <p className={styles.body}>{body}</p>
        <div className={styles.actions}>
          {installable ? (
            <button
              type="button"
              className={styles.openButton}
              onClick={startInstall}
              disabled={install.installing}
            >
              {install.installing ? 'Installing...' : 'Install and restart'}
            </button>
          ) : (
            <button type="button" className={styles.openButton} onClick={viewRelease}>
              View release
            </button>
          )}
          <button
            type="button"
            className={styles.dismissButton}
            onClick={dismiss}
            aria-label="Dismiss update notification"
            disabled={install.installing}
          >
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
};

export default AppUpdateNotifications;
