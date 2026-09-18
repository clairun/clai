import { useCallback, useRef } from 'react';
import type { WorkspaceTaskResponse } from '../generated/bindings';
import { getWorkspaceTask } from '../workspace/client';

export interface UseOpenTaskOptions {
  workspaceId: string;
  /** The tasks already in hand — the snapshot's newest 50 — read at click time. */
  loadedTasks: () => readonly WorkspaceTaskResponse[];
  /** Bring the tasks drawer forward. Its transcript only mounts while it is. */
  showTasksPanel: () => void;
  /** Whether the reader is still in the tasks drawer, read when a fetch lands. */
  isTasksPanelOpen: () => boolean;
  /** Open a transcript, or clear it with null. */
  setViewingTask: (task: WorkspaceTaskResponse | null) => void;
  /** Seam for tests; production always fetches through the workspace client. */
  fetchTask?: typeof getWorkspaceTask;
}

/**
 * Open the task a chat card names.
 *
 * Two things make this more than a state assignment. The snapshot carries only
 * the 50 most recently touched tasks, so a card pointing further back has to
 * fetch its own row — and a fetch is slow enough for the reader to have moved
 * on. So the drawer opens at once on what is already known, and the answer is
 * dropped if a newer click superseded it or the reader left the drawer; a
 * transcript that reappears after being dismissed is worse than none.
 */
export const useOpenTask = ({
  workspaceId,
  loadedTasks,
  showTasksPanel,
  isTasksPanelOpen,
  setViewingTask,
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
      setViewingTask(known);
      if (known) return;
      fetchTask(workspaceId, taskId)
        .then((task) => {
          if (requestId !== requestRef.current) return;
          if (!isTasksPanelOpen()) return;
          // Null means the task is gone from the database, not merely old; the
          // drawer's list is then the honest answer to the click.
          if (task) setViewingTask(task);
        })
        .catch((err) => {
          console.error('[Workspace] Failed to load the task a chat card names:', err);
        });
    },
    [fetchTask, isTasksPanelOpen, loadedTasks, setViewingTask, showTasksPanel, workspaceId]
  );
};
