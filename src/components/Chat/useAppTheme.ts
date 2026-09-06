import { useEffect, useState } from 'react';
import type { ResolvedTheme } from '../../theme';

const resolveAppTheme = (): ResolvedTheme =>
  document.documentElement.getAttribute('data-theme') === 'dark' ? 'dark' : 'light';

/**
 * The app theme currently applied to `<html data-theme>`, kept live across
 * Settings toggles and OS changes (with the "system" preference). Renderers
 * that draw outside the CSS cascade — mermaid, vega — re-render on change.
 */
export const useAppTheme = (): ResolvedTheme => {
  const [appTheme, setAppTheme] = useState<ResolvedTheme>(resolveAppTheme);

  useEffect(() => {
    const observer = new MutationObserver(() => setAppTheme(resolveAppTheme()));
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
    });
    return () => observer.disconnect();
  }, []);

  return appTheme;
};
