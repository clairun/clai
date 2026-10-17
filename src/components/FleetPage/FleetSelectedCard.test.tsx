import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter, Route, Routes, useLocation } from 'react-router';
import { useAssistantStore } from '../../assistant';
import { applyAssistantEvent, emptyCard, type FleetLive } from '../../fleet/fleetModel';
import type { AssistantEventEnvelope, AssistantMessage, ToolInvocation } from '../../generated/bindings';
import FleetSelectedCard from './FleetSelectedCard';

const mockInvoke = vi.hoisted(() => vi.fn(async () => undefined));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('./useFleetChat', () => ({ useFleetChat: () => ({ roster: [], error: null, loading: false }) }));

const NO_LIVE: FleetLive = { runs: {}, tasks: {} };

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
        <FleetSelectedCard card={state.cards.ws!} live={NO_LIVE} selected faces={{}} onSelect={() => {}} />
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
        <FleetSelectedCard card={card} live={NO_LIVE} selected={selected} faces={{}} onSelect={() => {}} />
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

  it('opens a task card in its workspace', () => {
    const message = { id: 'm1', sessionId: 'main', role: 'assistant', content: [{ type: 'tool_use', tool_call_id: 'tk', tool_name: 'workspace_assignTask', arguments: {} }], createdAt: 1n } as unknown as AssistantMessage;
    const toolCall = {
      id: 'tk', runId: 'r', sessionId: 'main', toolName: 'workspace_assignTask', params: {}, status: 'completed', hasFullResult: true, hasFullInput: false, error: null, startedAt: 0n, completedAt: 1n,
      result: { ok: true, task: { id: 'task-9', workspaceId: 'ws', assignedToWorkspaceAgentId: 'wa', assignedAgentDefinitionId: 'def', title: 'Round 3 review', instructions: 'Review.', status: 'queued', resultSummary: null, error: null } },
    } as unknown as ToolInvocation;
    useAssistantStore.setState((state) => ({
      sessions: { ...state.sessions, main: { messages: [message], toolCalls: [toolCall], runs: [], queuedMessageIds: [] } as never },
    }));
    const Opened = () => {
      const location = useLocation();
      return <p>{`${location.pathname} ${(location.state as { openTaskId: string }).openTaskId}`}</p>;
    };
    render(
      <MemoryRouter>
        <Routes>
          <Route path="/" element={<FleetSelectedCard card={{ ...emptyCard('ws', 'Infra'), mainSessionId: 'main' }} live={NO_LIVE} selected faces={{}} onSelect={() => {}} />} />
          <Route path="/workspace/:id" element={<Opened />} />
        </Routes>
      </MemoryRouter>
    );
    fireEvent.click(screen.getByRole('button', { name: /Round 3 review/ }));
    expect(screen.getByText('/workspace/ws task-9')).toBeInTheDocument();
  });
});
