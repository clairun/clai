import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes } from 'react-router';

import FleetLayout from './FleetLayout';
import type { WorkspaceListEntry } from '../generated/bindings';

vi.mock('../workspace/client', () => ({
  listWorkspaces: vi.fn(),
  deleteWorkspace: vi.fn(),
  forkWorkspace: vi.fn(),
  getWorkspaceSnapshot: vi.fn(),
  runWorkspaceNow: vi.fn(),
  setWorkspaceSchedulePaused: vi.fn(),
  setWorkspaceStarred: vi.fn(),
  getSchedulerPaused: vi.fn(),
  setSchedulerPaused: vi.fn(),
  createWorkspace: vi.fn(),
  copyWorkspacePath: vi.fn(),
}));

vi.mock('../hooks/useFleetActivity', () => ({
  useFleetActivity: () => ({}),
}));

vi.mock('../hooks/usePermissionAttention', () => ({
  usePermissionAttention: () => ({}),
}));

vi.mock('../components/AppUpdateBadge', () => ({
  default: () => null,
}));

vi.mock('../components/Settings/WorkspaceSettingsModal', () => ({
  default: () => null,
}));

vi.mock('../components/Settings', () => ({
  TABS: { PROVIDER: 'provider' },
  SettingsModal: () => null,
}));

vi.mock('../components/ProgressDialog', () => ({
  default: () => null,
}));

const workspaceClient = await import('../workspace/client');
const listWorkspaces = vi.mocked(workspaceClient.listWorkspaces);
const deleteWorkspace = vi.mocked(workspaceClient.deleteWorkspace);
const getSchedulerPaused = vi.mocked(workspaceClient.getSchedulerPaused);

const entry = (
  id: string,
  title: string,
  overrides: Partial<WorkspaceListEntry> = {},
): WorkspaceListEntry => ({
  id,
  kind: 'general',
  title,
  agentId: null,
  enabled: true,
  messageCount: 0n,
  runningTaskCount: 0n,
  blockedTaskCount: 0n,
  failedTaskCount: 0n,
  attentionTaskCount: 0n,
  latestAttentionTaskId: null,
  latestAttentionTaskTitle: null,
  latestAttentionTaskStatus: null,
  latestAttentionTaskSummary: null,
  latestAttentionTaskUpdatedAt: null,
  scheduleEnabled: false,
  schedulePaused: false,
  scheduleKind: null,
  nextRunInSeconds: null,
  unread: false,
  starred: false,
  updatedAt: 1n,
  ...overrides,
});

const renderFleet = (initialPath = '/workspace/a') =>
  render(
    <MemoryRouter initialEntries={[initialPath]}>
      <Routes>
        <Route element={<FleetLayout />}>
          <Route path="/fleet" element={<div data-testid="fleet-index" />} />
          <Route path="/workspace/:workspaceId" element={<div data-testid="workspace" />} />
        </Route>
      </Routes>
    </MemoryRouter>
  );

// The rail row is a role="button" whose accessible name contains the
// workspace title, so it is a stable scope for that row's ⋯ menu.
const openDeleteDialog = async (title = 'Alpha') => {
  const row = await screen.findByRole('button', { name: new RegExp(title) });
  await userEvent.click(within(row).getByRole('button', { name: 'More actions' }));
  const menu = screen.getByRole('menu');
  await userEvent.click(within(menu).getByRole('menuitem', { name: 'Delete' }));
  // `role="dialog"` is the stable handle; scoping by DOM ancestry would break
  // the moment ConfirmDialog wraps its heading. Matching on the accessible
  // name also pins the aria-labelledby wiring.
  return screen.findByRole('dialog', { name: 'Delete workspace?' });
};

const confirmDelete = async (dialog: HTMLElement) => {
  await userEvent.click(within(dialog).getByRole('button', { name: 'Delete workspace' }));
};

