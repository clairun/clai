import { beforeEach, describe, expect, it, vi } from 'vitest';
import type { AssistantMessage, AssistantSession } from '../../generated/bindings';

const createWorkspace = vi.fn<() => Promise<string>>();
const getOrCreateWorkspaceSession = vi.fn();
const listProviderConnections = vi.fn();
const sendMessage = vi.fn();
const openGlobalSettings = vi.fn();
const applyNewWorkspaceDraft = vi.fn();

vi.mock('../../workspace/client', () => ({
  createWorkspace: () => createWorkspace(),
  getOrCreateWorkspaceSession: (id: string) => getOrCreateWorkspaceSession(id),
  storeWorkspaceImage: vi.fn(),
  pickAndStoreWorkspaceImage: vi.fn(),
}));
vi.mock('../../fleet/newWorkspaceDraft', () => ({
  applyNewWorkspaceDraft: (id: string) => applyNewWorkspaceDraft(id),
}));
vi.mock('../../assistant/client', () => ({
  listProviderConnections: () => listProviderConnections(),
  sendMessage: (...args: unknown[]) => sendMessage(...args),
}));
vi.mock('../../utils/globalSettings', () => ({ openGlobalSettings: (d: unknown) => openGlobalSettings(d) }));
vi.mock('@tauri-apps/plugin-clipboard-manager', () => ({ readImage: vi.fn() }));

const { sendToNewWorkspace } = await import('./TerminalEmulatorWrapper');
const { useFleetStore } = await import('../../fleet/fleetStore');

const message = { id: 'm1', content: [{ type: 'text', text: 'hi' }], role: 'user', origin: { kind: 'user' } } as unknown as AssistantMessage;

describe('sending from the Fleet with no card selected', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useFleetStore.setState({ selectedId: null, buffer: null });
    listProviderConnections.mockResolvedValue([{ id: 'c', enabled: true }]);
    getOrCreateWorkspaceSession.mockResolvedValue({ session: { id: 's' } as AssistantSession, providerConnectionId: 'c' });
    sendMessage.mockResolvedValue({ message, queued: false });
  });

  it('creates one workspace for a double submit and selects it', async () => {
    createWorkspace.mockResolvedValue('new');
    const results = await Promise.all([sendToNewWorkspace('a', []), sendToNewWorkspace('b', [])]);
    expect(results).toEqual([{}, {}]);
    expect(createWorkspace).toHaveBeenCalledTimes(1);
    expect(applyNewWorkspaceDraft).toHaveBeenCalledWith('new');
    expect(applyNewWorkspaceDraft.mock.invocationCallOrder[0]).toBeLessThan(sendMessage.mock.invocationCallOrder[0]!);
    expect(sendMessage.mock.calls.map((call) => call[0])).toEqual(['new', 'new']);
    expect(useFleetStore.getState().selectedId).toBe('new');
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
});
