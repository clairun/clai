import React, { useCallback, useEffect, useRef, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { useLocation, useNavigate } from 'react-router-dom';
import type { PathGrantRequest, PermissionRequest } from '../generated/bindings';
// Reuse the WorkspaceTaskNotifications stack styles so the global
// notifications share visual language. The two surfaces are
// conceptually the same: a toast in the upper-right corner that the
// user can open or dismiss.
import styles from './WorkspaceTaskNotifications.module.css';

const PERMISSION_REQUEST_EVENT = 'permissions://request';
const PATH_GRANT_REQUEST_EVENT = 'path-grants://request';
const MAX_NOTIFICATIONS = 4;
const AUTO_DISMISS_MS = 15000;

interface NotificationItem {
  id: string;
  requestId: string;
  workspaceId: string;
  title: string;
  badge: string;
  body: string;
}

const truncate = (value: string, limit = 80): string =>
  value.length <= limit ? value : `${value.slice(0, limit - 1)}…`;

const buildPermissionNotification = (
  request: PermissionRequest,
): NotificationItem | null => {
  if (!request?.workspaceId || !request?.requestId) return null;
  const title = request.agentName || 'Agent';
  const command = (request.command || '').trim();
  const body = command
    ? `Needs approval to run \`${truncate(command)}\``
    : 'Needs approval to run a shell command.';
  return {
    id: `perm:${request.requestId}`,
    requestId: request.requestId,
    workspaceId: request.workspaceId,
    title,
    badge: 'Approval',
    body,
  };
};

const buildPathGrantNotification = (
  request: PathGrantRequest,
): NotificationItem | null => {
  if (!request?.workspaceId || !request?.requestId) return null;
  const title = request.agentName || 'Agent';
  const path = (request.requestedPath || '').trim();
  const body = path
    ? `Needs filesystem access to \`${truncate(path)}\``
    : 'Needs filesystem access.';
  return {
    id: `path:${request.requestId}`,
    requestId: request.requestId,
    workspaceId: request.workspaceId,
    title,
    badge: 'Path access',
    body,
  };
};

/**
 * Global notifier for cross-workspace permission and path-grant
 * requests. Mounted at MainLayout level so it catches events even when
 * the Fleet page (which renders pending-approval pills) is unmounted.
 *
 * Same-workspace events are suppressed: the in-page inline approval and
 * path-grant cards already surface those, so an extra toast would just
 * add noise. We resolve "same workspace" from the URL (`/workspace/:id`).
 */
const PermissionAttentionNotifications = () => {
  const navigate = useNavigate();
  const location = useLocation();
  const [notifications, setNotifications] = useState<NotificationItem[]>([]);
  const timersRef = useRef(new Map<string, number>());

  // Derive the currently-open workspace id from the URL. We re-read it
  // through a ref so the effect that sets up listeners doesn't need to
  // re-bind on every navigation — toasts arrive asynchronously, and
  // re-subscribing would drop in-flight events.
  const currentWorkspaceIdRef = useRef<string | null>(null);
  useEffect(() => {
    const match = location.pathname.match(/^\/workspace\/([^/]+)/);
    currentWorkspaceIdRef.current = match ? decodeURIComponent(match[1]!) : null;
  }, [location.pathname]);

  const dismiss = useCallback((id: string) => {
    const timer = timersRef.current.get(id);
    if (timer) {
      window.clearTimeout(timer);
      timersRef.current.delete(id);
    }
    setNotifications((current) => current.filter((item) => item.id !== id));
  }, []);

  const enqueue = useCallback(
    (item: NotificationItem | null) => {
      if (!item) return;
      // Same-workspace events are surfaced inline; don't double-notify.
      if (item.workspaceId === currentWorkspaceIdRef.current) return;

      setNotifications((current) => {
        // De-dupe by requestId in case the backend re-emits.
        const filtered = current.filter((existing) => existing.id !== item.id);
        return [item, ...filtered].slice(0, MAX_NOTIFICATIONS);
      });

      const existingTimer = timersRef.current.get(item.id);
      if (existingTimer) {
        window.clearTimeout(existingTimer);
      }
      timersRef.current.set(
        item.id,
        window.setTimeout(() => dismiss(item.id), AUTO_DISMISS_MS),
      );
    },
    [dismiss],
  );

  useEffect(() => {
    // Capture the timers map at effect setup so the cleanup clears exactly
    // the timers this effect set up, even if the ref is later swapped out
    // (e.g. by switching to useState/useReducer).
    const timers = timersRef.current;
    const unlistenPromises = [
      listen<PermissionRequest>(PERMISSION_REQUEST_EVENT, (event) => {
        enqueue(buildPermissionNotification(event.payload));
      }),
      listen<PathGrantRequest>(PATH_GRANT_REQUEST_EVENT, (event) => {
        enqueue(buildPathGrantNotification(event.payload));
      }),
    ];

    return () => {
      for (const promise of unlistenPromises) {
        promise.then((unlisten) => unlisten()).catch(() => {});
      }
      for (const timer of timers.values()) {
        window.clearTimeout(timer);
      }
      timers.clear();
    };
  }, [enqueue]);

  if (notifications.length === 0) return null;

  return (
    <div className={styles.stack} aria-live="polite" aria-label="Permission notifications">
      {notifications.map((item) => (
        <div key={item.id} className={styles.toast}>
          <div className={styles.toastHeader}>
            <span className={styles.title}>{item.title}</span>
            <span className={styles.status}>{item.badge}</span>
          </div>
          <p className={styles.body}>{item.body}</p>
          <div className={styles.actions}>
            <button
              type="button"
              className={styles.openButton}
              onClick={() => {
                navigate(`/workspace/${item.workspaceId}`);
                dismiss(item.id);
              }}
            >
              Open workspace
            </button>
            <button
              type="button"
              className={styles.dismissButton}
              onClick={() => dismiss(item.id)}
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

export default PermissionAttentionNotifications;
