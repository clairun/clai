import { act, fireEvent, render } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import VirtualizedList from './VirtualizedList';

type TestItem = {
  id: string;
  height: number;
  label: string;
};

class ResizeObserverMock {
  readonly targets = new Set<Element>();

  constructor(private readonly callback: ResizeObserverCallback) {
    resizeObservers.push(this);
  }

  observe(target: Element) {
    this.targets.add(target);
  }

  disconnect() {
    this.targets.clear();
  }

  trigger() {
    this.callback([], this as unknown as ResizeObserver);
  }
}

let resizeObservers: ResizeObserverMock[] = [];
let rectSpy: ReturnType<typeof vi.spyOn>;

const itemKey = (item: TestItem) => item.id;

const renderItem = (item: TestItem) => (
  <div data-testid="row" data-measure-height={item.height}>
    {item.label}
  </div>
);

const list = (height: number, throttledMeasureKeys?: ReadonlySet<string>) => (
  <VirtualizedList
    items={[{ id: 'row-1', height, label: 'Measured row' }]}
    itemKey={itemKey}
    renderItem={renderItem}
    className="virtual-list"
    estimateSize={1}
    overscan={100}
    throttledMeasureKeys={throttledMeasureKeys}
    measureThrottleMs={250}
  />
);

const getSizer = (container: HTMLElement) => {
  const viewport = container.querySelector('.virtual-list');
  const sizer = viewport?.firstElementChild;
  if (!(sizer instanceof HTMLElement)) {
    throw new Error('VirtualizedList sizer not found');
  }
  return sizer;
};

// Re-measure every mounted row. Rows only measure on mount and on a resize
// notification, so a rerender with taller rows is invisible to the list until
// their observers fire — which is how real rows grow from estimateSize to
// their real height.
const triggerRowResizes = () => {
  const rowObservers = resizeObservers.filter((observer) =>
    Array.from(observer.targets).some((target) => (
      target.isConnected
      && target.firstElementChild?.getAttribute('data-testid') === 'row'
    ))
  );
  if (rowObservers.length === 0) {
    throw new Error('No measured row ResizeObserver found');
  }
  rowObservers.forEach((observer) => observer.trigger());
};

const mockRowHeights = () =>
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(
    function getBoundingClientRect(this: HTMLElement) {
      const child = this.firstElementChild;
      const rawHeight = child instanceof HTMLElement
        ? child.getAttribute('data-measure-height')
        : null;
      const height = rawHeight ? Number(rawHeight) : 0;
      return {
        x: 0,
        y: 0,
        width: 0,
        height,
        top: 0,
        left: 0,
        right: 0,
        bottom: height,
        toJSON: () => ({}),
      } as DOMRect;
    }
  );

describe('VirtualizedList measurement throttling', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resizeObservers = [];
    vi.stubGlobal('ResizeObserver', ResizeObserverMock);
    rectSpy = mockRowHeights();
  });

  afterEach(() => {
    rectSpy.mockRestore();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('throttles resize measurements for selected keys and measures immediately when unthrottled', () => {
    const throttled = new Set(['row-1']);
    const { container, rerender } = render(list(100, throttled));
    const sizer = getSizer(container);

    expect(sizer.style.height).toBe('100px');

    rerender(list(200, throttled));
    act(() => triggerRowResizes());

    expect(sizer.style.height).toBe('100px');

    act(() => vi.advanceTimersByTime(249));
    expect(sizer.style.height).toBe('100px');

    act(() => vi.advanceTimersByTime(1));
    expect(sizer.style.height).toBe('200px');

    rerender(list(320, new Set()));
    expect(sizer.style.height).toBe('320px');
  });

  it('measures unthrottled resize notifications immediately', () => {
    const { container, rerender } = render(list(80));
    const sizer = getSizer(container);

    expect(sizer.style.height).toBe('80px');

    rerender(list(180));
    act(() => triggerRowResizes());

    expect(sizer.style.height).toBe('180px');
  });
});

