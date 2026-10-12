import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// vi.mock is hoisted; vi.hoisted lets us share the mock fns with assertions.
const mocks = vi.hoisted(() => ({
  getWorkspaceDetails: vi.fn(),
  updateWorkspaceSessionMcp: vi.fn(),
  setWorkspaceProvider: vi.fn(),
  getMcpServers: vi.fn(),
  getProviderConnections: vi.fn(),
}));

vi.mock('../client', () => ({
  getWorkspaceDetails: mocks.getWorkspaceDetails,
  updateWorkspaceSessionMcp: mocks.updateWorkspaceSessionMcp,
  setWorkspaceProvider: mocks.setWorkspaceProvider,
}));

vi.mock('../../api/client', () => ({
  getMcpServers: mocks.getMcpServers,
  getProviderConnections: mocks.getProviderConnections,
}));

import WorkspaceContextBar from './WorkspaceContextBar';
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

describe('WorkspaceContextBar MCP disable toggle', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mocks.getMcpServers.mockResolvedValue([server('srv-a', 'Alpha'), server('srv-b', 'Beta')]);
    mocks.getProviderConnections.mockResolvedValue([]);
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

    await userEvent.click(badge);

    await waitFor(() =>
      expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith('ws-1', ['srv-b'], ['srv-a'])
    );
    // The badge stays visible but flips to the enable affordance.
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

  it('rolls back the optimistic toggle when the backend rejects the update', async () => {
    mocks.getWorkspaceDetails.mockResolvedValue(
      details({ selectedMcpServerIds: ['srv-a', 'srv-b'] })
    );
    mocks.updateWorkspaceSessionMcp.mockRejectedValue(new Error('db locked'));
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const badge = await screen.findByTitle('Alpha: click to disable');
    await userEvent.click(badge);

    // The optimistic flip must revert to the persisted state once the
    // backend rejects, so the badge shows Alpha as enabled again.
    expect(await screen.findByTitle('Alpha: click to disable')).toBeTruthy();
    expect(screen.queryByTitle('Alpha: click to enable')).toBeNull();
    // The reason is shown, e.g. a workspace without a main agent.
    expect((await screen.findByRole('alert')).textContent).toBe('db locked');
    consoleError.mockRestore();
  });
});
