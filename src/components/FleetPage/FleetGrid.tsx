import { memo, useCallback } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import { GRID_GAP, cardPhase, stageSpan, type FleetCard as FleetCardModel, type FleetLive } from '../../fleet/fleetModel';
import FleetCard from './FleetCard';
import FleetSelectedCard from './FleetSelectedCard';
import styles from './FleetPage.module.css';

interface FleetGridProps {
  /** Card ids in slot order; the first is on the stage. */
  order: string[];
  cards: Record<string, FleetCardModel>;
  cols: number;
  rows: number;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  onSelect: (workspaceId: string | null) => void;
}

/**
 * The card grid: the stage card, in its large form, spans the top-left
 * 2 × 2; the rest flow into the 1 × 1 slots around it. Nothing animates: on
 * WebKitGTK even a short transition blinks.
 */
const FleetGrid = ({ order, cards, cols, rows, selectedId, live, faces, onSelect }: FleetGridProps) => {
  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);

  return (
    <div
      className={`${styles.grid} ${selectedId && order.includes(selectedId) ? styles.gridSelecting : ''}`}
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
        return (
          <article
            key={id}
            className={`${styles.card} ${selected ? styles.cardSelected : cardPhase(card, live) === 'working' ? styles.cardWorking : ''}`}
            aria-current={selected || undefined}
            style={stage ? { gridColumn: `1 / span ${stage.cols}`, gridRow: `1 / span ${stage.rows}` } : undefined}
          >
            {stage ? (
              <FleetSelectedCard card={card} live={live} selected={selected} onSelect={onSelect} />
            ) : (
              <FleetCard card={card} live={live} faces={faces} onSelect={handleSelect} />
            )}
          </article>
        );
      })}
    </div>
  );
};

export default memo(FleetGrid);
