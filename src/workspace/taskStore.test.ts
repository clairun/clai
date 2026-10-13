import { beforeEach, describe, expect, it } from 'vitest';
import { useTaskStore } from './taskStore';
import type { WorkspaceTaskResponse } from '../generated/bindings';

const task = (over: Partial<WorkspaceTaskResponse> = {}): WorkspaceTaskResponse => ({
  id: 'task-1',
  workspaceId: 'ws-1',
  createdByWorkspaceAgentId: null,
  createdByDisplayName: null,
  assignedToWorkspaceAgentId: 'wa-1',
  assignedAgentDefinitionId: 'def-1',
  assignedAgentDisplayName: 'Coder',
  title: 'Fix it',
  instructions: 'fix',
  status: 'running',
  failureReason: null,
  resultSummary: null,
  error: null,
  sessionId: null,
  runId: null,
  parentSessionId: null,
  parentRunId: null,
  parentTaskId: null,
  depth: 0n,
  cycleNo: 1n,
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  ...over,
});

const stored = () => useTaskStore.getState().tasks['ws-1']?.['task-1'];

describe('taskStore', () => {
  beforeEach(() => useTaskStore.setState({ tasks: {}, seeded: {} }));

  it('upsert ignores older updatedAt', () => {
    const { upsertTask } = useTaskStore.getState();
    upsertTask(task({ status: 'completed', updatedAt: 5n }));
    upsertTask(task({ status: 'running', updatedAt: 4n }));
    expect(stored()?.status).toBe('completed');
    upsertTask(task({ status: 'input_required', updatedAt: 6n }));
    expect(stored()?.status).toBe('input_required');
  });

  it('seed does not overwrite newer event copy', () => {
    const { upsertTask, seedTasks } = useTaskStore.getState();
    upsertTask(task({ status: 'completed', updatedAt: 5n }));
    seedTasks('ws-1', [task({ status: 'running', updatedAt: 3n })]);
    expect(stored()?.status).toBe('completed');
    expect(useTaskStore.getState().seeded['ws-1']).toBe(true);
    seedTasks('ws-1', [task({ status: 'failed', updatedAt: 7n })]);
    expect(stored()?.status).toBe('failed');
  });
});
