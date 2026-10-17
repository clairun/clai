/**
 * App-global Fleet store. Fed from app start by the global listeners
 * (`useAssistantEvents`, `useFleetAttentionFeed`), so it is current whenever
 * the Fleet opens. `loadSnapshot` follows subscribe → buffer → snapshot →
 * replay: events arriving while the snapshot loads are queued and applied
 * on top of it. The snapshot also seeds the task and run stores the cards
 * read.
 */
import { create } from 'zustand';
import type { AssistantEventEnvelope, AssistantMessage } from '../generated/bindings';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useTaskStore } from '../workspace/taskStore';
import { fleetSnapshot } from './client';
import {
  EMPTY_FLEET,
  applyAssistantEvent,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  isFleetEvent,
  seedFleet,
  type FleetPendingRequest,
  type FleetRequestKind,
  type FleetState,
} from './fleetModel';

type Reducer = (state: FleetState) => FleetState;

/** Snapshot size: enough to fill the largest grid (4 × 3) with room to spare. */
export const SNAPSHOT_LIMIT = 16;

interface FleetStoreState {
  fleet: FleetState;
  /** When `fleet` last changed or was reloaded: the clock recency is judged against, so rendering stays pure. */
  now: number;
  selectedId: string | null;
  /** `null` until the first snapshot lands. */
  loadedAt: number | null;
  loadError: string | null;
  /** Non-null while a snapshot is in flight: events queue here. */
  buffer: Reducer[] | null;
  dispatch: (reducer: Reducer) => void;
  applyEvent: (envelope: AssistantEventEnvelope) => void;
  applyRequest: (pending: FleetPendingRequest) => void;
  resolveRequest: (kind: FleetRequestKind, requestId: string) => void;
  /** The user sent `message` on `sessionId`, or cleared the chat into it (`message` null). */
  noteUserSent: (workspaceId: string, sessionId: string, message: AssistantMessage | null) => void;
  loadSnapshot: () => Promise<void>;
  select: (workspaceId: string | null) => void;
}

export const useFleetStore = create<FleetStoreState>()((set, get) => ({
  fleet: EMPTY_FLEET,
  now: Date.now(),
  selectedId: null,
  loadedAt: null,
  loadError: null,
  buffer: null,

  dispatch: (reducer) => {
    const { buffer, fleet } = get();
    if (buffer) {
      buffer.push(reducer);
      return;
    }
    const next = reducer(fleet);
    if (next !== fleet) set({ fleet: next, now: Date.now() });
  },

  applyEvent: (envelope) => {
    if (!isFleetEvent(envelope)) return;
    get().dispatch((s) => applyAssistantEvent(s, envelope));
  },
  applyRequest: (pending) => {
    const at = Date.now();
    get().dispatch((s) => applyRequest(s, pending, at));
  },
  resolveRequest: (kind, requestId) => {
    const at = Date.now();
    get().dispatch((s) => applyRequestResolved(s, kind, requestId, at));
  },
  noteUserSent: (workspaceId, sessionId, message) => {
    const at = Date.now();
    get().dispatch((s) => applyUserSent(s, workspaceId, sessionId, message, at));
  },

  loadSnapshot: async () => {
    if (get().buffer) return;
    const buffer: Reducer[] = [];
    set({ buffer });
    const runsBefore = useFleetActivityStore.getState().activeRunsByWorkspace;
    let seeded: Reducer = (s) => s;
    let loadError: string | null = null;
    try {
      const snapshot = await fleetSnapshot(SNAPSHOT_LIMIT);
      const now = Date.now();
      const { seedTasks } = useTaskStore.getState();
      const { setRuns, activeRunsByWorkspace: runsNow } = useFleetActivityStore.getState();
      for (const ws of snapshot) {
        seedTasks(ws.workspaceId, ws.tasks);
        // A run that started or ended while the snapshot loaded is newer than the snapshot.
        const before = runsBefore[ws.workspaceId] ?? [];
        const current = runsNow[ws.workspaceId] ?? [];
        const ids = new Set([...ws.activeRunIds, ...current]);
        setRuns(
          ws.workspaceId,
          [...ids].filter((id) =>
            before.includes(id) !== current.includes(id) ? current.includes(id) : ws.activeRunIds.includes(id)
          )
        );
      }
      seeded = (s) => seedFleet(s, snapshot, now);
    } catch (err) {
      loadError = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load the Fleet.';
    }
    const fleet = buffer.reduce((s, reducer) => reducer(s), seeded(get().fleet));
    const now = Date.now();
    // A selected card that is gone (deleted, or no longer recent) leaves nothing selected.
    const { selectedId } = get();
    set({
      fleet,
      now,
      buffer: null,
      loadedAt: now,
      loadError,
      selectedId: selectedId && fleet.cards[selectedId] ? selectedId : null,
    });
  },

  select: (workspaceId) => set({ selectedId: workspaceId, now: Date.now() }),
}));
