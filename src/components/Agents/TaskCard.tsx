/**
 * A workspace task as the chat shows it: the assignee's face, what it was
 * asked to do, and how that call answered — instead of an opaque
 * workspace-task tool row. Clicking opens the task's own log.
 *
 * Every value describing the task comes from the `TaskCallCard` it is given;
 * only the assignee's face and name are read from the live crew, so a renamed
 * agent is not misattributed. The card never subscribes to the task itself:
 * whoever renders it decides whether `card.status` is frozen or live.
 */
import { memo, useMemo } from 'react';
import type { WorkspaceAgentResponse } from '../../generated/bindings';
import { isTaskAction, type TaskCallCard } from '../AssistantChat/toolDisplay';
import { taskStatusLabel } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import { agentDisplayName, taskIdentity } from './agentIdentity';
import pills from './taskStatus.module.css';
import styles from './TaskCard.module.css';

export interface TaskCardProps {
  card: TaskCallCard;
  /** The crew, for the assignee's face and name. Empty renders a fallback face. */
  roster: readonly WorkspaceAgentResponse[];
  /** Omit to render the card inert — e.g. inside a task's own transcript. */
  onOpen?: (taskId: string) => void;
}

/** Assignee name from the crew; tasks whose agent has left keep a generic one. */
const assigneeName = (
  card: TaskCallCard,
  roster: readonly WorkspaceAgentResponse[]
): string => {
  const agent = roster.find((entry) => entry.id === card.assignedToWorkspaceAgentId);
  return agent ? agentDisplayName(agent) : 'Agent';
};

const lead: Record<TaskCallCard['kind'], string> = {
  assign: 'Delegated to ',
  poll: '',
  stop: 'Stopped task · ',
  message: 'Messaged task · ',
};

const TaskCard = ({ card, roster, onOpen }: TaskCardProps) => {
  // An action card's pill is live, so the call's saved summary or error
  // could contradict it; the card shows neither.
  const isAction = isTaskAction(card);
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
        label={name}
        className={styles.face}
      />
      <span className={styles.main}>
        <span className={styles.head}>
          <span className={styles.who}>
            {lead[card.kind] && <span className={styles.lead}>{lead[card.kind]}</span>}
            {name}
          </span>
          {pill}
        </span>
        <span className={styles.title}>{card.title}</span>
        {isAction ? null : card.detail ? (
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
