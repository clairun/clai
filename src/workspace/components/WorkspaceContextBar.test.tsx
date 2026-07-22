import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// vi.mock is hoisted; vi.hoisted lets us share the mock fns with assertions.
const mocks = vi.hoisted(() => ({
  getWorkspaceSnapshot: vi.fn(),
  updateWorkspaceSessionMcp: vi.fn(),
  setWorkspaceProvider: vi.fn(),
  getMcpServers: vi.fn(),
  getProviderConnections: vi.fn(),
}));

vi.mock('../client', () => ({
  getWorkspaceSnapshot: mocks.getWorkspaceSnapshot,
  updateWorkspaceSessionMcp: mocks.updateWorkspaceSessionMcp,
  setWorkspaceProvider: mocks.setWorkspaceProvider,
}));

vi.mock('../../api/client', () => ({
  getMcpServers: mocks.getMcpServers,
  getProviderConnections: mocks.getProviderConnections,
}));

import WorkspaceContextBar from './WorkspaceContextBar';
import type { SessionContext, WorkspaceSnapshot } from '../../generated/bindings';

const server = (id: string, name: string) => ({
  id,
  name,
  enabled: true,
  transport: { type: 'http', url: `https://example.com/${id}` },
});

// Typed against the generated bindings so a backend field rename (e.g.
// disabledMcpServerIds) fails this test at compile time instead of silently
// making the mock stale. The config-derived enabled set and the disabled
// remainder both travel on the snapshot; the session context only matters
// as a legacy fallback when the config records nothing.
const snapshot = (
  context: Partial<SessionContext>,
  extra: Partial<WorkspaceSnapshot> = {}
) => ({
  kind: 'general',
  providerConnectionIds: [],
  selectedMcpServerIds: [],
  disabledMcpServerIds: [],
  session: { context },
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
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({}, { selectedMcpServerIds: ['srv-a', 'srv-b'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    const badge = await screen.findByTitle('Alpha: click to disable');
    await userEvent.click(badge);

    await waitFor(() =>
      expect(mocks.updateWorkspaceSessionMcp).toHaveBeenCalledWith('ws-1', ['srv-b'], ['srv-a'])
    );
    // The badge stays visible but flips to the enable affordance.
    expect(await screen.findByTitle('Alpha: click to enable')).toBeTruthy();
  });

  it('restores the persisted disabled state from the snapshot', async () => {
    // Backend stores enabled and disabled separately; the bar must show the
    // union with the disabled badge toggled off after a restart.
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({}, { selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByTitle('Alpha: click to enable')).toBeTruthy();
    expect(await screen.findByTitle('Beta: click to disable')).toBeTruthy();
  });

  it('re-enabling persists the id back into the enabled list', async () => {
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({}, { selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
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
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({}, { selectedMcpServerIds: ['srv-b'], disabledMcpServerIds: ['srv-a'] })
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

  it('prefers the config-derived list over a stale session context', async () => {
    // Workspace Settings rewrites the manager's selection without touching
    // the session row; the bar must surface the config truth, not the stale
    // session list, or its next persist would overwrite the Settings change.
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({ mcpServerIds: ['srv-a'] }, { selectedMcpServerIds: ['srv-b'] })
    );

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByTitle('Beta: click to disable')).toBeTruthy();
    expect(screen.queryByTitle('Alpha: click to disable')).toBeNull();
  });

  it('falls back to the legacy session list when the config records nothing', async () => {
    // Sessions that predate the config mirror (or manager-less workspaces)
    // keep their enabled list on the session row only.
    mocks.getWorkspaceSnapshot.mockResolvedValue(snapshot({ mcpServerIds: ['srv-a'] }));

    render(<WorkspaceContextBar workspaceId="ws-1" />);
    expect(await screen.findByTitle('Alpha: click to disable')).toBeTruthy();
  });

  it('rolls back the optimistic toggle when the backend rejects the update', async () => {
    mocks.getWorkspaceSnapshot.mockResolvedValue(
      snapshot({}, { selectedMcpServerIds: ['srv-a', 'srv-b'] })
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
    consoleError.mockRestore();
  });
});
