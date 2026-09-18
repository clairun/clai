import { describe, expect, it, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

import { useOpenTask } from './useOpenTask';
import type { WorkspaceTaskResponse } from '../generated/bindings';

const task = (id: string): WorkspaceTaskResponse => ({
  id,
  workspaceId: 'ws-1',
  createdByWorkspaceAgentId: null,
  createdByDisplayName: null,
  assignedToWorkspaceAgentId: 'wa-1',
  assignedAgentDefinitionId: 'def-1',
  assignedAgentDisplayName: 'Rust',
  title: `Task ${id}`,
  instructions: 'do it',
  status: 'completed',
  resultSummary: null,
  error: null,
  sessionId: `sess-${id}`,
  runId: null,
  createdAt: 1n,
  updatedAt: 1n,
  completedAt: null,
  attentionAcknowledgedAt: null,
  userResponse: null,
  userResponseAt: null,
});

const setup = (
  over: {
    loaded?: WorkspaceTaskResponse[];
    fetched?: WorkspaceTaskResponse | null;
    fetchTask?: ReturnType<typeof vi.fn>;
    tasksPanelOpen?: () => boolean;
  } = {}
) => {
  const showTasksPanel = vi.fn();
  const setViewingTask = vi.fn();
  const fetchTask = over.fetchTask ?? vi.fn(async () => over.fetched ?? null);
  const { result } = renderHook(() =>
    useOpenTask({
      workspaceId: 'ws-1',
      loadedTasks: () => over.loaded ?? [],
      showTasksPanel,
      isTasksPanelOpen: over.tasksPanelOpen ?? (() => true),
      setViewingTask,
      fetchTask: fetchTask as never,
    })
  );
  return { open: result.current, showTasksPanel, setViewingTask, fetchTask };
};

describe('useOpenTask', () => {
  it('opens a task already in the snapshot without asking the backend', () => {
    const known = task('t-1');
    const { open, showTasksPanel, setViewingTask, fetchTask } = setup({ loaded: [known] });

    act(() => open('t-1'));

    // The drawer has to come forward: its transcript only mounts while it is.
    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(setViewingTask).toHaveBeenCalledWith(known);
    expect(fetchTask).not.toHaveBeenCalled();
  });

  it('fetches a task older than the snapshot window, by workspace then id', async () => {
    const old = task('t-old');
    const { open, showTasksPanel, setViewingTask, fetchTask } = setup({ fetched: old });

    act(() => open('t-old'));

    // Opens on nothing first, so the click is never silently ignored.
    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(setViewingTask).toHaveBeenNthCalledWith(1, null);
    // Argument order matters: both are strings, and a swapped pair would read
    // another workspace's database without erroring.
    expect(fetchTask).toHaveBeenCalledWith('ws-1', 't-old');
    await waitFor(() => expect(setViewingTask).toHaveBeenNthCalledWith(2, old));
  });

  it('leaves the drawer on its list when the task is gone from the database', async () => {
    const { open, setViewingTask, fetchTask } = setup({ fetched: null });

    act(() => open('t-deleted'));
    await waitFor(() => expect(fetchTask).toHaveBeenCalled());

    expect(setViewingTask).toHaveBeenCalledTimes(1);
    expect(setViewingTask).toHaveBeenCalledWith(null);
  });

  it('drops a fetch the reader has already clicked past', async () => {
    const first = task('t-first');
    const second = task('t-second');
    const fetchTask = vi.fn(async (_workspaceId: string, taskId: string) => {
      // The first click's fetch is the slower one, and lands last.
      if (taskId === 't-first') {
        await new Promise((resolve) => setTimeout(resolve, 20));
        return first;
      }
      return second;
    });
    const { open, setViewingTask } = setup({ fetchTask: fetchTask as never });

    act(() => open('t-first'));
    act(() => open('t-second'));
    await waitFor(() => expect(setViewingTask).toHaveBeenCalledWith(second));
    await new Promise((resolve) => setTimeout(resolve, 40));

    expect(setViewingTask).not.toHaveBeenCalledWith(first);
  });

  it('does not reopen a transcript the reader has since dismissed', async () => {
    const old = task('t-old');
    let panelOpen = true;
    const fetchTask = vi.fn(async () => {
      // The reader closes the drawer while the fetch is in flight.
      panelOpen = false;
      return old;
    });
    const { open, setViewingTask } = setup({
      fetchTask: fetchTask as never,
      tasksPanelOpen: () => panelOpen,
    });

    act(() => open('t-old'));
    await waitFor(() => expect(fetchTask).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(setViewingTask).toHaveBeenCalledTimes(1);
    expect(setViewingTask).toHaveBeenCalledWith(null);
  });

  it('survives a failing fetch without losing the drawer', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const fetchTask = vi.fn(async () => {
      throw new Error('ipc is down');
    });
    const { open, showTasksPanel, setViewingTask } = setup({ fetchTask: fetchTask as never });

    act(() => open('t-old'));
    await waitFor(() => expect(error).toHaveBeenCalled());

    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(setViewingTask).toHaveBeenCalledTimes(1);
    error.mockRestore();
  });
});
