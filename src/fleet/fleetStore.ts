/**
 * App-global Fleet store. Fed from app start by the global listeners
 * (`useAssistantEvents`, `useFleetAttentionFeed`), so it is current whenever
 * the Fleet opens. `loadSnapshot` follows subscribe → buffer → snapshot →
 * replay: events arriving while the snapshot loads are queued and applied
 * on top of it.
 */
import { create } from 'zustand';
import type {
  AssistantEventEnvelope,
  AssistantMessage,
  PathGrantRequest,
  PermissionRequest,
} from '../generated/bindings';
import { fleetSnapshot } from './client';
import {
  EMPTY_FLEET,
  applyAssistantEvent,
  applyAttentionCount,
  applyRequest,
  applyRequestResolved,
  applyTitles,
  applyUserSent,
  isFleetEvent,
  seedFleet,
  seedFromSnapshot,
  type FleetRequestKind,
  type FleetState,
} from './fleetModel';

type Reducer = (state: FleetState) => FleetState;

const MOTION_KEY = 'clai.fleet.motion';
/** Snapshot size: enough to fill the largest grid (4 × 3) with room to spare. */
export const SNAPSHOT_LIMIT = 16;

const readMotion = (): boolean => {
  try {
    return window.localStorage.getItem(MOTION_KEY) !== 'off';
  } catch {
    return true;
  }
};

interface FleetStoreState {
  fleet: FleetState;
  /** When `fleet` last changed: the clock recency is judged against, so rendering stays pure. */
  now: number;
  selectedId: string | null;
  motion: boolean;
  /** `null` until the first snapshot lands. */
  loadedAt: number | null;
  loadError: string | null;
  /** Non-null while a snapshot is in flight: events queue here. */
  buffer: Reducer[] | null;
  dispatch: (reducer: Reducer) => void;
  applyEvent: (envelope: AssistantEventEnvelope) => void;
  applyRequest: (kind: FleetRequestKind, request: PermissionRequest | PathGrantRequest) => void;
  resolveRequest: (kind: FleetRequestKind, requestId: string) => void;
  setAttentionCount: (kind: FleetRequestKind, workspaceId: string, count: number) => void;
  noteUserSent: (workspaceId: string, sessionId: string, message: AssistantMessage) => void;
  setTitles: (titles: Record<string, string>) => void;
  loadSnapshot: () => Promise<void>;
  select: (workspaceId: string | null) => void;
  setMotion: (on: boolean) => void;
}

export const useFleetStore = create<FleetStoreState>()((set, get) => ({
  fleet: EMPTY_FLEET,
  now: Date.now(),
  selectedId: null,
  motion: readMotion(),
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
  applyRequest: (kind, request) => {
    const at = Date.now();
    get().dispatch((s) => applyRequest(s, kind, request, at));
  },
  resolveRequest: (kind, requestId) => {
    const at = Date.now();
    get().dispatch((s) => applyRequestResolved(s, kind, requestId, at));
  },
  setAttentionCount: (kind, workspaceId, count) => {
    const at = Date.now();
    get().dispatch((s) => applyAttentionCount(s, kind, workspaceId, count, at));
  },
  noteUserSent: (workspaceId, sessionId, message) => {
    const at = Date.now();
    get().dispatch((s) => applyUserSent(s, workspaceId, sessionId, message, at));
  },
  setTitles: (titles) => get().dispatch((s) => applyTitles(s, titles)),

  loadSnapshot: async () => {
    if (get().buffer) return;
    const buffer: Reducer[] = [];
    set({ buffer });
    let seeded: Reducer = (s) => s;
    let loadError: string | null = null;
    try {
      const snapshot = await fleetSnapshot(SNAPSHOT_LIMIT);
      const now = Date.now();
      const seed = snapshot.map((ws) => seedFromSnapshot(ws, now));
      seeded = (s) => seedFleet(s, seed, now);
    } catch (err) {
      loadError = typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to load the Fleet.';
    }
    const fleet = buffer.reduce((s, reducer) => reducer(s), seeded(get().fleet));
    const now = Date.now();
    set({ fleet, now, buffer: null, loadedAt: now, loadError });
  },

  select: (workspaceId) => set({ selectedId: workspaceId, now: Date.now() }),
  setMotion: (on) => {
    try {
      window.localStorage.setItem(MOTION_KEY, on ? 'on' : 'off');
    } catch {
      // The preference is a nicety; the in-memory value still applies.
    }
    set({ motion: on });
  },
}));
