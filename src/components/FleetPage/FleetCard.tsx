import { memo, useMemo, type CSSProperties } from 'react';
import { useAppTheme } from '../../hooks/useAppTheme';
import AgentAvatar from '../Agents/AgentAvatar';
import { faceOf, identityRingColor, mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import pillStyles from '../Agents/taskStatus.module.css';
import MarkdownMessage from '../Chat/MarkdownMessage';
import {
  cardAttention,
  cardItems,
  cardPhase,
  liveAgents,
  openRequests,
  type FleetAttention,
  type FleetCard as FleetCardModel,
  type FleetItem,
  type FleetLive,
  type FleetPhase,
} from '../../fleet/fleetModel';
import FleetRequestRow from './FleetRequestRow';
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

/** `14:05` today, `Oct 16` before. */
const eventTime = (at: number): string => {
  const date = new Date(at);
  return date.toDateString() === new Date().toDateString()
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: 'short', day: 'numeric' });
};

/** A chat message: a few clamped lines of markdown. */
const NoteRow = ({ item }: { item: FleetItem }) => (
  <li className={styles.note}>
    {item.note === 'user' && <span className={styles.userMessageLabel}>You</span>}
    <div className={styles.noteBody}>
      <MarkdownMessage content={item.text} />
    </div>
    <time className={styles.itemTime}>{eventTime(item.at)}</time>
  </li>
);

const MAX_HEADER_FACES = 4;

/** The faces of the agents with live work, for the card header: running first, queued muted. */
export const LiveFaces = ({ card, live, faces }: { card: FleetCardModel; live: FleetLive; faces: Record<string, AgentIdentity> }) => {
  const agents = useMemo(() => liveAgents(card, live), [card, live]);
  if (agents.length === 0) return null;
  const extra = agents.length - MAX_HEADER_FACES;
  return (
    <span className={styles.headerFaces}>
      {agents.slice(0, MAX_HEADER_FACES).map((agent) => (
        <span
          key={agent.seed}
          className={agent.running ? undefined : styles.queuedFace}
          title={`${agent.name}: ${agent.doing}`}
        >
          <AgentAvatar
            identity={agent.main ? mainIdentity() : faceOf(faces, agent.seed)}
            size={18}
            ring={false}
            label={`${agent.name}: ${agent.doing}`}
          />
        </span>
      ))}
      {extra > 0 && (
        <span className={styles.headerFacesMore} title={agents.slice(MAX_HEADER_FACES).map((a) => `${a.name}: ${a.doing}`).join('\n')}>
          +{extra}
        </span>
      )}
    </span>
  );
};

/** A running subtask: its agent's face and name in the agent's colour, larger than history. */
const LiveRow = ({ item, faces }: { item: FleetItem; faces: Record<string, AgentIdentity> }) => {
  const theme = useAppTheme();
  const lead = item.agents[0]!;
  const color = identityRingColor(faceOf(faces, lead.seed), theme);
  return (
    <li className={styles.liveItem} style={{ '--agent-color': color } as CSSProperties}>
      <AgentAvatar identity={faceOf(faces, lead.seed)} size={28} ring={false} label={lead.name} />
      <span className={styles.liveBody}>
        <span className={styles.liveAgent}>
          {lead.name}
          {item.agents.length > 1 && <span className={styles.liveMore}> +{item.agents.length - 1}</span>}
        </span>
        <span className={styles.liveText} title={item.text}>
          {item.text}
        </span>
      </span>
      <span className={`${pillStyles.pill} ${item.statusKey ? pillStyles[`pill_${item.statusKey}`] ?? '' : ''}`}>
        {item.status}
      </span>
    </li>
  );
};

const ItemRow = ({ item, faces }: { item: FleetItem; faces: Record<string, AgentIdentity> }) => (
  <li className={`${styles.item} ${styles[`tone_${item.tone}`]}`}>
    {item.agents.length > 0 && (
      <span className={styles.faces}>
        {item.agents.slice(0, 3).map((agent) => (
          <AgentAvatar
            key={agent.seed}
            identity={faceOf(faces, agent.seed)}
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
      className={`${pillStyles.pill} ${item.statusKey ? pillStyles[`pill_${item.statusKey}`] ?? '' : ''}`}
    >
      {item.status}
    </span>
    <time className={styles.itemTime}>{eventTime(item.at)}</time>
  </li>
);

interface FleetCardProps {
  card: FleetCardModel;
  live: FleetLive;
  /** Picked agent faces, by agent definition id. */
  faces: Record<string, AgentIdentity>;
  onSelect: (workspaceId: string) => void;
}

/** An unselected card: events and minimal text, and its open request answerable in place. */
const FleetCard = ({ card, live, faces, onSelect }: FleetCardProps) => {
  const phase = cardPhase(card, live);
  const attention = cardAttention(card);
  const items = useMemo(() => cardItems(card, live), [card, live]);
  const requests = useMemo(() => openRequests(card), [card]);
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
        <LiveFaces card={card} live={live} faces={faces} />
        <span className={styles.phase} data-phase={phase}>
          {phase === 'waiting' && (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
              <path d="M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
            </svg>
          )}
          {phaseLabel(phase, attention)}
        </span>
      </header>
      {requests.length === 0 && items.length > 0 && (
        <ul className={styles.items} aria-label="Activity">
          {items.map((item) => (
            item.note ? (
              <NoteRow key={item.key} item={item} />
            ) : item.tone === 'live' && item.agents.length > 0 ? (
              <LiveRow key={item.key} item={item} faces={faces} />
            ) : (
              <ItemRow key={item.key} item={item} faces={faces} />
            )
          ))}
        </ul>
      )}
      {requests.length > 0 && (
        <FleetRequestRow requests={requests} workspaceId={card.workspaceId} live={live} faces={faces} />
      )}
    </div>
  );
};

export default memo(FleetCard);
