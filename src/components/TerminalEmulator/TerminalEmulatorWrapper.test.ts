import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantMessage, AssistantRun, AssistantSession } from '../../generated/bindings';

const createWorkspace = vi.fn<() => Promise<string>>();
const getOrCreateWorkspaceSession = vi.fn();
const listProviderConnections = vi.fn();
const sendMessage = vi.fn();
const openGlobalSettings = vi.fn();
const setWorkspaceProvider = vi.fn();
const updateWorkspaceSessionMcp = vi.fn();
const storeWorkspaceImage = vi.fn();

vi.mock('../../workspace/client', () => ({
  createWorkspace: () => createWorkspace(),
  getOrCreateWorkspaceSession: (id: string) => getOrCreateWorkspaceSession(id),
  setWorkspaceProvider: (...args: unknown[]) => setWorkspaceProvider(...args),
  updateWorkspaceSessionMcp: (...args: unknown[]) => updateWorkspaceSessionMcp(...args),
  storeWorkspaceImage: (...args: unknown[]) => storeWorkspaceImage(...args),
  pickAndStoreWorkspaceImage: vi.fn(),
  pickImage: vi.fn(),
}));
vi.mock('../../assistant/client', () => ({
  listProviderConnections: () => listProviderConnections(),
  sendMessage: (...args: unknown[]) => sendMessage(...args),
}));
vi.mock('../../utils/globalSettings', () => ({ openGlobalSettings: (d: unknown) => openGlobalSettings(d) }));
vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({ readImage: vi.fn() }));

const { sendToNewWorkspace, sendToWorkspace } = await import('./TerminalEmulatorWrapper');
const { useFleetStore } = await import('../../fleet/fleetStore');
const { draftImagePart, newWorkspaceDraft } = await import('../../fleet/newWorkspaceDraft');
const { EMPTY_FLEET } = await import('../../fleet/fleetModel');
const { runFailure } = await import('../../assistant/runErrors');

const message = { id: 'm1', content: [{ type: 'text', text: 'hi' }], role: 'user', origin: { kind: 'user' } } as unknown as AssistantMessage;

