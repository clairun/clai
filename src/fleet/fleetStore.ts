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
import { getMessage } from '../assistant/client';
import { fleetSnapshot } from './client';
import {
  EMPTY_FLEET,
  RECENT_WINDOW_MS,
  applyAssistantEvent,
  applyOpened,
  applyRequest,
  applyRequestResolved,
  applyUserSent,
  applyWholeMessage,
  isFleetEvent,
  seedFleet,
  type FleetPendingRequest,
  type FleetRequestKind,
  type FleetState,
} from './fleetModel';

type Reducer = (state: FleetState) => FleetState;

/** Snapshot size: the six-card board plus the cards a full board leaves off. */
const SNAPSHOT_LIMIT = 16;

/** Where `dismissed` survives a reload, as the workspace rail's collapsed flag does. */
const DISMISSED_KEY = 'fleet.dismissed';

const readDismissed = (): Record<string, number> => {
  try {
    const stored: unknown = JSON.parse(localStorage.getItem(DISMISSED_KEY) ?? '{}');
    if (!stored || typeof stored !== 'object') return {};
    return Object.fromEntries(Object.entries(stored).filter(([, at]) => typeof at === 'number'));
  } catch {
    return {};
  }
};

const saveDismissed = (dismissed: Record<string, number>): Record<string, number> => {
  try {
    localStorage.setItem(DISMISSED_KEY, JSON.stringify(dismissed));
  } catch {
    // Kept for this session only.
  }
  return dismissed;
};

interface FleetStoreState {
  fleet: FleetState;
  /** When `fleet` last changed or was reloaded: the clock recency is judged against, so rendering stays pure. */
  now: number;
  selectedId: string | null;
  /** When each dismissed card was dismissed, by workspace (`undismissed`). */
  dismissed: Record<string, number>;
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
  /** Fetches a message the snapshot cut, whole, into its card; a failed fetch leaves the preview. */
  loadWhole: (workspaceId: string, sessionId: string, messageId: string) => void;
  loadSnapshot: () => Promise<void>;
  /** Selecting a card undoes its dismissal. */
  select: (workspaceId: string | null) => void;
  /** Takes the card off the board until something happens in it; a selected one is deselected. */
  dismiss: (workspaceId: string) => void;
}

export const useFleetStore = create<FleetStoreState>()((set, get) => ({
  fleet: EMPTY_FLEET,
  now: Date.now(),
  selectedId: null,
  dismissed: readDismissed(),
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
    get().dispatch((s) => applyRequestResolved(s, kind, requestId));
  },
  noteUserSent: (workspaceId, sessionId, message) => {
    const at = Date.now();
    get().dispatch((s) => applyUserSent(s, workspaceId, sessionId, message, at));
  },
  loadWhole: (workspaceId, sessionId, messageId) => {
    getMessage(workspaceId, sessionId, messageId)
      .then((message) => get().dispatch((s) => applyWholeMessage(s, workspaceId, message)))
      .catch(() => undefined);
  },

  loadSnapshot: async () => {
    if (get().buffer) return;
    const buffer: Reducer[] = [];
    set({ buffer });
    const runsBefore = useFleetActivityStore.getState().activeRunsByWorkspace;
    let seeded: Reducer = (s) => s;
    let loadError: string | null = null;
    const keep = get().selectedId;
    try {
      const snapshot = await fleetSnapshot(SNAPSHOT_LIMIT, keep);
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
      seeded = (s) => seedFleet(s, snapshot);
    } catch (err) {
      loadError = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load the Fleet.';
    }
    const fleet = buffer.reduce((s, reducer) => reducer(s), seeded(get().fleet));
    const now = Date.now();
    // The selection this load kept, if gone (deleted, or no longer recent), leaves nothing
    // selected; one made while it ran is not in it, so another load brings it.
    const { selectedId } = get();
    const missing = selectedId !== null && !fleet.cards[selectedId];
    set({
      fleet,
      now,
      buffer: null,
      loadedAt: now,
      loadError,
      selectedId: missing && selectedId === keep ? null : selectedId,
    });
    if (missing && selectedId !== keep) void get().loadSnapshot();
  },

  select: (workspaceId) => {
    const now = Date.now();
    const { fleet, dismissed } = get();
    const undone = workspaceId !== null && Object.hasOwn(dismissed, workspaceId);
    set({
      selectedId: workspaceId,
      now,
      fleet: workspaceId ? applyOpened(fleet, workspaceId, now) : fleet,
      dismissed: undone ? saveDismissed(Object.fromEntries(Object.entries(dismissed).filter(([id]) => id !== workspaceId))) : dismissed,
    });
  },

  dismiss: (workspaceId) => {
    const { dismissed, selectedId } = get();
    const now = Date.now();
    // An older dismissal hides nothing that is idle (`eligibleCards`), so it goes, a deleted workspace's too.
    const recent = Object.entries(dismissed).filter(([, at]) => at >= now - RECENT_WINDOW_MS);
    set({
      dismissed: saveDismissed({ ...Object.fromEntries(recent), [workspaceId]: now }),
      selectedId: selectedId === workspaceId ? null : selectedId,
    });
  },
}));
