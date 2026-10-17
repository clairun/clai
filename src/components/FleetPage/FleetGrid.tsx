import { useCallback, useLayoutEffect, useRef, type CSSProperties } from 'react';
import {
  GRID_GAP,
  cardPhase,
  workspaceHue,
  type FleetCard as FleetCardModel,
  type PlacedCard,
} from '../../fleet/fleetModel';
import FleetCard from './FleetCard';
import FleetSelectedCard from './FleetSelectedCard';
import styles from './FleetPage.module.css';

const MOVE_MS = 240;

interface FleetGridProps {
  placed: PlacedCard[];
  cards: Record<string, FleetCardModel>;
  cols: number;
  rowHeight: number;
  selectedId: string | null;
  motion: boolean;
  onSelect: (workspaceId: string | null) => void;
}

/**
 * The card grid. When a card changes place (it gained attention, was
 * selected, a newcomer arrived), each moved card glides from its old spot
 * once (FLIP, 240 ms). Nothing runs at rest, and a resize just snaps.
 */
const FleetGrid = ({ placed, cards, cols, rowHeight, selectedId, motion, onSelect }: FleetGridProps) => {
  const nodes = useRef(new Map<string, HTMLElement>());
  const rects = useRef(new Map<string, DOMRect>());
  const geometry = useRef('');
  const layoutKey = placed.map((p) => `${p.id}:${p.cols}x${p.rows}`).join('|');

  useLayoutEffect(() => {
    const nextGeometry = `${cols}:${Math.round(rowHeight)}`;
    const animate = motion && geometry.current === nextGeometry;
    geometry.current = nextGeometry;
    const next = new Map<string, DOMRect>();
    for (const [id, node] of nodes.current) {
      const rect = node.getBoundingClientRect();
      next.set(id, rect);
      const prev = rects.current.get(id);
      if (!animate || !prev) continue;
      const dx = prev.left - rect.left;
      const dy = prev.top - rect.top;
      if (Math.abs(dx) < 1 && Math.abs(dy) < 1) continue;
      node.style.transition = 'none';
      node.style.transform = `translate(${dx}px, ${dy}px)`;
      void node.offsetWidth;
      node.style.transition = `transform ${MOVE_MS}ms cubic-bezier(0.2, 0, 0, 1)`;
      node.style.transform = '';
      const clear = () => {
        node.style.transition = '';
        node.removeEventListener('transitionend', clear);
      };
      node.addEventListener('transitionend', clear);
    }
    rects.current = next;
  }, [layoutKey, cols, rowHeight, motion]);

  const bind = useCallback(
    (id: string) => (node: HTMLElement | null) => {
      if (node) nodes.current.set(id, node);
      else nodes.current.delete(id);
    },
    []
  );

  const handleSelect = useCallback((id: string) => onSelect(id), [onSelect]);
  const handleClose = useCallback(() => onSelect(null), [onSelect]);

  return (
    <div
      className={styles.grid}
      style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))`, gridAutoRows: `${rowHeight}px`, gap: GRID_GAP }}
      onClick={(e) => {
        if (e.target === e.currentTarget) onSelect(null);
      }}
    >
      {placed.map(({ id, cols: spanCols, rows: spanRows }) => {
        const card = cards[id];
        if (!card) return null;
        const selected = id === selectedId;
        return (
          <article
            key={id}
            ref={bind(id)}
            className={`${styles.card} ${selected ? styles.cardSelected : ''}`}
            data-phase={cardPhase(card)}
            aria-current={selected || undefined}
            style={
              {
                gridColumn: `span ${spanCols}`,
                gridRow: `span ${spanRows}`,
                '--ws-hue': workspaceHue(id),
              } as CSSProperties
            }
          >
            {selected ? (
              <FleetSelectedCard card={card} onClose={handleClose} />
            ) : (
              <FleetCard card={card} onSelect={handleSelect} />
            )}
          </article>
        );
      })}
    </div>
  );
};

export default FleetGrid;
