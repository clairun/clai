import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

import FleetPage from './FleetPage';
import { useFleetStore } from '../fleet/fleetStore';
import { useFleetActivityStore } from '../stores/fleetActivityStore';
import { EMPTY_FLEET, emptyCard } from '../fleet/fleetModel';
import type { FleetWorkspace } from '../generated/bindings';

const mockInvoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

vi.mock('../fleet/client', () => ({ fleetSnapshot: vi.fn() }));
vi.mock('../api/client', () => ({ listAgentDefinitions: vi.fn(async () => []) }));
vi.mock('../components/AppUpdateBadge', () => ({ default: () => null }));
vi.mock('../components/FleetPage/FleetSelectedCard', () => ({
  default: ({ card, selected, onSelect }: { card: { workspaceId: string; title: string }; selected: boolean; onSelect: (id: string) => void }) =>
    selected ? (
      <h2>Selected {card.title}</h2>
    ) : (
      <button type="button" aria-label={`Select ${card.title}`} onClick={() => onSelect(card.workspaceId)}>
        {card.title}
      </button>
    ),
}));

const { fleetSnapshot } = vi.mocked(await import('../fleet/client'));

const workspace = (workspaceId: string, title: string, ageMs = 0): FleetWorkspace => ({
  workspaceId,
  title,
  starred: false,
  lastActivityAt: BigInt(Date.now() - ageMs),
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
  mockInvoke.mockClear();
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
    // The same swap as a click: it takes the stage, the stage card takes its slot.
    expect([screen.getByRole('heading', { name: /^Selected / }), ...screen.getAllByRole('button', { name: /^Select / })].map((e) => e.textContent)).toEqual([
      `Selected ${order[1]}`,
      order[0],
      order[2],
    ]);

    // Escape closes it with no change.
    press();
    expect(highlighted()).toContain(order[0]); // the next slot after the stage, now
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
      workspace('staged', 'Staged'),
      { ...workspace('live', 'Alpha', 1000), tasks: [task('t1', 'Old research', 'completed'), task('t2', 'New research', 'running')] },
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
      workspace('staged', 'Staged'),
      {
        ...workspace('a', 'Alpha', 1000),
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

  it('deselects on Escape, from the terminal too, and on a click on the empty grid, leaving the card on the stage', async () => {
    fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000)]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    expect(useFleetStore.getState().selectedId).toBe('a');
    const stage = () => screen.getAllByRole('article')[0]!;
    expect(stage()).toHaveTextContent('Selected Alpha');
    expect(stage().style.gridColumn).toBe('1 / span 2');
    fireEvent.keyDown(screen.getByRole('textbox', { name: 'Terminal' }), { key: 'Escape' });
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(stage()).toHaveTextContent('Alpha');
    expect(stage()).not.toHaveTextContent('Staged');

    fireEvent.click(screen.getByRole('button', { name: 'Select Alpha' }));
    fireEvent.click(stage().parentElement!);
    expect(useFleetStore.getState().selectedId).toBeNull();
    expect(screen.getAllByRole('article').map((a) => a.textContent)).toEqual(['Alpha', expect.stringContaining('Staged')]);
  });
  it('dims no card while another is selected', async () => {
    fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000), workspace('b', 'Beta', 2000)]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    const articles = screen.getAllByRole('article');
    expect(articles[0]!.parentElement!.className).not.toMatch(/gridSelecting/);
    for (const article of articles.slice(1)) expect(article.className).not.toMatch(/cardSelected/);
  });

  describe('a small card with an open request', () => {
    const ask = (pendingId: string, question: string, requestedAt: bigint, options: { label: string; description: string | null }[] | null) => ({
      pendingId,
      workspaceId: 'b',
      sessionId: 'session-b',
      runId: 'run-b',
      question,
      options,
      multiSelect: false,
      extraContext: null,
      requestedAt,
    });
    const setup = async (patch: Partial<FleetWorkspace>) => {
      fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), { ...workspace('b', 'Beta', 1000), ...patch }]);
      renderFleet();
      await screen.findByRole('button', { name: 'Select Beta' });
      return () => within(screen.getByRole('button', { name: 'Select Beta' }).closest('article')!);
    };
    const stageText = () => screen.getAllByRole('article')[0]!.textContent;

    it('answers a question from its options without selecting the card or moving the stage', async () => {
      const card = await setup({ pendingAsks: [ask('q1', 'Ship it?', 5n, [{ label: 'Yes', description: null }, { label: 'No', description: null }])] });
      const row = card().getByRole('region', { name: 'Needs you' });
      expect(row).toHaveTextContent('Ship it?');
      expect(within(row).getByRole('button', { name: 'Other…' })).toBeInTheDocument();
      const before = stageText();

      fireEvent.click(within(row).getByRole('button', { name: 'No' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
          request: { pendingId: 'q1', answer: 'No', selectedOptionIndex: 1, selectedOptionIndexes: null },
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
      expect(stageText()).toBe(before);
      // Still up until the backend resolves it.
      expect(card().getByRole('region', { name: 'Needs you' })).toBeInTheDocument();

      fireEvent.click(within(row).getByText('Ship it?'));
      expect(useFleetStore.getState().selectedId).toBe('b');
    });

    it('answers a question without options from a one-line input, Enter sends', async () => {
      const card = await setup({ pendingAsks: [ask('q1', 'Which branch?', 5n, null)] });
      const input = card().getByRole('textbox', { name: 'Your answer' });
      fireEvent.change(input, { target: { value: 'main' } });
      fireEvent.keyDown(input, { key: 'Enter' });
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
          request: { pendingId: 'q1', answer: 'main', selectedOptionIndex: null, selectedOptionIndexes: null },
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
    });

    it('approves or denies a command, and drops the row once resolved', async () => {
      const permission = { requestId: 'r1', workspaceId: 'b', agentId: null, agentName: 'Scout', command: 'cargo test', segments: [{}, {}] };
      const card = await setup({ pendingPermissions: [permission as never] });
      const row = card().getByRole('region', { name: 'Needs you' });
      expect(row).toHaveTextContent('Scout');
      expect(row).toHaveTextContent('cargo test');

      fireEvent.click(within(row).getByRole('button', { name: 'Deny' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
          requestId: 'r1',
          decisions: [{ kind: 'denyOnce' }, { kind: 'denyOnce' }],
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();

      act(() => useFleetStore.getState().resolveRequest('approval', 'r1'));
      expect(card().queryByRole('region', { name: 'Needs you' })).not.toBeInTheDocument();
    });

    it('approves a command once', async () => {
      const permission = { requestId: 'r1', workspaceId: 'b', agentId: null, agentName: 'Scout', command: 'ls', segments: [{}] };
      const card = await setup({ pendingPermissions: [permission as never] });
      fireEvent.click(card().getByRole('button', { name: 'Approve' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', { requestId: 'r1', decisions: [{ kind: 'allowOnce' }] })
      );
    });

    it('offers once, always and deny on a path grant', async () => {
      const grant = { requestId: 'g1', workspaceId: 'b', agentId: null, agentName: 'Scout', requestedPath: '/tmp/x', requestedAccess: 'read_only', reason: '' };
      const card = await setup({ pendingPathGrants: [grant as never] });
      fireEvent.click(card().getByRole('button', { name: 'Always allow' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('submit_path_grant_decision', {
          requestId: 'g1',
          decision: { kind: 'allowAlways', path: '/tmp/x', access: 'read_only', scope: 'agent' },
        })
      );
      expect(card().getByRole('button', { name: 'Allow once' })).toBeInTheDocument();
      expect(card().getByRole('button', { name: 'Deny' })).toBeInTheDocument();
    });

    it('keeps the request outside the clipped history, shows the newest, and selects on "+N more"', async () => {
      const tasks = Array.from({ length: 20 }, (_, i) =>
        ({ id: `t${i}`, title: `Task ${i}`, status: 'completed', parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' }) as never
      );
      const card = await setup({ tasks, pendingAsks: [ask('old', 'Older question?', 4n, null), ask('new', 'Newer question?', 9n, null)] });
      const row = card().getByRole('region', { name: 'Needs you' });
      const history = card().getByRole('list', { name: 'Activity' });
      expect(history.contains(row)).toBe(false);
      expect(history.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
      expect(row).toHaveTextContent('Newer question?');
      expect(row).not.toHaveTextContent('Older question?');

      fireEvent.click(within(row).getByRole('button', { name: '+1 more' }));
      expect(useFleetStore.getState().selectedId).toBe('b');
    });
  });
});
