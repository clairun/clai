import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';

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
  userResponse: null,
  userResponseAt: null,
  ...over,
});

const message: AssistantMessage = {
  id: 'm1',
  sessionId: SESSION_ID,
  role: 'assistant',
  content: [{ type: 'text', text: 'porting the reducer' }],
  createdAt: 0n,
  providerMetadata: null,
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
      },
    },
  } as never);
};

describe('WorkspaceTaskTranscriptPanel', () => {
  beforeEach(() => {
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
        <WorkspaceTaskTranscriptPanel task={task()} onClose={() => {}} />
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
      <WorkspaceTaskTranscriptPanel task={task()} onClose={() => {}} />
    );
    const face = () => screen.getByRole('img', { name: 'Rust Architect' });
    expect(face().dataset.activity).toBe('running');

    rerender(
      <WorkspaceTaskTranscriptPanel
        task={task({ status: 'failed', completedAt: 1n })}
        onClose={() => {}}
      />
    );
    expect(face().dataset.activity).toBe('attention');

    rerender(
      <WorkspaceTaskTranscriptPanel
        task={task({ status: 'completed', completedAt: 1n })}
        onClose={() => {}}
      />
    );
    expect(face().dataset.activity).toBe('none');
  });
});
