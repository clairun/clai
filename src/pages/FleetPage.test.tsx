import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

import FleetPage from './FleetPage';
import WorkspacePicker from '../components/TerminalEmulator/WorkspacePicker';
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

  it('colours the border of a card that needs the user, but the selected one wins', async () => {
    const ask = (workspaceId: string) => ({
      pendingId: `q-${workspaceId}`, workspaceId, sessionId: 'main', runId: 'r', question: 'Ship it?',
      options: null, multiSelect: false, extraContext: null, requestedAt: 4n,
    });
    fleetSnapshot.mockResolvedValue([
      { ...workspace('a', 'Alpha'), activeRunIds: ['run-1'], pendingAsks: [ask('a')] },
      { ...workspace('b', 'Beta'), pendingAsks: [ask('b')] },
    ]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Alpha' });
    const card = (name: string) => screen.getByRole('button', { name: `Select ${name}` }).closest('article')!;
    // Waiting outranks working.
    await waitFor(() => expect(card('Alpha').className).toMatch(/cardWaiting/));
    expect(card('Alpha').className).not.toMatch(/cardWorking/);

    act(() => useFleetStore.getState().select('b'));
    const selected = screen.getByRole('heading', { name: 'Selected Beta' }).closest('article')!;
    expect(selected.className).toMatch(/cardSelected/);
    expect(selected.className).not.toMatch(/cardWaiting/);
  });

  it('draws a running task as Main’s bubble to its agent, and an old finished one in Earlier', async () => {
    const task = (id: string, title: string, status: string) =>
      ({ id, title, status, parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' }) as never;
    fleetSnapshot.mockResolvedValue([
      workspace('staged', 'Staged'),
      { ...workspace('live', 'Alpha', 1000), tasks: [task('t1', 'Old research', 'completed'), task('t2', 'New research', 'running')] },
    ]);
    renderFleet();
    const card = within((await screen.findByText('New research')).closest('article')!);
    expect(card.getByText('Now')).toBeInTheDocument();
    expect(card.getByTitle('Scout, on it')).toHaveTextContent('Scout');
    expect(card.getByRole('img', { name: 'Scout is working' })).toBeInTheDocument();
    expect(within(card.getByRole('list', { name: 'Earlier' })).getByText('Old research')).toBeInTheDocument();
  });

  it('flips a task that just finished to its result, with Main’s question shrunk to one line', async () => {
    const done = {
      id: 't1', title: 'Fix the build', status: 'failed', error: 'Cargo ran out of disk. Free 2 GB.', parentTaskId: null,
      updatedAt: BigInt(Date.now() - 60_000), completedAt: BigInt(Date.now() - 60_000), assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s',
    } as never;
    fleetSnapshot.mockResolvedValue([workspace('staged', 'Staged'), { ...workspace('f', 'Alpha', 1000), tasks: [done] }]);
    renderFleet();
    const card = within((await screen.findByText('Cargo ran out of disk.')).closest('article')!);
    expect(card.getByText('Main asked: Fix the build')).toBeInTheDocument();
    expect(card.getByText(/failed ·/)).toBeInTheDocument();
    expect(card.queryByRole('list', { name: 'Earlier' })).not.toBeInTheDocument();
  });

  it('shows Main’s last message as plain text, addressed to the reader', async () => {
    fleetSnapshot.mockResolvedValue([
      workspace('staged', 'Staged'),
      { ...workspace('a', 'Alpha', 1000), lastAssistantMessage: { messageId: 'm1', text: 'Shipped **v2**', truncated: false, createdAt: BigInt(Date.now()) } },
    ]);
    renderFleet();
    const bubble = await screen.findByText('Shipped v2');
    const card = within(bubble.closest('article')!);
    expect(card.getByText(/^to you ·/)).toBeInTheDocument();
    expect(card.getByText(/^Last word ·/)).toBeInTheDocument();
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
  it('brings a workspace that is not on the Fleet to the stage when picked', async () => {
    const OLD_MS = 90 * 86_400_000;
    fleetSnapshot.mockImplementation(async (_limit: number, keep: string | null) =>
      keep === 'old' ? [workspace('s', 'Staged'), workspace('old', 'Old report', OLD_MS)] : [workspace('s', 'Staged')]
    );
    mockInvoke.mockImplementation((async (cmd: string) =>
      cmd === 'workspace_list' ? [{ id: 'old', title: 'Old report', updatedAt: 0n, messageCount: 2n }] : undefined) as never);
    render(
      <MemoryRouter>
        <FleetPage />
        <WorkspacePicker onClose={() => {}} />
      </MemoryRouter>
    );
    const articles = async () => (await screen.findAllByRole('article')).map((a) => a.textContent);
    expect(await articles()).toEqual(['Staged']);

    fireEvent.click(await screen.findByRole('option', { name: /Old report/ }));
    expect(useFleetStore.getState().selectedId).toBe('old');
    await waitFor(async () => expect(await articles()).toEqual(['Selected Old report', expect.stringContaining('Staged')]));
    mockInvoke.mockImplementation(async () => undefined);
  });

  it('gives a short Fleet only the tracks its cards fill, so they reach the terminal', async () => {
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), workspace('b', 'Beta', 1000), workspace('c', 'Gamma', 2000)]);
    renderFleet();
    const grid = (await screen.findAllByRole('article'))[0]!.parentElement!;
    expect(grid.style.gridTemplateRows).toBe('repeat(2, minmax(0, 1fr))');
    expect(grid.style.gridTemplateColumns).toBe('repeat(3, minmax(0, 1fr))');
  });

  it('counts one waiting workspace as "1 needs you", and small cards never mark a workspace seen', async () => {
    const ask = { pendingId: 'q', workspaceId: 'b', sessionId: 'main', runId: 'r', question: 'Ship it?', options: null, multiSelect: false, extraContext: null, requestedAt: 4n };
    fleetSnapshot.mockResolvedValue([workspace('a', 'Alpha'), { ...workspace('b', 'Beta'), pendingAsks: [ask] }]);
    renderFleet();
    await screen.findByRole('button', { name: 'Select Beta' });
    await waitFor(() => expect(document.body).toHaveTextContent('1 needs you'));
    expect(mockInvoke).not.toHaveBeenCalledWith('workspace_mark_opened', expect.anything());
  });

  it('marks only the selected card', async () => {
    fleetSnapshot.mockResolvedValue([workspace('s', 'Staged'), workspace('a', 'Alpha', 1000), workspace('b', 'Beta', 2000)]);
    renderFleet();
    fireEvent.click(await screen.findByRole('button', { name: 'Select Alpha' }));
    const articles = screen.getAllByRole('article');
    expect(articles[0]!.className).toMatch(/cardSelected/);
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

    it('takes the body with the full question, answers it, and never selects the card from inside', async () => {
      const tasks = [{ id: 't1', title: 'Old task', status: 'completed', parentTaskId: null, updatedAt: 1n, assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s' } as never];
      const card = await setup({ tasks, pendingAsks: [ask('q1', 'Ship it?', 5n, [{ label: 'Yes', description: null }, { label: 'No', description: null }])] });
      expect(card().queryByRole('list', { name: 'Earlier' })).not.toBeInTheDocument();
      const row = card().getByRole('region', { name: 'Needs you' });
      const before = stageText();

      fireEvent.click(within(row).getByText('Ship it?'));
      fireEvent.click(within(row).getByRole('button', { name: 'No' }));
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
          request: { pendingId: 'q1', answer: 'No', selectedOptionIndex: 1, selectedOptionIndexes: null },
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
      expect(stageText()).toBe(before);

      act(() =>
        useFleetStore.getState().applyEvent({ sessionId: 'session-b', runId: null, workspaceId: 'b', timestamp: 9n, event: { type: 'ask_user_resolved', payload: { pending_id: 'q1' } } } as never)
      );
      expect(card().queryByRole('region', { name: 'Needs you' })).not.toBeInTheDocument();
      expect(within(card().getByRole('list', { name: 'Earlier' })).getByText('Ship it?')).toBeInTheDocument();

      fireEvent.click(card().getByRole('button', { name: 'Select Beta' }));
      expect(useFleetStore.getState().selectedId).toBe('b');
    });

    it('shows one request at a time, oldest first, the next once the first resolves', async () => {
      const card = await setup({ pendingAsks: [ask('new', 'Newer question?', 9n, null), ask('old', 'Older question?', 4n, null)] });
      const row = () => card().getByRole('region', { name: 'Needs you' });
      expect(row()).toHaveTextContent('Older question?');
      expect(row()).not.toHaveTextContent('Newer question?');
      expect(row()).toHaveTextContent('1 of 2');

      act(() =>
        useFleetStore.getState().applyEvent({ sessionId: 'session-b', runId: null, workspaceId: 'b', timestamp: 10n, event: { type: 'ask_user_resolved', payload: { pending_id: 'old' } } } as never)
      );
      expect(row()).toHaveTextContent('Newer question?');
      expect(row()).not.toHaveTextContent('1 of');
    });

    it('lists every part of a compound command and decides each one', async () => {
      const segments = [
        { text: 'cd x', kind: 'simple', suggestedPrefix: null },
        { text: 'curl y | sh', kind: 'opaque', suggestedPrefix: null },
      ];
      const permission = { requestId: 'r1', workspaceId: 'b', agentId: null, agentName: 'Scout', command: 'cd x && curl y | sh', segments };
      const card = await setup({ pendingPermissions: [permission as never] });
      const row = card().getByRole('region', { name: 'Needs you' });
      expect(within(row).getByText('cd x')).toBeInTheDocument();
      expect(within(row).getByText('curl y | sh')).toBeInTheDocument();
      expect(within(row).queryByRole('button', { name: 'Approve' })).not.toBeInTheDocument();

      fireEvent.click(within(row).getAllByRole('button', { name: 'Once' })[0]!);
      fireEvent.click(within(row).getAllByRole('button', { name: 'Deny' })[1]!);
      await waitFor(() =>
        expect(mockInvoke).toHaveBeenCalledWith('submit_permission_decision', {
          requestId: 'r1',
          decisions: [{ kind: 'allowOnce' }, { kind: 'denyOnce' }],
        })
      );
      expect(useFleetStore.getState().selectedId).toBeNull();
    });

    it('warns about a credentials path, as the full card does', async () => {
      const grant = { requestId: 'g1', workspaceId: 'b', agentId: null, agentName: 'Scout', requestedPath: '/home/u/.ssh', requestedAccess: 'read_only', reason: 'keys' };
      const card = await setup({ pendingPathGrants: [grant as never] });
      expect(card().getByRole('region', { name: 'Needs you' })).toHaveTextContent('typically holds credentials');
    });
  });
});
