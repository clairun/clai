import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantEventEnvelope, FleetWorkspace } from '../generated/bindings';

let resolveSnapshot: (value: FleetWorkspace[]) => void = () => {};
vi.mock('./client', () => ({
  fleetSnapshot: vi.fn(() => new Promise<FleetWorkspace[]>((resolve) => (resolveSnapshot = resolve))),
}));

const { useFleetStore } = await import('./fleetStore');
const { EMPTY_FLEET } = await import('./fleetModel');

const runEvent = (type: 'run_started' | 'run_completed', at: number): AssistantEventEnvelope =>
  ({
    sessionId: 'main',
    runId: 'r1',
    workspaceId: 'ws',
    timestamp: BigInt(at),
    event: { type, payload: { run: { id: 'r1', sessionId: 'main', trigger: 'user_message' } } },
  }) as unknown as AssistantEventEnvelope;

const snapshot: FleetWorkspace = {
  workspaceId: 'ws',
  title: 'Infra',
  starred: false,
  lastActivityAt: 5n,
  mainSessionId: 'main',
  activeRunId: 'r1',
  tasks: [],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [],
  lastAssistantMessage: { messageId: 'm', text: 'Done', truncated: false, createdAt: 4n },
};

describe('fleet store snapshot', () => {
  beforeEach(() => useFleetStore.setState({ fleet: EMPTY_FLEET, buffer: null, loadedAt: null }));

  it('buffers events while the snapshot loads and replays them on top', async () => {
    const loading = useFleetStore.getState().loadSnapshot();
    // Arrives mid-load: the run the snapshot still reports as active ends.
    useFleetStore.getState().applyEvent(runEvent('run_completed', 10));
    expect(useFleetStore.getState().fleet.cards.ws).toBeUndefined();

    resolveSnapshot([snapshot]);
    await loading;
    const card = useFleetStore.getState().fleet.cards.ws!;
    expect(card.title).toBe('Infra');
    expect(card.mainRuns).toEqual([]);
    expect(card.lastMessage?.text).toBe('Done');
    expect(useFleetStore.getState().buffer).toBeNull();
  });

  it('ignores streaming events entirely', () => {
    const before = useFleetStore.getState().fleet;
    useFleetStore.getState().applyEvent({
      ...runEvent('run_started', 1),
      event: { type: 'assistant_delta', payload: { message_id: 'm', text: 'x' } },
    } as AssistantEventEnvelope);
    expect(useFleetStore.getState().fleet).toBe(before);
  });
});
