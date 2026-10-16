/**
 * A task's result as it arrived in the caller's chat: who sent it and the
 * status it had. The status is frozen at delivery — the tasks drawer owns
 * "what is happening now". The result text is written for the agent, so the
 * card leaves it to the task's log.
 */
import type { MessageSender } from '../AssistantChat/messageSender';
import { taskDisplayStatus } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import pills from './taskStatus.module.css';
import styles from './TaskResultCard.module.css';

export interface TaskResultCardProps {
  sender: Extract<MessageSender, { kind: 'task' }>;
  /** The failure reason of the card's cycle, when known; only the live task carries it. */
  failureReason?: string | null;
  /** Omit to render the card without "Open task". */
  onOpen?: (taskId: string) => void;
}

const TaskResultCard = ({ sender, failureReason, onOpen }: TaskResultCardProps) => {
  const { agent, taskId, title, status, cycleNo } = sender;
  const pill = taskDisplayStatus({ status, failureReason });

  return (
    <article className={styles.card} aria-label={`Result from ${agent.name}: ${title}`}>
      <AgentAvatar
        identity={agent.identity}
        size={30}
        activity={agent.presence === 'disabled' ? 'disabled' : 'none'}
        className={styles.face}
      />
      <div className={styles.main}>
        <div className={styles.head}>
          <span className={styles.who}>
            {agent.name}
            <span className={styles.lead}> · task result</span>
          </span>
          {cycleNo > 1 && <span className={styles.cycle}>cycle {cycleNo}</span>}
          <span className={`${pills.pill} ${pills[`pill_${pill.key}`] || ''}`}>{pill.label}</span>
        </div>
        <div className={styles.title}>{title}</div>
        {onOpen && (
          <button
            type="button"
            className={styles.action}
            aria-label={`Open task ${title}`}
            onClick={() => onOpen(taskId)}
          >
            Open task
          </button>
        )}
      </div>
    </article>
  );
};

export default TaskResultCard;
