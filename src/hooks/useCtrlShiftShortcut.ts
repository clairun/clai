import { useEffect } from 'react';

/**
 * Runs `onPress` on Ctrl+Shift+`code` (Cmd works for Ctrl, so macOS gets
 * Cmd+Shift); with Alt it is a different chord. `code` is the physical key
 * (`'KeyM'`), so the chord is layout independent. Capture phase, and the key
 * is consumed, so it works whatever has focus (composer, terminal) and never
 * reaches it. A null `onPress` leaves the key alone.
 */
export const useCtrlShiftShortcut = (code: string, onPress: (() => void) | null) => {
  useEffect(() => {
    if (!onPress) return undefined;
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey || e.altKey || e.code !== code) return;
      e.preventDefault();
      e.stopPropagation();
      // Held down, the chord acts once; its repeats are still swallowed.
      if (!e.repeat) onPress();
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [code, onPress]);
};
