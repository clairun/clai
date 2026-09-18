/**
 * The Tasks drawer: who is doing what. Every task leads with the face of the
 * agent it was handed to, ringed by what the task is doing now; the status
 * pill stays for colour-blind readers and for text search. A face row on top
 * narrows the list to one or more agents while the drawer is open.
 */
import { useCallback, useEffect, useMemo, useState } from 'react';
import { acknowledgeWorkspaceTask } from '../../workspace/client';
import type { WorkspaceAgentResponse, WorkspaceTaskResponse } from '../../generated/bindings';
import {
  formatRelativeTime,
  isTaskActive,
  isTaskAttention,
  taskStatusLabel,
} from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import { identityFor, taskIdentity, type AgentActivity, type AgentIdentity } from './agentIdentity';
import styles from './TaskList.module.css';

export interface TaskListProps {
  workspaceId: string;
  tasks: readonly WorkspaceTaskResponse[];
  /** The crew, for faces and names; tasks from agents that left still resolve. */
  roster: readonly WorkspaceAgentResponse[];
  onChanged: () => void | Promise<void>;
  onViewTask?: (task: WorkspaceTaskResponse) => void;
}

const errorMessage = (error: unknown, fallback: string): string => {
  if (typeof error === 'string') return error;
  if (error instanceof Error && error.message) return error.message;
  return fallback;
};

/** The ring a task row draws: what the task is doing, not the agent overall. */
export const taskActivity = (task: WorkspaceTaskResponse): AgentActivity =>
  isTaskActive(task) ? 'running' : isTaskAttention(task) ? 'attention' : 'none';

export interface FilterFace {
  agentId: string;
  name: string;
  identity: AgentIdentity;
  /** Any unacknowledged problem on this agent's tasks. */
  attention: boolean;
}

/** One face per agent that has tasks here, in first-seen order. */
export const filterFaces = (
  tasks: readonly WorkspaceTaskResponse[],
  roster: readonly WorkspaceAgentResponse[]
): FilterFace[] => {
  const faces = new Map<string, FilterFace>();
  for (const task of tasks) {
    const id = task.assignedToWorkspaceAgentId;
    const existing = faces.get(id);
    if (existing) {
      existing.attention ||= isTaskAttention(task);
      continue;
    }
    const agent = roster.find((entry) => entry.id === id);
    const name = agent?.isDefault ? 'Main' : agent?.displayName || task.assignedAgentDisplayName;
    faces.set(id, {
      agentId: id,
      name,
      identity: taskIdentity(task, roster),
      attention: isTaskAttention(task),
    });
  }
  return [...faces.values()];
};

