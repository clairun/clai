/**
 * Feeds approval and path-grant events into the Fleet store, app-wide.
 * Mounted once in MainLayout next to the assistant event listener.
 */
import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { PathGrantRequest, PermissionRequest } from '../generated/bindings';
import { useFleetStore } from './fleetStore';
import type { FleetRequestKind } from './fleetModel';

const CHANNELS: Record<FleetRequestKind, string> = {
  approval: 'permissions',
  grant: 'path-grants',
};

export function useFleetAttentionFeed(): void {
  useEffect(() => {
    const store = useFleetStore.getState;
    const unlisten: Promise<() => void>[] = [];
    unlisten.push(
      listen<PermissionRequest>('permissions://request', (event) => {
        if (event.payload) store().applyRequest({ kind: 'approval', request: event.payload });
      }),
      listen<PathGrantRequest>('path-grants://request', (event) => {
        if (event.payload) store().applyRequest({ kind: 'grant', request: event.payload });
      })
    );
    for (const kind of ['approval', 'grant'] as const) {
      const channel = CHANNELS[kind];
      unlisten.push(
        // Fires for every way a request leaves the queue, user decisions included.
        listen<{ requestId?: string }>(`${channel}://resolved`, (event) => {
          if (event.payload?.requestId) store().resolveRequest(kind, event.payload.requestId);
        })
      );
    }
    return () => {
      for (const p of unlisten) p.then((fn) => fn()).catch(() => {});
    };
  }, []);
}
