import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

import FleetPage from './FleetPage';
import { useFleetStore } from '../fleet/fleetStore';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { EMPTY_FLEET, emptyCard } from '../fleet/fleetModel';
import type { FleetWorkspace } from '../generated/bindings';

vi.mock('../fleet/client', () => ({ fleetSnapshot: vi.fn() }));
vi.mock('../api/client', () => ({ listAgentDefinitions: vi.fn(async () => []) }));
vi.mock('../components/AppUpdateBadge', () => ({ default: () => null }));
vi.mock('../components/FleetPage/FleetSelectedCard', () => ({
  default: ({ card }: { card: { title: string } }) => <h2>Selected {card.title}</h2>,
}));

const { fleetSnapshot } = vi.mocked(await import('../fleet/client'));

const workspace = (workspaceId: string, title: string): FleetWorkspace => ({
  workspaceId,
  title,
  starred: false,
  lastActivityAt: BigInt(Date.now()),
  mainSessionId: null,
  activeRunIds: [],
  tasks: [],
  pendingPermissions: [],
  pendingPathGrants: [],
  pendingAsks: [],
  lastAssistantMessage: null,
});

const renderFleet = () =>
  render(
    <MemoryRouter>
      <FleetPage />
      <textarea aria-label="Terminal" />
    </MemoryRouter>
  );

beforeEach(() => {
  useFleetStore.setState({ fleet: EMPTY_FLEET, selectedId: null, loadedAt: null, loadError: null, buffer: null });
  useFleetActivityStore.setState({ activeRunsByWorkspace: {} });
  // A 4 × 3 grid.
  vi.stubGlobal(
    'ResizeObserver',
    class {
      constructor(private cb: ResizeObserverCallback) {}
      observe() {
        this.cb([{ contentRect: { width: 1400, height: 700 } } as ResizeObserverEntry], this as never);
      }
      disconnect() {}
    }
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
  fleetSnapshot.mockReset();
});

describe('FleetPage', () => {
  it('reloads the snapshot once for a card born untitled, and not again while it stays untitled', async () => {
    fleetSnapshot.mockResolvedValueOnce([]).mockResolvedValue([workspace('ws-new', '')]);
    renderFleet();
    await waitFor(() => expect(useFleetStore.getState().loadedAt).not.toBeNull());
    expect(fleetSnapshot).toHaveBeenCalledTimes(1);

    const bump = () =>
      act(() =>
        useFleetStore.getState().dispatch((s) => ({
          cards: { ...s.cards, 'ws-new': { ...(s.cards['ws-new'] ?? emptyCard('ws-new')), lastActivityAt: Date.now() + 1 } },
        }))
      );
    bump();
    await waitFor(() => expect(fleetSnapshot).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(useFleetStore.getState().buffer).toBeNull());
    bump();
    await act(async () => {});
    expect(fleetSnapshot).toHaveBeenCalledTimes(2);
  });

  it('opens the switcher on Ctrl+Tab, moves only its highlight while Ctrl is held, and selects on release', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), workspace('b', 'Beta'), workspace('c', 'Gamma')]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const terminal = screen.getByRole('textbox', { name: 'Terminal' });
    const press = (shiftKey = false) => fireEvent.keyDown(terminal, { key: 'Tab', ctrlKey: true, shiftKey });
    const titles = () => screen.getAllByRole('button', { name: /^Select / }).map((b) => b.textContent);
    const highlighted = () =>
      within(screen.getByRole('listbox', { name: 'Switch workspace' })).getByRole('option', { selected: true })
        .textContent;
    const order = titles(); // slot order
    const ids: Record<string, string> = { Alpha: 'a', Beta: 'b', Gamma: 'c' };

    expect(press()).toBe(false); // default (a tab, or a focus move) prevented
    expect(screen.getAllByRole('option').map((o) => o.textContent)).toEqual(
      order.map((title) => expect.stringContaining(title!))
    );
    expect(highlighted()).toContain(order[0]);
    press();
    press(true);
    press();
    expect(highlighted()).toContain(order[1]);
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(screen.queryByRole('heading', { name: /^Selected / })).not.toBeInTheDocument();

    fireEvent.keyUp(terminal, { key: 'Control' });
    expect(useFleetStore.getState().selectedId).toBe(ids[order[1]!]);
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();

    // Escape closes it with no change.
    press();
    expect(highlighted()).toContain(order[2]);
    fireEvent.keyDown(terminal, { key: 'Escape', ctrlKey: true });
    expect(screen.queryByRole('listbox')).not.toBeInTheDocument();
    fireEvent.keyUp(terminal, { key: 'Control' });
    expect(useFleetStore.getState().selectedId).toBe(ids[order[1]!]);
  });

  it('colours the border of a working card, but the selected one wins', async () => {
    fleetSnapshot.mockResolvedValue([
      { ...workspace('a', 'Alpha'), activeRunIds: ['run-1'] },
      { ...workspace('b', 'Beta'), activeRunIds: ['run-2'] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    await waitFor(() => expect(card('Alpha').className).toMatch(/cardWorking/));
    act(() => useFleetActivityStore.getState().markRunEnded('a', 'run-1'));
    expect(card('Alpha').className).not.toMatch(/cardWorking/);

    act(() => useFleetStore.getState().select('b'));
    const selected = screen.getByRole('heading', { name: 'Selected Beta' }).closest('article')!;
    expect(selected.className).toMatch(/cardSelected/);
    expect(selected.className).not.toMatch(/cardWorking/);
  });

  it('draws a running subtask larger than a finished one', async () => {
    const task = (id: string, title: string, status: string) =>
      ({ id, title, status, parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' }) as never;
    fleetSnapshot.mockResolvedValue([
      { ...workspace('live', 'Alpha'), tasks: [task('t1', 'Old research', 'completed'), task('t2', 'New research', 'running')] },
    ]);
    renderFleet();
    const live = (await screen.findByText('New research')).closest('li')!;
    const done = screen.getByText('Old research').closest('li')!;
    expect(live.className).toMatch(/liveItem/);
    expect(done.className).not.toMatch(/liveItem/);
    expect(within(live).getByText('Scout')).toBeInTheDocument();
  });

  it('renders the last message as markdown, after the events it followed', async () => {
    fleetSnapshot.mockResolvedValue([
      {
        ...workspace('a', 'Alpha'),
        tasks: [{ id: 't1', title: 'Research', status: 'completed', parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Main', assignedAgentDefinitionId: 'def-m' } as never],
        lastAssistantMessage: { messageId: 'm1', text: 'Shipped **v2**', truncated: false, createdAt: 2n },
      },
    ]);
    renderFleet();
    const bold = await screen.findByText('v2');
    expect(bold.tagName).toBe('STRONG');
    const rows = within(screen.getByRole('list', { name: 'Activity' })).getAllByRole('listitem');
    expect(rows.map((row) => row.textContent)).toEqual([expect.stringContaining('Research'), expect.stringContaining('Shipped v2')]);
  });

  it('deselects on Escape, from the terminal too, and on a click on the empty grid', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha')]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    expect(useFleetStore.getState().selectedId).toBe('a');
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Terminal' }), { key: 'Escape' });
    expect(useFleetStore.getState().selectedId).toBeNull();

    fireEvent.click(screen.getByRole('button', { name: 'Select Alpha' }));
    fireEvent.click(screen.getByRole('article').parentElement!);
    expect(useFleetStore.getState().selectedId).toBeNull();
  });
});
