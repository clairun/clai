/**
 * Workspace tasks as last seen, from three sources: each workspace-details
 * poll, `task_updated` events, and single-task fetches. A copy never
 * replaces a newer one, so a slow poll cannot roll back an event.
 */
import { create } from 'zustand';
import type { WorkspaceTaskResponse } from '../generated/bindings';

type TaskMap = Record<string, WorkspaceTaskResponse>;

interface TaskStoreState {
  tasks: Record<string, TaskMap>;
  /** When (ms) the workspace's first details load came in. Restarts emit no events, so it is the baseline. */
  seeded: Record<string, number>;
  seedTasks: (workspaceId: string, tasks: readonly WorkspaceTaskResponse[]) => void;
  upsertTask: (task: WorkspaceTaskResponse) => void;
}

const isOlder = (task: WorkspaceTaskResponse, current: WorkspaceTaskResponse | undefined) =>
  !!current && task.updatedAt < current.updatedAt;

/** `current` itself when nothing in `tasks` differs, so an idle poll re-renders nothing. */
const merge = (current: TaskMap | undefined, tasks: readonly WorkspaceTaskResponse[]): TaskMap => {
  let next: TaskMap | null = null;
  for (const task of tasks) {
    const existing = current?.[task.id];
    if (isOlder(task, existing) || task.updatedAt === existing?.updatedAt) continue;
    next ??= { ...current };
    next[task.id] = task;
  }
  return next ?? current ?? {};
};

export const useTaskStore = create<TaskStoreState>()((set) => ({
  tasks: {},
  seeded: {},
  seedTasks: (workspaceId, tasks) =>
    set((state) => {
      const merged = merge(state.tasks[workspaceId], tasks);
      return {
        tasks:
          merged === state.tasks[workspaceId]
            ? state.tasks
            : { ...state.tasks, [workspaceId]: merged },
        seeded: state.seeded[workspaceId]
          ? state.seeded
          : { ...state.seeded, [workspaceId]: Date.now() },
      };
    }),
  upsertTask: (task) =>
    set((state) => {
      const current = state.tasks[task.workspaceId];
      if (isOlder(task, current?.[task.id])) return state;
      return { tasks: { ...state.tasks, [task.workspaceId]: { ...current, [task.id]: task } } };
    }),
}));

export const useTask = (
  workspaceId: string,
  taskId: string | null | undefined
): WorkspaceTaskResponse | null =>
  useTaskStore((state) => (taskId ? state.tasks[workspaceId]?.[taskId] : undefined) ?? null);

export const useWorkspaceTasks = (workspaceId: string): TaskMap | undefined =>
  useTaskStore((state) => state.tasks[workspaceId]);
