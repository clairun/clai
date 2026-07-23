import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

import WorkspaceRail from './WorkspaceRail';
import type { WorkspaceListEntry } from '../../generated/bindings';

// Typed against the generated bindings so a backend field rename fails this
// test at compile time instead of silently making the mock stale.
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
  assignedAgentCount: 1,
  defaultManagerName: null,
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
  createdAt: 1n,
  updatedAt: 1n,
  ...overrides,
});

const noop = () => {};

const renderRail = (
  workspaces: WorkspaceListEntry[],
  overrides: Partial<React.ComponentProps<typeof WorkspaceRail>> = {},
) =>
  render(
    <WorkspaceRail
      workspaces={workspaces}
      selectedId={null}
      attentionCounts={{}}
      activeRuns={{}}
      collapsed={false}
      onToggleCollapsed={noop}
      onSelect={noop}
      onCreate={noop}
      onRunNow={noop}
      onTogglePause={noop}
      onToggleStar={noop}
      onSettings={noop}
      onFork={noop}
      onDelete={noop}
      runNowBusyId={null}
      forkBusyId={null}
      pauseBusyId={null}
      schedulerPaused={false}
      schedulerPauseBusy={false}
      onToggleSchedulerPaused={noop}
      {...overrides}
    />,
  );

describe('WorkspaceRail sections', () => {
  it('renders a plain headerless list when nothing is starred or in attention', () => {
    renderRail([
      entry('a', 'Alpha', { updatedAt: 3n }),
      entry('b', 'Beta', { updatedAt: 2n, scheduleEnabled: true }),
      entry('c', 'Gamma', { updatedAt: 1n }),
    ]);
    expect(screen.queryByText('Recent')).toBeNull();
    expect(screen.queryByText('Starred')).toBeNull();
    expect(screen.queryByText('Needs attention')).toBeNull();
    // Scheduled workspaces no longer jump the queue: pure recency order.
    const titles = screen
      .getAllByText(/Alpha|Beta|Gamma/)
      .map((el) => el.textContent);
    expect(titles).toEqual(['Alpha', 'Beta', 'Gamma']);
  });

  it('groups starred workspaces under a labeled Starred section above Recent', () => {
    renderRail([
      entry('a', 'Alpha', { updatedAt: 3n }),
      entry('b', 'Beta', { updatedAt: 2n, starred: true }),
      entry('c', 'Gamma', { updatedAt: 1n }),
    ]);
    expect(screen.getByText('Starred')).toBeTruthy();
    expect(screen.getByText('Recent')).toBeTruthy();
    const titles = screen
      .getAllByText(/Alpha|Beta|Gamma/)
      .map((el) => el.textContent);
    // Starred (Beta) first, then Recent in recency order (Alpha, Gamma).
    expect(titles).toEqual(['Beta', 'Alpha', 'Gamma']);
  });

  it('pins attention workspaces in a labeled section that outranks Starred', () => {
    renderRail(
      [
        entry('a', 'Alpha', { updatedAt: 3n, starred: true }),
        entry('b', 'Beta', { updatedAt: 2n }),
        entry('c', 'Gamma', { updatedAt: 1n, failedTaskCount: 1n, starred: true }),
      ],
      { attentionCounts: { b: 2 } },
    );
    expect(screen.getByText('Needs attention')).toBeTruthy();
    const titles = screen
      .getAllByText(/Alpha|Beta|Gamma/)
      .map((el) => el.textContent);
    // Attention: Beta (pending approvals) then Gamma (failed task), by
    // recency. Starred Alpha follows. Gamma sits under attention even
    // though starred — attention outranks the star.
    expect(titles).toEqual(['Beta', 'Gamma', 'Alpha']);
    expect(screen.queryByText('Recent')).toBeNull();
  });

  it('fires onToggleStar from the hover star button with the current state', async () => {
    const onToggleStar = vi.fn();
    renderRail(
      [entry('a', 'Alpha'), entry('b', 'Beta', { starred: true })],
      { onToggleStar },
    );
    await userEvent.click(screen.getByRole('button', { name: 'Star workspace' }));
    expect(onToggleStar).toHaveBeenCalledWith('a', false);
    await userEvent.click(screen.getByRole('button', { name: 'Unstar workspace' }));
    expect(onToggleStar).toHaveBeenCalledWith('b', true);
  });

  it('offers star/unstar in the per-row overflow menu', async () => {
    const onToggleStar = vi.fn();
    renderRail([entry('a', 'Alpha')], { onToggleStar });
    await userEvent.click(screen.getByRole('button', { name: 'More actions' }));
    const menu = screen.getByRole('menu');
    await userEvent.click(within(menu).getByRole('menuitem', { name: 'Star workspace' }));
    expect(onToggleStar).toHaveBeenCalledWith('a', false);
  });

  it('filter searches across sections and hides emptied ones', async () => {
    renderRail([
      entry('a', 'Alpha', { starred: true }),
      entry('b', 'Beta'),
    ]);
    await userEvent.type(
      screen.getByRole('textbox', { name: 'Filter workspaces by name' }),
      'bet',
    );
    expect(screen.queryByText('Starred')).toBeNull();
    expect(screen.getByText('Beta')).toBeTruthy();
    expect(screen.queryByText('Alpha')).toBeNull();
  });

  it('keeps the Starred header when every workspace is starred', () => {
    renderRail([
      entry('a', 'Alpha', { starred: true }),
      entry('b', 'Beta', { starred: true }),
    ]);
    // A lone non-Recent section still labels itself — otherwise the list
    // would silently look like a plain recency list while being pinned.
    expect(screen.getByText('Starred')).toBeTruthy();
    expect(screen.queryByText('Recent')).toBeNull();
  });

  it('collapsed rail shows no headers', () => {
    renderRail(
      [entry('a', 'Alpha', { starred: true }), entry('b', 'Beta')],
      { collapsed: true },
    );
    expect(screen.queryByText('Starred')).toBeNull();
    expect(screen.queryByText('Recent')).toBeNull();
  });
});