// ── Stick-to-bottom ───────────────────────────────────────────────────────
// jsdom does no layout, so scrollHeight/clientHeight are always 0 and the
// pin effect's `scrollHeight - clientHeight` is 0 — every assertion about
// scrollTop would pass vacuously. Give the scroll container real geometry:
// its content height comes from the sizer element VirtualizedList sizes to
// the measured rows, clamped to the viewport the way `minHeight: 100%` does.
const VIEWPORT_HEIGHT = 300;
const ROW_HEIGHT = 100;

const isViewport = (node: HTMLElement) => node.classList.contains('virtual-list');

const stubLayoutGeometry = () => {
  Object.defineProperty(HTMLElement.prototype, 'clientHeight', {
    configurable: true,
    get(this: HTMLElement) {
      return isViewport(this) ? VIEWPORT_HEIGHT : 0;
    },
  });
  Object.defineProperty(HTMLElement.prototype, 'scrollHeight', {
    configurable: true,
    get(this: HTMLElement) {
      if (!isViewport(this)) return 0;
      const sizer = this.firstElementChild;
      const height = sizer instanceof HTMLElement ? Number.parseFloat(sizer.style.height) : 0;
      return Math.max(Number.isFinite(height) ? height : 0, VIEWPORT_HEIGHT);
    },
  });
  // jsdom doesn't implement scrollTo; the initial-scroll path goes through it.
  Object.defineProperty(HTMLElement.prototype, 'scrollTo', {
    configurable: true,
    writable: true,
    value(this: HTMLElement, options: ScrollToOptions) {
      this.scrollTop = options.top ?? this.scrollTop;
    },
  });
};

const restoreLayoutGeometry = () => {
  Reflect.deleteProperty(HTMLElement.prototype, 'clientHeight');
  Reflect.deleteProperty(HTMLElement.prototype, 'scrollHeight');
  Reflect.deleteProperty(HTMLElement.prototype, 'scrollTo');
};

const listOfHeights = (heights: number[], initialScrollToBottom = false) => (
  <VirtualizedList
    items={heights.map((height, index) => ({
      id: `row-${index}`,
      height,
      label: `Row ${index}`,
    }))}
    itemKey={itemKey}
    renderItem={renderItem}
    className="virtual-list"
    estimateSize={1}
    overscan={5000}
    initialScrollToBottom={initialScrollToBottom}
  />
);

const scrollableList = (count: number, initialScrollToBottom = false, rowHeight = ROW_HEIGHT) =>
  listOfHeights(new Array<number>(count).fill(rowHeight), initialScrollToBottom);

const getViewport = (container: HTMLElement) => {
  const node = container.querySelector('.virtual-list');
  if (!(node instanceof HTMLElement)) {
    throw new Error('VirtualizedList viewport not found');
  }
  return node;
};

const bottomOf = (count: number) => count * ROW_HEIGHT - VIEWPORT_HEIGHT;

// Where the list actually placed a row, so assertions read the layout the
// component produced instead of recomputing it.
const rowTop = (container: HTMLElement, index: number) => {
  const row = Array.from(container.querySelectorAll('[data-testid="row"]'))
    .find((node) => node.textContent === `Row ${index}`);
  const positioned = row?.parentElement;
  if (!(positioned instanceof HTMLElement)) {
    throw new Error(`Row ${index} is not rendered`);
  }
  return Number.parseFloat(positioned.style.top);
};

