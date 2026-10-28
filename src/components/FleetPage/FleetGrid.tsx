import { memo, useCallback, useMemo, useRef, useState, type CSSProperties } from 'react';
import { Link } from 'react-router';
import type { AgentIdentity } from '../Agents/agentIdentity';
import {
  cardPhase,
  openRequests,
  type FleetCard as FleetCardModel,
  type FleetLive,
  type FleetPhase,
} from '../../fleet/fleetModel';
import { cardFeed } from '../../fleet/fleetFeed';
import { fleetLoudness } from '../../fleet/fleetCrew';
import { boardShape, boardSlots, hiddenActivity } from '../../fleet/boardSlots';
import { useAppTheme } from '../../hooks/useAppTheme';
import { workspaceHue } from '../../fleet/workspaceHue';
import { dispatchFocusTerminal } from '../../utils/workspaceUiEvents';
import FleetCard from './FleetCard';
import { accentStyle } from './FleetFace';
import fx from './FleetEffects';
import { useThoughtBeat } from './useThoughtBeat';
import styles from './FleetPage.module.css';

/** A card's border by phase; the selected card's own border wins. */
const PHASE_BORDER: Partial<Record<FleetPhase, string>> = { working: styles.cardWorking, waiting: styles.cardWaiting };

interface FleetGridProps {
  /** Card ids in order, each one in `cards` (`eligibleCards`). */
  order: string[];
  cards: Record<string, FleetCardModel>;
  selectedId: string | null;
  live: FleetLive;
  faces: Record<string, AgentIdentity>;
  /** The clock the panels are judged against: freshness, shouts and captions. */
  now: number;
  onSelect: (workspaceId: string | null) => void;
  /** Leave for the workspace's own page. */
  onOpen: (workspaceId: string) => void;
}

/**
 * The board: every card the same size (`boardShape`), never scrolling; when
 * there are more cards than fit, `boardSlots` picks the ones on it and a line
 * under it counts the active ones left off. The selected one is only
 * highlighted. Nothing on the board animates (on WebKitGTK even a short
 * transition blinks); only the zoom, a portal outside it, does. The drawn
 * effects switch on here.
 */
const FleetGrid = ({ order, cards, selectedId, live, faces, now, onSelect, onOpen }: FleetGridProps) => {
  // Choosing a card readies the terminal for it, as the switcher does.
  const handleSelect = useCallback(
    (id: string) => {
      onSelect(id);
      dispatchFocusTerminal();
    },
    [onSelect]
  );
  const gridRef = useRef<HTMLDivElement | null>(null);
  const theme = useAppTheme();
  const shape = boardShape(order.length);
  // The previous slots, so cards on the board keep them (derived state;
  // the slots are a fixed point of themselves).
  const [slots, setSlots] = useState<string[]>([]);
  const capacity = shape.cols * shape.rows;
  const board = useMemo(
    () => boardSlots(order, cards, { live, selectedId, prev: slots, capacity }),
    [order, cards, live, selectedId, slots, capacity]
  );
  if (board.visible.join(',') !== slots.join(',')) setSlots(board.visible);
  const { visible } = board;
  const more = hiddenActivity(board.hidden);
  useThoughtBeat(gridRef, visible.some((id) => cardPhase(cards[id]!, live) === 'working'));
  // Each card's feed, once. A card with an open request shows that instead.
  const panels = useMemo(
    () =>
      visible.map((id) => {
        const card = cards[id]!;
        const feed = openRequests(card).length > 0 ? null : cardFeed(card, live, now);
        return { card, feed, selected: id === selectedId, hue: workspaceHue(id) };
      }),
    [visible, cards, live, now, selectedId]
  );
  const loud = useMemo(() => fleetLoudness(panels), [panels]);
  const deselect = (e: React.MouseEvent) => {
    if (e.target === e.currentTarget) onSelect(null);
  };

  return (
    <>
    <div ref={gridRef} className={`${styles.board} ${fx.fx}`} onClick={deselect}>
      <div className={styles.cards} style={{ '--cols': shape.cols, '--rows': shape.rows } as CSSProperties} onClick={deselect}>
        {panels.map(({ card, feed, selected, hue }) => {
          const cardLoud = loud.cards[card.workspaceId] ?? null;
          return (
            <article
              key={card.workspaceId}
              className={`${styles.card} ${selected ? styles.cardSelected : (PHASE_BORDER[cardPhase(card, live)] ?? '')}`}
              aria-current={selected || undefined}
              data-loud={cardLoud ?? undefined}
              data-screentone
              style={accentStyle(hue, theme)}
            >
              <FleetCard card={card} feed={feed} shouts={loud.rows} live={live} faces={faces} now={now} loud={cardLoud} onSelect={handleSelect} onOpen={onOpen} />
            </article>
          );
        })}
      </div>
    </div>
    {more && (
      <p className={styles.offBoard}>
        {more} — Ctrl+Tab to switch, or open the <Link to="/workspace">Workspace view</Link>
      </p>
    )}
    </>
  );
};

export default memo(FleetGrid);
