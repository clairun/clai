import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import { useAssistantStore } from '../../assistant';
import { applyAssistantEvent, cardScene, emptyCard, openRequests, type FleetCard, type FleetLive } from '../../fleet/fleetModel';
import type { AssistantEventEnvelope, AssistantMessage } from '../../generated/bindings';
import FleetSelectedCard from './FleetSelectedCard';

const mockInvoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('./useFleetChat', () => ({ useFleetChat: () => ({ roster: [], error: null, loading: false }) }));

const NO_LIVE: FleetLive = { runs: {}, tasks: {} };
const NOW = 1_800_000_000_000;
const MIN = 60_000;
const task = (id: string, title: string, status: string, ago: number, extra: object = {}) =>
  ({
    id, workspaceId: 'ws', title, status, parentTaskId: null, sessionId: `s-${id}`, runId: null, failureReason: null,
    assignedAgentDisplayName: 'Scout', assignedAgentDefinitionId: 'def-s', assignedToWorkspaceAgentId: 'wa',
    updatedAt: BigInt(NOW - ago), completedAt: status === 'running' ? null : BigInt(NOW - ago), resultSummary: null, error: null, ...extra,
  }) as never;
/** What the grid hands the stage: its full scene, and no row shouting. */
const stageOf = (card: FleetCard, live: FleetLive) => ({
  card,
  live,
  scene: openRequests(card).length > 0 ? null : cardScene(card, live, NOW, { stage: true }),
  shouts: new Set<string>(),
});
const liveWith = (...tasks: { id: string }[]): FleetLive => ({ runs: {}, tasks: { ws: Object.fromEntries(tasks.map((t) => [t.id, t])) } }) as never;

const askOn = (sessionId: string, pendingId: string, question: string): AssistantEventEnvelope =>
  ({
    sessionId,
    runId: null,
    workspaceId: 'ws',
    timestamp: 1n,
    event: { type: 'ask_user_requested', payload: { pending_id: pendingId, question, options: null, multi_select: false } },
  }) as AssistantEventEnvelope;

describe('FleetSelectedCard', () => {
  it("answers a subtask's question on the stage", async () => {
    const state = applyAssistantEvent({ cards: { ws: { ...emptyCard('ws', 'Infra'), mainSessionId: 'main' } } }, askOn('task-session', 'q1', 'Which region?'));
    render(
      <MemoryRouter>
        <FleetSelectedCard {...stageOf(state.cards.ws!, NO_LIVE)} selected faces={{}} now={NOW} onSelect={() => {}} />
      </MemoryRouter>
    );
    const panel = screen.getByRole('dialog', { name: 'Agent question' });
    expect(panel).toHaveTextContent('Which region?');
    fireEvent.change(within(panel).getByRole('textbox'), { target: { value: 'eu' } });
    fireEvent.click(within(panel).getByRole('button', { name: 'Send answer' }));
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
        request: { pendingId: 'q1', answer: 'eu', selectedOptionIndex: null, selectedOptionIndexes: null },
      })
    );
  });

  it('stamps the workspace seen while it is selected, and again when it leaves', () => {
    const card = { ...emptyCard('ws', 'Infra'), mainSessionId: 'main' };
    const stage = (selected: boolean) => (
      <MemoryRouter>
        <FleetSelectedCard {...stageOf(card, NO_LIVE)} selected={selected} faces={{}} now={NOW} onSelect={() => {}} />
      </MemoryRouter>
    );
    const stamps = () => (mockInvoke.mock.calls as unknown[][]).filter(([cmd]) => cmd === 'workspace_mark_opened');
    mockInvoke.mockClear();
    const { rerender } = render(stage(false));
    expect(stamps()).toHaveLength(0);
    rerender(stage(true));
    expect(stamps()).toEqual([['workspace_mark_opened', { workspaceId: 'ws' }]]);
    rerender(stage(false));
    expect(stamps()).toHaveLength(2);
  });

  it('opens a task from the stage in its workspace', () => {
    const Opened = () => {
      const location = useLocation();
      return <p>{`${location.pathname} ${(location.state as { openTaskId: string }).openTaskId}`}</p>;
    };
    render(
      <MemoryRouter>
        <Routes>
          <Route path="/" element={<FleetSelectedCard {...stageOf({ ...emptyCard('ws', 'Infra'), mainSessionId: 'main' }, liveWith(task('task-9', 'Round 3 review', 'running', MIN)))} selected faces={{}} now={NOW} onSelect={() => {}} />} />
          <Route path="/workspace/:id" element={<Opened />} />
        </Routes>
      </MemoryRouter>
    );
    fireEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(screen.getByText('/workspace/ws task-9')).toBeInTheDocument();
  });

  it('shows everything, in the order it happened: the recent history, results in full, Main\'s message as markdown, then every running task', () => {
    const live = liveWith(
      ...['A', 'B', 'C', 'D'].map((n) => task(`r${n}`, `Running ${n}`, 'running', MIN)),
      task('f1', 'Fresh one', 'completed', MIN, { resultSummary: 'First sentence. Second sentence with **detail**.' }),
      ...Array.from({ length: 6 }, (_, i) => task(`old${i}`, `Old task ${i}`, 'completed', (60 + i) * MIN, { resultSummary: `Result of old ${i}. And more.` }))
    );
    const message = { id: 'm1', sessionId: 'main', role: 'assistant', content: [{ type: 'text', text: `Done. ${'Long words '.repeat(80)}\n\n- **bold** item` }], createdAt: 1n } as unknown as AssistantMessage;
    useAssistantStore.setState((state) => ({ sessions: { ...state.sessions, main: { messages: [message], toolCalls: [], runs: [], queuedMessageIds: [] } as never } }));
    const card = { ...emptyCard('ws', 'Infra'), mainSessionId: 'main', lastMessage: { id: 'm1', text: 'Done. (clipped)', at: NOW - MIN } };
    const { container } = render(
      <MemoryRouter>
        <FleetSelectedCard {...stageOf(card, live)} selected faces={{}} now={NOW} onSelect={() => {}} />
      </MemoryRouter>
    );
    for (const n of ['A', 'B', 'C', 'D']) expect(screen.getByRole('button', { name: `Running ${n}` })).toBeInTheDocument();
    expect(screen.getByText(/Second sentence with/)).toBeInTheDocument();
    expect(within(screen.getByRole('list', { name: 'Earlier' })).getAllByRole('listitem')).toHaveLength(6);
    expect(screen.getByText('Result of old 5. And more.')).toBeInTheDocument();
    // Results and the session's full message render as markdown, not the card's clipped copy.
    expect([...container.querySelectorAll('strong')].map((b) => b.textContent)).toEqual(['detail', 'bold']);
    expect(screen.queryByText(/clipped/)).not.toBeInTheDocument();
    // Read top to bottom, oldest first, as the chat is.
    const order = ['Old task 5', 'Old task 0', 'Fresh one', 'Long words', 'Running A'].map(
      (text) => screen.getAllByText(new RegExp(text))[0]!
    );
    for (let i = 1; i < order.length; i++) {
      expect(order[i - 1]!.compareDocumentPosition(order[i]!) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    }
  });
});
