import { useCallback, type CSSProperties } from 'react';
import type { AgentIdentity } from '../Agents/agentIdentity';
import { GRID_GAP, workspaceHue, type FleetCard as FleetCardModel, type FleetLive } from '../../fleet/fleetModel';
import FleetCard from './FleetCard';
import FleetSelectedCard from './FleetSelectedCard';
import styles from './FleetPage.module.css';

interface FleetGridProps {
  /** Card ids in slot order. */
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
 * The card grid. Every card keeps its slot; the selected one grows over its
 * neighbours from that slot, shifted inward at the grid's edges. Nothing
 * animates: on WebKitGTK even a short transition blinks.
 */
const FleetGrid = ({ order, cards, cols, rows, selectedId, live, faces, onSelect }: FleetGridProps) => {
  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);
  const handleClose = useCallback(() => onSelect(null), [onSelect]);

  return (
    <div
      className={styles.grid}
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
        const w = selected ? Math.min(2, cols) : 1;
        const h = selected ? Math.min(2, rows) : 1;
        const col = Math.min(slot % cols, cols - w);
        const row = Math.min(Math.floor(slot / cols), rows - h);
        return (
          <article
            key={id}
            className={`${styles.card} ${selected ? styles.cardSelected : ''}`}
            aria-current={selected || undefined}
            style={
              {
                gridColumn: `${col + 1} / span ${w}`,
                gridRow: `${row + 1} / span ${h}`,
                '--ws-hue': workspaceHue(id),
              } as CSSProperties
            }
          >
            {selected ? (
              <FleetSelectedCard card={card} live={live} onClose={handleClose} />
            ) : (
              <FleetCard card={card} live={live} faces={faces} onSelect={handleSelect} />
            )}
          </article>
        );
      })}
    </div>
  );
};

export default FleetGrid;