const TaskList = ({ workspaceId, tasks, roster, onChanged, onViewTask }: TaskListProps) => {
  const [busyTaskId, setBusyTaskId] = useState('');
  const [error, setError] = useState('');
  const [selectedAgents, setSelectedAgents] = useState<ReadonlySet<string>>(() => new Set());

  const faces = useMemo(() => filterFaces(tasks, roster), [tasks, roster]);
  // A face whose agent no longer has tasks drops out of the row; its
  // selection must not keep filtering everything away.
  const active = useMemo(
    () => new Set([...selectedAgents].filter((id) => faces.some((face) => face.agentId === id))),
    [selectedAgents, faces]
  );
  const visible =
    active.size === 0 ? tasks : tasks.filter((task) => active.has(task.assignedToWorkspaceAgentId));

  // Forget a pick once its face has left the row; otherwise the filter would
  // spring back on its own the next time that agent gets a task.
  useEffect(() => {
    if (active.size === selectedAgents.size) return;
    // eslint-disable-next-line react-hooks/set-state-in-effect -- reconciling state with data that arrived by props; only fires when a selected face actually disappeared.
    setSelectedAgents(active);
  }, [active, selectedAgents]);

  const toggleAgent = useCallback((agentId: string) => {
    setSelectedAgents((prev) => {
      const next = new Set(prev);
      if (next.has(agentId)) next.delete(agentId);
      else next.add(agentId);
      return next;
    });
  }, []);

  const handleAcknowledge = useCallback(
    async (taskId: string) => {
      if (busyTaskId) return;
      setBusyTaskId(taskId);
      setError('');
      try {
        await acknowledgeWorkspaceTask(workspaceId, taskId);
        await onChanged();
      } catch (err) {
        setError(errorMessage(err, 'Failed to acknowledge task.'));
      } finally {
        setBusyTaskId('');
      }
    },
    [busyTaskId, onChanged, workspaceId]
  );

  const nameOf = (agentId: string | null, fallback: string | null): string | null => {
    const agent = roster.find((entry) => entry.id === agentId);
    if (agent) return agent.isDefault ? 'Main' : agent.displayName;
    return fallback;
  };

  return (
    <section className={styles.tasks} aria-label="Workspace task activity">
      {faces.length > 1 && (
        <div className={styles.filter} role="group" aria-label="Filter by agent">
          {faces.map((face) => {
            const pressed = active.has(face.agentId);
            return (
              <button
                key={face.agentId}
                type="button"
                className={`${styles.filterFace} ${pressed ? styles.filterFacePressed : ''}`}
                aria-pressed={pressed}
                aria-label={face.name}
                title={face.name}
                onClick={() => toggleAgent(face.agentId)}
              >
                <AgentAvatar
                  identity={face.identity}
                  size={26}
                  activity={face.attention ? 'attention' : 'none'}
                />
              </button>
            );
          })}
          {active.size > 0 && (
            <button
              type="button"
              className={styles.filterClear}
              onClick={() => setSelectedAgents(new Set())}
            >
              Show all
            </button>
          )}
        </div>
      )}

      {error && <div className={styles.error}>{error}</div>}

      {visible.length > 0 ? (
        <ul className={styles.list}>
          {visible.map((task) => {
            const detail = task.error || task.resultSummary || task.instructions;
            const needsAttention = isTaskAttention(task);
            const creator = roster.find((entry) => entry.id === task.createdByWorkspaceAgentId);
            const creatorName = nameOf(task.createdByWorkspaceAgentId, task.createdByDisplayName);
            const assigneeName = nameOf(
              task.assignedToWorkspaceAgentId,
              task.assignedAgentDisplayName
            );
            return (
              <li key={task.id} className={styles.task}>
                <AgentAvatar
                  identity={taskIdentity(task, roster)}
                  size={28}
                  activity={taskActivity(task)}
                  className={styles.taskFace}
                />
                <div className={styles.taskMain}>
                  <div className={styles.titleRow}>
                    <span className={styles.title}>{task.title}</span>
                    <span className={`${styles.status} ${styles[`status_${task.status}`] || ''}`}>
                      {taskStatusLabel(task.status)}
                    </span>
                  </div>
                  <div className={styles.meta}>
                    {creatorName && (
                      <>
                        {creator && <AgentAvatar identity={identityFor(creator)} size={14} />}
                        <span>{creatorName}</span>
                        <span aria-hidden="true">→</span>
                      </>
                    )}
                    <span>{assigneeName}</span>
                    <span className={styles.metaTime}>{formatRelativeTime(task.updatedAt)}</span>
                  </div>
                  {detail && <p className={styles.summary}>{detail}</p>}
                  {(needsAttention || task.sessionId) && (
                    <div className={styles.actions}>
                      {needsAttention && (
                        <button
                          type="button"
                          className={styles.action}
                          onClick={() => handleAcknowledge(task.id)}
                          disabled={busyTaskId === task.id}
                        >
                          Mark reviewed
                        </button>
                      )}
                      {task.sessionId && (
                        <button
                          type="button"
                          className={styles.action}
                          onClick={() => onViewTask?.(task)}
                        >
                          View log
                        </button>
                      )}
                    </div>
                  )}
                </div>
              </li>
            );
          })}
        </ul>
      ) : (
        <div className={styles.empty}>
          {tasks.length === 0 ? 'No delegated tasks yet.' : 'No tasks for the selected agents.'}
        </div>
      )}
    </section>
  );
};

export default TaskList;
