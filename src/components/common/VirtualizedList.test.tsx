import { act, render } from '@testing-library/react';
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

const triggerRowResize = () => {
  const rowObserver = resizeObservers.find((observer) =>
    Array.from(observer.targets).some((target) => (
      target instanceof HTMLElement
      && target.firstElementChild?.getAttribute('data-testid') === 'row'
    ))
  );
  if (!rowObserver) {
    throw new Error('Measured row ResizeObserver not found');
  }
  rowObserver.trigger();
};

describe('VirtualizedList measurement throttling', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    resizeObservers = [];
    vi.stubGlobal('ResizeObserver', ResizeObserverMock);
    rectSpy = vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(
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
    act(() => triggerRowResize());

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
    act(() => triggerRowResize());

    expect(sizer.style.height).toBe('180px');
  });
});
