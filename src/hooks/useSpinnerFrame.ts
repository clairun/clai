/**
 * Shared discrete-step ticker for "busy" indicators.
 *
 * ## Why this is not a CSS animation
 *
 * On Linux, Tauri renders through `webkit2gtk-4.1`, which is a **GTK3** API,
 * and GTK3 has no GPU scene graph (that arrived in GTK4 as GSK). So the app
 * window is composited by cairo/pixman on the CPU: WebKit renders the page to
 * a GL texture on the GPU, then `gdk_cairo_draw_from_gl` pulls it back into a
 * CPU cairo surface, which is `pixman_fill`ed and blitted. Every frame costs a
 * full-window repaint on the main thread:
 *
 *   gdk_window_begin_draw_frame
 *     -> gdk_window_create_similar_surface   (fresh full-window surface)
 *     -> pixman_fill                          (solid fill)
 *     -> gdk_cairo_draw_from_gl               (GPU -> CPU readback)
 *
 * A *registered* CSS animation — even a transform-only one that WebKit
 * promotes to its own compositing layer — keeps WebKit's compositor frame loop
 * running at vblank for as long as it is mounted. Profiling the running app
 * showed ~56 of those repaints/sec and ~13% of a core with a run in progress,
 * against 0/sec and ~1.2% once the spinner unmounted. That single always-on
 * animation was the app's dominant CPU cost.
 *
 * Slowing the animation down does **not** help: a `steps()` timing function
 * changes the computed value, not the tick rate, so the frame loop stays
 * pinned at vblank either way (measured — quantising bought nothing). Only
 * having *no running animation* lets the loop go idle.
 *
 * So the rotation is driven from JS instead, at a rate we choose. 8 updates a
 * second still reads as continuous motion but asks for ~8 repaints/sec rather
 * than ~56.
 *
 * ## Why one module-level timer
 *
 * Several indicators can be on screen at once (the run footer, plus a spinner
 * per in-flight tool row). Each repaint costs a whole-window blit regardless of
 * how small the moving element is, so what matters is the number of *distinct*
 * update ticks, not how many things move on each one. A single shared timer
 * means N spinners cost one interval and one React render pass, all landing on
 * the same repaint, instead of N unsynchronised ones.
 */

import { useEffect, useState } from 'react';

/** Rotation steps in a full turn. 12 x 30 degrees reads as smooth at 8Hz. */
export const SPINNER_STEPS = 12;

/** 125ms ~= 8 updates/sec. See the module comment for why this is not 60. */
const TICK_MS = 125;

type Listener = (frame: number) => void;

const listeners = new Set<Listener>();
let timer: ReturnType<typeof setInterval> | null = null;
let frame = 0;

const isHidden = () => typeof document !== 'undefined' && document.hidden;

const tick = () => {
  frame = (frame + 1) % SPINNER_STEPS;
  for (const listener of listeners) listener(frame);
};

/**
 * Start or stop the shared timer to match demand. Called on every
 * subscribe/unsubscribe and on visibility changes, so the timer exists only
 * while something is both mounted and actually visible — a backgrounded window
 * should not be asking for repaints at all.
 */
const sync = () => {
  const wanted = listeners.size > 0 && !isHidden();
  if (wanted && timer === null) {
    timer = setInterval(tick, TICK_MS);
  } else if (!wanted && timer !== null) {
    clearInterval(timer);
    timer = null;
  }
};

if (typeof document !== 'undefined') {
  document.addEventListener('visibilitychange', sync);
}

/**
 * Subscribe to the shared ticker and re-render on each step.
 *
 * Returns the current frame index in `[0, SPINNER_STEPS)`. Pair it with
 * {@link spinnerRotation} to turn it into a `transform`.
 */
export const useSpinnerFrame = (): number => {
  const [current, setCurrent] = useState(frame);

  useEffect(() => {
    listeners.add(setCurrent);
    sync();
    return () => {
      listeners.delete(setCurrent);
      sync();
    };
  }, []);

  return current;
};

/** Turn a frame index into the `transform` value for that step. */
export const spinnerRotation = (step: number): string =>
  `rotate(${(step * 360) / SPINNER_STEPS}deg)`;
