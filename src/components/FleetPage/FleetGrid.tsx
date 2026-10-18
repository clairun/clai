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
import FleetCard from './FleetCard';
import FleetSelectedCard from './FleetSelectedCard';
import fx, { FLEET_FX } from './FleetEffects';
import { useThoughtBeat } from './useThoughtBeat';
import styles from './FleetPage.module.css';

/** An unselected card's border by phase; a selected card shows its phase in the header. */
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
 * The board: the selected workspace's conversation on the stage at the left,
 * the others as miniatures beside it (`boardShape`), scrolling when there are
 * more. With nothing selected, every card is a miniature. Nothing animates:
 * on WebKitGTK even a short transition blinks. The drawn effects switch on here.
 */
const FleetGrid = ({ order, cards, selectedId, live, faces, now, onSelect }: FleetGridProps) => {
  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);
  const gridRef = useRef<HTMLDivElement | null>(null);
  useThoughtBeat(gridRef, order.some((id) => id !== selectedId && cards[id] && cardPhase(cards[id], live) === 'working'));
  const staged = selectedId && cards[selectedId] && order.includes(selectedId) ? cards[selectedId] : null;
  // Each miniature's feed, once. A card with an open request shows that instead.
  const panels = useMemo(
    () =>
      order.flatMap((id) => {
        const card = cards[id];
        if (!card) return [];
        const selected = id === selectedId;
        const feed = selected || openRequests(card).length > 0 ? null : cardFeed(card, live, now);
        return [{ card, feed, selected }];
      }),
    [order, cards, live, now, selectedId]
  );
  const loud = useMemo(() => fleetLoudness(panels), [panels]);
  const minis = panels.filter((p) => !p.selected);
  const shape = boardShape(minis.length, !!staged);
  const deselect = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget) onSelect(null);
  };

  return (
    <div ref={gridRef} className={`${styles.board} ${fx.fx}`} data-fx={FLEET_FX} onClick={deselect}>
      {staged && (
        <article className={`${styles.card} ${styles.cardSelected} ${styles.stage}`} aria-current>
          <FleetSelectedCard card={staged} live={live} selected faces={faces} onSelect={onSelect} />
        </article>
      )}
      {minis.length > 0 && (
        <div className={styles.miniArea} data-staged={staged ? true : undefined}>
          <div
            className={styles.minis}
            style={{ '--cols': shape.cols, '--rows': shape.rows } as CSSProperties}
            onClick={deselect}
          >
            {minis.map(({ card, feed }) => {
              const cardLoud = loud.cards[card.workspaceId] ?? null;
              return (
                <article
                  key={card.workspaceId}
                  className={`${styles.card} ${PHASE_BORDER[cardPhase(card, live)] ?? ''}`}
                  data-loud={cardLoud ?? undefined}
                >
                  <FleetCard card={card} feed={feed} shouts={loud.rows} live={live} faces={faces} now={now} loud={cardLoud} onSelect={handleSelect} />
                </article>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
};

export default memo(FleetGrid);
