import { memo, useMemo } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import {
  cardAttention,
  cardCrew,
  cardPhase,
  requestScene,
  type FleetAttention,
  type FleetCard as FleetCardModel,
  type FleetLive,
  type FleetLoudness,
  type FleetPhase,
  type FleetRequestScene,
  type FleetFeed as Feed,
} from '../../fleet/fleetModel';
import { castFor } from './FleetFace';
import FleetCrew from './FleetCrew';
import FleetRequestPanel from './FleetRequestPanel';
import FleetFeed from './FleetFeed';
import fx from './FleetEffects';
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

/**
 * The header's phase. On the card that needs you it is also a skewed amber
 * sticker in two words; the full label stays for screen readers.
 */
export const PhaseLabel = ({ phase, attention, sticker }: { phase: FleetPhase; attention: FleetAttention; sticker: string | null }) => {
  const label = phaseLabel(phase, attention);
  return (
    <span className={styles.phase} data-phase={phase}>
      {sticker ? (
        <>
          <span className={fx.sticker} aria-hidden="true">
            {sticker}
          </span>
          <span className={fx.stickerLabel}>{label}</span>
        </>
      ) : (
        <>
          {phase === 'waiting' && (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
              <path d="M12 9v4M12 17h.01M10.3 3.9 1.8 18a2 2 0 0 0 1.7 3h17a2 2 0 0 0 1.7-3L13.7 3.9a2 2 0 0 0-3.4 0z" />
            </svg>
          )}
          {label}
        </>
      )}
    </span>
  );
};

/** The sticker of a loud card that needs you, by what its first request asks. */
export const stickerFor = (loud: FleetLoudness, request: FleetRequestScene | null): string | null =>
  loud === 'needs' && request ? (request.request.kind === 'approval' ? 'Approve?' : 'Needs you') : null;

interface FleetCardProps {
  card: FleetCardModel;
  /** What the miniature says (`cardFeed`); `null` while it shows an open request. */
  feed: Feed | null;
  /** Keys of the lines that shout (`fleetLoudness`). */
  shouts: ReadonlySet<string>;
  live: FleetLive;
  /** Picked agent faces, by agent definition id. */
  faces: Record<string, AgentIdentity>;
  /** The page's clock: the last Fleet event or the minute tick, whichever is later. */
  now: number;
  /** Drawn loud: it needs you, or a task on it just finished. */
  loud?: FleetLoudness;
  onSelect: (workspaceId: string) => void;
  onOpen: (workspaceId: string) => void;
}

/** A miniature: the workspace's conversation as highlights, or its open request, answerable in place. */
const FleetCard = ({ card, feed, shouts, live, faces, now, loud = null, onSelect, onOpen }: FleetCardProps) => {
  const cast = useMemo(() => castFor(faces, card.main), [faces, card.main]);
  const crew = useMemo(() => cardCrew(card, live, now), [card, live, now]);
  const phase = cardPhase(card, live);
  const attention = cardAttention(card);
  const request = useMemo(() => requestScene(card, live), [card, live]);
  const title = card.title || 'Workspace';

  return (
    <div className={styles.cardInner} onClick={() => onSelect(card.workspaceId)}>
      <header className={styles.cardHeader}>
        <button
          type="button"
          className={styles.cardTitle}
          data-card-title
          onClick={(e) => {
            e.stopPropagation();
            onSelect(card.workspaceId);
          }}
          aria-label={`Select ${title}`}
        >
          {title}
        </button>
        <PhaseLabel phase={phase} attention={attention} sticker={stickerFor(loud, request)} />
        <button
          type="button"
          className={styles.cardOpen}
          title="Open workspace"
          aria-label={`Open ${title}`}
          onClick={(e) => {
            e.stopPropagation();
            onOpen(card.workspaceId);
          }}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <path d="M14 4h6v6M20 4l-9 9M18 14v5a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1h5" />
          </svg>
        </button>
      </header>
      <FleetCrew crew={crew} cast={cast} />
      {feed && <FleetFeed workspaceId={card.workspaceId} feed={feed} shouts={shouts} now={now} faces={cast} />}
      {request && <FleetRequestPanel scene={request} workspaceId={card.workspaceId} faces={cast} loud={loud === 'needs'} now={now} />}
    </div>
  );
};

export default memo(FleetCard);
