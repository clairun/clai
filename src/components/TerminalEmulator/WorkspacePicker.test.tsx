import { beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import type { WorkspaceListEntry } from '../../generated/bindings';

beforeAll(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
});

const listWorkspaces = vi.hoisted(() => vi.fn());
vi.mock('../../workspace/client', () => ({ listWorkspaces }));
vi.mock('./WorkspaceTerminal', () => ({ default: () => null }));
vi.mock('../../workspace/components/WorkspaceContextBar', () => ({ default: () => null }));

import TerminalEmulator from './TerminalEmulator';
import { useFleetStore } from '../../fleet/fleetStore';
import { EMPTY_FLEET, emptyCard } from '../../fleet/fleetModel';

const entry = (id: string, title: string, updatedAt: number): WorkspaceListEntry =>
  ({ id, title, updatedAt: BigInt(updatedAt), messageCount: 3n }) as unknown as WorkspaceListEntry;

const renderComposer = () =>
  render(
    <MemoryRouter initialEntries={['/fleet']}>
      <TerminalEmulator onSendToChat={vi.fn(async () => ({}))} />
    </MemoryRouter>
  );

const optionNames = () =>
  within(screen.getByRole('listbox')).getAllByRole('option').map((o) => o.textContent);

describe('the Fleet terminal workspace picker', () => {
  beforeEach(() => {
    const now = Date.now();
    useFleetStore.setState({
      selectedId: null,
      buffer: null,
      loadSnapshot: vi.fn(async () => {}),
      fleet: { ...EMPTY_FLEET, cards: { live: { ...emptyCard('live', 'Live one'), lastActivityAt: now } } },
    });
    listWorkspaces.mockResolvedValue([
      entry('live', 'Live one', now),
      entry('new-1', 'New Workspace', now - 1000),
      entry('old', 'Old report', now - 90 * 86_400_000),
    ]);
  });

  it('opens on Ctrl+K; typing a name and Enter selects it and returns to the input', async () => {
    const user = userEvent.setup();
    renderComposer();
    const input = screen.getByRole('textbox');
    await user.keyboard('{Control>}k{/Control}');

    const search = screen.getByRole('combobox', { name: 'Search workspaces' });
    expect(search).toHaveFocus();
    expect(search).toHaveAttribute('placeholder', 'Search workspaces (Ctrl/Cmd+K)');
    await screen.findByText('Old report');

    await user.keyboard('old{Enter}');
    expect(useFleetStore.getState().selectedId).toBe('old');
    // Not among the loaded cards: the snapshot is reloaded to bring it in.
    expect(useFleetStore.getState().loadSnapshot).toHaveBeenCalled();
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(input).toHaveFocus();
  });

  it('lists the Fleet first, then the rest by recency, and filters by name', async () => {
    const user = userEvent.setup();
    renderComposer();
    await user.click(screen.getByRole('button', { name: '+ Create workspace' }));
    await screen.findByText('Old report');
    expect(optionNames()).toEqual([
      expect.stringContaining('Create workspace'),
      expect.stringContaining('Live one'),
      expect.stringContaining('New Workspace'),
      expect.stringContaining('Old report'),
    ]);

    await user.keyboard('rep');
    expect(optionNames()).toEqual([expect.stringContaining('Create workspace'), expect.stringContaining('Old report')]);
    await user.keyboard('{ArrowUp}{Enter}');
    expect(useFleetStore.getState().selectedId).toBeNull();
  });

  it('tells creating apart from a workspace named "New Workspace"', async () => {
    const user = userEvent.setup();
    renderComposer();
    await user.click(screen.getByRole('button', { name: '+ Create workspace' }));
    await screen.findByText('New Workspace');
    const [create, ...workspaces] = within(screen.getByRole('listbox')).getAllByRole('option');
    expect(create).toHaveTextContent(/^\+Create workspace$/);
    const named = workspaces.find((o) => o.textContent?.includes('New Workspace'))!;
    expect(named).toHaveTextContent(/New Workspace3 messages · \d+s ago/);
    expect(within(named).getByRole('img', { name: 'Main' })).toBeInTheDocument();

    await user.click(named);
    expect(useFleetStore.getState().selectedId).toBe('new-1');
    useFleetStore.setState((s) => ({
      fleet: { cards: { ...s.fleet.cards, 'new-1': emptyCard('new-1', 'New Workspace') } },
    }));
    expect(await screen.findByRole('button', { name: '→ New Workspace' })).toBeInTheDocument();
  });

  it('closes on Esc without changing the target, back in the input', async () => {
    const user = userEvent.setup();
    renderComposer();
    await user.click(screen.getByRole('button', { name: '+ Create workspace' }));
    await user.keyboard('{Escape}');
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(screen.getByRole('textbox')).toHaveFocus();
  });
});
