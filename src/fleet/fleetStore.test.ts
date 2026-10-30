import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantEventEnvelope, FleetWorkspace, WorkspaceTaskResponse } from '../generated/bindings';

let resolveSnapshot: (value: FleetWorkspace[]) => void = () => {};
vi.mock('./client', () => ({
  fleetSnapshot: vi.fn(() => new Promise<FleetWorkspace[]>((resolve) => (resolveSnapshot = resolve))),
}));

const { fleetSnapshot } = await import('./client');
const { useFleetStore } = await import('./fleetStore');
const { EMPTY_FLEET, RECENT_WINDOW_MS, emptyCard: emptyCardOf } = await import('./fleetModel');
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
  lastActivityAt: 5n,
  lastOpenedAt: 0n,
  agents: [],
  mainSessionId: 'main',
  lastMainRun: null,
  activeRunIds: ['r1', 'task-run'],
  tasks: [task],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [{ pendingId: 'q1', workspaceId: 'ws', sessionId: 'main', question: '?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n }],
  lastAssistantMessage: {
    message: { id: 'm', sessionId: 'main', role: 'assistant', content: [{ type: 'text', text: 'Done' }], createdAt: 4n, providerMetadata: null, origin: { kind: 'user' } },
    runId: null,
    truncated: false,
  },
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

  it("notifies only when Main's words grow: never for a delta, a tool call or a task's words", () => {
    useFleetStore.getState().noteUserSent('ws', 'main', null);
    const text = (t: string) => ({ type: 'text', text: t });
    const tool = { type: 'tool_use', tool_call_id: 'c1', tool_name: 'bash_exec', arguments: {} };
    const updated = (content: unknown[], sessionId = 'main') =>
      ({ ...askEvent('q', 2, 'ask_user_requested'), sessionId, event: { type: 'assistant_message_updated', payload: { message: { id: 'm', sessionId, role: 'assistant', content, origin: { kind: 'user' } } } } }) as AssistantEventEnvelope;
    const notified = vi.fn();
    const unsubscribe = useFleetStore.subscribe(notified);
    const { applyEvent } = useFleetStore.getState();
    applyEvent({ ...askEvent('q', 1, 'ask_user_requested'), event: { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } } } as AssistantEventEnvelope);
    applyEvent({ ...askEvent('q', 1, 'ask_user_requested'), event: { type: 'tool_call_started', payload: { tool_call: {} } } } as AssistantEventEnvelope);
    applyEvent(updated([tool]));
    applyEvent(updated([text('Busy.'), tool], 'task-session'));
    expect(notified).not.toHaveBeenCalled();
    applyEvent(updated([text('Found it.'), tool]));
    expect(notified).toHaveBeenCalledTimes(1);
    applyEvent(updated([text('Found it.'), tool, tool]));
    expect(notified).toHaveBeenCalledTimes(1);
    unsubscribe();
    expect(useFleetStore.getState().fleet.cards.ws!.said).toMatchObject([{ id: 'm', text: 'Found it.' }]);
  });
});

/** A `localStorage` of its own: Node's global one, without a file, has none. */
const memoryStorage = () => {
  const items = new Map<string, string>();
  return { getItem: (key: string) => items.get(key) ?? null, setItem: (key: string, value: string) => void items.set(key, value) };
};

describe('fleet store dismissals', () => {
  beforeEach(() => {
    vi.stubGlobal('localStorage', memoryStorage());
    useFleetStore.setState({
      fleet: { cards: { a: { ...emptyCardOf('a') }, b: { ...emptyCardOf('b') } } },
      selectedId: 'a',
      dismissed: { old: 1, b: 40 },
    });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.restoreAllMocks();
  });

  it('dismisses with the time, deselects only the dismissed card, and drops dismissals older than the recent window', () => {
    vi.spyOn(Date, 'now').mockReturnValue(RECENT_WINDOW_MS + 10);
    useFleetStore.getState().dismiss('c');
    expect(useFleetStore.getState().dismissed).toEqual({ b: 40, c: RECENT_WINDOW_MS + 10 });
    expect(useFleetStore.getState().selectedId).toBe('a');
    useFleetStore.getState().dismiss('a');
    expect(useFleetStore.getState().selectedId).toBeNull();
  });

  it('keeps dismissals across a reload; selecting a card undoes its own', async () => {
    useFleetStore.getState().dismiss('a');
    useFleetStore.getState().dismiss('b');
    const dismissed = useFleetStore.getState().dismissed;
    vi.resetModules();
    const reloaded = (await import('./fleetStore')).useFleetStore;
    expect(reloaded.getState().dismissed).toEqual(dismissed);

    reloaded.getState().select('a');
    expect(Object.keys(reloaded.getState().dismissed)).toEqual(['b']);
    vi.resetModules();
    expect(Object.keys((await import('./fleetStore')).useFleetStore.getState().dismissed)).toEqual(['b']);
  });
});
