import React, { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { AppUpdateAvailableEvent, AppUpdateInfo, AppUpdateStatus } from '../generated/bindings';
import {
  APP_UPDATE_AVAILABLE_EVENT,
  LATEST_RELEASE_URL,
  installAppUpdate,
  installEventText,
  updateErrorText,
} from '../utils/appUpdates';
import { openExternal } from '../utils/openExternal';
import styles from './WorkspaceTaskNotifications.module.css';

interface NotificationItem {
  update: AppUpdateInfo;
  error: string;
  progress: string;
  installing: boolean;
}

const AppUpdateNotifications = () => {
  const [item, setItem] = useState<NotificationItem | null>(null);

  const showUpdate = useCallback((update: AppUpdateInfo) => {
    setItem({
      update,
      error: '',
      progress: '',
      installing: false,
    });
  }, []);

  useEffect(() => {
    let cancelled = false;
    invoke<AppUpdateStatus>('get_app_update_status')
      .then((status) => {
        const update = status.lastCheck?.update;
        if (!cancelled && update) {
          showUpdate(update);
        }
      })
      .catch((error) => {
        console.error('[AppUpdateNotifications] Failed to read update status:', error);
      });

    const unlistenPromise = listen<AppUpdateAvailableEvent>(APP_UPDATE_AVAILABLE_EVENT, (event) => {
      if (event.payload?.update) {
        showUpdate(event.payload.update);
      }
    });

    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [showUpdate]);

  const dismiss = useCallback(() => setItem(null), []);

  const install = useCallback(async () => {
    setItem((current) =>
      current
        ? {
            ...current,
            error: '',
            installing: true,
            progress: 'Starting download...',
          }
        : current,
    );
    try {
      await installAppUpdate((event) => {
        setItem((current) =>
          current
            ? {
                ...current,
                progress: installEventText(event),
              }
            : current,
        );
      });
    } catch (error) {
      setItem((current) =>
        current
          ? {
              ...current,
              error: updateErrorText(error, 'Failed to install update.'),
              installing: false,
            }
          : current,
      );
    }
  }, []);

  const viewRelease = useCallback(() => {
    openExternal(LATEST_RELEASE_URL).catch((error) => {
      console.error('[AppUpdateNotifications] Failed to open release page:', error);
    });
  }, []);

  if (!item) return null;

  const installable = item.update.installable;
  const body = item.error
    ? item.error
    : item.progress ||
      (installable
        ? `CLAI v${item.update.version} is ready to install.`
        : `CLAI v${item.update.version} is available. This build updates outside CLAI — get it from GitHub Releases.`);

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
          <span className={styles.status}>v{item.update.version}</span>
        </div>
        <p className={styles.body}>{body}</p>
        <div className={styles.actions}>
          {installable ? (
            <button
              type="button"
              className={styles.openButton}
              onClick={install}
              disabled={item.installing}
            >
              {item.installing ? 'Installing...' : 'Install and restart'}
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
            disabled={item.installing}
          >
            Dismiss
          </button>
        </div>
      </div>
    </div>
  );
};

export default AppUpdateNotifications;