describe('FleetLayout workspace deletion', () => {
  it('dismisses the delete dialog as soon as the delete lands, without waiting for the rail refresh', async () => {
    // Hold the post-delete refresh open, so the assertions below can only
    // pass if the dialog closes on the delete itself rather than on the
    // refresh that follows it.
    let finishRefresh!: (workspaces: WorkspaceListEntry[]) => void;
    const refresh = new Promise<WorkspaceListEntry[]>((resolve) => {
      finishRefresh = resolve;
    });
    // Every call after the initial load hangs, including the 5s poll, so the
    // dialog can only close if it stops waiting on the refresh.
    listWorkspaces
      .mockResolvedValueOnce([entry('a', 'Alpha')])
      .mockReturnValue(refresh);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace.mockResolvedValue(undefined);

    renderFleet();
    await confirmDelete(await openDeleteDialog());

    await waitFor(() => expect(deleteWorkspace).toHaveBeenCalledWith('a'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    // Deleting the open workspace redirects to the fleet index, and that too
    // must not wait on the refresh.
    expect(await screen.findByTestId('fleet-index')).toBeInTheDocument();

    // Let the held refresh settle inside act so the trailing state update
    // does not leak into the next test.
    await act(async () => {
      finishRefresh([]);
    });
  });

  it('stays on the current workspace when a different workspace is deleted', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha'), entry('b', 'Beta')]);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace.mockResolvedValue(undefined);

    renderFleet('/workspace/b');
    await confirmDelete(await openDeleteDialog());

    await waitFor(() => expect(deleteWorkspace).toHaveBeenCalledWith('a'));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(screen.getByTestId('workspace')).toBeInTheDocument();
    expect(screen.queryByTestId('fleet-index')).toBeNull();
  });

  it('keeps the dialog open and reports the failure inline when the delete fails', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace.mockRejectedValue(new Error('disk refused'));

    renderFleet();
    const dialog = await openDeleteDialog();
    await confirmDelete(dialog);

    await waitFor(() => expect(deleteWorkspace).toHaveBeenCalledWith('a'));
    // Inside the dialog, deliberately: the page-level error banner is cleared
    // by loadWorkspaces on every successful 5s poll, so a failure reported
    // there would vanish within seconds of the user reading it.
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('disk refused');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    // Both buttons must come back, so the user can retry or back out instead
    // of being left with a dialog that only an outside click dismisses.
    expect(within(dialog).getByRole('button', { name: 'Delete workspace' })).toBeEnabled();
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeEnabled();
    // Reported once, inside the dialog -- not also copied to the page banner.
    expect(screen.getAllByText(/disk refused/)).toHaveLength(1);
  });

  it('leaves the rail usable while the post-delete refresh is still in flight', async () => {
    // The dialog closes before the refresh settles, so the overlay no longer
    // shields the rail. A delete opened in that window must not inherit the
    // previous delete's busy state: a busy dialog disables both buttons and
    // gates Escape and outside-click, leaving no way out at all.
    let finishRefresh!: (workspaces: WorkspaceListEntry[]) => void;
    const refresh = new Promise<WorkspaceListEntry[]>((resolve) => {
      finishRefresh = resolve;
    });
    listWorkspaces
      .mockResolvedValueOnce([entry('a', 'Alpha'), entry('b', 'Beta')])
      .mockReturnValue(refresh);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace.mockResolvedValue(undefined);

    renderFleet('/workspace/b');
    await confirmDelete(await openDeleteDialog());
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    const second = await openDeleteDialog('Beta');
    expect(within(second).getByRole('button', { name: 'Delete workspace' })).toBeEnabled();
    expect(within(second).getByRole('button', { name: 'Cancel' })).toBeEnabled();

    await act(async () => {
      finishRefresh([]);
    });
  });

  it('clears the previous failure and closes the dialog when a retry succeeds', async () => {
    let finishRetry!: () => void;
    const retry = new Promise<void>((resolve) => {
      finishRetry = resolve;
    });
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace
      .mockRejectedValueOnce(new Error('disk refused'))
      .mockReturnValueOnce(retry);

    renderFleet();
    const dialog = await openDeleteDialog();
    await confirmDelete(dialog);
    await within(dialog).findByRole('alert');

    await confirmDelete(dialog);

    // The stale failure goes as soon as the retry starts, so the user is not
    // reading an error about a request that is already superseded.
    await waitFor(() => expect(within(dialog).queryByRole('alert')).toBeNull());
    await act(async () => {
      finishRetry();
    });

    await waitFor(() => expect(deleteWorkspace).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(screen.queryByText('disk refused')).toBeNull();
  });

  it('drops a stale failure when the dialog is reopened', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    deleteWorkspace.mockRejectedValue(new Error('disk refused'));

    renderFleet();
    const dialog = await openDeleteDialog();
    await confirmDelete(dialog);
    await within(dialog).findByRole('alert');

    await userEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    const reopened = await openDeleteDialog();
    expect(within(reopened).queryByRole('alert')).toBeNull();
  });
});
