/**
 * A task's result or question as it arrived in the caller's chat: who sent
 * it, the status it had, and what it said. The status is frozen at delivery —
 * the tasks drawer owns "what is happening now"; only a question's footer
 * follows the live task.
 */
import { useId, useState } from 'react';
import type { MessageSender, QuestionState } from '../AssistantChat/messageSender';
import { clampCardText } from '../AssistantChat/toolDisplay';
import { taskStatusLabel } from '../../utils/taskDisplay';
import AgentAvatar from './AgentAvatar';
import { cardActivity } from './TaskCard';
import pills from './taskStatus.module.css';
import styles from './TaskResultCard.module.css';

export interface TaskResultCardProps {
  sender: Extract<MessageSender, { kind: 'task' }>;
  body: string;
  /** A question's live state; `null` when the task is unknown. Unused for results. */
  question?: QuestionState | null;
  /** Omit to render the card without "Open task". */
  onOpen?: (taskId: string) => void;
}

const TaskResultCard = ({ sender, body, question, onOpen }: TaskResultCardProps) => {
  const { agent, taskId, title, status, cycleNo } = sender;
  const isQuestion = status === 'input_required';
  const [expanded, setExpanded] = useState(false);
  const bodyId = useId();
  // A question is short and must be read whole to be answered.
  const clamped = isQuestion ? body : clampCardText(body);
  const isClamped = clamped !== body;
  const questionLabel = question ? `, ${question.open ? 'open' : question.label}` : '';
  const label = isQuestion
    ? `Question from ${agent.name}: ${title}${questionLabel}`
    : `Result from ${agent.name}: ${title}`;

  return (
    <article className={styles.card} aria-label={label}>
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
            <span className={styles.lead}>{isQuestion ? ' · needs an answer' : ' · task result'}</span>
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
        {question && !question.open && <div className={styles.footer}>{question.label}</div>}
        <div className={styles.actions}>
          {question?.open && onOpen && (
            <button
              type="button"
              className={styles.action}
              aria-label={`Answer ${title}`}
              onClick={() => onOpen(taskId)}
            >
              Answer…
            </button>
          )}
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
