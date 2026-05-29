/**
 * Theme preference + application.
 *
 * The visual theme is driven entirely by CSS tokens: `theme-light.css`
 * defines defaults under `:root`, `theme-dark.css` overrides them under
 * `:root[data-theme="dark"]`. This module's only job is to set the
 * `data-theme` attribute on <html> from the user's preference.
 *
 * Preference is one of light | dark | system. "system" follows the OS via
 * `prefers-color-scheme` and updates live when the OS theme changes.
 * Stored in localStorage so it can be applied before first paint (no flash)
 * without an async round-trip.
 */

export type ThemePreference = 'light' | 'dark' | 'system';
export type ResolvedTheme = 'light' | 'dark';

const STORAGE_KEY = 'clai.theme';
const DARK_QUERY = '(prefers-color-scheme: dark)';

export function getThemePreference(): ThemePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'light' || stored === 'dark' || stored === 'system') {
      return stored;
    }
  } catch {
    /* localStorage unavailable — fall through to default */
  }
  return 'system';
}

function systemPrefersDark(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function' &&
    window.matchMedia(DARK_QUERY).matches
  );
}

export function resolveTheme(pref: ThemePreference): ResolvedTheme {
  if (pref === 'system') return systemPrefersDark() ? 'dark' : 'light';
  return pref;
}

function applyResolved(pref: ThemePreference): void {
  if (typeof document === 'undefined') return;
  document.documentElement.setAttribute('data-theme', resolveTheme(pref));
}

let mediaListenerAttached = false;

/**
 * Apply the stored preference and (once) start following the OS theme while
 * the preference is "system". Call as early as possible (before render).
 */
export function initTheme(): void {
  applyResolved(getThemePreference());

  if (
    !mediaListenerAttached &&
    typeof window !== 'undefined' &&
    typeof window.matchMedia === 'function'
  ) {
    const mql = window.matchMedia(DARK_QUERY);
    const onChange = () => {
      if (getThemePreference() === 'system') applyResolved('system');
    };
    // addEventListener is the modern API; older WebKit only has addListener.
    if (typeof mql.addEventListener === 'function') {
      mql.addEventListener('change', onChange);
    } else if (typeof mql.addListener === 'function') {
      mql.addListener(onChange);
    }
    mediaListenerAttached = true;
  }
}

/** Persist and immediately apply a new theme preference. */
export function setThemePreference(pref: ThemePreference): void {
  try {
    localStorage.setItem(STORAGE_KEY, pref);
  } catch {
    /* non-fatal — still apply for this session */
  }
  applyResolved(pref);
}
