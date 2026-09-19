import { useCallback, useRef } from 'react';
import type { WorkspaceTaskResponse } from '../generated/bindings';
import { getWorkspaceTask } from '../workspace/client';

/** What the open-a-task rule needs to see before it may change anything. */
export interface TaskView {
  activePanel: string | null;
  viewingTask: WorkspaceTaskResponse | null;
}

export interface UseOpenTaskOptions {
  workspaceId: string;
  /** The tasks already in hand — the newest 50 the details carry — read at click time. */
  loadedTasks: () => readonly WorkspaceTaskResponse[];
  /** Bring the tasks drawer forward. Its transcript only mounts while it is. */
  showTasksPanel: () => void;
  /**
   * Change the open transcript, deciding from the view as it is at that
   * instant. Returning null leaves it untouched. It must read and write the
   * same workspace's view, so a late answer cannot land in another one.
   */
  updateViewingTask: (
    update: (current: TaskView) => { viewingTask: WorkspaceTaskResponse | null } | null
  ) => void;
  /** Seam for tests; production always fetches through the workspace client. */
  fetchTask?: typeof getWorkspaceTask;
}

/**
 * Open the task a chat card names.
 *
 * Two things make this more than a state assignment. The details carry only
 * the 50 most recently touched tasks, so a card pointing further back has to
 * fetch its own row — and a fetch is slow enough for the reader to have moved
 * on. So the drawer opens at once on what is already known, and the answer is
 * only applied if the reader is still waiting for it: still in the tasks
 * drawer, still on no transcript, and not already past this click. A
 * transcript that replaces the one someone chose meanwhile is worse than none.
 */
export const useOpenTask = ({
  workspaceId,
  loadedTasks,
  showTasksPanel,
  updateViewingTask,
  fetchTask = getWorkspaceTask,
}: UseOpenTaskOptions) => {
  // Which click is the current one; a fetch that is not it has been overtaken.
  const requestRef = useRef(0);

  return useCallback(
    (taskId: string) => {
      const requestId = ++requestRef.current;
      const known = loadedTasks().find((entry) => entry.id === taskId) ?? null;
      // The click must never look ignored, so the drawer opens before the
      // fetch, showing the list while the row is on its way.
      showTasksPanel();
      updateViewingTask(() => ({ viewingTask: known }));
      if (known) return;
      fetchTask(workspaceId, taskId)
        .then((task) => {
          if (requestId !== requestRef.current) return;
          // Null means the task is gone from the database, not merely old; the
          // drawer's list is then the honest answer to the click.
          if (!task) return;
          updateViewingTask((current) =>
            current.activePanel === 'tasks' && current.viewingTask === null
              ? { viewingTask: task }
              : null
          );
        })
        .catch((err) => {
          console.error('[Workspace] Failed to load the task a chat card names:', err);
        });
    },
    [fetchTask, loadedTasks, showTasksPanel, updateViewingTask, workspaceId]
  );
};
