import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
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
import type { PendingAskUser } from '../assistant/sessionStore';
import type { AssistantMessage, WorkspaceTaskResponse } from '../generated/bindings';

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
    useAssistantStore.setState({ sessions: {}, streamingText: {} } as never);
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
    await userEvent.type(screen.getByRole('textbox'), 'No, skip them{Enter}');
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
});
