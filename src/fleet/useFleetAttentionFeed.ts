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
    for (const kind of ['approval', 'grant'] as const) {
      const channel = CHANNELS[kind];
      unlisten.push(
        listen<PermissionRequest | PathGrantRequest>(`${channel}://request`, (event) => {
          if (event.payload) store().applyRequest(kind, event.payload);
        }),
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
