import { memo, useMemo } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { GENERATOR_VERSION } from '../Agents/agentIdentity';
import pillStyles from '../Agents/taskStatus.module.css';
import {
  cardAttention,
  cardItems,
  cardPhase,
  visibleLastMessage,
  type FleetAttention,
  type FleetCard as FleetCardModel,
  type FleetItem,
  type FleetLive,
  type FleetPhase,
} from '../../fleet/fleetModel';
import styles from './FleetPage.module.css';

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

export const attentionLabel = (a: FleetAttention): string =>
  [
    a.asks ? plural(a.asks, 'question', 'questions') : '',
    a.approvals ? plural(a.approvals, 'approval', 'approvals') : '',
    a.grants ? plural(a.grants, 'access request', 'access requests') : '',
  ]
    .filter(Boolean)
    .join(', ');

export const phaseLabel = (phase: FleetPhase, attention: FleetAttention): string =>
  phase === 'waiting' ? `Needs you · ${attentionLabel(attention)}` : phase === 'working' ? 'Working' : 'Idle';

const ItemRow = ({ item }: { item: FleetItem }) => (
  <li className={`${styles.item} ${styles[`tone_${item.tone}`]}`}>
    {item.agents.length > 0 && (
      <span className={styles.faces}>
        {item.agents.slice(0, 3).map((agent) => (
          <AgentAvatar
            key={agent.seed}
            identity={{ seed: agent.seed, generatorVersion: GENERATOR_VERSION }}
            size={16}
            ring={false}
            label={agent.name}
          />
        ))}
      </span>
    )}
    <span className={styles.itemText} title={item.text}>
      {item.text}
    </span>
    <span
      className={`${pillStyles.pill} ${item.statusKey ? pillStyles[`pill_${item.statusKey}`] ?? '' : item.tone === 'attention' ? pillStyles.pill_blocked : ''}`}
    >
      {item.status}
    </span>
  </li>
);

interface FleetCardProps {
  card: FleetCardModel;
  live: FleetLive;
  onSelect: (workspaceId: string) => void;
}

/** An unselected card: events and minimal text only. */
const FleetCard = ({ card, live, onSelect }: FleetCardProps) => {
  const phase = cardPhase(card, live);
  const attention = cardAttention(card);
  const items = useMemo(() => cardItems(card, live), [card, live]);
  const body = visibleLastMessage(card, live)?.text;
  const title = card.title || 'Workspace';

  return (
    <div className={styles.cardInner} onClick={() => onSelect(card.workspaceId)}>
      <header className={styles.cardHeader}>
        <button
          type="button"
          className={styles.cardTitle}
          onClick={(e) => {
            e.stopPropagation();
            onSelect(card.workspaceId);
          }}
          aria-label={`Select ${title}`}
        >
          {title}
        </button>
        <span className={styles.phase} data-phase={phase}>
          {phase === 'waiting' && (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
              <path d="M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
            </svg>
          )}
          {phaseLabel(phase, attention)}
        </span>
      </header>
      {card.userMessage && (
        <p className={styles.userMessage}>
          <span className={styles.userMessageLabel}>You</span> {card.userMessage.text}
        </p>
      )}
      {body && <p className={styles.lastMessage}>{body}</p>}
      {items.length > 0 && (
        <ul className={styles.items} aria-label="Activity">
          {items.map((item) => (
            <ItemRow key={item.key} item={item} />
          ))}
        </ul>
      )}
    </div>
  );
};

export default memo(FleetCard);
