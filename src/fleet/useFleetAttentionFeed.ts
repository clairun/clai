/**
 * Feeds approval and path-grant events into the Fleet store, app-wide.
 * Mounted once in MainLayout next to the assistant event listener.
 */
import { useEffect } from 'react';
import { listen } from '@tauri-apps/api/event';
import type { PathGrantRequest, PermissionRequest } from '../generated/bindings';
import { useFleetStore } from './fleetStore';

export function useFleetAttentionFeed(): void {
  useEffect(() => {
    const store = useFleetStore.getState;
    // A resolved event fires for every way a request leaves the queue, user decisions included.
    const unlisten = [
      listen<PermissionRequest>('permissions://request', (event) => {
        if (event.payload) store().applyRequest({ kind: 'approval', request: event.payload });
      }),
      listen<PathGrantRequest>('path-grants://request', (event) => {
        if (event.payload) store().applyRequest({ kind: 'grant', request: event.payload });
      }),
      listen<{ requestId?: string }>('permissions://resolved', (event) => {
        if (event.payload?.requestId) store().resolveRequest('approval', event.payload.requestId);
      }),
      listen<{ requestId?: string }>('path-grants://resolved', (event) => {
        if (event.payload?.requestId) store().resolveRequest('grant', event.payload.requestId);
      }),
    ];
    return () => {
      for (const p of unlisten) p.then((fn) => fn()).catch(() => {});
    };
  }, []);
}
