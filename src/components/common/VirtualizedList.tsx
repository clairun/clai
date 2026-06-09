import React, {
  memo,
  useCallback,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
} from 'react';

const DEFAULT_ESTIMATE_SIZE = 160;
const DEFAULT_OVERSCAN = 900;
const NEAR_BOTTOM_THRESHOLD = 180;
// How close to the top (px) a user-initiated upward scroll must get before
// onApproachTop fires. Generous on purpose: pages prepend while the reader
// still has runway, so they never hit a hard top edge mid-read.
const NEAR_TOP_THRESHOLD = 600;

interface Position {
  top: number;
  size: number;
  key: string;
}

interface MeasuredItemProps {
  cacheKey: string;
  top: number;
  children: React.ReactNode;
  onMeasure: (key: string, height: number) => void;
}

const MeasuredItem = memo(({ cacheKey, top, children, onMeasure }: MeasuredItemProps) => {
  const ref = useRef<HTMLDivElement | null>(null);

  useLayoutEffect(() => {
    const node = ref.current;
    if (!node) return undefined;

    const measure = () => {
      onMeasure(cacheKey, node.getBoundingClientRect().height);
    };

    measure();

    if (typeof ResizeObserver === 'undefined') {
      return undefined;
    }

    const observer = new ResizeObserver(measure);
    observer.observe(node);
    return () => observer.disconnect();
  }, [cacheKey, onMeasure]);

  return (
    <div
      ref={ref}
      style={{
        position: 'absolute',
        top,
        left: 0,
        right: 0,
        width: '100%',
      }}
    >
      {children}
    </div>
  );
});

MeasuredItem.displayName = 'MeasuredItem';

const findWindow = (
  positions: Position[],
  scrollTop: number,
  viewportHeight: number,
  overscan: number,
): { start: number; end: number } => {
  if (positions.length === 0) {
    return { start: 0, end: -1 };
  }

  const startPx = Math.max(0, scrollTop - overscan);
  const endPx = scrollTop + viewportHeight + overscan;

  let start = 0;
  while (
    start < positions.length - 1
    && positions[start]!.top + positions[start]!.size < startPx
  ) {
    start += 1;
  }

  let end = start;
  while (end < positions.length && positions[end]!.top < endPx) {
    end += 1;
  }

  return {
    start,
    end: Math.min(positions.length - 1, end),
  };
};

interface LayoutResult {
  positions: Position[];
  positionsByKey: Map<string, Position>;
  totalHeight: number;
  start: number;
  end: number;
  footerTop: number;
}

interface VirtualizedListProps<T> {
  items: T[];
  itemKey: (item: T, index?: number) => string;
  renderItem: (item: T, index: number) => React.ReactNode;
  className?: string;
  estimateSize?: number;
  overscan?: number;
  gap?: number;
  footer?: React.ReactNode;
  footerEstimateSize?: number;
  initialScrollToBottom?: boolean;
  scrollToBottomSignal?: number | null;
  scrollToBottomBehavior?: ScrollBehavior;
  // When this key changes to a new non-null value (e.g. the id of the user
  // message that was just sent), scroll to the bottom unconditionally — even
  // if the user had scrolled up — and resume following new content.
  forceScrollToBottomKey?: string | number | null;
  onNearBottomChange?: (isNearBottom: boolean) => void;
  // Fired when a *user-initiated* upward scroll brings the viewport within
  // NEAR_TOP_THRESHOLD of the top (and on wheel-up while already pinned at
  // the top). Programmatic adjustments — stick-to-bottom pins, the prepend
  // scroll-anchor correction — never fire it, so loading a page can't
  // cascade into loading the next one by itself. May fire repeatedly while
  // the user lingers near the top; callers are expected to gate re-entrancy
  // (e.g. an isLoading flag).
  onApproachTop?: () => void;
}

