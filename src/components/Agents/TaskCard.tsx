/**
 * A delegated task as the chat shows it: the assignee's face, what it was
 * asked to do, and how that call answered — instead of an opaque
 * `workspace_assignTask` row. Clicking opens the task's own log.
 *
 * Every value it draws comes from the `TaskCallCard` the tool call returned,
 * frozen at that moment. The card never subscribes to the task and never
 * re-reads it, so a running task does not repaint the chat; the tasks drawer
 * owns "what is happening now".
 */
import { memo, useMemo } from 'react';
import type { WorkspaceAgentResponse } from '../../generated/bindings';
import type { TaskCallCard } from '../AssistantChat/toolDisplay';
import { taskStatusLabel } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import { taskIdentity, type AgentActivity } from './agentIdentity';
import pills from './taskStatus.module.css';
import styles from './TaskCard.module.css';

export interface TaskCardProps {
  card: TaskCallCard;
  /** The crew, for the assignee's face and name. Empty renders a fallback face. */
  roster: readonly WorkspaceAgentResponse[];
  /** Omit to render the card inert — e.g. inside a task's own transcript. */
  onOpen?: (taskId: string) => void;
}

/**
 * The ring the face wears, from the status alone.
 *
 * `TaskList.taskActivity` can ask whether a stopped task was reviewed; a tool
 * payload carries no acknowledgement fields, so here every stop that is not a
 * clean finish reads as needing attention. That is the honest reading of a
 * card frozen at the moment of the call.
 */
export const cardActivity = (status: string): AgentActivity => {
  if (status === 'queued' || status === 'running') return 'running';
  if (status === 'completed') return 'none';
  return 'attention';
};

/** Assignee name from the crew; tasks whose agent has left keep a generic one. */
const assigneeName = (
  card: TaskCallCard,
  roster: readonly WorkspaceAgentResponse[]
): string => {
  const agent = roster.find((entry) => entry.id === card.assignedToWorkspaceAgentId);
  if (!agent) return 'Agent';
  return agent.isDefault ? 'Main' : agent.displayName || 'Agent';
};

const TaskCard = ({ card, roster, onOpen }: TaskCardProps) => {
  const identity = useMemo(() => taskIdentity(card, roster), [card, roster]);
  const name = assigneeName(card, roster);
  const label = taskStatusLabel(card.status);
  const pillClass = `${pills.pill} ${pills[`pill_${card.status}`] || ''}`;
  const openable = !!onOpen;
  const slim = card.variant === 'slim';

  const body = slim ? (
    <>
      <AgentAvatar identity={identity} size={18} label={name} />
      <span className={styles.slimTitle}>{card.title}</span>
      <span className={pillClass}>{label}</span>
    </>
  ) : (
    <>
      <AgentAvatar
        identity={identity}
        size={30}
        activity={cardActivity(card.status)}
        label={name}
        className={styles.face}
      />
      <span className={styles.main}>
        <span className={styles.head}>
          <span className={styles.who}>
            {card.kind === 'assign' && <span className={styles.lead}>Delegated to </span>}
            {name}
          </span>
          <span className={pillClass}>{label}</span>
        </span>
        <span className={styles.title}>{card.title}</span>
        {card.detail ? (
          <span className={`${styles.detail} ${card.detailIsError ? styles.detailError : ''}`}>
            {card.detail}
          </span>
        ) : (
          card.instructions && <span className={styles.detail}>{card.instructions}</span>
        )}
      </span>
    </>
  );

  const className = `${styles.card} ${slim ? styles.slim : ''} ${openable ? styles.openable : ''}`;

  if (!openable) {
    return <div className={className}>{body}</div>;
  }
  return (
    <button
      type="button"
      className={className}
      onClick={() => onOpen(card.taskId)}
      title={`Open the log of "${card.title}"`}
      aria-label={`Open the log of task ${card.title}`}
    >
      {body}
    </button>
  );
};

export default memo(TaskCard);
