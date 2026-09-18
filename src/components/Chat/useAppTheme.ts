import { useEffect, useState } from 'react';
import type { ResolvedTheme } from '../../theme';

const resolveAppTheme = (): ResolvedTheme =>
  document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';

// One MutationObserver for the whole app, alive while anyone subscribes. A
// screen full of agent faces used to mean one observer per face; they all
// watch the same attribute on the same element.
const listeners = new Set<(theme: ResolvedTheme) => void>();
let observer: MutationObserver | null = null;

const subscribe = (listener: (theme: ResolvedTheme) => void): (() => void) => {
  listeners.add(listener);
  if (!observer) {
    observer = new MutationObserver(() => {
      const theme = resolveAppTheme();
      listeners.forEach((notify) => notify(theme));
    });
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
  }
  return () => {
    listeners.delete(listener);
    if (listeners.size === 0 && observer) {
      observer.disconnect();
      observer = null;
    }
  };
};

/**
 * The app theme currently applied to `<html data-theme>`, kept live across
 * Settings toggles and OS changes (with the "system" preference). Renderers
 * that draw outside the CSS cascade — mermaid, vega, agent faces — re-render
 * on change.
 */
export const useAppTheme = (): ResolvedTheme => {
  const [appTheme, setAppTheme] = useState<ResolvedTheme>(resolveAppTheme);

  useEffect(() => {
    // The attribute may have flipped between the first render and subscribing.
    const unsubscribe = subscribe(setAppTheme);
    const current = resolveAppTheme();
    // eslint-disable-next-line react-hooks/set-state-in-effect -- catch-up for a theme change that raced the subscription; it only fires when the value actually differs.
    setAppTheme((prev) => (prev === current ? prev : current));
    return unsubscribe;
  }, []);

  return appTheme;
};
