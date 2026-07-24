/**
 * AppUpdateBadge Component
 *
 * Persistent "update available" pill for the fleet top bar. Unlike the
 * dismissible toast (AppUpdateNotifications), this stays visible until the
 * update is actually applied, so the user is always aware a new version
 * exists. Clicking it opens the global Settings modal at the About section,
 * which hosts the full install / view-release controls.
 */

import React from 'react';
import { useAvailableAppUpdate } from '../hooks/useAvailableAppUpdate';
import { openGlobalSettings } from '../utils/globalSettings';
import styles from './AppUpdateBadge.module.css';

const AppUpdateBadge = () => {
  const update = useAvailableAppUpdate();

  if (!update) return null;

  return (
    <button
      type="button"
      className={styles.badge}
      onClick={() => openGlobalSettings({ tab: 'about' })}
      title={
        update.installable
          ? `CLAI v${update.version} is ready to install — click to update`
          : `CLAI v${update.version} is available — click for details`
      }
      aria-label={`Update available: CLAI v${update.version}`}
    >
      <span className={styles.dot} aria-hidden="true" />
      Update available · v{update.version}
    </button>
  );
};

export default AppUpdateBadge;
