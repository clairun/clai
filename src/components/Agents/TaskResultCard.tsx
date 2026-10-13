/**
 * A task's result as it arrived in the caller's chat: who did it, the status
 * it ended with, and what it said. The status is frozen at delivery — the
 * tasks drawer owns "what is happening now".
 */
import { useId, useState } from 'react';
import type { MessageSender } from '../AssistantChat/messageSender';
import { clampCardText } from '../AssistantChat/toolDisplay';
import { taskStatusLabel } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import { cardActivity } from './TaskCard';
import pills from './taskStatus.module.css';
import styles from './TaskResultCard.module.css';

export interface TaskResultCardProps {
  sender: Extract<MessageSender, { kind: 'task' }>;
  body: string;
  /** Omit to render the card without "Open task". */
  onOpen?: (taskId: string) => void;
}

const TaskResultCard = ({ sender, body, onOpen }: TaskResultCardProps) => {
  const { agent, taskId, title, status, cycleNo } = sender;
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  const clamped = clampCardText(body);
  const isClamped = clamped !== body;

  return (
    <article className={styles.card} aria-label={`Result from ${agent.name}: ${title}`}>
      <AgentAvatar
        identity={agent.identity}
        size={30}
        activity={agent.presence === 'disabled' ? 'disabled' : cardActivity(status)}
        className={styles.face}
      />
      <div className={styles.main}>
        <div className={styles.head}>
          <span className={styles.who}>
            {agent.name}
            <span className={styles.lead}> · task result</span>
          </span>
          {cycleNo > 1 && <span className={styles.cycle}>cycle {cycleNo}</span>}
          <span className={`${pills.pill} ${pills[`pill_${status}`] || ''}`}>
            {taskStatusLabel(status)}
          </span>
        </div>
        <div className={styles.title}>{title}</div>
        <div id={bodyId} className={styles.body}>
          {expanded ? body : clamped}
        </div>
        <div className={styles.actions}>
          {isClamped && (
            <button
              type="button"
              className={styles.action}
              aria-expanded={expanded}
              aria-controls={bodyId}
              onClick={() => setExpanded((open) => !open)}
            >
              {expanded ? 'Show less' : 'Show full result'}
            </button>
          )}
          {onOpen && (
            <button type="button" className={styles.action} onClick={() => onOpen(taskId)}>
              Open task
            </button>
          )}
        </div>
      </div>
    </article>
  );
};

export default TaskResultCard;
