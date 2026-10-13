import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';

const mockInvoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke: mockInvoke }));

// The transcript renders a real ChatMessageList; stub only its heavy leaves
// (markdown, charts) and the virtualizer, which windows by scroll geometry
// jsdom does not have. See ChatMessageList.test.tsx for the same mocks.
vi.mock('./common/VirtualizedList', () => ({
  default: <T,>({
    items,
    renderItem,
    itemKey,
    footer,
  }: {
    items: T[];
    renderItem: (item: T, index: number) => React.ReactNode;
    itemKey: (item: T) => string;
    footer?: React.ReactNode;
  }) => (
    <div data-testid="virtual-list">
      {items.map((item, index) => (
        <div key={itemKey(item)}>{renderItem(item, index)}</div>
      ))}
      {footer}
    </div>
  ),
}));
vi.mock('./Chat/MarkdownMessage', () => ({
  default: ({ content }: { content: string }) => <div data-testid="markdown">{content}</div>,
}));
vi.mock('./Chat/StreamingMarkdown', () => ({
  default: ({ content }: { content: string }) => <div data-testid="streaming">{content}</div>,
}));
vi.mock('./Chat/VegaChart', () => ({ default: () => <div data-testid="vega-chart" /> }));

import WorkspaceTaskTranscriptPanel from './WorkspaceTaskTranscriptPanel';
import AskUserPanel from './AskUserPanel/AskUserPanel';
import { useAssistantStore } from '../assistant';
import { useTaskStore } from '../workspace/taskStore';
import type { PendingAskUser } from '../assistant/sessionStore';
import type {
  AssistantMessage,
  SignalStatus,
  WorkspaceTaskResponse,
  WorkspaceTaskSendResponse,
} from '../generated/bindings';

const SESSION_ID = 'sess-task-1';

const task = (over: Partial<WorkspaceTaskResponse> = {}): WorkspaceTaskResponse => ({
  id: 'task-1',
  workspaceId: 'ws-1',
  createdByWorkspaceAgentId: 'agent-main',
  createdByDisplayName: 'Main',
  assignedToWorkspaceAgentId: 'agent-2',
  assignedAgentDefinitionId: 'def-2',
  assignedAgentDisplayName: 'Rust Architect',
  title: 'Port the reducer',
  instructions: 'do it',
  status: 'running',
  resultSummary: null,
  error: null,
  sessionId: SESSION_ID,
  runId: 'run-1',
  createdAt: 0n,
  updatedAt: 0n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  failureReason: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 0n,
  stopRequestedAt: null,
  answeredBy: null,
  ...over,
});

const message: AssistantMessage = {
  id: 'm1',
  sessionId: SESSION_ID,
  role: 'assistant',
  content: [{ type: 'text', text: 'porting the reducer' }],
  createdAt: 0n,
  providerMetadata: null,
  origin: { kind: 'user' },
};

// Only the fields the panel reads; `session` and the rest of SessionState are
// never touched by it, so the fixture stays narrow on purpose.
const seedSession = (isStreaming: boolean, pendingAskUser: PendingAskUser | null = null) => {
  useAssistantStore.setState({
    sessions: {
      [SESSION_ID]: {
        messages: [message],
        toolCalls: [],
        isStreaming,
        runStartedAt: isStreaming ? Date.now() - 8000 : null,
        pendingAskUser,
        queuedMessageIds: [],
        totalMessageCount: null,
      },
    },
  } as never);
};

const question: PendingAskUser = {
  pendingId: 'pending-1',
  question: 'Also review the generated bindings?',
  options: null,
  extraContext: null,
};

