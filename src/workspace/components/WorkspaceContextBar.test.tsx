import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// vi.mock is hoisted; vi.hoisted lets us share the mock fns with assertions.
const mocks = vi.hoisted(() => ({
  getWorkspaceDetails: vi.fn(),
  updateWorkspaceSessionMcp: vi.fn(),
  setWorkspaceProvider: vi.fn(),
  getMcpServers: vi.fn(),
  listProviderConnections: vi.fn(),
}));

vi.mock('../client', () => ({
  getWorkspaceDetails: mocks.getWorkspaceDetails,
  updateWorkspaceSessionMcp: mocks.updateWorkspaceSessionMcp,
  setWorkspaceProvider: mocks.setWorkspaceProvider,
}));

vi.mock('../../api/client', () => ({
  getMcpServers: mocks.getMcpServers,
}));

vi.mock('../../assistant', () => ({
  assistantClient: { listProviderConnections: mocks.listProviderConnections },
}));

import WorkspaceContextBar from './WorkspaceContextBar';
import { applyNewWorkspaceDraft, newWorkspaceDraft } from '../../fleet/newWorkspaceDraft';
import type { WorkspaceDetails } from '../../generated/bindings';

const server = (id: string, name: string) => ({
  id,
  name,
  enabled: true,
  transport: { type: 'http', url: `https://example.com/${id}` },
});

// Typed against the generated bindings so a backend field rename (e.g.
// disabledMcpServerIds) fails this test at compile time instead of silently
// making the mock stale. The enabled set and the disabled remainder both
// travel on the workspace details.
const details = (extra: Partial<WorkspaceDetails> = {}) => ({
  kind: 'general',
  providerConnectionIds: [],
  selectedMcpServerIds: [],
  disabledMcpServerIds: [],
  defaultWorkspaceAgentId: 'agent-mgr',
  ...extra,
});

function deferred() {
  let resolve: (value: unknown) => void = () => {};
  let reject: (err: Error) => void = () => {};
  const promise = new Promise((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe('WorkspaceContextBar MCP disable toggle', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.getMcpServers.mockResolvedValue([server('srv-a', 'Alpha'), server('srv-b', 'Beta')]);
    mocks.listProviderConnections.mockResolvedValue([]);
    mocks.updateWorkspaceSessionMcp.mockResolvedValue(undefined);
  });

  it('persists a disable toggle: enabled list shrinks, disabled list carries the id', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-a', 'srv-b'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const badge = await screen.findByTitle('Alpha: click to disable');

    // The bar reads header fields only, so it must never ask for the file
    // walk: it mounts with the input bar for every workspace and refetches on
    // every workspace-settings-changed event, which would make a flipped flag
    // a recursive walk of the workspace root per open and per settings save.
    expect(mocks.getWorkspaceDetails).toHaveBeenCalledWith('ws-1', { includeFiles: false });

    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
    );
    await userEvent.click(badge);

    await waitFor(() =>
      expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith('ws-1', ['srv-b'], ['srv-a'])
    );
    // After the reload the badge stays visible but shows the enable affordance.
    expect(await screen.findByTitle('Alpha: click to enable')).toBeTruthy();
  });

  it('restores the persisted disabled state from the workspace details', async () => {
    // Backend stores enabled and disabled separately; the bar must show the
    // union with the disabled badge toggled off after a restart.
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByTitle('Alpha: click to enable')).toBeTruthy();
    expect(await screen.findByTitle('Beta: click to disable')).toBeTruthy();
  });

  it('re-enabling persists the id back into the enabled list', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const badge = await screen.findByTitle('Alpha: click to enable');
    await userEvent.click(badge);

    await waitFor(() =>
      expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith(
        'ws-1',
        ['srv-b', 'srv-a'],
        []
      )
    );
  });

  it('removing a server also clears it from the disabled list', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    // Open the selector via the Add MCP badge.
    await userEvent.click(await screen.findByText('Add MCP'));
    // Both servers are attached; items render in server-list order, so the
    // first Remove button belongs to Alpha (the disabled one).
    const [alphaRemove] = await screen.findAllByRole('button', { name: 'Remove' });
    expect(alphaRemove).toBeTruthy();
    await userEvent.click(alphaRemove!);

    await waitFor(() =>
      expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith('ws-1', ['srv-b'], [])
    );
  });

  it('shows no servers when the config selection is empty', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(details());

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByText('Add MCP')).toBeTruthy();
    expect(screen.queryByTitle('Alpha: click to disable')).toBeNull();
  });

  it('refetches the workspace details when workspace settings change', async () => {
    // The bar is self-loading; a Settings save dispatches
    // workspace-settings-changed so stale local state is replaced before the
    // next persist could overwrite the config.
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-a'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByTitle('Alpha: click to disable')).toBeTruthy();

    mocks.getWorkspaceDetails.mockResolvedValue(details({ selectedMcpServerIds: [] }));
    window.dispatchEvent(new CustomEvent('workspace-settings-changed'));

    await waitFor(() => expect(screen.queryByTitle('Alpha: click to disable')).toBeNull());
  });
});

