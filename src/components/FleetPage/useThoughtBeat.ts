/**
 * The thought clouds' dots, stepped from a timer instead of a CSS animation:
 * an endless animation keeps WebKitGTK repainting the whole window every
 * frame (about 13% of a core), while a step every `BEAT_MS` repaints three
 * small dots. It runs only while something is working and the window is
 * visible, and not at all under `prefers-reduced-motion`; the dots are then
 * drawn still.
 */
import { useEffect, type RefObject } from 'react';
import styles from './FleetPage.module.css';

export const BEAT_MS = 600;

export function useThoughtBeat(ref: RefObject<HTMLElement | null>, active: boolean) {
  useEffect(() => {
    if (!active) return undefined;
    const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)');
    const clouds = () => ref.current?.querySelectorAll(`.${styles.think}`) ?? [];
    let beat = 0;
    const id = setInterval(() => {
      if (document.visibilityState === 'hidden' || reduce?.matches) return;
      beat = (beat + 1) % 3;
      clouds().forEach((cloud) => cloud.setAttribute('data-beat', String(beat)));
    }, BEAT_MS);
    return () => {
      clearInterval(id);
      clouds().forEach((cloud) => cloud.removeAttribute('data-beat'));
    };
  }, [ref, active]);
}
