import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router';
import FleetGrid from '../components/FleetPage/FleetGrid';
import styles from '../components/FleetPage/FleetPage.module.css';
import {
  GRID_GAP,
  RECENT_WINDOW_MS,
  cardPhase,
  gridCapacity,
  placeCards,
  type PlacedCard,
} from '../fleet/fleetModel';
import { useFleetStore } from '../fleet/fleetStore';
import { listWorkspaces } from '../workspace/client';

const RECENT_HOURS = RECENT_WINDOW_MS / 3_600_000;

/** Content-box size of `ref`, updated only when it actually resizes. */
function useBoxSize(ref: React.RefObject<HTMLElement | null>) {
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const node = ref.current;
    if (!node || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(([entry]) => {
      if (!entry) return;
      const { width, height } = entry.contentRect;
      setSize((prev) =>
        Math.round(prev.width) === Math.round(width) && Math.round(prev.height) === Math.round(height)
          ? prev
          : { width, height }
      );
    });
    observer.observe(node);
    return () => observer.disconnect();
  }, [ref]);
  return size;
}

/**
 * The Fleet: one card per recently active workspace, sized to the screen,
 * with the shared terminal at the bottom. Clicking a card selects it — it
 * grows into the workspace's chat and the terminal talks to it.
 */
const FleetPage = () => {
  const navigate = useNavigate();
  const cards = useFleetStore((s) => s.fleet.cards);
  const now = useFleetStore((s) => s.now);
  const selectedId = useFleetStore((s) => s.selectedId);
  const select = useFleetStore((s) => s.select);
  const motion = useFleetStore((s) => s.motion);
  const setMotion = useFleetStore((s) => s.setMotion);
  const loadedAt = useFleetStore((s) => s.loadedAt);
  const loadError = useFleetStore((s) => s.loadError);
  const setTitles = useFleetStore((s) => s.setTitles);

  useEffect(() => {
    void useFleetStore.getState().loadSnapshot();
  }, []);

  const gridRef = useRef<HTMLDivElement | null>(null);
  const { width, height } = useBoxSize(gridRef);
  const capacity = useMemo(() => gridCapacity(width, height), [width, height]);
  const rowHeight = capacity.rows > 0 ? (height - GRID_GAP * (capacity.rows - 1)) / capacity.rows : 0;

  // The previous order, so cards already on screen keep their place.
  // Updated during render (React's derived-state pattern); placement is a
  // fixed point of its own order, so this settles in one extra pass.
  const [order, setOrder] = useState<string[]>([]);
  const placed: PlacedCard[] = useMemo(
    () =>
      width > 0
        ? placeCards(Object.values(cards), { now, capacity, selectedId, prevOrder: order })
        : [],
    [cards, now, capacity, selectedId, width, order]
  );
  const placedIds = placed.map((p) => p.id);
  if (placedIds.join(',') !== order.join(',')) setOrder(placedIds);

  // Cards born from live events (or a workspace just created here) have no
  // title yet: fetch the list once per new set of untitled cards.
  const untitled = placed.filter((p) => !cards[p.id]?.title).map((p) => p.id).join(',');
  useEffect(() => {
    if (!untitled) return;
    listWorkspaces()
      .then((list) => setTitles(Object.fromEntries(list.map((ws) => [ws.id, ws.title]))))
      .catch(() => {});
  }, [untitled, setTitles]);

  const counts = useMemo(() => {
    let working = 0;
    let waiting = 0;
    for (const p of placed) {
      const phase = cardPhase(cards[p.id]!);
      if (phase === 'working') working += 1;
      if (phase === 'waiting') waiting += 1;
    }
    return { working, waiting };
  }, [placed, cards]);

  const handleSelect = useCallback((id: string | null) => select(id), [select]);

  return (
    <div className={styles.page} data-motion={motion ? 'on' : 'off'}>
      <header className={styles.topBar}>
        <img src="/icon.svg" alt="Clai" className={styles.brandIcon} />
        <h1 className={styles.pageTitle}>Fleet</h1>
        <div className={styles.counters} role="status">
          <span>
            <strong>{counts.working}</strong> working
          </span>
          <span className={counts.waiting > 0 ? styles.countersAttention : ''}>
            <strong>{counts.waiting}</strong> need you
          </span>
        </div>
        <button
          type="button"
          className={styles.headerButton}
          aria-pressed={motion}
          onClick={() => setMotion(!motion)}
          title="Turn Fleet animations on or off"
        >
          Motion {motion ? 'on' : 'off'}
        </button>
        <button type="button" className={styles.headerButton} onClick={() => navigate('/workspace')}>
          Workspaces
        </button>
      </header>
      <main className={styles.gridArea} ref={gridRef}>
        {loadError && <p className={styles.notice}>{loadError}</p>}
        {placed.length > 0 ? (
          <FleetGrid
            placed={placed}
            cards={cards}
            cols={capacity.cols}
            rowHeight={rowHeight}
            selectedId={selectedId}
            motion={motion}
            onSelect={handleSelect}
          />
        ) : (
          loadedAt != null && (
            <div className={styles.empty}>
              <h2>Nothing active in the last {RECENT_HOURS} hours</h2>
              <p>Type below to start a new workspace, or open the workspaces page.</p>
            </div>
          )
        )}
      </main>
    </div>
  );
};

export default FleetPage;
