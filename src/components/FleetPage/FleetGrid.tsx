import { memo, useCallback, useRef } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import {
  GRID_GAP,
  cardLoudness,
  cardPhase,
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
  /** The time of the last Fleet event. */
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
      {order.map((id, slot) => {
        const card = cards[id];
        if (!card) return null;
        const selected = id === selectedId;
        const stage = slot === 0 ? stageSpan({ cols, rows }) : null;
        const loud = cardLoudness(card, live, now, selected);
        return (
          <article
            key={id}
            className={`${styles.card} ${selected ? styles.cardSelected : (PHASE_BORDER[cardPhase(card, live)] ?? '')}`}
            aria-current={selected || undefined}
            data-loud={loud ?? undefined}
            style={stage ? { gridColumn: `1 / span ${stage.cols}`, gridRow: `1 / span ${stage.rows}` } : undefined}
          >
            {stage ? (
              <FleetSelectedCard card={card} live={live} selected={selected} faces={faces} now={now} loud={loud} onSelect={onSelect} />
            ) : (
              <FleetCard card={card} live={live} faces={faces} now={now} loud={loud} onSelect={handleSelect} />
            )}
          </article>
        );
      })}
    </div>
  );
};

export default memo(FleetGrid);
