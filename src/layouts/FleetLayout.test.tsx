import React from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter, Route, Routes } from 'react-router';

import FleetLayout from './FleetLayout';
import { WindowUiProvider } from '../windowUi';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { useFleetStore } from '../fleet/fleetStore';
import { EMPTY_FLEET, emptyCard, type FleetAsk } from '../fleet/fleetModel';
import type { WorkspaceDetails, WorkspaceListEntry } from '../generated/bindings';

const schedulerHandlers = vi.hoisted(() => new Map<string, (event: { payload: boolean }) => void>());
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (event: string, handler: (event: { payload: boolean }) => void) => {
    schedulerHandlers.set(event, handler);
    return () => { schedulerHandlers.delete(event); };
  }),
}));
const mockInvoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

vi.mock('../workspace/client', () => ({
  listWorkspaces: vi.fn(),
  deleteWorkspace: vi.fn(),
  forkWorkspace: vi.fn(),
  getWorkspaceDetails: vi.fn(),
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

const permissionAttention = vi.hoisted(() => ({ counts: {} as Record<string, number> }));

vi.mock('../hooks/usePermissionAttention', () => ({
  usePermissionAttention: () => permissionAttention.counts,
}));

vi.mock('../components/AppUpdateBadge', () => ({
  default: () => null,
}));
vi.mock('../hooks/useAvailableAppUpdate', () => ({ useAvailableAppUpdate: () => ({ update: null, support: null }) }));

// Stands in for the real modal so the settings tests can drive the post-save
// `onChanged` path. Renders nothing while closed, and no `role="dialog"`, so
// the delete-dialog assertions below stay unambiguous.
vi.mock('../components/Settings/WorkspaceSettingsModal', () => ({
  default: ({ isOpen, onChanged }: { isOpen: boolean; onChanged: () => void }) =>
    isOpen ? (
      <button type="button" data-testid="settings-modal" onClick={onChanged}>
        Save settings
      </button>
    ) : null,
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
const setSchedulerPaused = vi.mocked(workspaceClient.setSchedulerPaused);
const getWorkspaceDetails = vi.mocked(workspaceClient.getWorkspaceDetails);

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
    <WindowUiProvider><MemoryRouter initialEntries={[initialPath]}>
      <Routes>
        <Route element={<FleetLayout />}>
          <Route path="/workspace" element={<div data-testid="fleet-index" />} />
          <Route path="/workspace/:workspaceId" element={<div data-testid="workspace" />} />
        </Route>
        <Route path="/fleet" element={<div data-testid="fleet" />} />
      </Routes>
    </MemoryRouter></WindowUiProvider>
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

describe('FleetLayout starred launcher warnings', () => {
  it('keeps a saved star and shows the launcher warning', async () => {
    let persisted = false;
    const warning = 'Workspace star was saved, but its app launcher could not be updated: permission denied';
    listWorkspaces.mockImplementation(async () => [entry('a', 'Alpha', { starred: persisted })]);
    vi.mocked(workspaceClient.setWorkspaceStarred).mockImplementation(async () => {
      persisted = true;
      return warning;
    });
    getSchedulerPaused.mockResolvedValue(false);

    renderFleet();
    const row = await screen.findByRole('button', { name: /Alpha/ });
    await userEvent.click(within(row).getByRole('button', { name: 'Star workspace' }));

    expect(await screen.findByText(warning)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Unstar workspace' })).toBeInTheDocument();
    expect(workspaceClient.setWorkspaceStarred).toHaveBeenCalledWith('a', true);
  });
});

describe('FleetLayout new window action', () => {
  it('opens a second window for the chosen rail workspace', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    renderFleet();
    const row = await screen.findByRole('button', { name: /Alpha/ });
    await userEvent.click(within(row).getByRole('button', { name: 'More actions' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Open in new window' }));
    expect(mockInvoke).toHaveBeenCalledWith('window_open', { workspaceId: 'a' });
    expect(screen.getByTestId('workspace')).toBeInTheDocument();
  });
});

describe('FleetLayout global scheduler pause', () => {
  it('offers safe retries after failed reads and toggles only after a current read succeeds', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockRejectedValueOnce(new Error('offline'))
      .mockRejectedValueOnce(new Error('still offline'))
      .mockResolvedValueOnce(true);
    renderFleet();

    const retry = await screen.findByRole('button', { name: 'Retry reading global pause state' });
    expect(retry).toBeEnabled();
    await userEvent.click(retry);
    await waitFor(() => expect(getSchedulerPaused).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Retry reading global pause state' })).toBeEnabled());
    expect(setSchedulerPaused).not.toHaveBeenCalled();

    await userEvent.click(retry);
    const resume = await screen.findByRole('button', { name: 'Resume all scheduled runs' });
    expect(resume).toBeEnabled();
    expect(setSchedulerPaused).not.toHaveBeenCalled();
    await userEvent.click(resume);
    expect(setSchedulerPaused).toHaveBeenCalledWith(false);
  });
  it('replaces a stale pause action with retry after a focus read fails', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValueOnce(false).mockRejectedValueOnce(new Error('offline'))
      .mockResolvedValueOnce(true);
    renderFleet();
    const pause = await screen.findByRole('button', { name: 'Pause all scheduled runs' });
    await waitFor(() => expect(pause).toBeEnabled());

    act(() => window.dispatchEvent(new Event('focus')));
    const retry = await screen.findByRole('button', { name: 'Retry reading global pause state' });
    expect(screen.queryByRole('button', { name: 'Pause all scheduled runs' })).toBeNull();
    await userEvent.click(retry);
    expect(await screen.findByRole('button', { name: 'Resume all scheduled runs' })).toBeEnabled();
    expect(setSchedulerPaused).not.toHaveBeenCalled();
  });
  it('holds the pause control until a focus refresh resolves', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValueOnce(false);
    renderFleet();
    const pause = await screen.findByRole('button', { name: 'Pause all scheduled runs' });
    await waitFor(() => expect(pause).toBeEnabled());

    let resolveRefresh!: (value: boolean) => void;
    getSchedulerPaused.mockImplementationOnce(() => new Promise<boolean>((resolve) => { resolveRefresh = resolve; }));
    act(() => window.dispatchEvent(new Event('focus')));
    expect(pause).toBeDisabled();
    await userEvent.click(pause);
    expect(setSchedulerPaused).not.toHaveBeenCalled();

    await act(async () => resolveRefresh(true));
    const resume = screen.getByRole('button', { name: 'Resume all scheduled runs' });
    expect(resume).toBeEnabled();
    await userEvent.click(resume);
    expect(setSchedulerPaused).toHaveBeenCalledWith(false);
  });
  it('refreshes from the backend event and on focus before the next click', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    renderFleet();
    await screen.findByRole('button', { name: 'Pause all scheduled runs' });
    await waitFor(() => expect(schedulerHandlers.has('scheduler://paused')).toBe(true));
    act(() => schedulerHandlers.get('scheduler://paused')!({ payload: true }));
    expect(screen.getByRole('button', { name: 'Resume all scheduled runs' })).toBeInTheDocument();
    getSchedulerPaused.mockResolvedValue(false);
    await act(async () => window.dispatchEvent(new Event('focus')));
    expect(screen.getByRole('button', { name: 'Pause all scheduled runs' })).toBeInTheDocument();
  });
});

describe('FleetLayout attention', () => {
  it('counts a workspace with a pending approval as needing attention', async () => {
    permissionAttention.counts = { a: 1 };
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha'), entry('b', 'Beta')]);
    getSchedulerPaused.mockResolvedValue(false);
    try {
      renderFleet();
      await screen.findByText('Alpha');
      expect(within(screen.getByRole('button', { name: /Alpha/ })).getAllByTitle('Needs attention').length).toBeGreaterThan(0);
    } finally {
      permissionAttention.counts = {};
    }
  });

  it('counts an open question as needing attention, like the Fleet does', async () => {
    permissionAttention.counts = { a: 1 };
    const ask = { pendingId: 'q', question: 'Ship it?', sessionId: 'main' } as FleetAsk;
    const request = { kind: 'ask' as const, ask, text: ask.question, at: 1 };
    useFleetStore.setState({ fleet: { cards: { a: { ...emptyCard('a'), requests: { 'ask:q': request } }, b: { ...emptyCard('b'), requests: { 'ask:q': request } } } } });
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha'), entry('b', 'Beta'), entry('c', 'Gamma')]);
    getSchedulerPaused.mockResolvedValue(false);
    try {
      renderFleet();
      await screen.findByText('Alpha');
      const badges = (title: string) =>
        within(screen.getByRole('button', { name: new RegExp(title) })).getAllByTitle('Needs attention').map((el) => el.textContent);
      expect(badges('Alpha')).toContain('2');
      expect(badges('Beta')).toContain('1');
    } finally {
      permissionAttention.counts = {};
      useFleetStore.setState({ fleet: EMPTY_FLEET });
    }
  });
});

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
    // Deleting the open workspace redirects to the workspace index, and that too
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

describe('FleetLayout workspace settings', () => {
  // The modal reads only workspace metadata, so the file flag must be off:
  // with it on, the command walks the whole workspace filesystem and that
  // await gates the modal's first paint.
  const METADATA_ONLY = { includeFiles: false };

  const openSettings = async (title = 'Alpha') => {
    const row = await screen.findByRole('button', { name: new RegExp(title) });
    await userEvent.click(within(row).getByRole('button', { name: 'More actions' }));
    const menu = screen.getByRole('menu');
    await userEvent.click(within(menu).getByRole('menuitem', { name: 'Settings' }));
    return screen.findByTestId('settings-modal');
  };

  it('requests metadata-only details when opening workspace settings', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    getWorkspaceDetails.mockResolvedValue({ workspaceId: 'a' } as WorkspaceDetails);

    renderFleet();
    await openSettings();

    expect(getWorkspaceDetails).toHaveBeenCalledWith('a', METADATA_ONLY);
  });

  it('requests metadata-only details when refreshing after a save', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    getWorkspaceDetails.mockResolvedValue({ workspaceId: 'a' } as WorkspaceDetails);

    renderFleet();
    await userEvent.click(await openSettings());

    await waitFor(() => expect(getWorkspaceDetails).toHaveBeenCalledTimes(2));
    for (const call of getWorkspaceDetails.mock.calls) {
      expect(call).toEqual(['a', METADATA_ONLY]);
    }
  });
});

describe('FleetLayout run now', () => {
  beforeEach(() => useFleetActivityStore.setState({ activeRunsByWorkspace: {}, runNowSkipped: [] }));
  it('shows skipped feedback in the rail and clears it on retry', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha', { scheduleEnabled: true })]);
    getSchedulerPaused.mockResolvedValue(false);
    vi.mocked(workspaceClient.runWorkspaceNow).mockResolvedValue(undefined);
    useFleetActivityStore.setState({ runNowSkipped: ['a'] });

    renderFleet();
    const row = await screen.findByRole('button', { name: /Alpha/ });
    expect(within(row).getByRole('status')).toHaveTextContent('Run now skipped: session busy');
    await userEvent.click(within(row).getByRole('button', { name: 'Run now' }));
    await waitFor(() => expect(within(row).queryByRole('status')).not.toBeInTheDocument());
  });
  it('run now other error keeps failure message', async () => {
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha', { scheduleEnabled: true })]);
    getSchedulerPaused.mockResolvedValue(false);
    vi.mocked(workspaceClient.runWorkspaceNow).mockRejectedValue({ code: 'unknown' });

    renderFleet();
    const row = await screen.findByRole('button', { name: /Alpha/ });
    await userEvent.click(within(row).getByRole('button', { name: 'Run now' }));

    expect(await screen.findByText('Failed to start run.')).toBeInTheDocument();
    expect(useFleetActivityStore.getState().runNowSkipped).toEqual([]);
    expect(useFleetActivityStore.getState().activeRunsByWorkspace).toEqual({});
  });
});

describe('FleetLayout switch to the Fleet', () => {
  it('starts a workspace launch with no rail width and toggles it in this window', async () => {
    window.history.replaceState({}, '', '/?workspace=a');
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
    renderFleet('/workspace/a');
    expect(document.documentElement.style.getPropertyValue('--fleet-rail-width')).toBe('0px');
    expect(screen.queryByRole('navigation', { name: 'Workspaces' })).toBeNull();
    await userEvent.keyboard('{Control>}{Shift>}F{/Shift}{/Control}');
    expect(await screen.findByRole('navigation', { name: 'Workspaces' })).toBeInTheDocument();
    expect(document.documentElement.style.getPropertyValue('--fleet-rail-width')).toBe('248px');
    expect(screen.getByRole('button', { name: 'Open settings' })).toBeInTheDocument();
    window.history.replaceState({}, '', '/');
  });

  it('goes to the Fleet on Ctrl+Shift+M with this workspace selected there', async () => {
    useFleetStore.setState({ fleet: EMPTY_FLEET, selectedId: null });
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha'), entry('b', 'Beta')]);
    getSchedulerPaused.mockResolvedValue(false);
    renderFleet('/workspace/a');
    expect(await screen.findByTestId('workspace')).toBeInTheDocument();

    await userEvent.keyboard('{Control>}{Shift>}M{/Shift}{/Control}');
    expect(await screen.findByTestId('fleet')).toBeInTheDocument();
    expect(useFleetStore.getState().selectedId).toBe('a');
  });
});

describe('FleetLayout rail in a narrow window', () => {
  const PREFERENCE_KEY = 'fleet.rail.collapsed';
  // A window whose width the test sets; matchMedia evaluates its width
  // query against it on every call and notifies its listeners on a resize,
  // as the browser does.
  const WIDE = 1107;
  const NARROW = 1106;
  const media = { width: WIDE, listeners: new Set<() => void>() };
  const originalMatchMedia = window.matchMedia;
  const matchesWidth = (query: string) => {
    const match = /^\((max|min)-width: ([\d.]+)px\)$/.exec(query);
    if (!match) throw new Error(`unsupported media query: ${query}`);
    const limit = Number(match[2]);
    return match[1] === 'max' ? media.width <= limit : media.width >= limit;
  };
  const resize = (width: number) =>
    act(() => {
      media.width = width;
      media.listeners.forEach((listener) => listener());
    });
  const railWidth = () => document.documentElement.style.getPropertyValue('--fleet-rail-width');
  // A `localStorage` of its own: Node's global one, without a file, has none.
  const memoryStorage = () => {
    const items = new Map<string, string>();
    return { getItem: (key: string) => items.get(key) ?? null, setItem: (key: string, value: string) => void items.set(key, value) };
  };

  beforeEach(() => {
    vi.stubGlobal('localStorage', memoryStorage());
    media.width = WIDE;
    media.listeners.clear();
    window.matchMedia = vi.fn((query: string) => ({
      get matches() {
        return matchesWidth(query);
      },
      media: query,
      addEventListener: (_type: string, listener: () => void) => media.listeners.add(listener),
      removeEventListener: (_type: string, listener: () => void) => media.listeners.delete(listener),
    })) as unknown as typeof window.matchMedia;
    listWorkspaces.mockResolvedValue([entry('a', 'Alpha')]);
    getSchedulerPaused.mockResolvedValue(false);
  });

  afterEach(() => {
    window.matchMedia = originalMatchMedia;
    vi.unstubAllGlobals();
  });

  it('collapses an expanded rail while narrow and restores it on widening, leaving the saved preference alone', async () => {
    localStorage.setItem(PREFERENCE_KEY, '0');
    media.width = NARROW;
    renderFleet();

    expect(await screen.findByRole('button', { name: 'Expand sidebar' })).toBeInTheDocument();
    expect(railWidth()).toBe('52px');

    resize(WIDE);
    expect(screen.getByRole('button', { name: 'Collapse sidebar' })).toBeInTheDocument();
    expect(railWidth()).toBe('248px');

    resize(NARROW);
    expect(screen.getByRole('button', { name: 'Expand sidebar' })).toBeInTheDocument();
    expect(localStorage.getItem(PREFERENCE_KEY)).toBe('0');
  });

  it('expands the rail from its toggle while narrow without saving that, until the window widens', async () => {
    localStorage.setItem(PREFERENCE_KEY, '1');
    media.width = NARROW;
    renderFleet();

    await userEvent.click(await screen.findByRole('button', { name: 'Expand sidebar' }));
    expect(railWidth()).toBe('248px');
    expect(localStorage.getItem(PREFERENCE_KEY)).toBe('1');
    await userEvent.click(screen.getByRole('button', { name: 'Collapse sidebar' }));
    expect(railWidth()).toBe('52px');

    // Expanded again, then widened: the saved (collapsed) preference wins,
    // and narrowing once more starts collapsed rather than expanded.
    await userEvent.click(screen.getByRole('button', { name: 'Expand sidebar' }));
    resize(WIDE);
    expect(railWidth()).toBe('52px');
    resize(NARROW);
    expect(railWidth()).toBe('52px');
    expect(localStorage.getItem(PREFERENCE_KEY)).toBe('1');
  });

  it('stops listening for window resizes once unmounted', async () => {
    const { unmount } = renderFleet();
    await screen.findByRole('button', { name: 'Collapse sidebar' });
    expect(media.listeners.size).toBeGreaterThan(0);

    unmount();
    expect(media.listeners.size).toBe(0);
  });

  it('still saves the toggle in a wide window', async () => {
    localStorage.setItem(PREFERENCE_KEY, '0');
    renderFleet();

    await userEvent.click(await screen.findByRole('button', { name: 'Collapse sidebar' }));
    expect(railWidth()).toBe('52px');
    expect(localStorage.getItem(PREFERENCE_KEY)).toBe('1');
  });
});
