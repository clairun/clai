/**
 * The chain of tasks that delegated the open one, root first. An ancestor the
 * store has not seen reads "…" and ends the walk.
 */
import type { WorkspaceTaskResponse } from '../../generated/bindings';
import { useWorkspaceTasks } from '../../workspace/taskStore';
import styles from './TaskBreadcrumb.module.css';

export interface TaskBreadcrumbProps {
  task: WorkspaceTaskResponse;
  onOpenTask: (taskId: string) => void;
}

const TaskBreadcrumb = ({ task, onOpenTask }: TaskBreadcrumbProps) => {
  const tasks = useWorkspaceTasks(task.workspaceId);
  if (!task.parentTaskId) return null;

  const ancestors: WorkspaceTaskResponse[] = [];
  const seen = new Set([task.id]);
  let parentId: string | null = task.parentTaskId;
  let truncated = false;
  while (parentId && !seen.has(parentId)) {
    const parent: WorkspaceTaskResponse | undefined = tasks?.[parentId];
    if (!parent) {
      truncated = true;
      break;
    }
    seen.add(parentId);
    ancestors.unshift(parent);
    parentId = parent.parentTaskId;
  }

  return (
    <nav className={styles.trail} aria-label="Task ancestry">
      <ol>
        {truncated && <li>…</li>}
        {ancestors.map((ancestor) => (
          <li key={ancestor.id}>
            <button type="button" className={styles.link} onClick={() => onOpenTask(ancestor.id)}>
              {ancestor.title}
            </button>
          </li>
        ))}
        <li aria-current="page">{task.title}</li>
      </ol>
    </nav>
  );
};

export default TaskBreadcrumb;
