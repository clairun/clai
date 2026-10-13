import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';

import type { WorkspaceTaskResponse } from '../generated/bindings';

type Handler = (event: { payload: unknown }) => void;
let handler: Handler | null = null;
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (_name: string, next: Handler) => {
    handler = next;
    return () => {};
  }),
}));

const { default: WorkspaceTaskNotifications } = await import('./WorkspaceTaskNotifications');

const task = (over: Partial<WorkspaceTaskResponse> = {}): WorkspaceTaskResponse => ({
  id: 'task-1',
  workspaceId: 'ws-1',
  createdByWorkspaceAgentId: 'wa-main',
  createdByDisplayName: 'Main',
  assignedToWorkspaceAgentId: 'wa-coder',
  assignedAgentDefinitionId: 'def-coder',
  assignedAgentDisplayName: 'Coder',
  title: 'Fix flaky test',
  instructions: 'fix it',
  status: 'running',
  resultSummary: null,
  error: null,
  sessionId: 'sess-task-1',
  runId: 'run-1',
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  failureReason: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 1n,
  ...over,
});

const emitTaskUpdated = (updated: WorkspaceTaskResponse) =>
  act(() =>
    handler?.({
      payload: {
        sessionId: updated.sessionId,
        runId: updated.runId,
        workspaceId: updated.workspaceId,
        timestamp: 0,
        event: { type: 'task_updated', payload: { task: updated } },
      },
    })
  );

const renderNotifications = async () => {
  render(
    <MemoryRouter>
      <WorkspaceTaskNotifications />
    </MemoryRouter>
  );
  await act(async () => {});
};

describe('WorkspaceTaskNotifications', () => {
  beforeEach(() => {
    handler = null;
  });

  it('task_updated with input_required raises one toast per cycle', async () => {
    await renderNotifications();

    emitTaskUpdated(task({ status: 'running' }));
    expect(screen.queryByText('Fix flaky test')).toBeNull();

    emitTaskUpdated(task({ status: 'input_required', updatedAt: 2n }));
    emitTaskUpdated(task({ status: 'input_required', updatedAt: 3n }));
    expect(screen.getAllByText('Needs an answer')).toHaveLength(1);

    // Dismissed, then the same question re-announced: still no second toast.
    act(() => screen.getByRole('button', { name: 'Dismiss notification' }).click());
    emitTaskUpdated(task({ status: 'input_required', updatedAt: 4n }));
    expect(screen.queryByText('Needs an answer')).toBeNull();

    // The next cycle's question is a new one.
    emitTaskUpdated(task({ status: 'input_required', cycleNo: 2n, updatedAt: 5n }));
    expect(screen.getAllByText('Needs an answer')).toHaveLength(1);
  });

  it('failed with failureReason blocked toasts as blocked', async () => {
    await renderNotifications();

    emitTaskUpdated(
      task({ status: 'failed', failureReason: 'blocked', error: 'Need repo access', completedAt: 2n })
    );

    expect(screen.getByText('Blocked')).toBeInTheDocument();
    expect(screen.queryByText('Failed')).toBeNull();
    expect(screen.getByText('Need repo access')).toBeInTheDocument();

    emitTaskUpdated(task({ id: 'task-2', title: 'Other', status: 'failed', failureReason: 'error' }));
    expect(screen.getByText('Failed')).toBeInTheDocument();
  });
});
