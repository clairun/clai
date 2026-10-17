import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { applyAssistantEvent, emptyCard, type FleetLive } from '../../fleet/fleetModel';
import type { AssistantEventEnvelope } from '../../generated/bindings';
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
});
