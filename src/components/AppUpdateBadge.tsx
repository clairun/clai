/**
 * AppUpdateBadge Component
 *
 * The app's only update surface: a persistent pill in the fleet top bar,
 * visible from every route (FleetLayout wraps both `/fleet` and
 * `/workspace/:id`). It stays up until the update is applied — deliberately
 * not dismissible, and deliberately not a toast: an update is worth knowing
 * about, never worth interrupting for.
 *
 * Clicking the pill opens Settings > About, which hosts the release notes,
 * the manual check and the notify-only (Linux package manager) copy.
 *
 * Once the package has finished downloading in the background, a "Restart to
 * install" button appears beside the pill. That click is the ONLY thing that
 * installs an update; the package is already on disk, so it is a restart, not
 * a download wait. Ignoring it costs the user nothing — the app keeps running
 * the current version and picks the download up again on the next launch.
 */

import React, { useCallback, useState } from 'react';
import { useAvailableAppUpdate } from '../hooks/useAvailableAppUpdate';
import { openGlobalSettings } from '../utils/globalSettings';
import { installAppUpdate, updateErrorText } from '../utils/appUpdates';
import styles from './AppUpdateBadge.module.css';

const AppUpdateBadge = () => {
  const { update } = useAvailableAppUpdate();
  const [restarting, setRestarting] = useState(false);
  const [error, setError] = useState('');

  // A newer version arriving must not inherit the previous version's failed
  // attempt. Render-phase state adjustment (React's documented pattern for
  // derived resets) keeps the reset in the same commit as the new version.
  const version = update?.version ?? null;
  const [seenVersion, setSeenVersion] = useState<string | null>(version);
  if (version !== seenVersion) {
    setSeenVersion(version);
    setRestarting(false);
    setError('');
  }

  const restartToInstall = useCallback(async () => {
    setRestarting(true);
    setError('');
    try {
      // The backend installs the cached package and restarts the app, so a
      // resolved promise is not the success path — the process is gone by
      // then. Only the rejection matters here.
      await installAppUpdate(() => {});
      setRestarting(false);
    } catch (err) {
      setError(updateErrorText(err, 'Could not install the update.'));
      setRestarting(false);
    }
  }, []);

  if (!update) return null;

  return (
    <div className={styles.group}>
      <button
        type="button"
        className={styles.badge}
        onClick={() => openGlobalSettings({ tab: 'about' })}
        title={
          update.downloaded
            ? `CLAI v${update.version} is downloaded — restart to install it`
            : `CLAI v${update.version} is available — click for details`
        }
        aria-label={`Update available: CLAI v${update.version}`}
      >
        <span className={styles.dot} aria-hidden="true" />
        Update available · v{update.version}
      </button>
      {update.downloaded && (
        <button
          type="button"
          className={styles.action}
          onClick={restartToInstall}
          disabled={restarting}
        >
          {restarting ? 'Restarting...' : 'Restart to install'}
        </button>
      )}
      {error && (
        <span className={styles.error} role="alert" title={error}>
          {error}
        </span>
      )}
    </div>
  );
};

export default AppUpdateBadge;
