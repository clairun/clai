import { useEffect, useRef } from 'react';

/**
 * Registers a body-portaled overlay while it is open, so the app can answer
 * the two questions no single overlay can answer alone.
 *
 * Escape must dismiss only the topmost one. These modals portal to `<body>`
 * and listen on `document`, so one keypress reaches all of them at once and
 * nothing in the DOM says which is on top — with two stacked (global Settings
 * over workspace Settings, a form modal over Settings) a plain listener in
 * each closes both. Overlays register here in the order they open, which is
 * the order they stack: one opened from inside another opens later. Only the
 * last registered acts on the key.
 *
 * Body scroll must stay locked until the last one closes. Each modal used to
 * restore `overflow` on its own close, which unlocked the page behind a modal
 * that was still open.
 *
 * Visual order is the `--z-portal-*` scale in theme-light.css; this is its
 * behavioural counterpart, so the layer that paints on top is the one that
 * takes Escape.
 */
const openOverlays: object[] = [];

export const useOverlayLayer = (active: boolean, onEscape: () => void): void => {
  // Kept in a ref so a fresh handler identity does not re-register the
  // overlay: that would move it back to the top of the stack while something
  // else is stacked above it.
  const handler = useRef(onEscape);
  useEffect(() => {
    handler.current = onEscape;
  }, [onEscape]);

  useEffect(() => {
    if (!active) return undefined;
    const token = {};
    openOverlays.push(token);
    document.body.style.overflow = 'hidden';

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key !== 'Escape') return;
      if (openOverlays[openOverlays.length - 1] !== token) return;
      handler.current();
    };
    document.addEventListener('keydown', onKeyDown);

    return () => {
      document.removeEventListener('keydown', onKeyDown);
      openOverlays.splice(openOverlays.indexOf(token), 1);
      if (openOverlays.length === 0) document.body.style.overflow = '';
    };
  }, [active]);
};
