import { useEffect } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { isWorkspaceUnread, markWorkspaceOpened } from '../workspace/client';

/** A workspace is read when it appears in this window, even without focus. */
export function useVisibleWorkspaceRead(workspaceId: string | null, revision: unknown): void {
  useEffect(() => {
    if (!workspaceId) return;
    let mounted = true;
    let checking = false;
    let seen = false;
    const appWindow = getCurrentWindow();
    const check = async () => {
      if (checking) return;
      checking = true;
      try {
        const [visible, minimized] = await Promise.all([appWindow.isVisible(), appWindow.isMinimized()]);
        const shown = visible && !minimized;
        if (!shown) {
          seen = false;
        } else if (mounted) {
          // A minimize/restore pair can happen entirely between polls. Query
          // the backend's unread state while visible so that transition cannot
          // hide a run completion, without writing on every poll.
          const shouldMark = !seen || await isWorkspaceUnread(workspaceId);
          if (mounted && shouldMark) {
            const stillShown = await Promise.all([appWindow.isVisible(), appWindow.isMinimized()]);
            if (mounted && stillShown[0] && !stillShown[1]) {
              seen = true;
              await markWorkspaceOpened(workspaceId);
            } else {
              seen = false;
            }
          }
        }
      } catch {
        seen = false; // Retry a failed native read or stamp on the next recheck.
      } finally {
        checking = false;
      }
    };
    const onVisibility = () => { void check(); };
    void check();
    document.addEventListener('visibilitychange', onVisibility);
    window.addEventListener('focus', onVisibility);
    // Webview visibility/resize events vary by OS. Reconcile backend unread
    // state on the mounted poll even if a minimize/restore pair was missed.
    const interval = window.setInterval(onVisibility, 1000);
    const unlistenResize = appWindow.onResized(onVisibility);
    return () => {
      mounted = false;
      document.removeEventListener('visibilitychange', onVisibility);
      window.removeEventListener('focus', onVisibility);
      window.clearInterval(interval);
      void unlistenResize.then((unlisten) => unlisten()).catch(() => {});
    };
  }, [workspaceId, revision]);
}
