/**
 * A workspace task as the chat shows it: the assignee's face, its title, and
 * how that call answered — instead of an opaque
 * workspace-task tool row. Clicking opens the task's own log. A stop or a
 * message is drawn by `TaskActionCard` instead.
 *
 * Every value describing the task comes from the `TaskCallCard` it is given;
 * only the assignee's face and name are read from the live crew, so a renamed
 * agent is not misattributed. The card never subscribes to the task itself:
 * its status is frozen as the call saw it.
 */
import { memo, useMemo, type ReactNode } from 'react';
import type { WorkspaceAgentResponse } from '../../generated/bindings';
import type { TaskCallCard } from '../AssistantChat/toolDisplay';
import type { SenderAgent } from '../AssistantChat/messageSender';
import { taskDisplayStatus, taskStatusLabel } from '../../utils/taskDisplay';
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

/** Assignee name from the crew; one that has left keeps the name the call saw. */
const assigneeName = (
  card: TaskCallCard,
  roster: readonly WorkspaceAgentResponse[]
): string => {
  const agent = roster.find((entry) => entry.id === card.assignedToWorkspaceAgentId);
  return agent ? agentDisplayName(agent) : card.assignedAgentDisplayName || 'Agent';
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
   *
   * Only a failure goes through `taskDisplayStatus`, for its reason: the card
   * has no `runId`, which that helper would read as waiting for subtasks.
   */
  const status =
    card.status === 'failed'
      ? taskDisplayStatus({ status: card.status, failureReason: card.failureReason })
      : { key: card.status, label: taskStatusLabel(card.status) };
  const pill =
    card.kind !== 'poll' ? null : (
      <span className={`${pills.pill} ${pills[`pill_${status.key}`] || ''}`}>
        {status.label}
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
            {card.kind === 'assign' && <span className={styles.lead}>Delegated to </span>}
            {name}
          </span>
          {pill}
        </span>
        <span className={styles.title}>{card.title}</span>
        {card.error && <span className={styles.error}>{card.error}</span>}
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
      // error, and a poll's status — with a shorter sentence. The tooltip
      // says what the click does; the content says what it is about.
      title={`Open the log of "${card.title}"`}
    >
      {body}
    </button>
  );
};

export default memo(TaskCard);

export interface TaskActionCardProps {
  card: TaskCallCard;
  /** The agent whose turn made the call; null when no workspace agent owns it. */
  origin: SenderAgent | null;
  roster: readonly WorkspaceAgentResponse[];
  onOpen?: (taskId: string) => void;
  /** A message's text, drawn as the body of the card. */
  children?: ReactNode;
}

/**
 * A stop or a message, drawn as what it is: one agent addressing another.
 * The task is only context, so the card carries no status — the task's status
 * describes the task, not what was sent to it.
 */
const TaskActionCardImpl = ({ card, origin, roster, onOpen, children }: TaskActionCardProps) => {
  const identity = useMemo(() => taskIdentity(card, roster), [card, roster]);
  const tooltip = `Open the log of "${card.title}"`;
  // A stop has nothing else to click, so the whole card opens the task. A
  // message's text can hold links and its own button, which a card-wide
  // button cannot contain, so there only the title opens it.
  const wholeCard = card.kind === 'stop';
  const title =
    onOpen && !wholeCard ? (
      <button
        type="button"
        className={`${styles.taskLink} ${styles.taskLinkOpenable}`}
        onClick={() => onOpen(card.taskId)}
        title={tooltip}
      >
        {card.title}
      </button>
    ) : (
      <span className={styles.taskLink}>{card.title}</span>
    );

  const body = (
    <>
      <span className={styles.route}>
        {origin && <AgentAvatar identity={origin.identity} size={18} />}
        <span className={styles.who}>{origin ? origin.name : 'Clai'}</span>
        <span className={styles.arrow} aria-hidden="true">→</span>
        <span className="sr-only">to</span>
        <AgentAvatar identity={identity} size={18} />
        <span className={styles.who}>{assigneeName(card, roster)}</span>
      </span>
      {children && <div className={styles.bubble}>{children}</div>}
      <span className={styles.context}>
        {card.kind === 'stop' ? (
          <>
            <span className={styles.stopIcon} aria-hidden="true" />
            <span>Requested to stop</span>
          </>
        ) : (
          <span>Task</span>
        )}
        {title}
      </span>
    </>
  );

  if (!wholeCard || !onOpen) {
    return <div className={`${styles.card} ${styles.action}`}>{body}</div>;
  }
  return (
    <button
      type="button"
      className={`${styles.card} ${styles.action} ${styles.openable}`}
      onClick={() => onOpen(card.taskId)}
      title={tooltip}
    >
      {body}
    </button>
  );
};

export const TaskActionCard = memo(TaskActionCardImpl);
