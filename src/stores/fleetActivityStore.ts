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

/** Placeholder run id Fleet shows between "Run now" and the real run's events. */
export const optimisticRunNowId = (workspaceId: string): string =>
  `optimistic-runnow:${workspaceId}`;

interface SkippedRun {
  sessionId: string;
  activeRunId: string | null;
  at: bigint;
}

interface FleetActivityState {
  /** workspaceId -> in-flight run ids. */
  activeRunsByWorkspace: Record<string, string[]>;
  /** Workspaces whose manual "Run now" was skipped because the session was busy. */
  runNowSkipped: string[];
  skippedRunByWorkspace: Record<string, SkippedRun>;
  latestAcceptedRunBySession: Record<string, { runId: string; startedAt: bigint }>;
  markRunStarted: (workspaceId: string, runId: string) => void;
  markRunEnded: (workspaceId: string, runId: string) => void;
  /** Replace a workspace's runs with a fresh read (the Fleet snapshot). */
  setRuns: (workspaceId: string, runIds: readonly string[]) => void;
  markRunNowSkipped: (
    workspaceId: string,
    sessionId: string,
    activeRunId: string | null,
    at: bigint
  ) => void;
  observeAcceptedRun: (
    workspaceId: string,
    sessionId: string,
    runId: string,
    startedAt: bigint
  ) => void;
  dismissRunNowSkipped: (workspaceId: string) => void;
}

export const useFleetActivityStore = create<FleetActivityState>()((set, get) => ({
  activeRunsByWorkspace: {},
  runNowSkipped: [],
  skippedRunByWorkspace: {},
  latestAcceptedRunBySession: {},

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

  setRuns: (workspaceId, runIds) =>
    set((state) => {
      const map = { ...state.activeRunsByWorkspace };
      if (runIds.length === 0) {
        delete map[workspaceId];
      } else {
        map[workspaceId] = [...runIds];
      }
      return { activeRunsByWorkspace: map };
    }),

  markRunNowSkipped: (workspaceId, sessionId, activeRunId, at) => {
    const current = get().skippedRunByWorkspace[workspaceId];
    if (current && current.at >= at) return;
    const accepted = get().latestAcceptedRunBySession[sessionId];
    if (accepted && accepted.runId !== activeRunId && accepted.startedAt >= at) return;
    get().markRunEnded(workspaceId, optimisticRunNowId(workspaceId));
    set((state) => ({
      runNowSkipped: [workspaceId, ...state.runNowSkipped.filter((id) => id !== workspaceId)],
      skippedRunByWorkspace: {
        ...state.skippedRunByWorkspace,
        [workspaceId]: { sessionId, activeRunId, at },
      },
    }));
  },

  observeAcceptedRun: (workspaceId, sessionId, runId, startedAt) => {
    const at = BigInt(startedAt);
    set((state) => {
      const previous = state.latestAcceptedRunBySession[sessionId];
      if (previous && previous.startedAt > at) return state;
      return {
        latestAcceptedRunBySession: {
          ...state.latestAcceptedRunBySession,
          [sessionId]: { runId, startedAt: at },
        },
      };
    });
    const skipped = get().skippedRunByWorkspace[workspaceId];
    if (skipped?.sessionId === sessionId && skipped.activeRunId !== runId && at >= skipped.at) {
      get().dismissRunNowSkipped(workspaceId);
    }
  },

  dismissRunNowSkipped: (workspaceId) =>
    set((state) => {
      const skippedRunByWorkspace = { ...state.skippedRunByWorkspace };
      delete skippedRunByWorkspace[workspaceId];
      return {
        runNowSkipped: state.runNowSkipped.filter((id) => id !== workspaceId),
        skippedRunByWorkspace,
      };
    }),
}));
