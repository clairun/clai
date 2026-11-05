import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import {
  listPendingPathGrantCounts,
  listPendingPermissionCounts,
} from '../permissions/client';
import { listPendingNetworkAccessCounts } from '../permissions/networkAccessClient';

const PERMISSION_ATTENTION_EVENT = 'permissions://attention';
const PATH_GRANT_ATTENTION_EVENT = 'path-grants://attention';
const NETWORK_ACCESS_ATTENTION_EVENT = 'network-access://attention';

interface AttentionPayload {
  pendingCount: number;
  workspaceId?: string | null;
}

type Counts = Record<string, number>;

const NULL_KEY = '__null';

const applyUpdate = (current: Counts, payload: AttentionPayload | undefined): Counts => {
  if (!payload || typeof payload.pendingCount !== 'number') return current;
  const key = payload.workspaceId ?? NULL_KEY;
  if (payload.pendingCount <= 0) {
    if (!(key in current)) return current;
    const next = { ...current };
    delete next[key];
    return next;
  }
  if (current[key] === payload.pendingCount) return current;
  return { ...current, [key]: payload.pendingCount };
};

/**
 * Subscribes to backend per-workspace pending-approval count updates.
 * Returns an object keyed by workspace id (or '__null' for the
 * unattributed bucket) → pending count.
 *
 * Both initial state and live updates contribute: on mount, the hook
 * calls `list_pending_permission_counts` / `list_pending_path_grant_counts`
 * to capture requests that fired before the listener was attached
 * (e.g. the user was on another page when an agent requested approval).
 * After that the event stream (`permissions://attention`,
 * `path-grants://attention`) keeps the map current.
 *
 * The streams are merged: callers see one count per workspace spanning
 * shell-command approvals, filesystem path grants and network access
 * requests, since "needs attention" is a single UI signal.
 */
export function usePermissionAttention(): Counts {
  const [permissionCounts, setPermissionCounts] = useState<Counts>({});
  const [pathGrantCounts, setPathGrantCounts] = useState<Counts>({});
  const [networkCounts, setNetworkCounts] = useState<Counts>({});

  useEffect(() => {
    let cancelled = false;
    const unlistenPromises: Promise<() => void>[] = [];

    // Seed before subscribing so we don't miss requests that fired
    // before mount. A race where an event arrives during the seed is
    // harmless: each event carries the absolute count, so the latest
    // value wins.
    Promise.all([listPendingPermissionCounts(), listPendingPathGrantCounts(), listPendingNetworkAccessCounts()])
      .then(([perms, paths, network]) => {
        if (cancelled) return;
        setPermissionCounts(perms || {});
        setPathGrantCounts(paths || {});
        setNetworkCounts(network || {});
      })
      .catch(() => {
        // Non-fatal: live events will still update the badge.
      });

    unlistenPromises.push(
      listen<AttentionPayload>(PERMISSION_ATTENTION_EVENT, (event) => {
        setPermissionCounts((current) => applyUpdate(current, event.payload));
      }),
    );
    unlistenPromises.push(
      listen<AttentionPayload>(PATH_GRANT_ATTENTION_EVENT, (event) => {
        setPathGrantCounts((current) => applyUpdate(current, event.payload));
      }),
    );
    unlistenPromises.push(
      listen<AttentionPayload>(NETWORK_ACCESS_ATTENTION_EVENT, (event) => {
        setNetworkCounts((current) => applyUpdate(current, event.payload));
      }),
    );

    return () => {
      cancelled = true;
      for (const promise of unlistenPromises) {
        promise.then((unlisten) => unlisten()).catch(() => {});
      }
    };
  }, []);

  const merged: Counts = { ...permissionCounts };
  for (const counts of [pathGrantCounts, networkCounts]) {
    for (const [key, value] of Object.entries(counts)) {
      merged[key] = (merged[key] || 0) + value;
    }
  }
  return merged;
}
