import React, { useCallback, useState } from 'react';
import {
  installAppUpdate,
  installEventText,
  updateErrorText,
} from '../utils/appUpdates';
import { useAvailableAppUpdate } from '../hooks/useAvailableAppUpdate';
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
  const { update, support } = useAvailableAppUpdate();
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

  if (!update || update.version === dismissedVersion) return null;

  // Linux packages (deb/rpm/Flatpak) update through the OS package manager,
  // not through CLAI itself — so we deliberately suppress the toast and any
  // install CTA. `support` is `null` until the initial
  // `get_app_update_status` resolve completes; treat that as "not Linux" so
  // we don't flicker the toast on first render while support is still
  // loading.
  if (support === null || support.platform === 'linux') return null;

  // installable is always true at this point: the early return at the top
  // of this component filters Linux (which the backend marks
  // installable:false), so we never need to render the "get it from
  // GitHub Releases" branch or its View release button here.
  const body = install.error
    ? install.error
    : install.progress ||
      (update.downloaded
        ? `CLAI v${update.version} has been downloaded. Restart to apply it.`
        : `CLAI v${update.version} is ready to install.`);

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
          <button
            type="button"
            className={styles.openButton}
            onClick={startInstall}
            disabled={install.installing}
          >
            {install.installing
              ? 'Installing...'
              : update.downloaded
                ? 'Restart now'
                : 'Install and restart'}
          </button>
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
