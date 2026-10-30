/**
 * The Fleet's clock, ticking once a minute while the window is visible (and
 * once on becoming visible again), so captions age and a finished task's
 * cheer fades without waiting for the next event.
 */
import { useSyncExternalStore } from 'react';

const MINUTE_TICK_MS = 60_000;

let clock = Date.now();
const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | undefined;

const tick = () => {
  if (document.visibilityState === 'hidden') return;
  clock = Date.now();
  listeners.forEach((listener) => listener());
};

const subscribe = (listener: () => void) => {
  listeners.add(listener);
  if (timer === undefined) {
    clock = Date.now();
    timer = setInterval(tick, MINUTE_TICK_MS);
    document.addEventListener('visibilitychange', tick);
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size > 0 || timer === undefined) return;
    clearInterval(timer);
    timer = undefined;
    document.removeEventListener('visibilitychange', tick);
  };
};

/** The time, to the last minute tick. */
export const useMinute = (): number => useSyncExternalStore(subscribe, () => clock);
