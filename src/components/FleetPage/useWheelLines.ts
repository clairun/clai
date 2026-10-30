import { useEffect, type RefObject } from 'react';

/**
 * While `scrolls`, a wheel notch moves the scroller one of its lines, as many
 * as notches, so the line read stays in view: the browser's own notch is a
 * share of the window, taller than a bubble. A notch is a `wheelDeltaY` in
 * whole 120s, as WebKit and Chromium report wheel clicks; a trackpad's or a
 * smooth wheel's fractions keep the browser's pixels, and Ctrl its zoom; an
 * event the browser scrolls regardless (not cancelable) is left to it.
 */
export const useWheelLines = (ref: RefObject<HTMLElement | null>, scrolls: boolean) => {
  useEffect(() => {
    const scroller = ref.current;
    if (!scroller || !scrolls) return undefined;
    const onWheel = (e: WheelEvent) => {
      const notches = -(e as WheelEvent & { wheelDeltaY: number }).wheelDeltaY / 120;
      if (!e.cancelable || e.ctrlKey || !Number.isInteger(notches) || notches === 0) return;
      e.preventDefault();
      scroller.scrollTop += notches * parseFloat(getComputedStyle(scroller).lineHeight);
    };
    scroller.addEventListener('wheel', onWheel, { passive: false });
    return () => scroller.removeEventListener('wheel', onWheel);
  }, [ref, scrolls]);
};
