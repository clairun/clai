/**
 * Fleet Activity Store
 *
 * Tracks which workspaces have an assistant run in flight, keyed by
 * workspace id. Fed by the app-global assistant event listener
 * (`useAssistantEvents`, mounted in MainLayout) so the state PERSISTS across
 * navigation — a run that starts while you're on a workspace page is still
 * reflected when you land on Fleet. (The previous Fleet-page-scoped hook
 * reset to empty on every mount, so it missed those.)
 *
 * Membership is a Set<runId> per workspace (not a refcount): duplicate
 * "start" events (the engine emits both RunQueued and RunStarted for the
 * same run) collapse to one entry, and a missing event can't drift a
 * counter permanently.
 */

import { create } from 'zustand';

interface FleetActivityState {
  /** workspaceId -> in-flight run ids. */
  activeRunsByWorkspace: Record<string, string[]>;
  markRunStarted: (workspaceId: string, runId: string) => void;
  markRunEnded: (workspaceId: string, runId: string) => void;
}

export const useFleetActivityStore = create<FleetActivityState>()((set) => ({
  activeRunsByWorkspace: {},

  markRunStarted: (workspaceId, runId) =>
    set((state) => {
      const existing = state.activeRunsByWorkspace[workspaceId] || [];
      if (existing.includes(runId)) return state;
      return {
        activeRunsByWorkspace: {
          ...state.activeRunsByWorkspace,
          [workspaceId]: [...existing, runId],
        },
      };
    }),

  markRunEnded: (workspaceId, runId) =>
    set((state) => {
      const existing = state.activeRunsByWorkspace[workspaceId];
      if (!existing || !existing.includes(runId)) return state;
      const next = existing.filter((id) => id !== runId);
      const map = { ...state.activeRunsByWorkspace };
      if (next.length === 0) {
        delete map[workspaceId];
      } else {
        map[workspaceId] = next;
      }
      return { activeRunsByWorkspace: map };
    }),
}));
