/**
 * A delegated task as the chat shows it: the assignee's face, what it was
 * asked to do, and how that call answered — instead of an opaque
 * `workspace_assignTask` row. Clicking opens the task's own log.
 *
 * Every value describing the task comes from the `TaskCallCard` the tool call
 * returned, frozen at that moment; only the assignee's face and name are read
 * from the live crew, so a renamed agent is not misattributed. The card never
 * subscribes to the task and never re-reads it, so a running task does not
 * repaint the chat; the tasks drawer owns "what is happening now".
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
 * Never the spinning `running` ring, however fresh the status looks: a card is
 * frozen at the moment of its call, and a hand-off is answered the instant the
 * row is stamped — almost always `queued` — so that ring would spin forever on
 * hand-offs made weeks ago, a claim about right now that the card cannot
 * support, at the cost of an endless animation per card. Live progress belongs
 * to the tasks drawer.
 *
 * A stop that is not a clean finish still earns the static attention ring.
 * `TaskList.taskActivity` can ask whether such a task was since reviewed; a
 * tool payload carries no acknowledgement fields, so here it stands as the
 * call saw it.
 */
export const cardActivity = (status: string): AgentActivity =>
  status === 'failed' || status === 'blocked' ? 'attention' : 'none';

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
  const openable = !!onOpen;
  const slim = card.variant === 'slim';

  /*
   * No pill on a hand-off. The status a delegation carries is whatever the row
   * happened to be stamped with before the call returned — `queued`, or
   * `completed` if the worker won the race — so the pill would report that race
   * rather than the task, frozen forever. A poll's status is something the
   * agent actually went and looked at, so that card keeps its pill.
   *
   * Every delegation is a full card (`inlineTaskCard` forces `variant: 'full'`
   * for `kind: 'assign'`), so the slim line below always has one to draw.
   */
  const pill =
    card.kind === 'assign' ? null : (
      <span className={`${pills.pill} ${pills[`pill_${card.status}`] || ''}`}>
        {taskStatusLabel(card.status)}
      </span>
    );

  const body = slim ? (
    <>
      <AgentAvatar identity={identity} size={18} label={name} />
      <span className={styles.slimTitle}>{card.title}</span>
      {pill}
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
          {pill}
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
      // No aria-label: it would replace the card's own text — assignee, title,
      // summary, and a poll's status — with a shorter sentence. The tooltip
      // says what the click does; the content says what it is about.
      title={`Open the log of "${card.title}"`}
    >
      {body}
    </button>
  );
};

export default memo(TaskCard);
