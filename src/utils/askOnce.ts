import { ask } from '@tauri-apps/plugin-dialog';

let asking = false;

/**
 * `ask`, but while a dialog is open further calls resolve false without
 * opening another: a double-click must not stack two dialogs.
 */
export const askOnce = async (...args: Parameters<typeof ask>): Promise<boolean> => {
  if (asking) return false;
  asking = true;
  try {
    return await ask(...args);
  } finally {
    asking = false;
  }
};
