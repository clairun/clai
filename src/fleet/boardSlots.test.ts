import { describe, expect, it } from 'vitest';
import { boardSlots, hiddenActivity } from './boardSlots';
import { emptyCard, type FleetCard, type FleetLive } from './fleetModel';

const card = (id: string, lastActivityAt: number, waiting = false): FleetCard => ({
  ...emptyCard(id, id),
  lastActivityAt,
  requests: waiting
    ? { 'ask:q': { kind: 'ask', ask: { pendingId: 'q', question: '?', options: null, extraContext: null, sessionId: 's' }, text: '?', at: lastActivityAt } }
    : {},
} as FleetCard);

/** Six idle cards c0…c5 (c0 most recent) and two more off the board, h6 and h7. */
const ids = ['c0', 'c1', 'c2', 'c3', 'c4', 'c5', 'h6', 'h7'];
const cardsOf = (waiting: string[] = []) =>
  Object.fromEntries(ids.map((id, i) => [id, card(id, 1000 - i, waiting.includes(id))]));
const working = (...ws: string[]): FleetLive => ({ runs: Object.fromEntries(ws.map((w) => [w, ['run']])), tasks: {} });
const NO_LIVE = working();
const slots = (opts: { cards?: Record<string, FleetCard>; live?: FleetLive; selectedId?: string | null; prev?: string[]; capacity?: number } = {}) =>
  boardSlots(ids, opts.cards ?? cardsOf(), {
    live: opts.live ?? NO_LIVE,
    selectedId: opts.selectedId ?? null,
    prev: opts.prev ?? [],
    capacity: opts.capacity ?? 6,
  });
const FULL = ['c0', 'c1', 'c2', 'c3', 'c4', 'c5'];

describe('boardSlots', () => {
  it('shows no more cards than fit, the most pressing first, then the most recent', () => {
    expect(slots().visible).toEqual(FULL);
    expect(slots({ capacity: 4 }).visible).toEqual(['c0', 'c1', 'c2', 'c3']);
    expect(slots({ live: working('h7'), cards: cardsOf(['h6']) }).visible).toEqual(['h6', 'h7', 'c0', 'c1', 'c2', 'c3']);
    // Too many on the board for the capacity: the least pressing go.
    expect(slots({ prev: [...FULL, 'h6'], live: working('h6') }).visible).toEqual(['c0', 'c1', 'c2', 'c3', 'c4', 'h6']);
  });

  it('keeps cards in their slots; a card that starts working takes the least recent idle slot only', () => {
    expect(slots({ prev: FULL, live: working('h7') }).visible).toEqual(['c0', 'c1', 'c2', 'c3', 'c4', 'h7']);
    expect(slots({ prev: ['c5', 'c4', 'c3', 'c2', 'c1', 'c0'], live: working('h7') }).visible).toEqual(['h7', 'c4', 'c3', 'c2', 'c1', 'c0']);
    // A card needing you outranks a working one.
    const allWorking = working('c0', 'c1', 'c2', 'c3', 'c4', 'c5');
    expect(slots({ prev: FULL, live: allWorking, cards: cardsOf(['h6']) }).visible).toEqual(['c0', 'c1', 'c2', 'c3', 'c4', 'h6']);
  });

  it('never lets an idle card push in, and ties stay where they are', () => {
    const fresher = { ...cardsOf(), h6: card('h6', 5000) };
    expect(slots({ prev: FULL, cards: fresher }).visible).toEqual(FULL);
    const allWorking = working('c0', 'c1', 'c2', 'c3', 'c4', 'c5', 'h6');
    expect(slots({ prev: FULL, live: allWorking, cards: fresher }).visible).toEqual(FULL);
  });

  it('always shows the selected card, and keeps it after deselecting', () => {
    const picked = slots({ prev: FULL, selectedId: 'h7', live: working('c0', 'c1', 'c2', 'c3', 'c4', 'c5') });
    expect(picked.visible).toEqual(['c0', 'c1', 'c2', 'c3', 'c4', 'h7']);
    expect(slots({ prev: picked.visible }).visible).toEqual(picked.visible);
  });

  it('fills a freed slot from off the board', () => {
    const left = boardSlots(ids.filter((id) => id !== 'c2'), cardsOf(), { live: NO_LIVE, selectedId: null, prev: FULL, capacity: 6 });
    expect(left.visible).toEqual(['c0', 'c1', 'c3', 'c4', 'c5', 'h6']);
  });

  it('counts the active cards left off the board', () => {
    const allWaiting = cardsOf([...FULL, 'h7']);
    expect(slots({ prev: FULL, live: working('h6'), cards: allWaiting }).hidden).toEqual({ working: 1, waiting: 1 });
    expect(slots({ prev: FULL }).hidden).toEqual({ working: 0, waiting: 0 });
  });
});

describe('hiddenActivity', () => {
  it('says what is left off the board, or nothing when every active card is on it', () => {
    expect(hiddenActivity({ working: 2, waiting: 1 })).toBe('2 more working · 1 needs you');
    expect(hiddenActivity({ working: 1, waiting: 0 })).toBe('1 more working');
    expect(hiddenActivity({ working: 0, waiting: 3 })).toBe('3 more need you');
    expect(hiddenActivity({ working: 0, waiting: 0 })).toBeNull();
  });
});
