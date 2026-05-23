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

const MeasuredItem = memo(({ cacheKey, top, children, onMeasure }) => {
  const ref = useRef(null);

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

const findWindow = (positions, scrollTop, viewportHeight, overscan) => {
  if (positions.length === 0) {
    return { start: 0, end: -1 };
  }

  const startPx = Math.max(0, scrollTop - overscan);
  const endPx = scrollTop + viewportHeight + overscan;

  let start = 0;
  while (
    start < positions.length - 1
    && positions[start].top + positions[start].size < startPx
  ) {
    start += 1;
  }

  let end = start;
  while (end < positions.length && positions[end].top < endPx) {
    end += 1;
  }

  return {
    start,
    end: Math.min(positions.length - 1, end),
  };
};

const VirtualizedList = memo(({
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
  stickToBottom = false,
  onNearBottomChange,
}) => {
  const scrollRef = useRef(null);
  const heightsRef = useRef(new Map());
  const rafRef = useRef(null);
  const didInitialScrollRef = useRef(false);
  const prevLayoutRef = useRef(null);
  const [measurementVersion, setMeasurementVersion] = useState(0);
  const [viewport, setViewport] = useState({ scrollTop: 0, height: 0 });
  const [footerHeight, setFooterHeight] = useState(footer ? footerEstimateSize : 0);

  const scrollToBottom = useCallback((behavior = 'auto') => {
    const node = scrollRef.current;
    if (!node) return;

    window.requestAnimationFrame(() => {
      const current = scrollRef.current;
      if (!current) return;
      const top = Math.max(0, current.scrollHeight - current.clientHeight);
      current.scrollTo({ top, behavior });
    });
  }, []);

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

    const distanceFromBottom = node.scrollHeight - node.scrollTop - node.clientHeight;
    onNearBottomChange?.(distanceFromBottom < NEAR_BOTTOM_THRESHOLD);
  }, [onNearBottomChange]);

  const handleScroll = useCallback(() => {
    if (rafRef.current) return;
    rafRef.current = window.requestAnimationFrame(() => {
      rafRef.current = null;
      syncViewport();
    });
  }, [syncViewport]);

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

  const handleMeasure = useCallback((key, height) => {
    if (!Number.isFinite(height) || height <= 0) return;

    const current = heightsRef.current.get(key);
    if (current !== undefined && Math.abs(current - height) < 1) return;

    heightsRef.current.set(key, height);
    setMeasurementVersion((version) => version + 1);
  }, []);

  const handleFooterMeasure = useCallback((height) => {
    if (!Number.isFinite(height) || height < 0) return;
    setFooterHeight((current) => (Math.abs(current - height) < 1 ? current : height));
  }, []);

  const layout = useMemo(() => {
    const positions = [];
    const positionsByKey = new Map();
    let top = 0;

    for (let index = 0; index < items.length; index += 1) {
      const key = itemKey(items[index], index);
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
    measurementVersion,
    footer,
    footerHeight,
  ]);

  useLayoutEffect(() => {
    if (!initialScrollToBottom || didInitialScrollRef.current || items.length === 0) return;
    didInitialScrollRef.current = true;
    scrollToBottom('auto');
  }, [initialScrollToBottom, items.length, layout.totalHeight, scrollToBottom]);

  useEffect(() => {
    if (scrollToBottomSignal === null || scrollToBottomSignal === undefined) return;
    if (!didInitialScrollRef.current && initialScrollToBottom) return;
    scrollToBottom(scrollToBottomBehavior);
  }, [initialScrollToBottom, scrollToBottom, scrollToBottomBehavior, scrollToBottomSignal]);

  useLayoutEffect(() => {
    const node = scrollRef.current;
    const prev = prevLayoutRef.current;
    prevLayoutRef.current = layout;

    if (!node) return;
    if (stickToBottom) return;
    if (!prev || prev.positions.length === 0 || layout.positions.length === 0) return;

    const currentScrollTop = node.scrollTop;

    let lo = 0;
    let hi = prev.positions.length - 1;
    let anchorIdx = 0;
    while (lo <= hi) {
      const mid = (lo + hi) >> 1;
      if (prev.positions[mid].top <= currentScrollTop) {
        anchorIdx = mid;
        lo = mid + 1;
      } else {
        hi = mid - 1;
      }
    }

    const anchor = prev.positions[anchorIdx];
    const newPos = layout.positionsByKey.get(anchor.key);
    if (!newPos) return;

    const delta = newPos.top - anchor.top;
    if (Math.abs(delta) >= 1) {
      node.scrollTop = currentScrollTop + delta;
    }
  }, [layout, stickToBottom]);

  useLayoutEffect(() => {
    if (stickToBottom) {
      scrollToBottom('auto');
    }
  }, [layout.totalHeight, scrollToBottom, stickToBottom]);

  const visibleItems = [];
  for (let index = layout.start; index <= layout.end; index += 1) {
    if (!items[index]) continue;
    visibleItems.push({ item: items[index], index, position: layout.positions[index] });
  }

  return (
    <div ref={scrollRef} className={className} onScroll={handleScroll}>
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
});

VirtualizedList.displayName = 'VirtualizedList';

export default VirtualizedList;
