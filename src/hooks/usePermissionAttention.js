import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';

const PERMISSION_ATTENTION_EVENT = 'permissions://attention';

/**
 * Subscribes to backend per-workspace pending-approval count updates.
 * Returns an object keyed by workspace id (or '__null' for the
 * unattributed bucket) → pending count.
 *
 * The backend emits one update per state transition: when a new request
 * is registered (count++), when the user submits a decision (count--),
 * and when a request times out (count--).
 */
export function usePermissionAttention() {
  const [counts, setCounts] = useState({});

  useEffect(() => {
    const unlistenPromise = listen(PERMISSION_ATTENTION_EVENT, (event) => {
      const payload = event.payload;
      if (!payload || typeof payload.pendingCount !== 'number') {
        return;
      }
      const key = payload.workspaceId ?? '__null';
      setCounts((current) => {
        if (payload.pendingCount <= 0) {
          if (!(key in current)) return current;
          const next = { ...current };
          delete next[key];
          return next;
        }
        if (current[key] === payload.pendingCount) {
          return current;
        }
        return { ...current, [key]: payload.pendingCount };
      });
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

  return counts;
}
