import { Channel, invoke } from '@tauri-apps/api/core';
import type { AppUpdateInstallEvent } from '../generated/bindings';

export const APP_UPDATE_AVAILABLE_EVENT = 'app-updates://available';

/** Where notify-only builds (e.g. Flatpak) send users to fetch the update. */
export const LATEST_RELEASE_URL = 'https://github.com/clairun/clai/releases/latest';

export const updateErrorText = (error: unknown, fallback: string): string =>
  typeof error === 'string' ? error : fallback;

const formatBytes = (value: bigint | number): string => {
  const bytes = Number(value);
  if (!Number.isFinite(bytes) || bytes <= 0) return '0 MB';
  const mb = bytes / (1024 * 1024);
  if (mb < 1) return `${Math.max(0.1, Math.round(mb * 10) / 10)} MB`;
  return `${Math.round(mb * 10) / 10} MB`;
};

export const installEventText = (event: AppUpdateInstallEvent): string => {
  switch (event.type) {
    case 'started':
      return 'Starting download...';
    case 'progress': {
      const downloaded = formatBytes(event.downloaded);
      return event.total ? `${downloaded} of ${formatBytes(event.total)}` : downloaded;
    }
    case 'downloadFinished':
      return 'Download complete.';
    case 'installing':
      return 'Installing update...';
    default:
      return '';
  }
};

export const installAppUpdate = async (
  onEvent: (event: AppUpdateInstallEvent) => void,
): Promise<void> => {
  const channel = new Channel<AppUpdateInstallEvent>();
  channel.onmessage = onEvent;
  await invoke('install_app_update', { onEvent: channel });
};
