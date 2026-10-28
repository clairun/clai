import { RECENT_WINDOW_MS, cardPhase, type FleetCard, type FleetLive } from './fleetModel';

interface BoardSlots {
  /** The cards on the board, in slot order; at most the capacity. */
  visible: string[];
  /** Cards off the board that are still active. */
  hidden: { working: number; waiting: number };
}

/**
 * Which cards fit on a board that never scrolls. Cards on it keep their
 * slots; a free slot takes the most pressing card off the board. A card off
 * the board takes the slot of the least pressing card on it only when it
 * outranks it: selected > needs you > working > idle (an idle card never
 * pushes in; ties stay). Within a rank the least recent card goes first.
 */
export const boardSlots = (
  order: readonly string[],
  cards: Record<string, FleetCard>,
  options: { live: FleetLive; selectedId: string | null; prev: readonly string[]; capacity: number }
): BoardSlots => {
  const { live, selectedId, prev, capacity } = options;
  const phase = new Map(order.map((id) => [id, cardPhase(cards[id]!, live)]));
  const rank = (id: string) => (id === selectedId ? 0 : { waiting: 1, working: 2, stopped: 3 }[phase.get(id)!]);
  // Most pressing first.
  const byPressing = (a: string, b: string) => rank(a) - rank(b) || cards[b]!.lastActivityAt - cards[a]!.lastActivityAt;
  const least = (slots: readonly string[]) => slots.reduce((low, id) => (byPressing(id, low) > 0 ? id : low));

  let visible = prev.filter((id) => phase.has(id));
  while (visible.length > capacity) visible = visible.filter((id) => id !== least(visible));
  const off = order.filter((id) => !visible.includes(id)).sort(byPressing);
  while (visible.length < capacity && off.length > 0) visible.push(off.shift()!);
  while (off.length > 0 && visible.length > 0) {
    const low = least(visible);
    if (rank(off[0]!) >= rank(low)) break;
    visible[visible.indexOf(low)] = off.shift()!;
    off.push(low);
    off.sort(byPressing);
  }
  return {
    visible,
    hidden: {
      working: off.filter((id) => phase.get(id) === 'working').length,
      waiting: off.filter((id) => phase.get(id) === 'waiting').length,
    },
  };
};

/** "2 more working · 1 needs you", or null when every active card is on the board. */
export const hiddenActivity = ({ working, waiting }: BoardSlots['hidden']): string | null => {
  const needs = `${waiting === 1 ? 'needs' : 'need'} you`;
  if (working > 0) return waiting > 0 ? `${working} more working · ${waiting} ${needs}` : `${working} more working`;
  return waiting > 0 ? `${waiting} more ${needs}` : null;
};

// ── Which cards ─────────────────────────────────────────────────────────

/**
 * The cards the Fleet shows: the selected card, anything live, and
 * workspaces active within `RECENT_WINDOW_MS`. Their order is the board's
 * (`boardSlots`) and the switcher's (`recentlyOpened`).
 */
export const eligibleCards = (
  cards: readonly FleetCard[],
  options: { now: number; selectedId: string | null; live: FleetLive }
): string[] => {
  const { now, selectedId, live } = options;
  return cards
    .filter(
      (card) =>
        card.workspaceId === selectedId || cardPhase(card, live) !== 'stopped' || card.lastActivityAt >= now - RECENT_WINDOW_MS
    )
    .map((card) => card.workspaceId);
};

/**
 * The switcher's order: most recently opened by the user first, so it doesn't
 * reshuffle while agents work; never opened, or a tie, by latest activity.
 */
export const recentlyOpened = (ids: readonly string[], cards: Record<string, FleetCard>): string[] =>
  [...ids].sort((a, b) => {
    const ca = cards[a]!;
    const cb = cards[b]!;
    return cb.lastOpenedAt - ca.lastOpenedAt || cb.lastActivityAt - ca.lastActivityAt;
  });

/** The grid: columns and rows; the board never scrolls (`boardSlots`). */
interface BoardShape {
  cols: number;
  rows: number;
}

/**
 * Six cards 3 × 2, the rest off the board; with fewer, each grows. Never fewer than
 * two columns, so a lone card's bubbles don't stretch into very long lines.
 */
export const boardShape = (count: number): BoardShape => {
  if (count <= 3) return { cols: Math.max(2, count), rows: 1 };
  return count === 4 ? { cols: 2, rows: 2 } : { cols: 3, rows: 2 };
};
