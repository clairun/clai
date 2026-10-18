import { memo, useCallback, useMemo, useRef } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import {
  GRID_GAP,
  cardPhase,
  cardScene,
  fleetLoudness,
  openRequests,
  stageSpan,
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
  /** Card ids in slot order; the first is on the stage. */
  order: string[];
  cards: Record<string, FleetCardModel>;
  cols: number;
  rows: number;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  /** The clock the panels are judged against: freshness, shouts and captions. */
  now: number;
  onSelect: (workspaceId: string | null) => void;
}

/**
 * The card grid: the stage card, in its large form, spans the top-left
 * 2 × 2; the rest flow into the 1 × 1 slots around it. Nothing animates: on
 * WebKitGTK even a short transition blinks. The drawn effects switch on here.
 */
const FleetGrid = ({ order, cards, cols, rows, selectedId, live, faces, now, onSelect }: FleetGridProps) => {
  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);
  const gridRef = useRef<HTMLDivElement | null>(null);
  useThoughtBeat(gridRef, order.some((id) => cards[id] && cardPhase(cards[id], live) === 'working'));
  // Each panel's scene, once: the stage's in full. A card with an open request shows that instead.
  const panels = useMemo(
    () =>
      order.flatMap((id, slot) => {
        const card = cards[id];
        if (!card) return [];
        const scene = openRequests(card).length > 0 ? null : cardScene(card, live, now, { stage: slot === 0 });
        return [{ card, scene, selected: id === selectedId }];
      }),
    [order, cards, live, now, selectedId]
  );
  const loud = useMemo(() => fleetLoudness(panels), [panels]);

  return (
    <div
      ref={gridRef}
      className={`${styles.grid} ${fx.fx}`}
      data-fx={FLEET_FX}
      style={{
        gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`,
        gridTemplateRows: `repeat(${rows}, minmax(0, 1fr))`,
        gap: GRID_GAP,
      }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onSelect(null);
      }}
    >
      {panels.map(({ card, scene, selected }, slot) => {
        const id = card.workspaceId;
        const stage = slot === 0 ? stageSpan({ cols, rows }) : null;
        const cardLoud = loud.cards[id] ?? null;
        return (
          <article
            key={id}
            className={`${styles.card} ${selected ? styles.cardSelected : (PHASE_BORDER[cardPhase(card, live)] ?? '')}`}
            aria-current={selected || undefined}
            data-loud={cardLoud ?? undefined}
            style={stage ? { gridColumn: `1 / span ${stage.cols}`, gridRow: `1 / span ${stage.rows}` } : undefined}
          >
            {stage ? (
              <FleetSelectedCard card={card} scene={scene} shouts={loud.rows} live={live} selected={selected} faces={faces} now={now} loud={cardLoud} onSelect={onSelect} />
            ) : (
              <FleetCard card={card} scene={scene} shouts={loud.rows} live={live} faces={faces} now={now} loud={cardLoud} onSelect={handleSelect} />
            )}
          </article>
        );
      })}
    </div>
  );
};

export default memo(FleetGrid);