describe('VirtualizedList stick-to-bottom', () => {
  let rectSpyLocal: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    vi.useFakeTimers();
    resizeObservers = [];
    vi.stubGlobal('ResizeObserver', ResizeObserverMock);
    rectSpyLocal = mockRowHeights();
    stubLayoutGeometry();
  });

  afterEach(() => {
    restoreLayoutGeometry();
    rectSpyLocal.mockRestore();
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('leaves a list that did not opt in at the top, on first layout and as content grows', () => {
    const { container, rerender } = render(scrollableList(10));
    const viewport = getViewport(container);

    expect(getSizer(container).style.height).toBe('1000px');
    expect(viewport.scrollTop).toBe(0);

    act(() => rerender(scrollableList(15)));
    act(() => vi.runAllTimers());

    expect(getSizer(container).style.height).toBe('1500px');
    expect(viewport.scrollTop).toBe(0);
  });

  it('follows new content for a list that opted into starting at the bottom', () => {
    const { container, rerender } = render(scrollableList(10, true));
    const viewport = getViewport(container);

    act(() => vi.runAllTimers());
    expect(viewport.scrollTop).toBe(bottomOf(10));

    act(() => rerender(scrollableList(15, true)));
    act(() => vi.runAllTimers());

    expect(viewport.scrollTop).toBe(bottomOf(15));
  });

  it('starts following once the user scrolls a non-opted-in list to the bottom', () => {
    const { container, rerender } = render(scrollableList(10));
    const viewport = getViewport(container);

    act(() => {
      viewport.scrollTop = bottomOf(10);
      fireEvent.scroll(viewport);
      vi.runAllTimers();
    });

    act(() => rerender(scrollableList(15)));
    act(() => vi.runAllTimers());

    expect(viewport.scrollTop).toBe(bottomOf(15));
  });

  // The seed fix is what lets a non-opted-in list reach the scroll-anchor
  // effect at all: it used to be skipped because nearBottomRef was always
  // true. At scrollTop 0 the anchor is row 0, whose top is 0 in every layout,
  // so the correction is structurally zero and unobservable — only a
  // mid-scrolled list can show that rows growing don't move the content the
  // reader is looking at.
  it('keeps the row under the viewport edge in place when rows grow mid-scroll', () => {
    const GROWN_HEIGHT = 150;
    const { container, rerender } = render(scrollableList(20));
    const viewport = getViewport(container);

    expect(getSizer(container).style.height).toBe('2000px');

    // Park the reader mid-list: row 8 (top 800) crosses the viewport's top
    // edge, 50px above it.
    const anchorIndex = 8;
    act(() => {
      viewport.scrollTop = 850;
      fireEvent.scroll(viewport);
      vi.runAllTimers();
    });
    const offsetBefore = rowTop(container, anchorIndex) - viewport.scrollTop;
    expect(offsetBefore).toBe(-50);

    act(() => rerender(scrollableList(20, false, GROWN_HEIGHT)));
    act(() => {
      triggerRowResizes();
      vi.runAllTimers();
    });

    expect(getSizer(container).style.height).toBe(`${20 * GROWN_HEIGHT}px`);
    expect(rowTop(container, anchorIndex)).toBe(anchorIndex * GROWN_HEIGHT);
    expect(rowTop(container, anchorIndex) - viewport.scrollTop).toBe(offsetBefore);
    expect(viewport.scrollTop).toBe(1250);
  });

  // Constructed probe for the anchor effect's near-bottom early return, which
  // no production scenario can expose. A reader at the bottom is owned by the
  // pin effect, and on any update that changes the total height the pin runs
  // after the anchor and overwrites it — so the guard is only observable on a
  // reflow that leaves the total unchanged, i.e. one row growing by exactly
  // what another loses. Real rows are measured independently in sub-pixel
  // floats and never cancel exactly, so this pins the guard, not a user
  // scenario.
  // It pins it only while both re-measurements land in the same commit:
  // triggerRowResizes fires every row observer inside one act(), so React
  // batches them into a single render and the list never sees the intermediate
  // 1950px total. If measurement is ever staggered across commits, the pin
  // effect would re-run on that total and put the view back at the bottom, and
  // this test would keep passing with the guard deleted — so if it starts
  // flaking, look at measurement batching before the component.
  it('does not anchor a list that is following the bottom when rows reflow', () => {
    const heights = new Array<number>(20).fill(ROW_HEIGHT);
    const { container, rerender } = render(listOfHeights(heights, true));
    const viewport = getViewport(container);

    act(() => vi.runAllTimers());
    expect(viewport.scrollTop).toBe(bottomOf(20));
    expect(rowTop(container, 17)).toBe(1700);

    const reflowed = [...heights];
    reflowed[0] = ROW_HEIGHT - 50;
    reflowed[17] = ROW_HEIGHT + 50;

    act(() => rerender(listOfHeights(reflowed, true)));
    act(() => {
      triggerRowResizes();
      vi.runAllTimers();
    });

    // The reflow really landed — row 17 moved up by the 50px row 0 lost —
    // while the total height, and so the pin effect, never changed.
    expect(rowTop(container, 17)).toBe(1650);
    expect(getSizer(container).style.height).toBe('2000px');
    expect(viewport.scrollTop).toBe(bottomOf(20));
  });
});