describe('WorkspaceContextBar provider picker', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.getMcpServers.mockResolvedValue([]);
    mocks.listProviderConnections.mockResolvedValue([
      { id: 'conn-a', name: 'Alpha', enabled: true },
      { id: 'conn-b', name: 'Beta', enabled: true },
      { id: 'conn-c', name: 'Gamma', enabled: true },
    ]);
  });

  it('disables the picker while saving, then reloads the persisted value', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(details({ providerConnectionIds: ['conn-a'] }));
    const pending = deferred();
    mocks.setWorkspaceProvider.mockReturnValue(pending.promise);

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const select = await screen.findByRole<HTMLSelectElement>('combobox');
    await waitFor(() => expect(select.value).toBe('conn-a'));
    await userEvent.selectOptions(select, 'conn-b');

    expect(mocks.setWorkspaceProvider).toHaveBeenCalledWith('ws-1', 'conn-b');
    expect(select.disabled).toBe(true);

    mocks.getWorkspaceDetails.mockResolvedValue(details({ providerConnectionIds: ['conn-b'] }));
    pending.resolve(undefined);

    await waitFor(() => expect(select.disabled).toBe(false));
    expect(mocks.getWorkspaceDetails).toHaveBeenCalledTimes(2);
    expect(select.value).toBe('conn-b');
  });

  it('shows the error and the persisted value when the save fails', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(details({ providerConnectionIds: ['conn-a'] }));
    mocks.setWorkspaceProvider.mockRejectedValue(
      new Error('Set up a main agent for this workspace first.')
    );
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const select = await screen.findByRole<HTMLSelectElement>('combobox');
    await waitFor(() => expect(select.value).toBe('conn-a'));
    await userEvent.selectOptions(select, 'conn-b');

    expect((await screen.findByRole('alert')).textContent).toBe(
      'Set up a main agent for this workspace first.'
    );
    await waitFor(() => expect(select.disabled).toBe(false));
    expect(select.value).toBe('conn-a');
    consoleError.mockRestore();
  });
});

describe('WorkspaceContextBar for a new workspace (no workspace yet)', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    Object.assign(newWorkspaceDraft, { providerConnectionId: '', mcpServerIds: [], disabledMcpServerIds: [] });
    mocks.setWorkspaceProvider.mockResolvedValue(undefined);
    mocks.updateWorkspaceSessionMcp.mockResolvedValue(undefined);
    mocks.getMcpServers.mockResolvedValue([server('srv-a', 'Alpha'), server('srv-b', 'Beta')]);
    mocks.listProviderConnections.mockResolvedValue([
      { id: 'conn-a', name: 'Alpha', enabled: true },
      { id: 'conn-b', name: 'Beta', enabled: true },
    ]);
  });

  it('shows the model picker (defaulting to the first connection) and Add MCP', async () => {
    render(<WorkspaceContextBar workspaceId={null} />);
    const select = await screen.findByRole<HTMLSelectElement>('combobox');
    expect(select.value).toBe('conn-a');
    expect(screen.getByText('Add MCP')).toBeTruthy();
    expect(mocks.getWorkspaceDetails).not.toHaveBeenCalled();
  });

  it('holds the choices and applies them to the workspace once created', async () => {
    render(<WorkspaceContextBar workspaceId={null} />);
    const select = await screen.findByRole<HTMLSelectElement>('combobox');
    await userEvent.selectOptions(select, 'conn-b');
    await waitFor(() => expect(select.value).toBe('conn-b'));

    await userEvent.click(screen.getByText('Add MCP'));
    await userEvent.click((await screen.findAllByRole('button', { name: 'Add' }))[0]!);
    await waitFor(() => expect(screen.getAllByRole('button', { name: 'Add' })).toHaveLength(1));
    await userEvent.click(screen.getByRole('button', { name: 'Add' }));
    await waitFor(() => expect(screen.getAllByRole('button', { name: 'Remove' })).toHaveLength(2));
    await userEvent.click(screen.getByRole('button', { name: 'Close MCP server selector' }));
    await userEvent.click(await screen.findByTitle('Alpha: click to disable'));
    await screen.findByTitle('Alpha: click to enable');

    expect(mocks.setWorkspaceProvider).not.toHaveBeenCalled();
    expect(mocks.updateWorkspaceSessionMcp).not.toHaveBeenCalled();

    await applyNewWorkspaceDraft('ws-new', await mocks.listProviderConnections());
    expect(mocks.setWorkspaceProvider).toHaveBeenCalledWith('ws-new', 'conn-b');
    expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith('ws-new', ['srv-b'], ['srv-a']);
  });
});

describe('WorkspaceContextBar across target switches', () => {
  it('renders at once on remount, without waiting for the app-wide lists again', async () => {
    vi.clearAllMocks();
    mocks.getWorkspaceDetails.mockResolvedValue(details({ providerConnectionIds: ['conn-a'] }));
    mocks.getMcpServers.mockResolvedValue([server('srv-a', 'Alpha')]);
    mocks.listProviderConnections.mockResolvedValue([{ id: 'conn-a', name: 'Alpha', enabled: true }]);
    const { unmount } = render(<WorkspaceContextBar workspaceId="ws-1" />);
    await screen.findByRole('combobox');
    unmount();

    mocks.getMcpServers.mockReturnValue(new Promise(() => {}));
    render(<WorkspaceContextBar workspaceId={null} />);
    expect(screen.getByRole('combobox')).toBeTruthy();
    expect(screen.getByText('Add MCP')).toBeTruthy();
  });
});
