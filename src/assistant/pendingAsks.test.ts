import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantSession, PendingAsk } from '../generated/bindings';

const listPendingAsks = vi.fn<(workspaceId: string) => Promise<PendingAsk[]>>();
vi.mock('./client', () => ({ listPendingAsks: (workspaceId: string) => listPendingAsks(workspaceId) }));

const { restorePendingAsk } = await import('./pendingAsks');
const { default: useAssistantStore } = await import('./sessionStore');

const ask = (pendingId: string): PendingAsk => ({
  pendingId,
  workspaceId: 'ws',
  sessionId: 's',
  question: `Question ${pendingId}?`,
  options: null,
  multiSelect: false,
  extraContext: null,
  requestedAt: 1n,
});

const pending = () => useAssistantStore.getState().sessions.s?.pendingAskUser?.pendingId ?? null;

describe('restorePendingAsk', () => {
  beforeEach(() => {
    listPendingAsks.mockReset();
    useAssistantStore.setState({ sessions: {} });
    useAssistantStore.getState().initSession({ id: 's' } as unknown as AssistantSession);
  });

  it('re-arms a question asked before the view loaded', async () => {
    listPendingAsks.mockResolvedValue([ask('x')]);
    await restorePendingAsk('ws', 's');
    expect(pending()).toBe('x');
  });

  it('does not bring back a question answered while the list loaded', async () => {
    listPendingAsks
      .mockImplementationOnce(async () => {
        // The answer lands before the stale list does: its clear finds no panel.
        useAssistantStore.getState().clearAskUserPending('s', 'x');
        return [ask('x')];
      })
      .mockResolvedValueOnce([]);
    await restorePendingAsk('ws', 's');
    expect(pending()).toBeNull();
  });

  it('does not overwrite a newer question that arrived live', async () => {
    listPendingAsks.mockImplementation(async () => {
      useAssistantStore.getState().setAskUserPending('s', { pendingId: 'y', question: 'New?', options: null, multiSelect: false, extraContext: null });
      return [ask('x')];
    });
    await restorePendingAsk('ws', 's');
    expect(pending()).toBe('y');
  });
});
