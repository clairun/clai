/** Tasks nested under the task that delegated them. */
import type { WorkspaceTaskResponse } from '../generated/bindings';

export interface TaskNode {
  task: WorkspaceTaskResponse;
  children: TaskNode[];
}

/**
 * Roots keep the input order; children sort by creation. A task whose parent
 * is not in `tasks` (outside the loaded window) is a root, and a parent loop
 * is broken where the walk first comes back to a task it has seen.
 */
export const buildTaskTree = (tasks: readonly WorkspaceTaskResponse[]): TaskNode[] => {
  const nodes = new Map(tasks.map((task) => [task.id, { task, children: [] as TaskNode[] }]));
  const childrenOf = new Map<string, WorkspaceTaskResponse[]>();
  for (const task of tasks) {
    if (!task.parentTaskId || !nodes.has(task.parentTaskId)) continue;
    const siblings = childrenOf.get(task.parentTaskId) ?? [];
    siblings.push(task);
    childrenOf.set(task.parentTaskId, siblings);
  }

  const visited = new Set<string>();
  const attach = (node: TaskNode) => {
    visited.add(node.task.id);
    const children = [...(childrenOf.get(node.task.id) ?? [])].sort((a, b) =>
      a.createdAt < b.createdAt ? -1 : a.createdAt > b.createdAt ? 1 : 0
    );
    for (const child of children) {
      if (visited.has(child.id)) continue;
      const childNode = nodes.get(child.id)!;
      node.children.push(childNode);
      attach(childNode);
    }
  };

  const roots: TaskNode[] = [];
  const isRoot = (task: WorkspaceTaskResponse) =>
    !task.parentTaskId || !nodes.has(task.parentTaskId);
  // Proper roots first, then whatever only a loop can reach.
  for (const pass of [isRoot, () => true]) {
    for (const task of tasks) {
      if (visited.has(task.id) || !pass(task)) continue;
      const node = nodes.get(task.id)!;
      roots.push(node);
      attach(node);
    }
  }
  return roots;
};
