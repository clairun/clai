import { memo, useCallback, useMemo, useRef, type CSSProperties } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import {
  boardShape,
  cardFeed,
  cardPhase,
  fleetLoudness,
  openRequests,
  type FleetCard as FleetCardModel,
  type FleetLive,
  type FleetPhase,
} from '../../fleet/fleetModel';
import { useAppTheme } from '../../hooks/useAppTheme';
import { workspaceLook } from '../../fleet/workspaceLook';
import FleetCard from './FleetCard';
import { accentStyle } from './FleetFace';
import fx, { FLEET_FX } from './FleetEffects';
import { useThoughtBeat } from './useThoughtBeat';
import styles from './FleetPage.module.css';

/** A card's border by phase; the selected card's own border wins. */
const PHASE_BORDER: Partial<Record<FleetPhase, string>> = { working: styles.cardWorking, waiting: styles.cardWaiting };

interface FleetGridProps {
  /** Card ids in order. */
  order: string[];
  cards: Record<string, FleetCardModel>;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  /** The clock the panels are judged against: freshness, shouts and captions. */
  now: number;
  onSelect: (workspaceId: string | null) => void;
}

/**
 * The board: every card the same size (`boardShape`), scrolling when there
 * are more; the selected one is only highlighted. Nothing animates: on
 * WebKitGTK even a short transition blinks. The drawn effects switch on here.
 */
const FleetGrid = ({ order, cards, selectedId, live, faces, now, onSelect }: FleetGridProps) => {
  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);
  const gridRef = useRef<HTMLDivElement | null>(null);
  const theme = useAppTheme();
  useThoughtBeat(gridRef, order.some((id) => cards[id] && cardPhase(cards[id], live) === 'working'));
  // Each card's feed, once. A card with an open request shows that instead.
  const panels = useMemo(
    () =>
      order.flatMap((id) => {
        const card = cards[id];
        if (!card) return [];
        const feed = openRequests(card).length > 0 ? null : cardFeed(card, live, now);
        return [{ card, feed, selected: id === selectedId, look: workspaceLook(id) }];
      }),
    [order, cards, live, now, selectedId]
  );
  const loud = useMemo(() => fleetLoudness(panels), [panels]);
  const shape = boardShape(panels.length);
  const deselect = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget) onSelect(null);
  };

  return (
    <div ref={gridRef} className={`${styles.board} ${fx.fx}`} data-fx={FLEET_FX} onClick={deselect}>
      <div className={styles.cards} style={{ '--cols': shape.cols, '--rows': shape.rows } as CSSProperties} onClick={deselect}>
        {panels.map(({ card, feed, selected, look }) => {
          const cardLoud = loud.cards[card.workspaceId] ?? null;
          return (
            <article
              key={card.workspaceId}
              className={`${styles.card} ${selected ? styles.cardSelected : (PHASE_BORDER[cardPhase(card, live)] ?? '')}`}
              aria-current={selected || undefined}
              data-loud={cardLoud ?? undefined}
              data-screentone
              style={accentStyle(look.hue, theme)}
            >
              <FleetCard card={card} feed={feed} shouts={loud.rows} live={live} faces={faces} now={now} loud={cardLoud} onSelect={handleSelect} />
            </article>
          );
        })}
      </div>
    </div>
  );
};

export default memo(FleetGrid);