const VirtualizedListInner = <T,>({
  items,
  itemKey,
  renderItem,
  className,
  estimateSize = DEFAULT_ESTIMATE_SIZE,
  overscan = DEFAULT_OVERSCAN,
  gap = 0,
  footer = null,
  footerEstimateSize = 1,
  initialScrollToBottom = false,
  scrollToBottomSignal = null,
  scrollToBottomBehavior = 'auto',
  forceScrollToBottomKey = null,
  onNearBottomChange,
  onApproachTop,
}: VirtualizedListProps<T>) => {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const heightsRef = useRef<Map<string, number>>(new Map());
  const rafRef = useRef<number | null>(null);
  const didInitialScrollRef = useRef(false);
  const prevLayoutRef = useRef<LayoutResult | null>(null);
  // Synchronous mirror of "should new content pull the view down", updated on
  // every scroll. The scroll-to-bottom effects read this to decide whether to
  // follow new content — a new message appended below doesn't fire a scroll
  // or resize event, so this ref still reflects where the user was *before*
  // the update, which is exactly the signal we want.
  const nearBottomRef = useRef(true);
  // Last seen scrollTop, to tell user "scrolled up" apart from "scrolled
  // down"; and a flag marking the next scroll event as programmatic (our own
  // pin/anchor adjustments), so it's never misread as user intent.
  const lastScrollTopRef = useRef(0);
  const programmaticScrollRef = useRef(false);
  const [measurementVersion, setMeasurementVersion] = useState(0);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });
  const [footerHeight, setFooterHeight] = useState(footer ? footerEstimateSize : 0);

  // Latest onApproachTop behind a stable ref, so callers passing an inline
  // callback don't churn the scroll-handler chain (updateFollowState →
  // syncViewport → handleScroll + its ResizeObserver) on every render.
  // Writing `ref.current` directly during render trips the `react-hooks/refs`
  // lint rule, so the mirror lives in an effect keyed on onApproachTop.
  const onApproachTopRef = useRef(onApproachTop);
  useEffect(() => {
    onApproachTopRef.current = onApproachTop;
  }, [onApproachTop]);

  const scrollToBottom = useCallback((behavior: ScrollBehavior = 'auto') => {
    const node = scrollRef.current;
    if (!node) return;

    window.requestAnimationFrame(() => {
      const current = scrollRef.current;
      if (!current) return;
      const top = Math.max(0, current.scrollHeight - current.clientHeight);
      if (Math.abs(current.scrollTop - top) < 1) return;
      programmaticScrollRef.current = true;
      current.scrollTo({ top, behavior });
    });
  }, []);

  // Recompute the follow-the-bottom state. Runs synchronously from the scroll
  // handler (NOT in a rAF) — deferring it a frame lets the stick-to-bottom
  // effect read a stale "near bottom" and yank the view down right after the
  // user scrolled up. A user-initiated upward scroll breaks following
  // immediately, even inside the near-bottom threshold; otherwise fast
  // streaming re-pins the view faster than the user can escape it. Scrolling
  // back to within the threshold of the bottom re-engages following.
  const updateFollowState = useCallback(() => {
    const node = scrollRef.current;
    if (!node) return;

    const { scrollTop } = node;
    const distanceFromBottom = node.scrollHeight - scrollTop - node.clientHeight;
    const scrolledUp = scrollTop < lastScrollTopRef.current - 1;
    const scrolledDown = scrollTop > lastScrollTopRef.current + 1;
    lastScrollTopRef.current = scrollTop;
    const wasProgrammatic = programmaticScrollRef.current;
    programmaticScrollRef.current = false;

    let isNearBottom: boolean;
    if (scrolledUp && !wasProgrammatic && distanceFromBottom > 1) {
      isNearBottom = false;
      // User is actively heading for older content and is close to the top —
      // ask for more. Gated on user intent (not raw position) so the
      // scroll-anchor delta applied after a prepend can never chain-load.
      if (scrollTop < NEAR_TOP_THRESHOLD) {
        onApproachTopRef.current?.();
      }
    } else if (scrolledUp && !wasProgrammatic) {
      // scrollTop dropped but we're still glued to the bottom: that's the
      // browser clamping scrollTop after the content got shorter (an item
      // re-measuring smaller, or a re-keyed group falling back to the size
      // estimate until it re-measures) — not the user scrolling up. The clamp
      // isn't one of our writes, so the programmatic flag can't cover it.
      // Treating it as user intent silently broke following mid-stream.
      isNearBottom = true;
    } else if (scrolledDown || wasProgrammatic) {
      isNearBottom = distanceFromBottom < NEAR_BOTTOM_THRESHOLD;
    } else {
      // Passive recompute (mount, resize, the rAF viewport resync) — the
      // position didn't move, so the user's intent didn't change. Re-deriving
      // from distance here would undo a within-threshold scroll-up one frame
      // later.
      isNearBottom = nearBottomRef.current;
    }
    nearBottomRef.current = isNearBottom;
    onNearBottomChange?.(isNearBottom);
  }, [onNearBottomChange]);

  const syncViewport = useCallback(() => {
    const node = scrollRef.current;
    if (!node) return;

    const next = {
      scrollTop: node.scrollTop,
      height: node.clientHeight,
    };

    setViewport((current) => (
      current.scrollTop === next.scrollTop && current.height === next.height
        ? current
        : next
    ));

    updateFollowState();
  }, [updateFollowState]);

  const handleScroll = useCallback(() => {
    updateFollowState();
    if (rafRef.current) return;
    rafRef.current = window.requestAnimationFrame(() => {
      rafRef.current = null;
      syncViewport();
    });
  }, [syncViewport, updateFollowState]);

  // Wheel-up is the earliest possible "I want to read older content" signal —
  // it fires before the scroll position even changes, so breaking the follow
  // here closes the race where a streaming commit pins the view between the
  // user's scroll and its scroll event. Guarded on scrollTop > 0: wheeling up
  // while already at the top produces no scroll, and must not strand a
  // fits-in-viewport conversation in the unfollowed state.
  const handleWheel = useCallback((event: React.WheelEvent<HTMLDivElement>) => {
    if (event.deltaY >= 0) return;
    const node = scrollRef.current;
    if (!node) return;
    if (node.scrollTop <= 0) {
      // Already pinned at the top (or the content fits the viewport, so no
      // scroll event will ever fire) — wheel-up is still an explicit "show
      // me older content".
      onApproachTopRef.current?.();
      return;
    }
    if (nearBottomRef.current) {
      nearBottomRef.current = false;
      onNearBottomChange?.(false);
    }
  }, [onNearBottomChange]);

  useLayoutEffect(() => {
    syncViewport();
    const node = scrollRef.current;
    if (!node || typeof ResizeObserver === 'undefined') return undefined;

    const observer = new ResizeObserver(syncViewport);
    observer.observe(node);
    return () => observer.disconnect();
  }, [syncViewport]);

  useEffect(() => () => {
    if (rafRef.current) {
      window.cancelAnimationFrame(rafRef.current);
    }
  }, []);

  useEffect(() => {
    const liveKeys = new Set(items.map((item, index) => itemKey(item, index)));
    let removed = false;
    for (const key of heightsRef.current.keys()) {
      if (!liveKeys.has(key) && key !== '__footer__') {
        heightsRef.current.delete(key);
        removed = true;
      }
    }
    if (removed) {
      setMeasurementVersion((version) => version + 1);
    }
  }, [items, itemKey]);

  const handleMeasure = useCallback((key: string, height: number) => {
    if (!Number.isFinite(height) || height <= 0) return;

    const current = heightsRef.current.get(key);
    if (current !== undefined && Math.abs(current - height) < 1) return;

    heightsRef.current.set(key, height);
    setMeasurementVersion((version) => version + 1);
  }, []);

  const handleFooterMeasure = useCallback((height: number) => {
    if (!Number.isFinite(height) || height < 0) return;
    setFooterHeight((current) => (Math.abs(current - height) < 1 ? current : height));
  }, []);

  // `measurementVersion` is a deliberate trigger dep: heightsRef is a ref
  // (not state), so React's hook dep tracking can't observe its mutations.
  // Bumping `measurementVersion` after writing to the ref forces this memo
  // to recompute and pick up the new cached heights.
  /* eslint-disable react-hooks/exhaustive-deps */
  const layout = useMemo<LayoutResult>(() => {
    const positions: Position[] = [];
    const positionsByKey = new Map<string, Position>();
    let top = 0;

    for (let index = 0; index < items.length; index += 1) {
      const key = itemKey(items[index]!, index);
      const cached = heightsRef.current.get(key);
      const size = cached !== undefined ? cached : estimateSize;
      const entry = { top, size, key };
      positions.push(entry);
      positionsByKey.set(key, entry);
      top += size;
      if (index < items.length - 1) {
        top += gap;
      }
    }

    const windowRange = findWindow(positions, viewport.scrollTop, viewport.height, overscan);

    return {
      positions,
      positionsByKey,
      totalHeight: top + (footer ? footerHeight : 0),
      start: windowRange.start,
      end: windowRange.end,
      footerTop: top,
    };
  }, [
    items,
    itemKey,
    estimateSize,
    gap,
    viewport.scrollTop,
    viewport.height,
    overscan,
    // `measurementVersion` is a deliberate trigger dep: heightsRef is a ref
    // (not state), so React's hook dep tracking can't observe its mutations.
    // Bumping `measurementVersion` after writing to the ref forces this memo
    // to recompute and pick up the new cached heights. The eslint-disable
    // is on the useMemo call line above.
    measurementVersion,
    footer,
    footerHeight,
  ]);
  /* eslint-enable react-hooks/exhaustive-deps */

  useLayoutEffect(() => {
    if (!initialScrollToBottom || didInitialScrollRef.current || items.length === 0) return;
    didInitialScrollRef.current = true;
    scrollToBottom('auto');
  }, [initialScrollToBottom, items.length, layout.totalHeight, scrollToBottom]);

  useEffect(() => {
    if (scrollToBottomSignal === null || scrollToBottomSignal === undefined) return;
    if (!didInitialScrollRef.current && initialScrollToBottom) return;
    // Only follow new content if the user was already at/near the bottom.
    // If they've scrolled up to read earlier messages, leave them there
    // instead of yanking the view back down on every update.
    if (!nearBottomRef.current) return;
    scrollToBottom(scrollToBottomBehavior);
  }, [initialScrollToBottom, scrollToBottom, scrollToBottomBehavior, scrollToBottomSignal]);

  useLayoutEffect(() => {
    const node = scrollRef.current;
    const prev = prevLayoutRef.current;
    prevLayoutRef.current = layout;

    if (!node) return;
    // Skip while at the bottom — the pin effect below owns the position there,
    // following new/re-measured content down. Gated on the live near-bottom
    // ref (not on whether a run is streaming): the initial-load and
    // post-completion settle windows aren't streaming, but a reader sitting at
    // the bottom still wants to stay there as rows grow from their size
    // estimate. A reader who scrolled up flips the ref false and lands here, so
    // their position stays anchored while items re-measure.
    if (nearBottomRef.current) return;
    if (!prev || prev.positions.length === 0 || layout.positions.length === 0) return;

    const currentScrollTop = node.scrollTop;

    let lo = 0;
    let hi = prev.positions.length - 1;
    let anchorIdx = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (prev.positions[mid]!.top <= currentScrollTop) {
        anchorIdx = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }

    const anchor = prev.positions[anchorIdx];
    if (!anchor) return;
    const newPos = layout.positionsByKey.get(anchor.key);
    if (!newPos) return;

    const delta = newPos.top - anchor.top;
    if (Math.abs(delta) >= 1) {
      programmaticScrollRef.current = true;
      node.scrollTop = currentScrollTop + delta;
    }
  }, [layout]);

  // Pin to the bottom whenever content grows and the reader is at the bottom.
  // Gated solely on the synchronous follow ref — not on whether a run is
  // streaming — so the initial load and the moment after a completion (rows
  // settling from their size estimate to real height) keep the view glued to
  // the latest content instead of drifting up. The ref is the user-intent
  // guard: a scroll/wheel-up flips it false before this runs, so it can't
  // fight the user. Applied synchronously (not via scrollToBottom's rAF) so a
  // stale queued frame can never undo a scroll-up that happened in between.
  useLayoutEffect(() => {
    if (!nearBottomRef.current) return;
    const node = scrollRef.current;
    if (!node) return;
    const top = Math.max(0, node.scrollHeight - node.clientHeight);
    if (node.scrollTop >= top - 1) return;
    programmaticScrollRef.current = true;
    node.scrollTop = top;
  }, [layout.totalHeight]);

  // Unconditional jump to the bottom — the user just sent a message (or an
  // equivalent "take me to the latest" event keyed by forceScrollToBottomKey).
  // Also re-engages following so the upcoming response streams into view.
  const lastForceKeyRef = useRef<string | number | null | undefined>(undefined);
  useEffect(() => {
    const prev = lastForceKeyRef.current;
    lastForceKeyRef.current = forceScrollToBottomKey;
    if (forceScrollToBottomKey == null) return;
    // First render: initialScrollToBottom owns the initial position.
    if (prev === undefined) return;
    if (prev === forceScrollToBottomKey) return;
    nearBottomRef.current = true;
    onNearBottomChange?.(true);
    scrollToBottom(scrollToBottomBehavior);
  }, [forceScrollToBottomKey, onNearBottomChange, scrollToBottom, scrollToBottomBehavior]);

  const visibleItems: { item: T; index: number; position: Position }[] = [];
  for (let index = layout.start; index <= layout.end; index += 1) {
    const item = items[index];
    const position = layout.positions[index];
    if (!item || !position) continue;
    visibleItems.push({ item, index, position });
  }

  return (
    <div ref={scrollRef} className={className} onScroll={handleScroll} onWheel={handleWheel}>
      <div
        style={{
          position: 'relative',
          height: layout.totalHeight,
          minHeight: '100%',
          flex: '0 0 auto',
        }}
      >
        {visibleItems.map(({ item, index, position }) => (
          <MeasuredItem
            key={position.key}
            cacheKey={position.key}
            top={position.top}
            onMeasure={handleMeasure}
          >
            {renderItem(item, index)}
          </MeasuredItem>
        ))}

        {footer && (
          <div
            ref={(node) => {
              if (node) {
                handleFooterMeasure(node.getBoundingClientRect().height);
              }
            }}
            style={{
              position: 'absolute',
              top: layout.footerTop,
              left: 0,
              right: 0,
              width: '100%',
            }}
          >
            {footer}
          </div>
        )}
      </div>
    </div>
  );
};

// memo erases the generic signature, so re-assert it on the export.
// The cast is sound: VirtualizedListInner is already generic and memo
// only adds prop-equality short-circuiting, not a type change.
const VirtualizedList = memo(VirtualizedListInner) as typeof VirtualizedListInner;

export default VirtualizedList;
