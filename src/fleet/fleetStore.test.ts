import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantEventEnvelope, FleetWorkspace, WorkspaceTaskResponse } from '../generated/bindings';

let resolveSnapshot: (value: FleetWorkspace[]) => void = () => {};
vi.mock('./client', () => ({
  fleetSnapshot: vi.fn(() => new Promise<FleetWorkspace[]>((resolve) => (resolveSnapshot = resolve))),
}));

const { fleetSnapshot } = await import('./client');
const { useFleetStore } = await import('./fleetStore');
const { EMPTY_FLEET } = await import('./fleetModel');
const { useFleetActivityStore } = await import('../stores/fleetActivityStore');
const { useTaskStore } = await import('../workspace/taskStore');

const askEvent = (pendingId: string, at: number, type: 'ask_user_requested' | 'ask_user_resolved'): AssistantEventEnvelope =>
  ({
    sessionId: 'main',
    runId: 'r1',
    workspaceId: 'ws',
    timestamp: BigInt(at),
    event: { type, payload: { pending_id: pendingId, question: '?', options: null, multi_select: false } },
  }) as unknown as AssistantEventEnvelope;

const task = { id: 't1', workspaceId: 'ws', status: 'running', updatedAt: 3n } as unknown as WorkspaceTaskResponse;

const snapshot: FleetWorkspace = {
  workspaceId: 'ws',
  title: 'Infra',
  starred: false,
  lastActivityAt: 5n,
  lastOpenedAt: 0n,
  mainSessionId: 'main',
  activeRunIds: ['r1', 'task-run'],
  tasks: [task],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [{ pendingId: 'q1', workspaceId: 'ws', sessionId: 'main', runId: 'r1', question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n }],
  lastAssistantMessage: { messageId: 'm', text: 'Done', truncated: false, createdAt: 4n },
  recentMessages: [],
};

describe('fleet store snapshot', () => {
  beforeEach(() => {
    useFleetStore.setState({ fleet: EMPTY_FLEET, buffer: null, loadedAt: null, selectedId: null });
    useFleetActivityStore.setState({ activeRunsByWorkspace: { ws: ['stale'] } });
    useTaskStore.setState({ tasks: {}, seeded: {} });
  });

  it('buffers events while the snapshot loads and replays them on top', async () => {
    const loading = useFleetStore.getState().loadSnapshot();
    // Arrives mid-load: the question the snapshot still reports is answered.
    useFleetStore.getState().applyEvent(askEvent('q1', 10, 'ask_user_resolved'));
    expect(useFleetStore.getState().fleet.cards.ws).toBeUndefined();

    resolveSnapshot([snapshot]);
    await loading;
    const card = useFleetStore.getState().fleet.cards.ws!;
    expect(card.title).toBe('Infra');
    expect(card.requests['ask:q1']).toBeUndefined();
    expect(card.said).toMatchObject([{ id: 'm', role: 'assistant', text: 'Done' }]);
    expect(useFleetStore.getState().buffer).toBeNull();
  });

  it('seeds the run and task stores the cards read', async () => {
    const loading = useFleetStore.getState().loadSnapshot();
    resolveSnapshot([snapshot]);
    await loading;
    expect(useFleetActivityStore.getState().activeRunsByWorkspace.ws).toEqual(['r1', 'task-run']);
    expect(useTaskStore.getState().tasks.ws?.t1).toBe(task);

    const reload = useFleetStore.getState().loadSnapshot();
    resolveSnapshot([{ ...snapshot, activeRunIds: [] }]);
    await reload;
    expect(useFleetActivityStore.getState().activeRunsByWorkspace.ws).toBeUndefined();
  });

  it('keeps runs that started or ended while the snapshot loaded', async () => {
    useFleetActivityStore.setState({ activeRunsByWorkspace: { ws: ['stale', 'r1'] } });
    const loading = useFleetStore.getState().loadSnapshot();
    useFleetActivityStore.getState().markRunStarted('ws', 'new');
    useFleetActivityStore.getState().markRunEnded('ws', 'r1');
    // Read before either event: still reports r1, not yet `new`.
    resolveSnapshot([snapshot]);
    await loading;
    expect(useFleetActivityStore.getState().activeRunsByWorkspace.ws).toEqual(['task-run', 'new']);
  });

  it('drops the selection when its card is gone after a reload', async () => {
    let loading = useFleetStore.getState().loadSnapshot();
    resolveSnapshot([snapshot]);
    await loading;
    useFleetStore.getState().select('ws');

    loading = useFleetStore.getState().loadSnapshot();
    resolveSnapshot([{ ...snapshot, workspaceId: 'other' }]);
    await loading;
    expect(useFleetStore.getState().selectedId).toBeNull();
  });

  it('keeps a workspace picked while a load runs, and loads it next', async () => {
    const loading = useFleetStore.getState().loadSnapshot();
    useFleetStore.getState().select('picked');
    // The pick's own load is skipped: one is already running, started without it.
    void useFleetStore.getState().loadSnapshot();
    resolveSnapshot([snapshot]);
    await loading;
    expect(useFleetStore.getState().selectedId).toBe('picked');
    expect(vi.mocked(fleetSnapshot).mock.calls.map((call) => call[1])).toEqual([null, 'picked']);

    resolveSnapshot([snapshot, { ...snapshot, workspaceId: 'picked' }]);
    await vi.waitFor(() => expect(useFleetStore.getState().fleet.cards.picked).toBeDefined());
    expect(useFleetStore.getState().selectedId).toBe('picked');
  });

  it('ignores streaming events entirely', () => {
    const before = useFleetStore.getState().fleet;
    useFleetStore.getState().applyEvent({
      ...askEvent('q', 1, 'ask_user_requested'),
      event: { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } },
    } as AssistantEventEnvelope);
    expect(useFleetStore.getState().fleet).toBe(before);
  });
});