describe('WorkspaceTaskTranscriptPanel', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    useAssistantStore.setState({
      sessions: {},
      streamingText: {},
      deliveredQueuedMessageIds: {},
    } as never);
    useTaskStore.setState({ tasks: {}, seeded: {} });
  });

  it('never shows a running footer in the transcript, streaming or not', () => {
    // The task card that opens this panel and the header right above already
    // say who is running. A third marker inside the log is noise — and it
    // used to appear only when the store had seen the run start live
    // (`loadSessionData` hydrates `isStreaming: false`), so a transcript
    // opened mid-run showed nothing and read as a bug.
    for (const isStreaming of [true, false]) {
      seedSession(isStreaming);
      const { unmount } = render(
        <WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />
      );
      expect(screen.getByText('porting the reducer')).not.toBeNull();
      expect(screen.queryByRole('img', { name: 'Working' })).toBeNull();
      expect(screen.queryByText(/^\d+:\d\d$/)).toBeNull();
      unmount();
    }
  });

  it('puts the task state on the header face instead', () => {
    seedSession(true);
    const { rerender } = render(
      <WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />
    );
    const face = () => screen.getByRole('img', { name: 'Rust Architect' });
    expect(face().dataset.activity).toBe('running');

    rerender(
      <WorkspaceTaskTranscriptPanel
        workspaceId="ws-1"
        task={task({ status: 'failed', completedAt: 1n })}
        onClose={() => {}}
      />
    );
    expect(face().dataset.activity).toBe('attention');

    rerender(
      <WorkspaceTaskTranscriptPanel
        workspaceId="ws-1"
        task={task({ status: 'completed', completedAt: 1n })}
        onClose={() => {}}
      />
    );
    expect(face().dataset.activity).toBe('none');
  });

  it('pending ask_user for task session renders in task panel', () => {
    seedSession(true, question);
    render(<WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />);
    const dialog = screen.getByRole('dialog', { name: 'Agent question' });
    expect(dialog.textContent).toContain('Also review the generated bindings?');
  });

  it('submitting answer invokes assistant_submit_user_input with task session id', async () => {
    mockInvoke.mockResolvedValue(undefined);
    seedSession(true, question);
    render(<WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />);
    const dialog = screen.getByRole('dialog', { name: 'Agent question' });
    await userEvent.type(within(dialog).getByRole('textbox'), 'No, skip them{Enter}');
    // The pending id is minted for the task session's ask, so answering it
    // here resolves that session's run.
    expect(mockInvoke).toHaveBeenCalledWith('assistant_submit_user_input', {
      request: {
        pendingId: 'pending-1',
        answer: 'No, skip them',
        selectedOptionIndex: null,
        selectedOptionIndexes: null,
      },
    });
  });

  it('manager chat does not show task session question', () => {
    seedSession(true, question);
    render(<AskUserPanel sessionId="sess-manager" />);
    expect(screen.queryByRole('dialog', { name: 'Agent question' })).toBeNull();
  });

  it('task transcript assistant header shows assignee', () => {
    seedSession(false);
    render(<WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />);
    // The assistant row is signed by the agent the task was handed to — the
    // header's name and the row's name — never "Clai". With an empty roster
    // the agent reads as removed, from the task's own copy of its name.
    expect(screen.getAllByText('Rust Architect').length).toBeGreaterThan(0);
    expect(screen.getByText('(removed)')).toBeInTheDocument();
    expect(screen.queryByText('Clai')).toBeNull();
  });

  it('task session shows message from non-manager agent', () => {
    const instruction: AssistantMessage = {
      ...message,
      id: 'm0',
      role: 'user',
      content: [{ type: 'text', text: 'Port it, please' }],
      origin: {
        kind: 'agent',
        agent: { workspaceAgentId: 'agent-3', displayName: 'Planner', isDefault: false },
      },
    };
    useAssistantStore.setState({
      sessions: {
        [SESSION_ID]: {
          messages: [instruction, message],
          toolCalls: [],
          isStreaming: false,
          runStartedAt: null,
          pendingAskUser: null,
        },
      },
    } as never);
    render(<WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />);
    expect(screen.getByText('Planner')).toBeInTheDocument();
    expect(screen.getByText('Port it, please')).toBeInTheDocument();
    expect(screen.queryByText('Main agent')).toBeNull();
    expect(screen.queryByText('You')).toBeNull();
  });

  it('stop chip shows Stop requested then stopped resume hint', () => {
    seedSession(false);
    const { rerender } = render(
      <WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task()} onClose={() => {}} />
    );
    expect(screen.getByRole('button', { name: 'Stop task Port the reducer' })).toBeEnabled();
    expect(screen.queryByText(/Stop requested/)).toBeNull();

    rerender(
      <WorkspaceTaskTranscriptPanel
        workspaceId="ws-1"
        task={task({ status: 'stopping' })}
        onClose={() => {}}
      />
    );
    expect(screen.getByText('Stop requested · waiting for a safe point')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Stop task Port the reducer' })).toBeDisabled();

    rerender(
      <WorkspaceTaskTranscriptPanel
        workspaceId="ws-1"
        task={task({ status: 'stopped' })}
        onClose={() => {}}
      />
    );
    expect(screen.queryByText(/Stop requested/)).toBeNull();
    expect(screen.getByText('Stopped · send a message to resume')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Stop task Port the reducer' })).toBeNull();
  });

  describe('task composer', () => {
    const sent: AssistantMessage = {
      ...message,
      id: 'm-sent',
      role: 'user',
      content: [{ type: 'text', text: 'Also check the CHANGELOG' }],
    };
    const sendResponse = (
      status: SignalStatus,
      over: Partial<WorkspaceTaskResponse> = {}
    ): WorkspaceTaskSendResponse => ({
      task: task({ updatedAt: 5n, ...over }),
      message: sent,
      signal: { id: 'sig-1', status },
      run: null,
    });
    const composer = () => screen.getByRole('textbox', { name: /^Message Rust Architect/ });
    const renderPanel = (over: Partial<WorkspaceTaskResponse> = {}) =>
      render(
        <WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task(over)} onClose={() => {}} />
      );
    const send = async (text = 'Also check the CHANGELOG') =>
      userEvent.type(composer(), `${text}{Enter}`);

    it('placeholder per task status', () => {
      seedSession(false);
      const cases: [Partial<WorkspaceTaskResponse>, string][] = [
        [{ status: 'queued' }, 'Queue a message for Rust Architect…'],
        [{ status: 'running' }, 'Queue a message for Rust Architect…'],
        [{ status: 'failed', failureReason: 'blocked' }, 'Reply to Rust Architect…'],
        [{ status: 'failed', failureReason: 'error' }, 'Continue this task…'],
        [{ status: 'completed' }, 'Continue this task…'],
        [{ status: 'stopped' }, 'Continue this task…'],
        [{ status: 'stopping' }, 'Stopping…'],
      ];
      for (const [over, placeholder] of cases) {
        const { unmount } = renderPanel(over);
        expect(composer()).toHaveAttribute('placeholder', placeholder);
        if (over.status === 'stopping') expect(composer()).toBeDisabled();
        else expect(composer()).toBeEnabled();
        unmount();
      }
    });

    it('input_required placeholder offers to answer', () => {
      seedSession(false);
      renderPanel({ status: 'input_required' });
      expect(composer()).toHaveAttribute(
        'placeholder',
        'Answer Rust Architect (or use the question above)…'
      );
      expect(composer()).toBeEnabled();
    });

    it('send to running task marks queued chip', async () => {
      seedSession(true);
      mockInvoke.mockResolvedValue(sendResponse('pending'));
      renderPanel();
      await send();
      expect(mockInvoke).toHaveBeenCalledWith('workspace_send_task_message', {
        request: { workspaceId: 'ws-1', taskId: 'task-1', message: 'Also check the CHANGELOG' },
      });
      expect(await screen.findByText('Queued')).toBeInTheDocument();
      expect(composer()).toHaveValue('');
    });

    it('queued_messages_delivered clears task chip', async () => {
      seedSession(true);
      mockInvoke.mockResolvedValue(sendResponse('pending'));
      renderPanel();
      await send();
      expect(await screen.findByText('Queued')).toBeInTheDocument();
      act(() => {
        useAssistantStore.getState().markQueuedMessagesDelivered(SESSION_ID, ['m-sent']);
      });
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('consumed send shows no chip', async () => {
      seedSession(true);
      mockInvoke.mockResolvedValue(sendResponse('consumed'));
      renderPanel();
      await send();
      expect(await screen.findByText('Also check the CHANGELOG')).toBeInTheDocument();
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('composer send to input_required answers ask', async () => {
      seedSession(true, question);
      // The returned task may still read input_required; task_updated follows.
      mockInvoke.mockResolvedValue(sendResponse('consumed', { status: 'input_required' }));
      renderPanel({ status: 'input_required' });
      await send('No, skip them');
      expect(mockInvoke).toHaveBeenCalledWith('workspace_send_task_message', {
        request: { workspaceId: 'ws-1', taskId: 'task-1', message: 'No, skip them' },
      });
      expect(mockInvoke).not.toHaveBeenCalledWith('assistant_submit_user_input', expect.anything());
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('send losing answer race shows queued chip', async () => {
      // The parent answered first, so the human's send waits for the run.
      seedSession(true);
      mockInvoke.mockResolvedValue(
        sendResponse('pending', {
          status: 'running',
          answeredBy: {
            kind: 'agent',
            agent: { workspaceAgentId: 'agent-main', displayName: 'Main', isDefault: true },
          },
        })
      );
      renderPanel({ status: 'input_required' });
      await send();
      expect(await screen.findByText('Queued')).toBeInTheDocument();
    });

    it('send to stopped task upserts queued task', async () => {
      seedSession(false);
      mockInvoke.mockResolvedValue(sendResponse('claimed', { status: 'queued', cycleNo: 2n }));
      renderPanel({ status: 'stopped' });
      await send();
      expect(useTaskStore.getState().tasks['ws-1']?.['task-1']).toMatchObject({
        status: 'queued',
        cycleNo: 2n,
      });
    });

    it('send error keeps text and shows alert', async () => {
      seedSession(false);
      mockInvoke.mockRejectedValue('Message is too long');
      renderPanel({ status: 'completed' });
      await send('Try again');
      expect(await screen.findByRole('alert')).toHaveTextContent('Message is too long');
      expect(composer()).toHaveValue('Try again');
    });
  });
});
