import { useMemo } from 'react';
import { useFleetActivityStore } from '../stores/fleetActivityStore';

/**
 * Returns `{ [workspaceId]: number-of-in-flight-runs }`.
 *
 * Reads the global `fleetActivityStore`, which is fed by the app-global
 * assistant event listener (`useAssistantEvents` in MainLayout). Because the
 * store lives above the page tree, run state PERSISTS across navigation —
 * a run that starts on a workspace page is still reflected when you land on
 * Fleet. (This hook used to own its own `assistant://event` subscription with
 * local state, which reset to empty on every Fleet mount and so missed runs
 * that started elsewhere.)
 *
 * Callers treat any positive value as "processing".
 */
export function useFleetActivity(): Record<string, number> {
  const activeRunsByWorkspace = useFleetActivityStore((s) => s.activeRunsByWorkspace);
  return useMemo(() => {
    const out: Record<string, number> = {};
    for (const [workspaceId, runIds] of Object.entries(activeRunsByWorkspace)) {
      out[workspaceId] = runIds.length;
    }
    return out;
  }, [activeRunsByWorkspace]);
}
