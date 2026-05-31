import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

// vi.mock is hoisted; vi.hoisted lets us share the mock fn with assertions.
const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

import AskUserPanel from './AskUserPanel';
import useAssistantStore from '../../assistant/sessionStore';
import type { PendingAskUser } from '../../assistant/sessionStore';
import type { AssistantSession } from '../../generated/bindings';

const SESSION: AssistantSession = {
  id: 'sess-1',
  kind: 'interactive',
  title: 'Test',
  context: {
    spaceId: null,
    roomId: null,
    workspaceId: 'ws-1',
    toolScopes: [],
    mcpServerIds: [],
    execution: {},
    netdataConversationId: null,
    cliSessionId: null,
    cliSessionProvider: null,
    automationId: null,
    agentWorkspaceId: null,
    automationName: null,
    interAgentCall: null,
    workspaceAgents: [],
  },
  createdAt: 0n,
  updatedAt: 0n,
};

const askUserRequest = (overrides: Partial<PendingAskUser> = {}): PendingAskUser => ({
  pendingId: 'pending-abc',
  question: 'Which option do you want?',
  options: [
    { label: 'Option A', description: null },
    { label: 'Option B', description: 'a longer description' },
  ],
  extraContext: null,
  ...overrides,
});

const mountWithPending = (pending: PendingAskUser | null) => {
  useAssistantStore.setState({
    sessions: {
      [SESSION.id]: {
        session: SESSION,
        messages: [],
        runs: [],
        toolCalls: [],
        streamingTextByMessageId: {},
        isStreaming: false,
        pendingAskUser: pending,
      },
    },
    activeSessionByTab: {},
  });
  return render(<AskUserPanel sessionId={SESSION.id} />);
};

beforeEach(() => {
  mockInvoke.mockReset();
  useAssistantStore.setState({ sessions: {}, activeSessionByTab: {} });
});

describe('AskUserPanel — rendering', () => {
  it('renders nothing when there is no pending request', () => {
    const { container } = mountWithPending(null);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders the question, all options, and an Other choice', () => {
    mountWithPending(askUserRequest());
    expect(screen.getByText('Which option do you want?')).toBeInTheDocument();
    expect(screen.getByText('Option A')).toBeInTheDocument();
    expect(screen.getByText('Option B')).toBeInTheDocument();
    expect(screen.getByText('a longer description')).toBeInTheDocument();
    expect(screen.getByText('Other')).toBeInTheDocument();
  });

  it('renders the extra context block when provided', () => {
    mountWithPending(askUserRequest({ extraContext: 'Background info.' }));
    expect(screen.getByText('Background info.')).toBeInTheDocument();
  });

  it('falls back to a plain textarea when no options are provided', () => {
    mountWithPending(askUserRequest({ options: null }));
    expect(screen.queryByText('Option A')).toBeNull();
    expect(screen.getByPlaceholderText('Type your answer…')).toBeInTheDocument();
  });
});

describe('AskUserPanel — submission', () => {
  it('submits the selected option label + index via assistant_submit_user_input', async () => {
    const user = userEvent.setup();
    mountWithPending(askUserRequest());

    // Send is disabled until a choice is made.
    const send = screen.getByRole('button', { name: /send answer/i });
    expect(send).toBeDisabled();

    await user.click(screen.getByLabelText(/Option B/));
    expect(send).toBeEnabled();

    mockInvoke.mockResolvedValueOnce(undefined);
    await user.click(send);

    expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
      request: {
        pendingId: 'pending-abc',
        answer: 'Option B',
        selectedOptionIndex: 1,
      },
    });
  });

  it('submits free-text via the Other branch', async () => {
    const user = userEvent.setup();
    mountWithPending(askUserRequest());

    await user.click(screen.getByLabelText(/Other/));
    const textarea = screen.getByPlaceholderText('Type your answer…');
    await user.type(textarea, 'my custom answer');

    mockInvoke.mockResolvedValueOnce(undefined);
    await user.click(screen.getByRole('button', { name: /send answer/i }));

    expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
      request: {
        pendingId: 'pending-abc',
        answer: 'my custom answer',
        selectedOptionIndex: null,
      },
    });
  });

  it('keeps the panel mounted when the backend rejects the submit', async () => {
    // Regression-style: the panel intentionally does NOT optimistically
    // clear on submit. Only the ask_user_resolved event clears it.
    const user = userEvent.setup();
    mountWithPending(askUserRequest({ options: null }));

    await user.type(screen.getByPlaceholderText('Type your answer…'), 'hi');
    mockInvoke.mockRejectedValueOnce(new Error('run already ended'));
    await user.click(screen.getByRole('button', { name: /send answer/i }));

    // Question still visible after the rejected submit; error surfaces.
    expect(screen.getByText('Which option do you want?')).toBeInTheDocument();
    expect(screen.getByText(/run already ended/)).toBeInTheDocument();
  });
});

describe('AskUserPanel — snapshot poll race regression', () => {
  // This is the failing case for the May 2026 bug: while a question
  // is open, a snapshot poll calls loadSessionData which used to wipe
  // pendingAskUser. The fix preserves the field; this test catches a
  // regression even if loadSessionData's preserve list is the broken
  // hop in the future.
  it('survives a snapshot refresh while a question is open', () => {
    mountWithPending(askUserRequest());
    expect(screen.getByText('Which option do you want?')).toBeInTheDocument();

    // Simulate the workspace poll firing while the question is open.
    const store = useAssistantStore.getState();
    store.loadSessionData(SESSION.id, SESSION, [], [], []);

    expect(screen.getByText('Which option do you want?')).toBeInTheDocument();
  });
});
