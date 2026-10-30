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
import { BOARD_CAPACITY, boardShape, boardSlots, hiddenActivity } from '../../fleet/boardSlots';
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
  /** Take a card off the board until something happens in it. */
  onDismiss: (workspaceId: string) => void;
}

/**
 * The board: every card the same size (`boardShape`), never scrolling; when
 * there are more cards than fit, `boardSlots` picks the ones on it and a line
 * under it counts the active ones left off. The selected one is only
 * highlighted. Nothing on the board animates (on WebKitGTK even a short
 * transition blinks); only the zoom, a portal outside it, does. The drawn
 * effects switch on here.
 */
const FleetGrid = ({ order, cards, selectedId, live, faces, now, onSelect, onOpen, onDismiss }: FleetGridProps) => {
  // Choosing a card readies the terminal for it, as the switcher does.
  const handleSelect = useCallback(
    (id: string) => {
      onSelect(id);
      dispatchFocusTerminal();
    },
    [onSelect]
  );
  // The dismissed card's controls leave with it: the keyboard goes to the terminal.
  const handleDismiss = useCallback(
    (id: string) => {
      onDismiss(id);
      dispatchFocusTerminal();
    },
    [onDismiss]
  );
  const gridRef = useRef<HTMLDivElement | null>(null);
  const theme = useAppTheme();
  // The previous board, so cards on it keep their slots and cards left off
  // stay off (derived state; the board is a fixed point of itself).
  const [prev, setPrev] = useState<{ visible: string[]; off: string[] }>({ visible: [], off: [] });
  const board = useMemo(
    () => boardSlots(order, cards, { live, selectedId, prev, capacity: BOARD_CAPACITY }),
    [order, cards, live, selectedId, prev]
  );
  if (board.visible.join(',') !== prev.visible.join(',') || board.off.join(',') !== prev.off.join(',')) setPrev(board);
  const { visible } = board;
  const shape = boardShape(visible.length);
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
    {visible.length === 0 ? (
      // Every card shown was closed; the ones left off stay off.
      <div className={styles.empty}>
        <h2>The board is empty</h2>
        <p>
          {board.off.length} {board.off.length === 1 ? 'card is' : 'cards are'} off it; Ctrl+Tab brings one in.
        </p>
      </div>
    ) : (
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
              <FleetCard card={card} feed={feed} shouts={loud.rows} live={live} faces={faces} now={now} loud={cardLoud} onSelect={handleSelect} onOpen={onOpen} onDismiss={handleDismiss} />
            </article>
          );
        })}
      </div>
    </div>
    )}
    {more && (
      <p className={styles.offBoard}>
        {more} — Ctrl+Tab to switch, or open the <Link to="/workspace">Workspace view</Link>
      </p>
    )}
    </>
  );
};

export default memo(FleetGrid);
