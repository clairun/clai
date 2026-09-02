import React from 'react';
import { describe, expect, it, vi } from 'vitest';
import { act, render, screen, within } from '@testing-library/react';
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

const noop = () => {};

const rail = (
  workspaces: WorkspaceListEntry[],
  overrides: Partial<React.ComponentProps<typeof WorkspaceRail>> = {},
) => (
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
  />
);

const renderRail = (
  workspaces: WorkspaceListEntry[],
  overrides: Partial<React.ComponentProps<typeof WorkspaceRail>> = {},
) => render(rail(workspaces, overrides));

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

  // The row menu used to be positioned inside the row, which lives in the
  // rail's scrolling list — on the bottom rows it was clipped out of view,
  // so the workspace could not be deleted at all. These tests pin the
  // portal and the upward flip that fixed it.
  describe('overflow menu placement', () => {
    const stubTriggerRect = (
      trigger: HTMLElement,
      rect: { top: number; bottom: number; right: number; width?: number },
    ) => {
      const width = rect.width ?? 24;
      vi.spyOn(trigger, 'getBoundingClientRect').mockReturnValue({
        ...rect,
        left: rect.right - width,
        width,
        height: rect.bottom - rect.top,
        x: rect.right - width,
        y: rect.top,
        toJSON: () => ({}),
      } as DOMRect);
    };

    const openMenuAt = async (rect: { top: number; bottom: number; right: number }) => {
      const trigger = screen.getByRole('button', { name: 'More actions' });
      stubTriggerRect(trigger, rect);
      await userEvent.click(trigger);
      return screen.getByRole('menu');
    };

    it('renders outside the rail so the scrolling list cannot clip it', async () => {
      renderRail([entry('a', 'Alpha')]);
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      const rail = screen.getByRole('navigation', { name: 'Workspaces' });
      expect(rail.contains(menu)).toBe(false);
      expect(document.body.contains(menu)).toBe(true);
    });

    it('drops below the trigger when there is room', async () => {
      renderRail([entry('a', 'Alpha')]);
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      // window.innerHeight is 768 in jsdom: 652px of room below is plenty.
      expect(menu.style.top).toBe('120px');
      expect(menu.style.bottom).toBe('');
      expect(menu.style.maxHeight).toBe('640px');
      // Right-aligned with the trigger (innerWidth 1024 − right edge 240).
      expect(menu.style.right).toBe('784px');
    });

    it('flips above the trigger for a row near the bottom of the window', async () => {
      renderRail([entry('a', 'Alpha')]);
      const menu = await openMenuAt({ top: 744, bottom: 760, right: 240 });
      // Only 8px below the trigger, so the menu hangs off its top edge.
      expect(menu.style.bottom).toBe('28px');
      expect(menu.style.top).toBe('');
      expect(menu.style.maxHeight).toBe('732px');
    });

    it('caps its height to the space left in a window too short for it', async () => {
      const realHeight = window.innerHeight;
      window.innerHeight = 200;
      try {
        renderRail([entry('a', 'Alpha')]);
        // 94px below, 90px above: neither side fits the menu, and below is
        // the roomier one, so it stays below and scrolls internally.
        const menu = await openMenuAt({ top: 90, bottom: 106, right: 240 });
        expect(menu.style.top).toBe('110px');
        expect(menu.style.maxHeight).toBe('82px');
      } finally {
        window.innerHeight = realHeight;
      }
    });

    it('re-anchors to its row when the list re-renders under it', async () => {
      const view = renderRail([entry('a', 'Alpha')]);
      const trigger = screen.getByRole('button', { name: 'More actions' });
      stubTriggerRect(trigger, { top: 100, bottom: 116, right: 240 });
      await userEvent.click(trigger);
      expect(screen.getByRole('menu').style.top).toBe('120px');
      // The 5s workspace poll hands down a fresh list every tick, which can
      // re-sort the rows out from under an open menu.
      stubTriggerRect(trigger, { top: 300, bottom: 316, right: 240 });
      await act(async () => {
        view.rerender(rail([entry('a', 'Alpha', { updatedAt: 2n })]));
      });
      expect(screen.getByRole('menu').style.top).toBe('320px');
    });

    it('forgets a menu whose row leaves the list', async () => {
      const view = renderRail([entry('a', 'Alpha')]);
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      await act(async () => {
        view.rerender(rail([]));
      });
      expect(screen.queryByRole('menu')).toBeNull();
      // Reappearing rows must not bring a stale menu back with them.
      await act(async () => {
        view.rerender(rail([entry('a', 'Alpha')]));
      });
      expect(screen.getByText('Alpha')).toBeTruthy();
      expect(screen.queryByRole('menu')).toBeNull();
    });

    it('keeps the row actions laid out while the menu is open', async () => {
      renderRail([entry('a', 'Alpha')]);
      const trigger = screen.getByRole('button', { name: 'More actions' });
      // The row only shows its actions on hover/focus-within, and opening
      // the menu moves focus out of the row into the portal — so the row
      // needs an explicit "menu open" class or the ⋯ trigger the menu is
      // positioned from collapses to a zero box. (CSS is off in jsdom, so
      // this asserts the class, not the computed display.)
      const row = trigger.closest('[role="button"]') as HTMLElement;
      expect(row.className).not.toMatch(/rowMenuOpen/);
      stubTriggerRect(trigger, { top: 100, bottom: 116, right: 240 });
      await userEvent.click(trigger);
      expect(row.className).toMatch(/rowMenuOpen/);
      // …and releases it again when the menu closes.
      await userEvent.keyboard('{Escape}');
      expect(row.className).not.toMatch(/rowMenuOpen/);
    });

    it('sizes the hover-action overlay per row: scheduled rows reserve room for run/pause', () => {
      renderRail([
        entry('a', 'Alpha'),
        entry('b', 'Beta', { scheduleEnabled: true }),
      ]);
      // The overlay width lives in CSS keyed off this class; jsdom cannot
      // measure it, so assert the class routing instead.
      const alphaRow = screen.getByText('Alpha').closest('[role="button"]');
      const betaRow = screen.getByText('Beta').closest('[role="button"]');
      expect(alphaRow?.className).not.toMatch(/rowScheduled/);
      expect(betaRow?.className).toMatch(/rowScheduled/);
    });

    it('holds its position when the trigger cannot be measured', async () => {
      const view = renderRail([entry('a', 'Alpha')]);
      const trigger = screen.getByRole('button', { name: 'More actions' });
      stubTriggerRect(trigger, { top: 100, bottom: 116, right: 240 });
      await userEvent.click(trigger);
      // A hidden trigger measures zero; re-anchoring to that would fling the
      // menu into the viewport corner.
      stubTriggerRect(trigger, { top: 0, bottom: 0, right: 0, width: 0 });
      await act(async () => {
        view.rerender(rail([entry('a', 'Alpha', { updatedAt: 2n })]));
      });
      expect(screen.getByRole('menu').style.top).toBe('120px');
    });

    it('does not select the workspace when a menu item is activated by keyboard', async () => {
      const onSelect = vi.fn();
      const onSettings = vi.fn();
      renderRail([entry('a', 'Alpha')], { onSelect, onSettings });
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      within(menu).getByRole('menuitem', { name: 'Settings' }).focus();
      // Keydowns bubble through the React tree out of the portal, so the
      // row's own Enter handler must ignore keys it did not receive.
      await userEvent.keyboard('{Enter}');
      expect(onSettings).toHaveBeenCalledWith('a');
      expect(onSelect).not.toHaveBeenCalled();
    });

    it('returns focus to the trigger when an item is activated', async () => {
      renderRail([entry('a', 'Alpha')]);
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      await userEvent.click(within(menu).getByRole('menuitem', { name: 'Star workspace' }));
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'More actions' }),
      );
    });

    it('lets Tab walk every item, then dismisses at either end', async () => {
      renderRail([entry('a', 'Alpha')]);
      const trigger = screen.getByRole('button', { name: 'More actions' });
      stubTriggerRect(trigger, { top: 100, bottom: 116, right: 240 });
      trigger.focus();
      await userEvent.keyboard('{Enter}');
      const menu = screen.getByRole('menu');
      // Reaching Delete is the whole point of the menu, and Tab is the only
      // way in without arrow keys — so Tab must move inside the menu and
      // only dismiss when it would leave it.
      for (const name of ['Settings', 'Fork workspace', 'Delete']) {
        await userEvent.tab();
        expect(document.activeElement).toBe(within(menu).getByRole('menuitem', { name }));
      }
      // The portal is the last thing in <body>, so letting focus out here
      // would strand the menu behind its click-swallowing backdrop.
      await userEvent.tab();
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(trigger);

      const reopened = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      expect(document.activeElement).toBe(
        within(reopened).getByRole('menuitem', { name: 'Star workspace' }),
      );
      await userEvent.tab({ shift: true });
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(trigger);
    });

    it('closes on a window resize', async () => {
      renderRail([entry('a', 'Alpha')]);
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      await act(async () => {
        window.dispatchEvent(new Event('resize'));
      });
      expect(screen.queryByRole('menu')).toBeNull();
      // Focus was inside the menu, so it comes back rather than falling to
      // <body> and restarting the next Tab from the top of the document.
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'More actions' }),
      );
    });

    it('closes on Escape and hands focus back to the trigger', async () => {
      renderRail([entry('a', 'Alpha')]);
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      await userEvent.keyboard('{Escape}');
      expect(screen.queryByRole('menu')).toBeNull();
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'More actions' }),
      );
    });

    it('moves focus into the menu on open, since it sits outside the tab order', async () => {
      renderRail([entry('a', 'Alpha')]);
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      expect(document.activeElement).toBe(
        within(menu).getByRole('menuitem', { name: 'Star workspace' }),
      );
    });

    it('does not select the workspace when a menu item is clicked', async () => {
      const onSelect = vi.fn();
      const onDelete = vi.fn();
      renderRail([entry('a', 'Alpha')], { onSelect, onDelete });
      const menu = await openMenuAt({ top: 100, bottom: 116, right: 240 });
      // Portaled children still bubble through the React tree, so the row's
      // onClick is one stopPropagation away from firing on every menu click.
      await userEvent.click(within(menu).getByRole('menuitem', { name: 'Delete' }));
      expect(onDelete).toHaveBeenCalledWith('a', 'Alpha');
      expect(onSelect).not.toHaveBeenCalled();
      expect(screen.queryByRole('menu')).toBeNull();
    });

    it('closes on a backdrop click without selecting the workspace', async () => {
      const onSelect = vi.fn();
      renderRail([entry('a', 'Alpha')], { onSelect });
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      // The backdrop is aria-hidden by design, so there is no role to query.
      const backdrop = document.querySelector<HTMLElement>('[class*="menuBackdrop"]');
      expect(backdrop).not.toBeNull();
      await userEvent.click(backdrop!);
      expect(screen.queryByRole('menu')).toBeNull();
      expect(onSelect).not.toHaveBeenCalled();
      // The backdrop is a close path the user drove, so focus comes back
      // rather than being stranded on <body>.
      expect(document.activeElement).toBe(
        screen.getByRole('button', { name: 'More actions' }),
      );
    });

    it('ignores scrolling elsewhere in the app', async () => {
      renderRail([entry('a', 'Alpha')]);
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      // Other panes scroll themselves programmatically while a run streams
      // (VirtualizedList pins to the bottom); that must not close the menu.
      const otherPane = document.createElement('div');
      document.body.appendChild(otherPane);
      await act(async () => {
        otherPane.dispatchEvent(new Event('scroll', { bubbles: false }));
      });
      expect(screen.queryByRole('menu')).not.toBeNull();
      otherPane.remove();
    });

    it('closes when the rail list scrolls out from under it', async () => {
      renderRail([entry('a', 'Alpha')]);
      await openMenuAt({ top: 100, bottom: 116, right: 240 });
      // A fixed menu cannot follow its row, so scrolling dismisses it.
      // Scroll does not bubble, hence the capture-phase listener.
      await act(async () => {
        screen
          .getByRole('navigation', { name: 'Workspaces' })
          .dispatchEvent(new Event('scroll', { bubbles: false }));
      });
      expect(screen.queryByRole('menu')).toBeNull();
    });
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
