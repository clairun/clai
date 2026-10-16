import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
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
import { useAssistantStore } from '../assistant';
import { useTaskStore } from '../workspace/taskStore';
import type {
  AssistantMessage,
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
  failureReason: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 0n,
  stopRequestedAt: null,
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
const seedSession = (isStreaming: boolean) => {
  useAssistantStore.setState({
    sessions: {
      [SESSION_ID]: {
        messages: [message],
        toolCalls: [],
        isStreaming,
        runStartedAt: isStreaming ? Date.now() - 8000 : null,
        pendingAskUser: null,
        queuedMessageIds: [],
        totalMessageCount: null,
      },
    },
  } as never);
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
    expect(face().dataset.activity).toBe('none');

    rerender(
      <WorkspaceTaskTranscriptPanel
        workspaceId="ws-1"
        task={task({ status: 'completed', completedAt: 1n })}
        onClose={() => {}}
      />
    );
    expect(face().dataset.activity).toBe('none');
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

  it('stop chip shows Stop requested, then leaves the resume hint to the composer', () => {
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
    expect(screen.queryByText(/resume/)).toBeNull();
    expect(screen.getByPlaceholderText('Message Rust Architect…')).toBeEnabled();
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
      queued: boolean,
      over: Partial<WorkspaceTaskResponse> = {}
    ): WorkspaceTaskSendResponse => ({
      task: task({ updatedAt: 5n, ...over }),
      message: sent,
      queued,
      run: null,
    });
    const composer = () => screen.getByRole('textbox', { name: /^Message Rust Architect/ });
    const renderPanel = (over: Partial<WorkspaceTaskResponse> = {}) =>
      render(
        <WorkspaceTaskTranscriptPanel workspaceId="ws-1" task={task(over)} onClose={() => {}} />
      );
    const send = async (text = 'Also check the CHANGELOG') =>
      userEvent.type(composer(), `${text}{Enter}`);

    it('composer is enabled for every task status with a session', () => {
      seedSession(false);
      const states: Partial<WorkspaceTaskResponse>[] = [
        { status: 'queued' },
        { status: 'running', runId: null },
        { status: 'stopping' },
        { status: 'stopped' },
        { status: 'completed', runId: null },
        { status: 'failed' },
      ];
      for (const state of states) {
        const { unmount } = renderPanel(state);
        expect(composer()).toHaveAttribute('placeholder', 'Message Rust Architect…');
        expect(composer()).toBeEnabled();
        unmount();
      }
    });

    it('composer is disabled without a session', () => {
      renderPanel({ status: 'queued', sessionId: null });
      expect(composer()).toBeDisabled();
    });

    it('send to running task marks queued chip', async () => {
      seedSession(true);
      mockInvoke.mockResolvedValue(sendResponse(true));
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
      mockInvoke.mockResolvedValue(sendResponse(true));
      renderPanel();
      await send();
      expect(await screen.findByText('Queued')).toBeInTheDocument();
      act(() => {
        useAssistantStore.getState().markQueuedMessagesDelivered(SESSION_ID, ['m-sent']);
      });
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('unqueued send shows no chip', async () => {
      seedSession(true);
      mockInvoke.mockResolvedValue(sendResponse(false));
      renderPanel();
      await send();
      expect(await screen.findByText('Also check the CHANGELOG')).toBeInTheDocument();
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('send from a completed task starts a new cycle', async () => {
      seedSession(false);
      mockInvoke.mockResolvedValue(sendResponse(false, { status: 'running', cycleNo: 1n }));
      renderPanel({ status: 'completed' });
      await send();
      expect(mockInvoke).toHaveBeenCalledWith('workspace_send_task_message', {
        request: { workspaceId: 'ws-1', taskId: 'task-1', message: 'Also check the CHANGELOG' },
      });
      expect(await screen.findByText('Also check the CHANGELOG')).toBeInTheDocument();
      expect(screen.queryByText('Queued')).toBeNull();
    });

    it('send error keeps text and shows alert', async () => {
      seedSession(false);
      mockInvoke.mockRejectedValue('Message is too long');
      renderPanel({ status: 'running' });
      await send('Try again');
      expect(await screen.findByRole('alert')).toHaveTextContent('Message is too long');
      expect(composer()).toHaveValue('Try again');
    });
  });
});
