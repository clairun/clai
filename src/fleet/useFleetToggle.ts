import { useEffect } from 'react';

/**
 * Ctrl+Shift+F (Cmd on macOS) switches between the Fleet and Workspace mode;
 * `onToggle` says where to. Capture phase, so a focused terminal doesn't eat
 * it as a control character.
 */
export const useFleetToggle = (onToggle: (() => void) | null) => {
  useEffect(() => {
    if (!onToggle) return undefined;
    const onKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || !e.shiftKey || e.altKey || e.code !== 'KeyF') return;
      e.preventDefault();
      e.stopPropagation();
      onToggle();
    };
    window.addEventListener('keydown', onKeyDown, true);
    return () => window.removeEventListener('keydown', onKeyDown, true);
  }, [onToggle]);
};
