import { beforeEach, describe, expect, it } from 'vitest';
import { act, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import type { WorkspaceTaskResponse } from '../generated/bindings';
import WorkspaceTaskNotifications from './WorkspaceTaskNotifications';
import { useTaskStore } from '../workspace/taskStore';

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
  stopRequestedAt: null,
  ...over,
});

// What `useAssistantEvents` does with a `task_updated` event.
const emitTaskUpdated = (updated: WorkspaceTaskResponse) =>
  act(() => useTaskStore.getState().upsertTask(updated));

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
    useTaskStore.setState({ tasks: {}, seeded: {} });
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

  it('opening an old failed task of a loaded workspace does not toast', async () => {
    await renderNotifications();

    act(() => useTaskStore.getState().seedTasks('ws-1', [task({ id: 'task-0' })]));
    // What opening a task outside the details window does: fetch, then upsert.
    emitTaskUpdated(task({ status: 'failed', error: 'old failure', updatedAt: 2n }));
    expect(screen.queryByText('old failure')).toBeNull();
  });

  it('a task created failed after the workspace loaded toasts on its first poll', async () => {
    await renderNotifications();

    act(() => useTaskStore.getState().seedTasks('ws-1', [task({ id: 'task-0' })]));
    // An assignment with no usable connection is stored failed and emits nothing.
    const createdAt = BigInt(Date.now() + 1000);
    act(() =>
      useTaskStore
        .getState()
        .seedTasks('ws-1', [
          task({ id: 'task-0' }),
          task({ status: 'failed', error: 'no connection', createdAt, updatedAt: createdAt }),
        ])
    );
    expect(screen.getByText('no connection')).toBeInTheDocument();
  });

  it('an acknowledged failure that fails again next cycle toasts again', async () => {
    await renderNotifications();

    emitTaskUpdated(task());
    emitTaskUpdated(task({ status: 'failed', error: 'first', updatedAt: 2n }));
    act(() => screen.getByRole('button', { name: 'Dismiss notification' }).click());
    emitTaskUpdated(task({ status: 'failed', error: 'first', attentionAcknowledgedAt: 3n, updatedAt: 3n }));
    emitTaskUpdated(task({ status: 'queued', cycleNo: 2n, attentionAcknowledgedAt: 3n, updatedAt: 4n }));
    // The backend clears the acknowledgement when the task needs attention again.
    emitTaskUpdated(task({ status: 'failed', error: 'second', cycleNo: 2n, updatedAt: 5n }));
    expect(screen.getByText('second')).toBeInTheDocument();
  });

  it('names the task in the Open task button', async () => {
    await renderNotifications();

    emitTaskUpdated(task());
    emitTaskUpdated(task({ status: 'failed', updatedAt: 2n }));
    expect(screen.getByRole('button', { name: 'Open task Fix flaky test' })).toBeInTheDocument();
  });
});
