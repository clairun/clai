import { describe, expect, it, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';

import { useOpenTask, type TaskView } from './useOpenTask';
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
    view?: TaskView;
  } = {}
) => {
  const showTasksPanel = vi.fn();
  // A stand-in for the page's view state, so the hook's decisions are made
  // against something that can change under it, as it does in the app.
  const view: TaskView = over.view ?? { activePanel: 'tasks', viewingTask: null };
  const applied: (WorkspaceTaskResponse | null)[] = [];
  const updateViewingTask = vi.fn(
    (update: (current: TaskView) => { viewingTask: WorkspaceTaskResponse | null } | null) => {
      const patch = update(view);
      if (!patch) return;
      view.viewingTask = patch.viewingTask;
      applied.push(patch.viewingTask);
    }
  );
  const fetchTask = over.fetchTask ?? vi.fn(async () => over.fetched ?? null);
  const { result } = renderHook(() =>
    useOpenTask({
      workspaceId: 'ws-1',
      loadedTasks: () => over.loaded ?? [],
      showTasksPanel,
      updateViewingTask,
      fetchTask: fetchTask as never,
    })
  );
  return { open: result.current, showTasksPanel, updateViewingTask, fetchTask, view, applied };
};

describe('useOpenTask', () => {
  it('opens a task already in the snapshot without asking the backend', () => {
    const known = task('t-1');
    const { open, showTasksPanel, applied, fetchTask } = setup({ loaded: [known] });

    act(() => open('t-1'));

    // The drawer has to come forward: its transcript only mounts while it is.
    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(applied).toEqual([known]);
    expect(fetchTask).not.toHaveBeenCalled();
  });

  it('fetches a task older than the snapshot window, by workspace then id', async () => {
    const old = task('t-old');
    const { open, showTasksPanel, applied, fetchTask } = setup({ fetched: old });

    act(() => open('t-old'));

    // Opens on nothing first, so the click is never silently ignored.
    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(applied).toEqual([null]);
    // Argument order matters: both are strings, and a swapped pair would read
    // another workspace's database without erroring.
    expect(fetchTask).toHaveBeenCalledWith('ws-1', 't-old');
    await waitFor(() => expect(applied).toEqual([null, old]));
  });

  it('leaves the drawer on its list when the task is gone from the database', async () => {
    const { open, applied, fetchTask } = setup({ fetched: null });

    act(() => open('t-deleted'));
    await waitFor(() => expect(fetchTask).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(applied).toEqual([null]);
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
    const { open, applied } = setup({ fetchTask: fetchTask as never });

    act(() => open('t-first'));
    act(() => open('t-second'));
    await waitFor(() => expect(applied).toContain(second));
    await new Promise((resolve) => setTimeout(resolve, 40));

    expect(applied).not.toContain(first);
  });

  it('does not reopen a transcript the reader has since dismissed', async () => {
    const old = task('t-old');
    const view: TaskView = { activePanel: 'tasks', viewingTask: null };
    const fetchTask = vi.fn(async () => {
      // The reader closes the drawer while the fetch is in flight.
      view.activePanel = null;
      return old;
    });
    const { open, applied } = setup({ fetchTask: fetchTask as never, view });

    act(() => open('t-old'));
    await waitFor(() => expect(fetchTask).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(applied).toEqual([null]);
  });

  it('never replaces a transcript the reader opened while waiting', async () => {
    // The drawer's own list can open a task too, and it does not go through
    // this hook: a late card fetch must not swap out what they chose.
    const old = task('t-old');
    const chosen = task('t-chosen');
    const view: TaskView = { activePanel: 'tasks', viewingTask: null };
    const fetchTask = vi.fn(async () => {
      view.viewingTask = chosen;
      return old;
    });
    const { open, applied } = setup({ fetchTask: fetchTask as never, view });

    act(() => open('t-old'));
    await waitFor(() => expect(fetchTask).toHaveBeenCalled());
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(applied).toEqual([null]);
    expect(view.viewingTask).toBe(chosen);
  });

  it('survives a failing fetch without losing the drawer', async () => {
    const error = vi.spyOn(console, 'error').mockImplementation(() => {});
    const fetchTask = vi.fn(async () => {
      throw new Error('ipc is down');
    });
    const { open, showTasksPanel, applied } = setup({ fetchTask: fetchTask as never });

    act(() => open('t-old'));
    await waitFor(() => expect(error).toHaveBeenCalled());

    expect(showTasksPanel).toHaveBeenCalledTimes(1);
    expect(applied).toEqual([null]);
    error.mockRestore();
  });
});
