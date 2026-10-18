import { memo, useMemo } from 'react';
import AgentAvatar from '../Agents/AgentAvatar';
import { faceOf, mainIdentity, type AgentIdentity } from '../Agents/agentIdentity';
import {
  cardAttention,
  cardPhase,
  cardScene,
  liveAgents,
  requestScene,
  type FleetAttention,
  type FleetCard as FleetCardModel,
  type FleetLive,
  type FleetPhase,
} from '../../fleet/fleetModel';
import FleetRequestPanel from './FleetRequestPanel';
import FleetScene from './FleetScene';
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

interface FleetCardProps {
  card: FleetCardModel;
  live: FleetLive;
  /** Picked agent faces, by agent definition id. */
  faces: Record<string, AgentIdentity>;
  /** The time of the last Fleet event: the card changes on events, not on a timer. */
  now: number;
  onSelect: (workspaceId: string) => void;
}

/** An unselected card: a comic panel of what happened, or its open request, answerable in place. */
const FleetCard = ({ card, live, faces, now, onSelect }: FleetCardProps) => {
  const phase = cardPhase(card, live);
  const attention = cardAttention(card);
  const request = useMemo(() => requestScene(card, live, now), [card, live, now]);
  const scene = useMemo(() => (request ? null : cardScene(card, live, now)), [request, card, live, now]);
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
      {scene && <FleetScene scene={scene} now={now} faces={faces} />}
      {request && <FleetRequestPanel scene={request} workspaceId={card.workspaceId} faces={faces} />}
    </div>
  );
};

export default memo(FleetCard);