describe('sending from the Fleet with no card selected', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useFleetStore.setState({ selectedId: null, buffer: null });
    Object.assign(newWorkspaceDraft, { providerConnectionId: '', mcpServerIds: [], disabledMcpServerIds: [] });
    listProviderConnections.mockResolvedValue([{ id: 'c', enabled: true }]);
    getOrCreateWorkspaceSession.mockResolvedValue({ session: { id: 's' } as AssistantSession, providerConnectionId: 'c' });
    sendMessage.mockResolvedValue({ message, queued: false });
  });

  it('creates one workspace for a double submit and selects it', async () => {
    createWorkspace.mockResolvedValue('new');
    const results = await Promise.all([sendToNewWorkspace('a', []), sendToNewWorkspace('b', [])]);
    expect(results).toEqual([{}, {}]);
    expect(createWorkspace).toHaveBeenCalledTimes(1);
    expect(sendMessage.mock.calls.map((call) => call[0])).toEqual(['new', 'new']);
    expect(useFleetStore.getState().selectedId).toBe('new');
  });

  it('sets up the new workspace from the draft before sending, images included', async () => {
    createWorkspace.mockResolvedValue('new');
    listProviderConnections.mockResolvedValue([{ id: 'c', enabled: true }, { id: 'c2', enabled: true }]);
    Object.assign(newWorkspaceDraft, { providerConnectionId: 'c2', mcpServerIds: ['m1'], disabledMcpServerIds: ['m2'] });
    const stored = { type: 'image', id: 'img', path: 'img.png', media_type: 'image/png' };
    storeWorkspaceImage.mockResolvedValue(stored);
    const draftImage = draftImagePart({ dataBase64: 'AAAA', mediaType: 'image/png', filename: 'shot.png' });

    await sendToNewWorkspace('look', [draftImage]);

    expect(setWorkspaceProvider).toHaveBeenCalledWith('new', 'c2');
    expect(updateWorkspaceSessionMcp).toHaveBeenCalledWith('new', ['m1'], ['m2']);
    expect(storeWorkspaceImage).toHaveBeenCalledWith('new', 'AAAA', 'image/png', 'shot.png');
    expect(sendMessage).toHaveBeenCalledWith('new', 's', 'look', 'c', [stored]);
    const sentAt = sendMessage.mock.invocationCallOrder[0]!;
    expect(setWorkspaceProvider.mock.invocationCallOrder[0]).toBeLessThan(sentAt);
    expect(updateWorkspaceSessionMcp.mock.invocationCallOrder[0]).toBeLessThan(sentAt);
  });

  it('gives the new workspace the default model when the drafted connection is gone or disabled', async () => {
    createWorkspace.mockResolvedValue('new');
    listProviderConnections.mockResolvedValue([{ id: 'off', enabled: false }, { id: 'c', enabled: true }]);
    for (const stale of ['deleted', 'off']) {
      newWorkspaceDraft.providerConnectionId = stale;
      await sendToNewWorkspace('a', []);
      expect(setWorkspaceProvider).toHaveBeenLastCalledWith('new', 'c');
    }
  });

  it('keeps a card the user selected while the workspace was being created', async () => {
    createWorkspace.mockImplementation(async () => {
      useFleetStore.getState().select('picked');
      return 'new';
    });
    await sendToNewWorkspace('a', []);
    expect(useFleetStore.getState().selectedId).toBe('picked');
  });

  it('with no provider, opens Settings and creates nothing', async () => {
    listProviderConnections.mockResolvedValue([{ id: 'c', enabled: false }]);
    await expect(sendToNewWorkspace('a', [])).rejects.toMatch(/provider connection/);
    expect(createWorkspace).not.toHaveBeenCalled();
    expect(openGlobalSettings).toHaveBeenCalledWith({ tab: 'provider', providerAction: 'new' });
  });

  it('leaves nothing selected when the send fails, and the next prompt creates again', async () => {
    createWorkspace.mockResolvedValue('new');
    getOrCreateWorkspaceSession.mockResolvedValueOnce({ session: { id: 's' }, providerConnectionId: null });
    expect((await sendToNewWorkspace('a', [])).error).toMatch(/provider connection/);
    expect(useFleetStore.getState().selectedId).toBeNull();
    await sendToNewWorkspace('b', []);
    expect(createWorkspace).toHaveBeenCalledTimes(2);
  });

  it('keeps a draft image for a retry after a failed send, and forgets it once sent', async () => {
    createWorkspace.mockResolvedValue('new');
    storeWorkspaceImage.mockResolvedValue({ type: 'image', id: 'img', path: 'img.png', media_type: 'image/png' });
    const draftImage = draftImagePart({ dataBase64: 'AAAA', mediaType: 'image/png', filename: null });
    getOrCreateWorkspaceSession.mockResolvedValueOnce({ session: { id: 's' }, providerConnectionId: null });
    expect((await sendToNewWorkspace('a', [draftImage])).error).toBeDefined();
    expect(await sendToNewWorkspace('a', [draftImage])).toEqual({});
    expect(storeWorkspaceImage).toHaveBeenCalledTimes(2);

    // Sent: the image is no longer held, so nothing is left to store.
    await sendToNewWorkspace('a', [draftImage]);
    expect(storeWorkspaceImage).toHaveBeenCalledTimes(2);
  });
});

describe('sending from a workspace', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useFleetStore.setState({ fleet: EMPTY_FLEET, buffer: null });
    getOrCreateWorkspaceSession.mockResolvedValue({ session: { id: 's' } as AssistantSession, providerConnectionId: 'c' });
    sendMessage.mockResolvedValue({ message, queued: false });
  });

  it("shows the first send's credits failure on its Fleet card", async () => {
    await sendToWorkspace('ws', 'hi', []);
    const run = { id: 'r1', sessionId: 's', status: 'failed', error: 'Credit balance is too low' } as AssistantRun;
    useFleetStore.getState().applyEvent({ sessionId: 's', runId: 'r1', workspaceId: 'ws', timestamp: 1n, event: { type: 'run_failed', payload: { run } } });
    const card = useFleetStore.getState().fleet.cards.ws!;
    expect(card.mainSessionId).toBe('s');
    expect(runFailure(card.lastMainRun)).toEqual({ text: 'Credit balance is too low', isLimit: false });
  });
});
