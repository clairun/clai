/**
 * Shared subscription to the "an app update is available" state.
 *
 * Seeds from the backend's last check result (so a UI mounted after the
 * startup check still sees the update) and then follows the
 * `app-updates://available` event emitted by later checks. Used by both
 * the dismissible toast (AppUpdateNotifications) and the persistent
 * top-bar badge (AppUpdateBadge) so they can't drift apart.
 */

import { useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type {
  AppUpdateAvailableEvent,
  AppUpdateInfo,
  AppUpdateStatus,
} from '../generated/bindings';
import { APP_UPDATE_AVAILABLE_EVENT } from '../utils/appUpdates';

export const useAvailableAppUpdate = (): AppUpdateInfo | null => {
  const [update, setUpdate] = useState<AppUpdateInfo | null>(null);

  useEffect(() => {
    let cancelled = false;

    invoke<AppUpdateStatus>('get_app_update_status')
      .then((status) => {
        const found = status.lastCheck?.update;
        if (!cancelled && found) {
          // Seed only fills the initial gap: if the live event already
          // delivered an update, keep it (it is at least as fresh).
          setUpdate((current) => current ?? found);
        }
      })
      .catch((error) => {
        console.error('[useAvailableAppUpdate] Failed to read update status:', error);
      });

    const unlistenPromise = listen<AppUpdateAvailableEvent>(APP_UPDATE_AVAILABLE_EVENT, (event) => {
      if (event.payload?.update) {
        setUpdate(event.payload.update);
      }
    });

    return () => {
      cancelled = true;
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  return update;
};
