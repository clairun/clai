import { useLayoutEffect, useRef, type RefObject } from 'react';
import { moves } from './FleetZoom';

const SLIDE_MS = 300;

/**
 * Puts the scroller at its end if it was there by the end last kept, and
 * keeps the new end; returns how far it scrolled. The end is the content's
 * height, not `scrollHeight`, which a slide's transform stretches.
 */
const keepEnd = (scroller: HTMLElement, end: RefObject<number | null>): number => {
  const from = scroller.scrollTop;
  const atEnd = end.current === null || from >= end.current - 1;
  end.current = Math.max(0, (scroller.firstElementChild as HTMLElement).offsetHeight - scroller.clientHeight);
  if (!atEnd) return 0;
  scroller.scrollTop = end.current;
  return scroller.scrollTop - from;
};

/**
 * A scroller that follows its `end`: at the end, new text scrolls into view and
 * its lines slide in from below, and a new size keeps the end in view;
 * scrolled up, it stays until the reader is back at the end. Once it no
 * longer follows, it shows its `start`; the `reader`'s (an open bubble) starts
 * there and is then theirs to scroll as it grows.
 */
export const useFollowEnd = (anchor: 'end' | 'start' | 'reader', text: string) => {
  const following = anchor === 'end';
  /** Only the end moves with new text. */
  const tracked = following ? text : null;
  const ref = useRef<HTMLDivElement | null>(null);
  /** The end's scroll position when last kept; null while not following. */
  const end = useRef<number | null>(null);

  useLayoutEffect(() => {
    const scroller = ref.current;
    if (!scroller) return;
    if (anchor !== 'end') {
      scroller.scrollTop = 0;
      end.current = null;
      return;
    }
    const followed = end.current !== null;
    const moved = keepEnd(scroller, end);
    if (followed && moved > 0 && moves()) {
      // From where it was, at most a bubble's height below; added to a slide still going.
      const content = scroller.firstElementChild as HTMLElement;
      content.animate([{ transform: `translateY(${Math.min(moved, scroller.clientHeight)}px)` }, { transform: 'none' }], {
        duration: SLIDE_MS,
        easing: 'ease-out',
        composite: 'add',
      });
    }
  }, [anchor, tracked]);

  useLayoutEffect(() => {
    const scroller = ref.current;
    if (!scroller || !following || typeof ResizeObserver === 'undefined') return undefined;
    const observer = new ResizeObserver(() => keepEnd(scroller, end));
    observer.observe(scroller);
    observer.observe(scroller.firstElementChild as HTMLElement);
    return () => observer.disconnect();
  }, [following]);

  return ref;
};
