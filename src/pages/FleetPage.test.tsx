import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

import FleetPage from './FleetPage';
import { useFleetStore } from '../fleet/fleetStore';
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

  it('cycles the selection through the slots with Ctrl+Tab and Ctrl+Shift+Tab, from the terminal too', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), workspace('b', 'Beta'), workspace('c', 'Gamma')]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const terminal = screen.getByRole('textbox', { name: 'Terminal' });
    const press = (shiftKey = false) => fireEvent.keyDown(terminal, { key: 'Tab', ctrlKey: true, shiftKey });

    expect(press(true)).toBe(false); // default (a tab, or a focus move) prevented
    expect(useFleetStore.getState().selectedId).toBe('c');
    press();
    expect(useFleetStore.getState().selectedId).toBe('a');
    press();
    expect(screen.getByRole('heading', { name: 'Selected Beta' })).toBeInTheDocument();
    press(true);
    press(true);
    expect(useFleetStore.getState().selectedId).toBe('c');
    expect(screen.getByRole('main', { name: 'Fleet' })).toBeInTheDocument();
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
